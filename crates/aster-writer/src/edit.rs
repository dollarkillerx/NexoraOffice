//! Editing operations on a [`Document`]: typing, paragraphs, deleting,
//! formatting, lists, copying and undo.
//!
//! Everything here is a plain function on the model. There is no layout and
//! no toolkit: the view decides what a click, a key or a word is, and calls
//! these with positions.
//!
//! **Positions.** A [`Pos`] is a paragraph and a byte offset into its text.
//! Offsets are put on a `char` boundary; an offset inside a picture, a page
//! number or any other object is moved to the object's start (to its end
//! where a range's end is meant). A path that leads to no paragraph makes the
//! operation do nothing. No operation panics on a bad position.
//!
//! **What stays shared.** Only the paragraphs an operation changes are
//! copied; every other block stays the same [`Arc`], so layout caches and
//! saving see what has not changed. After an operation that adds, removes or
//! joins paragraphs or changes lists, [`Document::renumber`] has run.
//!
//! **Markers are never removed.** [`Inline::Marker`] runs (bookmarks, comment
//! ranges, field codes, tracked deletions) show nothing and take no room, so
//! a deletion cannot cover them. Where text around them goes, they stay at
//! the place the text was; where paragraphs are joined or removed, their
//! markers move into the paragraph that remains.
//!
//! **Where it works.** In the body, table cells included. Headers, footers,
//! notes and text boxes are not edited in this version.

use std::sync::Arc;

use crate::{
    Align, Block, Break, Document, Inline, LineSpacing, List, ListLevel, ListRef, NumberFormat,
    Numbering, Paragraph, Path, Pos, Run, RunLook, RunPatch, Suffix, Twips,
};

/// How far one step of indenting moves a paragraph, in twips (half an inch).
pub const INDENT_STEP: Twips = 720;

// ---------------------------------------------------------------------------
// Selections
// ---------------------------------------------------------------------------

/// The text a user has selected: where it was started and where the caret is
/// now. The two are in either order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: Pos,
    pub focus: Pos,
}

impl Selection {
    pub fn new(anchor: Pos, focus: Pos) -> Self {
        Self { anchor, focus }
    }

    /// A caret: nothing selected.
    pub fn caret(pos: Pos) -> Self {
        Self {
            anchor: pos.clone(),
            focus: pos,
        }
    }

    /// The start and the end, in reading order.
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.focus {
            (self.anchor.clone(), self.focus.clone())
        } else {
            (self.focus.clone(), self.anchor.clone())
        }
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.focus
    }
}

// ---------------------------------------------------------------------------
// Small helpers on paragraphs
// ---------------------------------------------------------------------------

/// `offset` on a boundary the text can be cut at: a `char` boundary, and not
/// inside an object (a picture, a page number, a ruby). Inside an object it
/// is the object's start, or, with `end_side`, its end.
fn snap(p: &Paragraph, offset: usize, end_side: bool) -> usize {
    let offset = offset.min(p.len());
    let mut start = 0;
    for run in &p.runs {
        let end = start + run.text.len();
        if offset <= start {
            break;
        }
        if offset < end {
            if run.is_text() {
                let mut at = offset - start;
                while !run.text.is_char_boundary(at) {
                    if end_side {
                        at += 1;
                    } else {
                        at -= 1;
                    }
                }
                return start + at;
            }
            return if end_side { end } else { start };
        }
        start = end;
    }
    offset
}

/// Where in `runs` a run that starts at `offset` goes, `offset` being on a
/// run boundary. At the end every marker is before it, at the start every
/// marker is after it; in between the markers that follow the text before
/// stay behind it only when they come before the next text.
fn cut_index(p: &Paragraph, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    if offset >= p.len() {
        return p.runs.len();
    }
    let mut start = 0;
    let mut index = 0;
    for (i, run) in p.runs.iter().enumerate() {
        let end = start + run.text.len();
        if !run.text.is_empty() {
            if end <= offset {
                index = i + 1;
            } else {
                break;
            }
        }
        start = end;
    }
    index
}

/// Make `offset` a boundary between runs, splitting a text run if it is
/// inside one; the index at which a run starting there goes.
fn split_run(p: &mut Paragraph, offset: usize) -> usize {
    let offset = snap(p, offset, false);
    let mut start = 0;
    for i in 0..p.runs.len() {
        let end = start + p.runs[i].text.len();
        if offset > start && offset < end {
            let tail = p.runs[i].text.split_off(offset - start);
            let mut right = p.runs[i].clone();
            right.text = tail;
            p.runs.insert(i + 1, right);
            return i + 1;
        }
        start = end;
    }
    cut_index(p, offset)
}

/// Neighbouring text runs that have the same look and are one run's pieces
/// (or were both made here) become one again.
fn merge_runs(p: &mut Paragraph) {
    let mut i = 0;
    while i + 1 < p.runs.len() {
        let (a, b) = (&p.runs[i], &p.runs[i + 1]);
        let joinable = a.is_text()
            && b.is_text()
            && a.same_look(b)
            && match (&a.source, &b.source) {
                (None, None) => true,
                (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                _ => false,
            };
        if joinable {
            let next = p.runs.remove(i + 1);
            p.runs[i].text.push_str(&next.text);
        } else {
            i += 1;
        }
    }
}

/// Take text `a..b` out of a paragraph. Objects the range touches go whole;
/// markers stay. The offset the text now starts at.
fn remove_range(p: &mut Paragraph, a: usize, b: usize) -> usize {
    let a = snap(p, a, false);
    let b = snap(p, b, true).max(a);
    if a == b {
        return a;
    }
    let mut start = 0;
    let mut out: Vec<Run> = Vec::with_capacity(p.runs.len());
    for mut run in std::mem::take(&mut p.runs) {
        let end = start + run.text.len();
        if run.text.is_empty() || end <= a || start >= b {
            out.push(run);
        } else if a <= start && end <= b {
            // Covered whole: dropped.
        } else if run.is_text() {
            let from = a.saturating_sub(start);
            let to = (b - start).min(run.text.len());
            run.text.replace_range(from..to, "");
            out.push(run);
        } else {
            // An object the range only touches goes whole, but snapping has
            // made that impossible: keep it.
            out.push(run);
        }
        start = end;
    }
    p.runs = out;
    merge_runs(p);
    a
}

fn markers_of(p: &Paragraph) -> impl Iterator<Item = &Run> {
    p.runs
        .iter()
        .filter(|run| matches!(run.kind, Inline::Marker))
}

/// Every marker under `block`, in reading order.
fn collect_markers(block: &Block, out: &mut Vec<Run>) {
    match block {
        Block::Paragraph(p) => out.extend(markers_of(p).cloned()),
        Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for block in &cell.blocks {
                        collect_markers(block, out);
                    }
                }
            }
        }
    }
}

macro_rules! rebase_fields {
    ($look:ident, $old:ident, $new:ident: $($field:ident),+) => {
        $( if $look.$field == $old.$field { $look.$field = $new.$field.clone(); } )+
    };
}

/// Where a look had what its paragraph's `old` base gave it, it now has what
/// `new` gives: it was inherited, so it follows. What the run set itself
/// stays.
fn rebase_look(look: &mut RunLook, old: &RunLook, new: &RunLook) {
    rebase_fields!(
        look, old, new: font, east_asian_font, size, bold, italic, underline, strike,
        double_strike, color, highlight, shading, position, caps, small_caps, hidden, spacing,
        scale, raise
    );
}

fn take_paragraph(arc: Arc<Paragraph>) -> Paragraph {
    Arc::try_unwrap(arc).unwrap_or_else(|shared| (*shared).clone())
}

/// `pos` with a valid path and an offset on a boundary, or `None`.
fn locate(doc: &Document, pos: &Pos) -> Option<Pos> {
    let p = doc.paragraph(&pos.path)?;
    Some(Pos::new(pos.path.clone(), snap(p, pos.offset, false)))
}

/// The paragraphs from `from` to `to` (both inclusive), by path.
fn paths_between(doc: &Document, from: &Path, to: &Path) -> Vec<Path> {
    doc.paragraphs()
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path >= from && path <= to)
        .collect()
}

/// The two ends of a range in order, each on a boundary: the start is moved
/// back out of a char or an object, the end forward; `None` if either path
/// is bad.
fn ends(doc: &Document, from: &Pos, to: &Pos) -> Option<(Pos, Pos)> {
    let (a, b) = if from <= to { (from, to) } else { (to, from) };
    let start = Pos::new(
        a.path.clone(),
        snap(doc.paragraph(&a.path)?, a.offset, false),
    );
    let end = Pos::new(
        b.path.clone(),
        snap(doc.paragraph(&b.path)?, b.offset, true),
    );
    Some((start, end))
}

// ---------------------------------------------------------------------------
// Typing
// ---------------------------------------------------------------------------

/// Put `text` at `at` and return where the caret goes.
///
/// `\n` starts a new paragraph as [`split_paragraph`] does, `\t` is a tab,
/// other control characters (and U+FFFC) are dropped. The text takes `look`,
/// or with `None` the look text typed there has ([`Paragraph::look_at`]). It
/// extends the text run it is typed into or at the end of when that has the
/// same look (so a run that was read keeps its source and merely has other
/// text); otherwise it becomes a run of its own, which does not take a
/// hyperlink the look was given explicitly against.
pub fn insert_text(doc: &mut Document, at: &Pos, text: &str, look: Option<&RunLook>) -> Pos {
    let Some(mut pos) = locate(doc, at) else {
        return at.clone();
    };
    let mut chunk = String::new();
    for c in text.chars() {
        match c {
            '\n' => {
                pos = flush(doc, pos, &mut chunk, look);
                pos = split_paragraph(doc, &pos);
            }
            '\t' => {
                pos = flush(doc, pos, &mut chunk, look);
                pos = insert_object(doc, &pos, Inline::Tab, look);
            }
            '\u{FFFC}' | '\u{FFFE}' | '\u{FFFF}' => {}
            c if c.is_control() => {}
            c => chunk.push(c),
        }
    }
    flush(doc, pos, &mut chunk, look)
}

fn flush(doc: &mut Document, pos: Pos, chunk: &mut String, look: Option<&RunLook>) -> Pos {
    if chunk.is_empty() {
        return pos;
    }
    let text = std::mem::take(chunk);
    insert_plain(doc, &pos, &text, look)
}

/// Text with no control characters, into one paragraph.
fn insert_plain(doc: &mut Document, at: &Pos, text: &str, look: Option<&RunLook>) -> Pos {
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return at.clone();
    };
    let offset = snap(p, at.offset, false);
    let want = look.cloned().unwrap_or_else(|| p.look_at(offset).clone());

    // The run that has the place at its end or inside, and the one that
    // starts there.
    let (mut before, mut after) = (None, None);
    let mut start = 0;
    for (i, run) in p.runs.iter().enumerate() {
        let end = start + run.text.len();
        if !run.text.is_empty() {
            if offset > start && offset <= end {
                before = Some((i, start));
            }
            if offset == start && after.is_none() {
                after = Some((i, start));
            }
        }
        start = end;
    }
    let result = Pos::new(at.path.clone(), offset + text.len());
    if let Some((i, run_start)) = before
        && p.runs[i].is_text()
        && p.runs[i].look == want
    {
        p.runs[i].text.insert_str(offset - run_start, text);
        return result;
    }
    if let Some((i, _)) = after
        && p.runs[i].is_text()
        && p.runs[i].look == want
    {
        p.runs[i].text.insert_str(0, text);
        return result;
    }
    let mut run = Run::text(text, want);
    if look.is_none()
        && let Some((i, _)) = before
    {
        run.style = p.runs[i].style.clone();
        run.link = p.runs[i].link.clone();
    }
    let index = split_run(p, offset);
    p.runs.insert(index, run);
    result
}

/// A tab, line break or other single thing at `at`.
fn insert_object(doc: &mut Document, at: &Pos, kind: Inline, look: Option<&RunLook>) -> Pos {
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return at.clone();
    };
    let offset = snap(p, at.offset, false);
    let want = look.cloned().unwrap_or_else(|| p.look_at(offset).clone());
    let neighbour = p.run_at(offset).map(|(i, _)| i);
    let mut run = Run::object(kind, want);
    if look.is_none()
        && let Some(i) = neighbour
    {
        run.style = p.runs[i].style.clone();
        run.link = p.runs[i].link.clone();
    }
    let size = run.text.len();
    let index = split_run(p, offset);
    p.runs.insert(index, run);
    Pos::new(at.path.clone(), offset + size)
}

/// A line, page or column break at `at`; where the caret goes.
pub fn insert_break(doc: &mut Document, at: &Pos, kind: Break) -> Pos {
    insert_object(doc, at, Inline::Break(kind), None)
}

// ---------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------

/// Enter: cut the paragraph at `at`. The second half is a new paragraph with
/// the same properties; its runs keep their sources. The paragraph mark that
/// carried a section stays at the end, so the second half carries it. At the
/// end of a paragraph whose style names a next style (a heading) the new
/// paragraph is of that style. Enter in an empty list item takes it out of
/// the list instead. Where the caret goes: the start of the second half.
pub fn split_paragraph(doc: &mut Document, at: &Pos) -> Pos {
    let Some(at) = locate(doc, at) else {
        return at.clone();
    };
    if let Some(p) = doc.paragraph(&at.path)
        && p.props.list.is_some()
        && p.is_empty()
    {
        remove_from_list(doc, &at.path);
        doc.renumber();
        return at;
    }
    split_raw(doc, &at, true)
}

/// The cut itself, without the rules for list items.
fn split_raw(doc: &mut Document, at: &Pos, next_style: bool) -> Pos {
    let styles = doc.styles.clone();
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return at.clone();
    };
    let offset = snap(p, at.offset, false);
    let at_end = offset >= p.len();
    let cut = if at_end {
        p.runs.len()
    } else {
        split_run(p, offset)
    };
    let tail: Vec<Run> = p.runs.split_off(cut);
    let mut second = Paragraph {
        runs: tail,
        props: p.props.clone(),
        base: p.base.clone(),
        mark: p.mark.clone(),
        label: None,
        section: p.section.take(),
        source: p.source.clone(),
    };
    if at_end
        && next_style
        && let Some(next) = p
            .props
            .style
            .as_deref()
            .and_then(|id| styles.get(id))
            .and_then(|style| style.next.as_deref())
            .filter(|id| Some(*id) != p.props.style.as_deref())
            .and_then(|id| styles.get(id))
            .filter(|style| style.kind == crate::StyleKind::Paragraph)
    {
        second.props = next.para.clone();
        second.base = next.run.clone();
        second.mark = next.run.clone();
        second.source = None;
    }
    let index = at.path.0.last().copied().unwrap_or(0) as usize;
    let mut path = at.path.0.clone();
    if let Some(siblings) = doc.siblings_mut(&at.path)
        && index < siblings.len()
    {
        siblings.insert(index + 1, Block::paragraph(second));
        *path.last_mut().expect("a path has steps") += 1;
    }
    doc.renumber();
    Pos::new(Path(path), 0)
}

// ---------------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------------

/// Take out the text between `from` and `to` (in either order) and return
/// the caret's place: the start of the range.
///
/// Objects the range covers or touches go whole. Markers stay. Within one
/// paragraph the text goes; across paragraphs of the same container (the body
/// or one cell) the paragraphs in between go and the two ends are joined,
/// keeping the first one's properties and the second one's section. A range
/// that covers several cells, or starts or ends outside the table it touches,
/// only empties the text it covers: no paragraphs are joined across a cell or
/// table boundary and no row is removed. A table that lies entirely inside
/// the range is removed.
pub fn delete(doc: &mut Document, from: &Pos, to: &Pos) -> Pos {
    let Some((f, t)) = ends(doc, from, to) else {
        return from.clone();
    };
    if f == t {
        return f;
    }
    if f.path == t.path {
        let Some(p) = doc.paragraph_mut(&f.path) else {
            return f;
        };
        let at = remove_range(p, f.offset, t.offset);
        return Pos::new(f.path, at);
    }
    let n = f.path.0.len();
    let same_container = n == t.path.0.len() && f.path.0[..n - 1] == t.path.0[..n - 1];
    let caret = if same_container {
        delete_joining(doc, &f, &t)
    } else {
        delete_across(doc, &f, &t)
    };
    doc.renumber();
    caret
}

/// Both ends are paragraphs of the same container: what is between them
/// goes, and they become one.
fn delete_joining(doc: &mut Document, f: &Pos, t: &Pos) -> Pos {
    let fi = f.path.0[f.path.0.len() - 1] as usize;
    let ti = t.path.0[t.path.0.len() - 1] as usize;
    let mut moved: Vec<Run> = Vec::new();
    let Some(siblings) = doc.siblings_mut(&f.path) else {
        return f.clone();
    };
    if ti >= siblings.len() || fi >= ti {
        return f.clone();
    }
    for block in &siblings[fi + 1..ti] {
        collect_markers(block, &mut moved);
    }
    siblings.drain(fi + 1..ti);
    let Block::Paragraph(last) = siblings.remove(fi + 1) else {
        return f.clone();
    };
    let mut last = take_paragraph(last);
    remove_range(&mut last, 0, t.offset);
    let Some(first) = doc.paragraph_mut(&f.path) else {
        return f.clone();
    };
    let at = remove_range(first, f.offset, usize::MAX);
    first.runs.extend(moved);
    for mut run in last.runs {
        rebase_look(&mut run.look, &last.base, &first.base);
        first.runs.push(run);
    }
    first.section = last.section;
    merge_runs(first);
    Pos::new(f.path.clone(), at)
}

/// The ends are in different containers: empty what is covered, remove what
/// is covered whole, join nothing.
fn delete_across(doc: &mut Document, f: &Pos, t: &Pos) -> Pos {
    let (bi, bj) = (f.path.top(), t.path.top());
    // The two ends' own text.
    let mut at = f.offset;
    if let Some(p) = doc.paragraph_mut(&f.path) {
        at = remove_range(p, f.offset, usize::MAX);
    }
    if let Some(p) = doc.paragraph_mut(&t.path) {
        remove_range(p, 0, t.offset);
    }
    // Whole blocks of the body between the two ends.
    let mut moved: Vec<Run> = Vec::new();
    let mut t = t.clone();
    if bj > bi + 1 && bj <= doc.body.blocks.len() {
        for block in &doc.body.blocks[bi + 1..bj] {
            collect_markers(block, &mut moved);
        }
        doc.body.blocks.drain(bi + 1..bj);
        t.path.0[0] = (bi + 1) as u32;
    }
    // Tables that are cut through: empty the paragraphs between, remove the
    // tables that are inside.
    let bj = t.path.top();
    clear_between(&mut doc.body.blocks, bi, &mut Vec::new(), f, &t, &mut moved);
    if bj != bi {
        clear_between(&mut doc.body.blocks, bj, &mut Vec::new(), f, &t, &mut moved);
    }
    if !moved.is_empty()
        && let Some(p) = doc.paragraph_mut(&f.path)
    {
        p.runs.extend(moved);
    }
    Pos::new(f.path.clone(), at)
}

/// In `blocks[index]`, if it is a table the range cuts through: empty the
/// paragraphs strictly between `f` and `t`, and take out the nested tables
/// that lie wholly between them.
fn clear_between(
    blocks: &mut [Block],
    index: usize,
    prefix: &mut Vec<u32>,
    f: &Pos,
    t: &Pos,
    moved: &mut Vec<Run>,
) {
    let mut here = prefix.clone();
    here.push(index as u32);
    // Look before copying: a table the range does not touch stays shared.
    let Some(block @ Block::Table(_)) = blocks.get(index) else {
        return;
    };
    let (first, last) = table_span(block, &here);
    if last < f.path || first > t.path {
        return;
    }
    let Some(Block::Table(table)) = blocks.get_mut(index) else {
        return;
    };
    let table = Arc::make_mut(table);
    for (r, row) in table.rows.iter_mut().enumerate() {
        for (c, cell) in row.cells.iter_mut().enumerate() {
            let mut inner = here.clone();
            inner.extend([r as u32, c as u32]);
            clear_cell(&mut cell.blocks, &mut inner, f, t, moved);
        }
    }
}

fn clear_cell(
    blocks: &mut Vec<Block>,
    prefix: &mut Vec<u32>,
    f: &Pos,
    t: &Pos,
    moved: &mut Vec<Run>,
) {
    let mut k = 0;
    while k < blocks.len() {
        let mut here = prefix.clone();
        here.push(k as u32);
        if blocks[k].as_table().is_some() {
            let (first, last) = table_span(&blocks[k], &here);
            if first > f.path && last < t.path {
                collect_markers(&blocks[k], moved);
                blocks.remove(k);
                continue;
            }
            clear_between(blocks, k, prefix, f, t, moved);
        } else if Path(here.clone()) > f.path
            && Path(here) < t.path
            && let Block::Paragraph(p) = &mut blocks[k]
            && !p.is_empty()
        {
            let p = Arc::make_mut(p);
            let len = p.len();
            remove_range(p, 0, len);
        }
        k += 1;
    }
    // A cell ends with a paragraph and has at least one.
    if blocks.last().is_none_or(|b| b.as_table().is_some()) {
        blocks.push(Block::paragraph(Paragraph::default()));
    }
}

/// The paths of the first and last paragraph of the table `block` that is
/// at `path`.
fn table_span(block: &Block, path: &[u32]) -> (Path, Path) {
    let inner = crate::paragraphs_of(std::slice::from_ref(block));
    let full = |p: &Path| {
        let mut steps = path.to_vec();
        steps.extend_from_slice(&p.0[1..]);
        Path(steps)
    };
    match (inner.first(), inner.last()) {
        (Some((a, _)), Some((b, _))) => (full(a), full(b)),
        _ => (Path(path.to_vec()), Path(path.to_vec())),
    }
}

/// Backspace: take out the char before the caret. At the start of a list item
/// the item first leaves the list; at the start of a paragraph the paragraph
/// is joined to the one before it, unless that is a table or there is none
/// in this body or cell, when nothing happens.
pub fn delete_backward(doc: &mut Document, at: &Pos) -> Pos {
    let Some(at) = locate(doc, at) else {
        return at.clone();
    };
    let Some(p) = doc.paragraph(&at.path) else {
        return at;
    };
    if at.offset > 0 {
        let text = p.text();
        let before = text[..at.offset]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
        return delete(doc, &Pos::new(at.path.clone(), before), &at);
    }
    if p.props.list.is_some() {
        remove_from_list(doc, &at.path);
        doc.renumber();
        return at;
    }
    let index = at.path.0.last().copied().unwrap_or(0) as usize;
    if index == 0 {
        return at;
    }
    let mut previous = at.path.clone();
    *previous.0.last_mut().expect("a path has steps") -= 1;
    match doc.paragraph(&previous) {
        Some(prev) => {
            let end = Pos::new(previous, prev.len());
            delete(doc, &end, &at)
        }
        None => at,
    }
}

/// Delete: take out the char after the caret; at the end of a paragraph join
/// the next one, unless that is a table or there is none in this body or
/// cell, when nothing happens.
pub fn delete_forward(doc: &mut Document, at: &Pos) -> Pos {
    let Some(at) = locate(doc, at) else {
        return at.clone();
    };
    let Some(p) = doc.paragraph(&at.path) else {
        return at;
    };
    if at.offset < p.len() {
        let text = p.text();
        let after = text[at.offset..]
            .chars()
            .next()
            .map_or(at.offset, |c| at.offset + c.len_utf8());
        delete(doc, &at, &Pos::new(at.path.clone(), after));
        return at;
    }
    let mut next = at.path.clone();
    *next.0.last_mut().expect("a path has steps") += 1;
    if doc.paragraph(&next).is_some() {
        delete(doc, &at, &Pos::new(next, 0));
    }
    at
}

// ---------------------------------------------------------------------------
// Character formatting
// ---------------------------------------------------------------------------

/// Apply `patch` to the text from `from` to `to`: runs are split at the
/// ends, the patch goes on the text and object runs inside, and neighbouring
/// runs that end up the same become one where that loses nothing. A paragraph
/// wholly inside the range gets the patch on its mark too, and so does an
/// empty paragraph the caret is in.
pub fn apply_run_patch(doc: &mut Document, from: &Pos, to: &Pos, patch: &RunPatch) {
    if patch.is_empty() {
        return;
    }
    let Some((f, t)) = ends(doc, from, to) else {
        return;
    };
    for path in paths_between(doc, &f.path, &t.path) {
        let Some(p) = doc.paragraph(&path) else {
            continue;
        };
        let len = p.len();
        let a = if path == f.path { f.offset } else { 0 };
        let b = if path == t.path { t.offset } else { len };
        let whole = (path > f.path || (path == f.path && f.offset == 0)) && path < t.path;
        let empty_caret = f == t && p.is_empty();
        let mark_changes = (whole || empty_caret) && changed_by(patch, &p.mark);
        let run_changes = a < b && range_changes(p, a, b, patch);
        if !mark_changes && !run_changes {
            continue;
        }
        let Some(p) = doc.paragraph_mut(&path) else {
            continue;
        };
        if mark_changes {
            patch.apply(&mut p.mark);
        }
        if run_changes {
            split_run(p, b);
            split_run(p, a);
            let mut start = 0;
            for run in &mut p.runs {
                let end = start + run.text.len();
                if !run.text.is_empty() && start >= a && end <= b {
                    patch.apply(&mut run.look);
                }
                start = end;
            }
            merge_runs(p);
        }
    }
}

fn changed_by(patch: &RunPatch, look: &RunLook) -> bool {
    let mut after = look.clone();
    patch.apply(&mut after);
    after != *look
}

/// Whether the patch changes the look of any run in `a..b`.
fn range_changes(p: &Paragraph, a: usize, b: usize, patch: &RunPatch) -> bool {
    let mut start = 0;
    for run in &p.runs {
        let end = start + run.text.len();
        if !run.text.is_empty() && start < b && end > a && changed_by(patch, &run.look) {
            return true;
        }
        start = end;
    }
    false
}

// ---------------------------------------------------------------------------
// Paragraph formatting
// ---------------------------------------------------------------------------

/// Change the paragraphs the range touches; `change` says whether it did
/// anything, and a paragraph it did nothing to stays shared.
fn for_paragraphs(
    doc: &mut Document,
    from: &Pos,
    to: &Pos,
    mut change: impl FnMut(&mut Paragraph),
    probe: impl Fn(&Paragraph) -> bool,
) {
    let Some((f, t)) = ends(doc, from, to) else {
        return;
    };
    for path in paths_between(doc, &f.path, &t.path) {
        let wanted = doc.paragraph(&path).is_some_and(&probe);
        if wanted && let Some(p) = doc.paragraph_mut(&path) {
            change(p);
        }
    }
}

pub fn set_align(doc: &mut Document, from: &Pos, to: &Pos, align: Align) {
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            p.props.align = align;
        },
        |p| p.props.align != align,
    );
}

/// Move the left indent by `steps` times [`INDENT_STEP`] (negative to take
/// out); never below zero.
pub fn change_indent(doc: &mut Document, from: &Pos, to: &Pos, steps: i32) {
    let target =
        |p: &Paragraph| (p.props.indent_left + steps.saturating_mul(INDENT_STEP)).clamp(0, 31_680);
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            p.props.indent_left = target(p);
        },
        |p| p.props.indent_left != target(p),
    );
}

pub fn set_line_spacing(doc: &mut Document, from: &Pos, to: &Pos, line: LineSpacing) {
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            p.props.line = line;
        },
        |p| p.props.line != line,
    );
}

pub fn set_space_before(doc: &mut Document, from: &Pos, to: &Pos, space: Twips) {
    let space = space.max(0);
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            p.props.space_before = space;
        },
        |p| p.props.space_before != space,
    );
}

pub fn set_space_after(doc: &mut Document, from: &Pos, to: &Pos, space: Twips) {
    let space = space.max(0);
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            p.props.space_after = space;
        },
        |p| p.props.space_after != space,
    );
}

/// Give the paragraphs the range touches the paragraph style `style_id`: its
/// properties and base look. Where a run's look had what the old base gave it
/// (it was inherited) it takes the new base's; what the run set stays. A name
/// that is no paragraph style does nothing.
pub fn set_style(doc: &mut Document, from: &Pos, to: &Pos, style_id: &str) {
    let styles = doc.styles.clone();
    let Some(style) = styles
        .get(style_id)
        .filter(|s| s.kind == crate::StyleKind::Paragraph)
    else {
        return;
    };
    let wanted = |p: &Paragraph| {
        let old = styles.paragraph_look(p.props.style.as_deref()).1;
        p.props != style.para || rebased(&p.base, old, &style.run) != p.base
    };
    for_paragraphs(
        doc,
        from,
        to,
        |p| {
            let old = styles.paragraph_look(p.props.style.as_deref()).1.clone();
            let base = rebased(&p.base, &old, &style.run);
            for run in &mut p.runs {
                rebase_look(&mut run.look, &p.base, &base);
            }
            rebase_look(&mut p.mark, &p.base, &base);
            p.base = base;
            p.props = style.para.clone();
        },
        wanted,
    );
    doc.renumber();
}

fn rebased(base: &RunLook, old: &RunLook, new: &RunLook) -> RunLook {
    let mut out = base.clone();
    rebase_look(&mut out, old, new);
    out
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// What a list looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// •, ◦, ▪ by level.
    Bullet,
    /// 1., a., i. by level.
    Numbered,
}

fn kind_of(list: &List) -> Option<ListKind> {
    match list.levels.first()?.format {
        NumberFormat::Bullet => Some(ListKind::Bullet),
        NumberFormat::None => None,
        _ => Some(ListKind::Numbered),
    }
}

fn level_for(kind: ListKind, level: usize) -> ListLevel {
    let (format, text) = match kind {
        ListKind::Bullet => (NumberFormat::Bullet, ["•", "◦", "▪"][level % 3].to_owned()),
        ListKind::Numbered => (
            [
                NumberFormat::Decimal,
                NumberFormat::LowerLetter,
                NumberFormat::LowerRoman,
            ][level % 3]
                .clone(),
            format!("%{}.", level + 1),
        ),
    };
    ListLevel {
        format,
        text,
        start: 1,
        restart: None,
        suffix: Suffix::Tab,
        align: Align::Left,
        look: RunPatch::default(),
        indent_left: 720 + 360 * level as Twips,
        first_line: -360,
        legal: false,
    }
}

fn new_list(numbering: &mut Numbering, kind: ListKind) -> u32 {
    let id = numbering.lists.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let definition = numbering
        .lists
        .iter()
        .map(|l| l.definition)
        .max()
        .map_or(0, |d| d + 1);
    numbering.lists.push(List {
        id,
        definition,
        levels: (0..9).map(|level| level_for(kind, level)).collect(),
    });
    id
}

/// Make the paragraphs the range touches items of a list of `kind`, or with
/// `None` take them out of their lists. A list of the kind that the
/// paragraph before the range is in is continued; otherwise the first list
/// of that kind in the document is used, or a new one is made. Items keep
/// the level they were on; the list's indents go on the paragraph.
pub fn set_list(doc: &mut Document, from: &Pos, to: &Pos, kind: Option<ListKind>) {
    let Some((f, t)) = ends(doc, from, to) else {
        return;
    };
    let paths = paths_between(doc, &f.path, &t.path);
    let Some(kind) = kind else {
        for path in &paths {
            remove_from_list(doc, path);
        }
        doc.renumber();
        return;
    };
    // The list: the one just before, an existing one, a new one.
    let previous = f
        .path
        .0
        .last()
        .copied()
        .filter(|&i| i > 0)
        .and_then(|i| {
            let mut path = f.path.clone();
            *path.0.last_mut()? = i - 1;
            doc.paragraph(&path)?.props.list
        })
        .and_then(|r| doc.numbering.get(r.list))
        .filter(|list| kind_of(list) == Some(kind))
        .map(|list| list.id);
    let existing = || {
        doc.numbering
            .lists
            .iter()
            .find(|list| {
                kind_of(list) == Some(kind) && list.levels.iter().all(|l| l.restart.is_none())
            })
            .map(|list| list.id)
    };
    let id = match previous.or_else(existing) {
        Some(id) => id,
        None => new_list(Arc::make_mut(&mut doc.numbering), kind),
    };
    for path in &paths {
        let Some(p) = doc.paragraph(path) else {
            continue;
        };
        let level = p.props.list.map_or(0, |r| r.level);
        let wanted = ListRef { list: id, level };
        let (left, first) = level_indents(doc, wanted);
        if p.props.list == Some(wanted)
            && p.props.indent_left == left
            && p.props.first_line == first
        {
            continue;
        }
        if let Some(p) = doc.paragraph_mut(path) {
            p.props.list = Some(wanted);
            p.props.indent_left = left;
            p.props.first_line = first;
        }
    }
    doc.renumber();
}

/// The indents the list's level gives its items.
fn level_indents(doc: &Document, list: ListRef) -> (Twips, Twips) {
    doc.numbering
        .get(list.list)
        .and_then(|l| l.levels.get(usize::from(list.level)))
        .map_or((0, 0), |level| (level.indent_left, level.first_line))
}

/// Take one paragraph out of its list. Indents that were the list level's go
/// back to the paragraph style's.
fn remove_from_list(doc: &mut Document, path: &Path) {
    let Some(p) = doc.paragraph(path) else {
        return;
    };
    let Some(list) = p.props.list else {
        return;
    };
    let (left, first) = level_indents(doc, list);
    let inherited = p.props.indent_left == left && p.props.first_line == first;
    let (style_left, style_first) = {
        let look = doc.styles.paragraph_look(p.props.style.as_deref()).0;
        (look.indent_left, look.first_line)
    };
    if let Some(p) = doc.paragraph_mut(path) {
        p.props.list = None;
        p.label = None;
        if inherited {
            p.props.indent_left = style_left;
            p.props.first_line = style_first;
        }
    }
}

/// Move list items `delta` levels deeper (negative for shallower), within
/// the levels their list has. Paragraphs that are no list items stay as they
/// are.
pub fn change_level(doc: &mut Document, from: &Pos, to: &Pos, delta: i32) {
    let Some((f, t)) = ends(doc, from, to) else {
        return;
    };
    for path in paths_between(doc, &f.path, &t.path) {
        let Some(list) = doc.paragraph(&path).and_then(|p| p.props.list) else {
            continue;
        };
        let deepest = doc
            .numbering
            .get(list.list)
            .map_or(0, |l| l.levels.len().saturating_sub(1))
            .min(8) as i32;
        let level = (i32::from(list.level) + delta).clamp(0, deepest) as u8;
        if level == list.level {
            continue;
        }
        let wanted = ListRef {
            list: list.list,
            level,
        };
        let (left, first) = level_indents(doc, wanted);
        if let Some(p) = doc.paragraph_mut(&path) {
            p.props.list = Some(wanted);
            p.props.indent_left = left;
            p.props.first_line = first;
        }
    }
    doc.renumber();
}

// ---------------------------------------------------------------------------
// Reading ranges, copy and paste
// ---------------------------------------------------------------------------

/// The start and the end of the paragraph `at` is in.
pub fn paragraph_range(doc: &Document, at: &Pos) -> Option<(Pos, Pos)> {
    let p = doc.paragraph(&at.path)?;
    Some((
        Pos::new(at.path.clone(), 0),
        Pos::new(at.path.clone(), p.len()),
    ))
}

/// The text of the range, one paragraph per line. Objects show nothing; a
/// line break inside a paragraph is a line break.
pub fn plain_text(doc: &Document, from: &Pos, to: &Pos) -> String {
    let Some((f, t)) = ends(doc, from, to) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for (path, p) in doc.paragraphs() {
        if path < f.path || path > t.path {
            continue;
        }
        let a = if path == f.path { f.offset } else { 0 };
        let b = if path == t.path { t.offset } else { p.len() };
        let text = p.text();
        let b = b.min(text.len());
        let a = a.min(b);
        lines.push(text[a..b].replace(crate::OBJECT, ""));
    }
    lines.join("\n")
}

/// The paragraphs of the range as detached copies, for copying inside the
/// application: the first and last cut to the range. Only the paragraphs of
/// the container the range starts in are taken; tables are left out in this
/// version.
pub fn fragment(doc: &Document, from: &Pos, to: &Pos) -> Vec<Block> {
    let Some((f, t)) = ends(doc, from, to) else {
        return Vec::new();
    };
    let n = f.path.0.len();
    let mut out = Vec::new();
    for (path, p) in doc.paragraphs() {
        if path < f.path || path > t.path {
            continue;
        }
        if path.0.len() != n || path.0[..n - 1] != f.path.0[..n - 1] {
            continue;
        }
        let mut copy = (**p).clone();
        if path == t.path {
            remove_range(&mut copy, t.offset, usize::MAX);
        }
        if path == f.path {
            remove_range(&mut copy, 0, f.offset);
        }
        out.push(Block::paragraph(copy.detached()));
    }
    out
}

/// Put what [`fragment`] made at `at`; the caret's place after it. One
/// paragraph goes in the text; several split the paragraph at `at`, the first
/// joins the first half, the last the second, the ones between are new
/// paragraphs with their properties. Tables in the fragment are ignored.
pub fn insert_fragment(doc: &mut Document, at: &Pos, blocks: &[Block]) -> Pos {
    let Some(at) = locate(doc, at) else {
        return at.clone();
    };
    let paragraphs: Vec<&Paragraph> = blocks.iter().filter_map(Block::as_paragraph).collect();
    let Some((first, rest)) = paragraphs.split_first() else {
        return at;
    };
    let Some((last, middle)) = rest.split_last() else {
        return insert_runs(doc, &at, &first.runs);
    };
    let second = split_raw(doc, &at, false);
    let mut first_path = second.path.clone();
    *first_path.0.last_mut().expect("a path has steps") -= 1;
    let end = doc.paragraph(&first_path).map_or(0, Paragraph::len);
    insert_runs(doc, &Pos::new(first_path.clone(), end), &first.runs);
    // The paragraphs between, after the first half.
    let index = first_path.0.last().copied().unwrap_or(0) as usize;
    let mut tail_path = second.path.clone();
    if let Some(siblings) = doc.siblings_mut(&first_path) {
        for (k, p) in middle.iter().enumerate() {
            let mut copy = (*p).clone();
            copy.source = None;
            copy.section = None;
            copy.label = None;
            siblings.insert(index + 1 + k, Block::paragraph(copy));
        }
        *tail_path.0.last_mut().expect("a path has steps") += middle.len() as u32;
    }
    let size: usize = last.runs.iter().map(|r| r.text.len()).sum();
    insert_runs(doc, &Pos::new(tail_path.clone(), 0), &last.runs);
    doc.renumber();
    Pos::new(tail_path, size)
}

/// Runs without a source into the text at `at`.
fn insert_runs(doc: &mut Document, at: &Pos, runs: &[Run]) -> Pos {
    let size: usize = runs.iter().map(|r| r.text.len()).sum();
    if runs.is_empty() {
        return at.clone();
    }
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return at.clone();
    };
    let offset = snap(p, at.offset, false);
    let mut index = split_run(p, offset);
    for run in runs {
        let mut run = run.clone();
        run.source = None;
        p.runs.insert(index, run);
        index += 1;
    }
    merge_runs(p);
    Pos::new(at.path.clone(), offset + size)
}

// ---------------------------------------------------------------------------
// Comparing documents
// ---------------------------------------------------------------------------

/// Two documents have the same content: the same blocks with the same text,
/// kinds, looks and properties, the same tables, sections and lists. Where
/// they were read from (sources) and whether blocks are the same `Arc` do not
/// matter; neither does how neighbouring markers or neighbouring text runs of
/// the same look are split up.
pub fn same_content(a: &Document, b: &Document) -> bool {
    same_blocks(&a.body.blocks, &b.body.blocks)
        && same_section(&a.last_section, &b.last_section)
        && same_numbering(&a.numbering, &b.numbering)
        && a.styles == b.styles
        && a.settings == b.settings
        && a.marginals.len() == b.marginals.len()
        && a.marginals
            .iter()
            .zip(&b.marginals)
            .all(|(x, y)| same_blocks(&x.blocks, &y.blocks))
}

fn same_numbering(a: &Numbering, b: &Numbering) -> bool {
    a.lists.len() == b.lists.len()
        && a.lists
            .iter()
            .zip(&b.lists)
            .all(|(x, y)| x.id == y.id && x.levels == y.levels)
}

fn same_section(a: &crate::Section, b: &crate::Section) -> bool {
    let strip = |s: &crate::Section| crate::Section {
        source: None,
        ..s.clone()
    };
    strip(a) == strip(b)
}

/// The blocks of two lists have the same content.
pub fn same_blocks(a: &[Block], b: &[Block]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_block(x, y))
}

fn same_block(a: &Block, b: &Block) -> bool {
    match (a, b) {
        (Block::Paragraph(x), Block::Paragraph(y)) => same_paragraph(x, y),
        (Block::Table(x), Block::Table(y)) => {
            x.columns == y.columns
                && x.props == y.props
                && x.rows.len() == y.rows.len()
                && x.rows.iter().zip(&y.rows).all(|(r, s)| {
                    r.height == s.height
                        && r.header == s.header
                        && r.cant_split == s.cant_split
                        && r.grid_before == s.grid_before
                        && r.grid_after == s.grid_after
                        && r.cells.len() == s.cells.len()
                        && r.cells.iter().zip(&s.cells).all(|(c, d)| {
                            c.span == d.span
                                && c.merge == d.merge
                                && c.borders == d.borders
                                && c.shading == d.shading
                                && c.margins == d.margins
                                && c.align == d.align
                                && same_blocks(&c.blocks, &d.blocks)
                        })
                })
        }
        _ => false,
    }
}

/// A run as comparison sees it: neighbouring markers are one thing, and
/// neighbouring text with one look is one run.
struct Seen<'a> {
    text: String,
    kind: &'a Inline,
    look: Option<&'a RunLook>,
    style: &'a Option<Arc<str>>,
    link: &'a Option<Arc<crate::Link>>,
}

fn seen(p: &Paragraph) -> Vec<Seen<'_>> {
    let mut out: Vec<Seen<'_>> = Vec::new();
    for run in &p.runs {
        if matches!(run.kind, Inline::Marker) {
            if out.last().is_some_and(|s| matches!(s.kind, Inline::Marker)) {
                continue;
            }
            out.push(Seen {
                text: String::new(),
                kind: &run.kind,
                look: None,
                style: &None,
                link: &None,
            });
            continue;
        }
        if run.text.is_empty() && run.is_text() {
            continue;
        }
        if let Some(last) = out.last_mut()
            && run.is_text()
            && matches!(last.kind, Inline::Text)
            && last.look == Some(&run.look)
            && last.style == &run.style
            && last.link == &run.link
        {
            last.text.push_str(&run.text);
            continue;
        }
        out.push(Seen {
            text: run.text.clone(),
            kind: &run.kind,
            look: Some(&run.look),
            style: &run.style,
            link: &run.link,
        });
    }
    out
}

fn same_paragraph(a: &Paragraph, b: &Paragraph) -> bool {
    if a.props != b.props
        || a.base != b.base
        || a.mark != b.mark
        || a.label != b.label
        || a.section.is_some() != b.section.is_some()
    {
        return false;
    }
    if let (Some(x), Some(y)) = (&a.section, &b.section)
        && !same_section(x, y)
    {
        return false;
    }
    let (x, y) = (seen(a), seen(b));
    x.len() == y.len()
        && x.iter().zip(&y).all(|(m, n)| {
            m.text == n.text
                && m.kind == n.kind
                && m.look == n.look
                && m.style == n.style
                && m.link == n.link
        })
}

// ---------------------------------------------------------------------------
// Undo and redo
// ---------------------------------------------------------------------------

/// What kind of step an edit was, for steps that run together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// Typing a character: following steps of this kind are one undo step.
    Typing,
    /// Backspace, one character at a time.
    DeleteBackward,
    /// Delete, one character at a time.
    DeleteForward,
}

#[derive(Clone, Debug)]
struct Step {
    /// The document and selection to go back to.
    restore: Document,
    restore_selection: Selection,
    /// The selection on the other side of the step.
    other_selection: Selection,
    kind: Option<StepKind>,
}

/// Undo and redo by keeping earlier documents (cloning a document is cheap:
/// the blocks are shared) with the selection before and after each step.
#[derive(Clone, Debug)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(200)
    }
}

impl History {
    /// A history that keeps at most `limit` steps (at least one).
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// How many steps undo goes back.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// Note an edit that has been made: `before` is the document as it was,
    /// with the selection then and the selection now. A step of the same
    /// `kind` (typing, single deletions) as the one before it, which began
    /// where that one ended, joins it. Redo is dropped.
    pub fn record(
        &mut self,
        before: &Document,
        selection_before: &Selection,
        selection_after: &Selection,
        kind: Option<StepKind>,
    ) {
        self.redo.clear();
        if let Some(last) = self.undo.last_mut()
            && kind.is_some()
            && last.kind == kind
            && last.other_selection == *selection_before
        {
            last.other_selection = selection_after.clone();
            return;
        }
        self.undo.push(Step {
            restore: before.clone(),
            restore_selection: selection_before.clone(),
            other_selection: selection_after.clone(),
            kind,
        });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    /// Stop the next step joining the last one (the caret moved, a pause).
    pub fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.kind = None;
        }
    }

    /// Go back one step: `current` is the document now. Gives the document
    /// and selection to restore.
    pub fn undo(&mut self, current: &Document) -> Option<(Document, Selection)> {
        let step = self.undo.pop()?;
        self.redo.push(Step {
            restore: current.clone(),
            restore_selection: step.other_selection.clone(),
            other_selection: step.restore_selection.clone(),
            kind: None,
        });
        Some((step.restore, step.restore_selection))
    }

    /// Go forward one step that was undone.
    pub fn redo(&mut self, current: &Document) -> Option<(Document, Selection)> {
        let step = self.redo.pop()?;
        self.undo.push(Step {
            restore: current.clone(),
            restore_selection: step.other_selection.clone(),
            other_selection: step.restore_selection.clone(),
            kind: None,
        });
        Some((step.restore, step.restore_selection))
    }
}

#[cfg(test)]
mod tests;
