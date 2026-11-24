//! Formatting commands on Markdown source: wrap or unwrap emphasis and code,
//! make lines headings, list items or quotes, insert links, images and
//! tables. The editor writes the syntax for the user; nothing here knows
//! about a view.
//!
//! Each command reads only the lines the selection touches, so it costs the
//! same in a 100 MB file as in a short one, and returns one [`Edit`]: a
//! range of the text, what replaces it, and the selection afterwards. The
//! document applies it as one undo step.

use std::ops::Range;

use ropey::Rope;

use crate::document::{Selection, slice};
use crate::highlight::{Highlighter, Marks};

/// A replacement to make, and where the selection goes afterwards (offsets
/// in the text after the replacement).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Selection,
}

/// Emphasis and code, written around text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Strike,
    Code,
}

impl Inline {
    fn mark(self) -> &'static str {
        match self {
            Inline::Bold => "**",
            Inline::Italic => "*",
            Inline::Strike => "~~",
            Inline::Code => "`",
        }
    }

    /// How the highlighter marks text with this style.
    fn marks(self) -> Marks {
        match self {
            Inline::Bold => Marks::STRONG,
            Inline::Italic => Marks::EMPHASIS,
            Inline::Strike => Marks::STRIKE,
            Inline::Code => Marks::CODE,
        }
    }

    /// The character the mark is made of.
    fn unit(self) -> char {
        self.mark().chars().next().expect("marks are not empty")
    }

    /// Whether runs of `left` and `right` mark characters around some text
    /// mean it already has this style.
    fn surrounds(self, left: usize, right: usize) -> bool {
        match self {
            // `***x***` is bold and italic.
            Inline::Bold => matches!(left, 2 | 3) && matches!(right, 2 | 3),
            Inline::Italic => matches!(left, 1 | 3) && matches!(right, 1 | 3),
            Inline::Strike => left == 2 && right == 2,
            Inline::Code => left >= 1 && left == right,
        }
    }
}

/// Line-level styles that are written as a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Bullet,
    Numbered,
    Task,
    Quote,
}

/// The lines a selection touches: from the start of its first line to the
/// end of its last line, without the last line break. A selection that ends
/// at the very start of a line does not take that line.
fn line_span(text: &Rope, range: &Range<usize>) -> Range<usize> {
    let first = text.byte_to_line(range.start);
    let mut last = text.byte_to_line(range.end);
    if last > first && text.line_to_byte(last) == range.end {
        last -= 1;
    }
    let start = text.line_to_byte(first);
    let end = if last + 1 < text.len_lines() {
        text.line_to_byte(last + 1) - 1
    } else {
        text.len_bytes()
    };
    start..end.max(start)
}

/// A few bytes either side of `range`, clamped to the text.
fn around(text: &Rope, range: &Range<usize>, reach: usize) -> (String, String) {
    let len = text.len_bytes();
    let before = floor(text, range.start.saturating_sub(reach));
    let after = ceil(text, (range.end + reach).min(len));
    (
        slice(text, before..range.start),
        slice(text, range.end..after),
    )
}

fn floor(text: &Rope, mut at: usize) -> usize {
    while !crate::document::is_char_boundary(text, at) {
        at -= 1;
    }
    at
}

fn ceil(text: &Rope, mut at: usize) -> usize {
    while at < text.len_bytes() && !crate::document::is_char_boundary(text, at) {
        at += 1;
    }
    at
}

fn run_at_end(text: &str, unit: char) -> usize {
    text.chars().rev().take_while(|c| *c == unit).count()
}

fn run_at_start(text: &str, unit: char) -> usize {
    text.chars().take_while(|c| *c == unit).count()
}

/// Make the selection bold, italic, struck through or code, or take that
/// style away if it already has it. With nothing selected the marks go in
/// with the caret between them. Over several lines each line is wrapped on
/// its own, since emphasis cannot cross a paragraph.
pub fn toggle_inline(text: &Rope, selection: Selection, kind: Inline) -> Edit {
    let range = selection.range();
    if let Some(edit) = unwrap(text, &range, kind) {
        return edit;
    }
    wrap(text, range, kind)
}

/// The edit that takes the style away from a selection that is exactly a
/// styled text, with or without its marks; `None` for any other selection.
fn unwrap(text: &Rope, range: &Range<usize>, kind: Inline) -> Option<Edit> {
    let unit = kind.unit();
    let width = kind.mark().len();

    // Marks just outside the selection: `**|text|**`.
    let (before, after) = around(text, range, 3);
    let left = run_at_end(&before, unit);
    let right = run_at_start(&after, unit);
    if !range.is_empty() && kind.surrounds(left, right) {
        let take = if kind == Inline::Code { left } else { width };
        let inner = slice(text, range.clone());
        let start = range.start - take;
        return Some(Edit {
            range: start..range.end + take,
            text: inner.clone(),
            selection: Selection {
                anchor: start,
                focus: start + inner.len(),
            },
        });
    }

    let selected = slice(text, range.clone());
    // The selection includes its marks: `|**text**|`.
    if selected.len() > 2 * width {
        let lead = run_at_start(&selected, unit);
        let trail = run_at_end(&selected, unit);
        if kind.surrounds(lead, trail) && !selected.contains('\n') {
            let take = if kind == Inline::Code { lead } else { width };
            let inner = selected[take..selected.len() - take].to_owned();
            return Some(Edit {
                range: range.clone(),
                text: inner.clone(),
                selection: Selection {
                    anchor: range.start,
                    focus: range.start + inner.len(),
                },
            });
        }
    }
    None
}

/// Put the marks around a selection, or between a caret's sides.
fn wrap(text: &Rope, range: Range<usize>, kind: Inline) -> Edit {
    let unit = kind.unit();
    let selected = slice(text, range.clone());

    if range.is_empty() {
        let mark = kind.mark();
        return Edit {
            range: range.clone(),
            text: format!("{mark}{mark}"),
            selection: Selection::caret(range.start + mark.len()),
        };
    }

    // Wrap each line's text, leaving white space at its ends outside the
    // marks: `** word **` is not emphasis. A line taken from its start
    // keeps its heading, list or quote marker outside the marks too.
    let starts_line = range.start == text.line_to_byte(text.byte_to_line(range.start));
    let mut out = String::with_capacity(selected.len() + 8);
    let mut first_skip = 0;
    for (index, whole) in selected.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let skip = if index > 0 || starts_line {
            line_marker(whole)
        } else {
            0
        };
        if index == 0 {
            first_skip = skip;
        }
        out.push_str(&whole[..skip]);
        let line = &whole[skip..];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push_str(line);
            continue;
        }
        let lead = &line[..line.len() - line.trim_start().len()];
        let trail = &line[line.trim_end().len()..];
        let mark = match kind {
            // Backticks inside code need a longer fence, and a space
            // between the fence and a backtick at either end.
            Inline::Code => {
                let longest = trimmed.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                "`".repeat(longest + 1)
            }
            _ => kind.mark().to_owned(),
        };
        let pad = if kind == Inline::Code && (trimmed.starts_with('`') || trimmed.ends_with('`')) {
            " "
        } else {
            ""
        };
        out.push_str(lead);
        out.push_str(&mark);
        out.push_str(pad);
        out.push_str(trimmed);
        out.push_str(pad);
        out.push_str(&mark);
        out.push_str(trail);
    }
    let selection = if selected.contains('\n') {
        Selection {
            anchor: range.start,
            focus: range.start + out.len(),
        }
    } else {
        // The same text stays selected, now inside its marks.
        let rest = &selected[first_skip..];
        let lead = first_skip + rest.len() - rest.trim_start().len();
        let opening = out[lead..].len() - out[lead..].trim_start_matches(unit).len();
        let start = range.start + lead + opening;
        Selection {
            anchor: start,
            focus: start + rest.trim().len(),
        }
    };
    Edit {
        range,
        text: out,
        selection,
    }
}

/// How far a styled stretch is followed from the selection, each way. A
/// longer one is treated as not styled: looking costs nothing then.
const RUN_REACH: usize = 64 * 1024;

/// The stretch with style `kind` that contains `range`, from the start of its
/// opening mark to the end of its closing one, as the highlighter sees it. A
/// caret counts only inside: just before the opening mark or just after the
/// closing one is outside.
fn styled_run(
    highlighter: &Highlighter,
    range: &Range<usize>,
    kind: Inline,
) -> Option<Range<usize>> {
    let mark = kind.marks();
    let span_at = |at: usize| {
        highlighter
            .spans(at..at + 1)
            .into_iter()
            .find(|span| span.range.contains(&at) && span.marks.contains(mark))
    };
    // The bytes that must have the style: either side of a caret, or the
    // first and last of a selection.
    let (first, last) = if range.is_empty() {
        (range.start.checked_sub(1)?, range.start)
    } else {
        (range.start, range.end - 1)
    };
    let mut start = span_at(first)?.range.start;
    let mut end = span_at(first)?.range.end;
    // Every byte up to `last` must have it too.
    while end <= last {
        if end - range.start > RUN_REACH {
            return None;
        }
        end = span_at(end)?.range.end;
    }
    while let Some(span) = start.checked_sub(1).and_then(span_at) {
        if range.start - span.range.start > RUN_REACH {
            return None;
        }
        start = span.range.start;
    }
    while let Some(span) = span_at(end) {
        if span.range.end - range.end > RUN_REACH {
            return None;
        }
        end = span.range.end;
    }
    Some(start..end)
}

/// How many bytes the opening mark of a styled stretch takes; the closing
/// mark takes as many.
fn mark_width(text: &Rope, run: &Range<usize>, kind: Inline) -> usize {
    let width = match kind {
        Inline::Bold => 2,
        Inline::Italic => 1,
        // `~x~` and `~~x~~`; any number of backticks.
        Inline::Strike | Inline::Code => {
            let end = ceil(text, (run.start + 16).min(run.end));
            run_at_start(&slice(text, run.start..end), kind.unit())
        }
    };
    width.min(run.len() / 2)
}

/// [`toggle_inline`], knowing where the text already has the style. With
/// the caret inside styled text, the style comes off the whole of it; a
/// selection inside styled text comes out of it, and what is left on either
/// side keeps the style. Pass the highlighting only while it is up to date.
pub fn toggle_inline_in(
    text: &Rope,
    highlighter: &Highlighter,
    selection: Selection,
    kind: Inline,
) -> Edit {
    let range = selection.range();
    let Some(run) = styled_run(highlighter, &range, kind) else {
        return toggle_inline(text, selection, kind);
    };
    let width = mark_width(text, &run, kind);
    let inner = run.start + width..run.end - width;
    if range.is_empty() {
        let caret = range.start.clamp(inner.start, inner.end) - width;
        return Edit {
            range: run.clone(),
            text: slice(text, inner),
            selection: Selection::caret(caret),
        };
    }
    if range == inner || range.start < inner.start || range.end > inner.end {
        return toggle_inline(text, selection, kind);
    }
    let mut open = slice(text, run.start..inner.start);
    let mut close = slice(text, inner.end..run.end);
    // `_` emphasis does not work inside a word, which is where the pieces
    // now meet: `__一__二__三__` is plain text. Stars do.
    if open.starts_with('_') {
        open = kind.mark().to_owned();
        close = open.clone();
    }
    // A side keeps the style; white space at its ends goes outside the
    // marks, where emphasis needs it.
    let side = |part: String| {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return part;
        }
        let lead = &part[..part.len() - part.trim_start().len()];
        let trail = &part[part.trim_end().len()..];
        format!("{lead}{open}{trimmed}{close}{trail}")
    };
    let left = side(slice(text, inner.start..range.start));
    let middle = slice(text, range.clone());
    let right = side(slice(text, range.end..inner.end));
    let start = run.start + left.len();
    Edit {
        range: run,
        selection: Selection {
            anchor: start,
            focus: start + middle.len(),
        },
        text: left + &middle + &right,
    }
}

/// The styles the toolbar shows pressed for a selection: those a click
/// would take away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Styles {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// Every line of the selection is this kind of list item, or quoted.
    pub block: Option<Block>,
    /// The selection starts in a table: the table commands can be used.
    pub table: bool,
}

impl Styles {
    pub fn has(&self, kind: Inline) -> bool {
        match kind {
            Inline::Bold => self.bold,
            Inline::Italic => self.italic,
            Inline::Strike => self.strike,
            Inline::Code => self.code,
        }
    }
}

/// The styles of a selection. Selections longer than this show none: the
/// answer would cost a read of all of it on every caret move.
const STYLES_REACH: usize = 64 * 1024;

/// What the selection's text already is: the styles [`toggle_inline_in`]
/// and [`toggle_block`] would take away. Pass the highlighting only while it
/// is up to date; without it only a selection that is exactly a styled
/// text, or that includes its marks, counts.
pub fn styles(text: &Rope, highlighter: Option<&Highlighter>, selection: Selection) -> Styles {
    let range = selection.range();
    if range.len() > STYLES_REACH {
        return Styles::default();
    }
    let has = |kind: Inline| {
        highlighter.is_some_and(|h| styled_run(h, &range, kind).is_some())
            || unwrap(text, &range, kind).is_some()
    };
    Styles {
        bold: has(Inline::Bold),
        italic: has(Inline::Italic),
        strike: has(Inline::Strike),
        code: has(Inline::Code),
        block: block_of(text, &range),
        table: crate::table::in_table(text, highlighter, range.start),
    }
}

/// The block every line of `range` is, as [`toggle_block`] decides it;
/// blank lines among several do not count.
fn block_of(text: &Rope, range: &Range<usize>) -> Option<Block> {
    let span = line_span(text, range);
    // A marker is near the start of its line, and a line can be a whole
    // 100 MB file.
    let lines = if text.byte_to_line(span.start) == text.byte_to_line(span.end) {
        slice(
            text,
            span.start..ceil(text, (span.start + 64).min(span.end)),
        )
    } else if span.len() <= STYLES_REACH {
        slice(text, span)
    } else {
        return None;
    };
    let several = lines.contains('\n');
    let mut found = None;
    for line in lines.split('\n') {
        if several && line.trim().is_empty() {
            continue;
        }
        let kind = parse_block(line).1?;
        if found.is_some_and(|seen| seen != kind) {
            return None;
        }
        found = Some(kind);
    }
    found
}

/// How much of a line is read for its list and quote markers.
const PREFIX_REACH: usize = 256;
/// How far after a new list item the numbers of the items below are put
/// right. Further down they stay as they are.
const RENUMBER_REACH: usize = 64 * 1024;

/// The markers at the start of a line that Enter carries on to the next:
/// quotes, then at most one list marker. Returns how many bytes they take
/// and what the next line starts with: the same quotes, the same bullet, the
/// next number, an unticked box.
fn continuation(line: &str) -> Option<(usize, String)> {
    let mut at = 0;
    let mut next = String::new();
    loop {
        let (parsed, kind) = parse_block(&line[at..]);
        let kind = kind?;
        next.push_str(parsed.indent);
        at += parsed.indent.len() + parsed.prefix.len();
        match kind {
            Block::Quote => {
                next.push_str(parsed.prefix.trim_end());
                next.push(' ');
                // A quote alone is enough; a list inside it is carried too.
                if parse_block(&line[at..]).1.is_none() {
                    return Some((at, next));
                }
            }
            Block::Bullet => {
                next.push_str(parsed.prefix);
                return Some((at, next));
            }
            Block::Task => {
                next.push_str(&parsed.prefix[..2]);
                next.push_str("[ ] ");
                return Some((at, next));
            }
            Block::Numbered => {
                let digits = parsed.prefix.len() - 2;
                let number: u64 = parsed.prefix[..digits].parse().ok()?;
                let delimiter = &parsed.prefix[digits..digits + 1];
                next.push_str(&format!("{}{delimiter} ", number + 1));
                return Some((at, next));
            }
        }
    }
}

/// What Enter does on a list item or in a quote: the new line starts with
/// the same markers (the next number, an unticked box), and the numbers of
/// the items below go up by one. Enter on an item with nothing in it ends
/// the list instead, taking the marker away. `None` where Enter is a plain
/// line break: elsewhere, inside the markers, or in a code block (pass the
/// highlighting while it is up to date, to tell).
pub fn line_break(
    text: &Rope,
    highlighter: Option<&Highlighter>,
    selection: Selection,
) -> Option<Edit> {
    let range = selection.range();
    let line = text.byte_to_line(range.start);
    let start = text.line_to_byte(line);
    let end = line_span(text, &(start..start)).end;
    let head = slice(text, start..ceil(text, (start + PREFIX_REACH).min(end)));
    let (marker, next) = continuation(&head)?;
    if range.start < start + marker {
        return None;
    }
    if let Some(highlighter) = highlighter
        && highlighter
            .spans(start..start + marker)
            .iter()
            .any(|span| span.marks.contains(Marks::CODE_BLOCK))
    {
        return None;
    }

    // An empty item: the list ends here.
    let rest = start + marker..end;
    if range.is_empty() && rest.len() <= PREFIX_REACH && slice(text, rest).trim().is_empty() {
        return Some(Edit {
            range: start..end,
            text: String::new(),
            selection: Selection::caret(start),
        });
    }

    let mut out = format!("\n{next}");
    let caret = range.start + out.len();
    let mut edit_end = range.end;
    if let Some(number) = numbered(&next) {
        let indent = next.len() - next.trim_start().len();
        if let Some((renumbered, until)) = renumber(text, range.end, indent, number + 1) {
            out.push_str(&renumbered);
            edit_end = until;
        }
    }
    Some(Edit {
        range: range.start..edit_end,
        text: out,
        selection: Selection::caret(caret),
    })
}

/// The number of a numbered item's marker with nothing before it but
/// indentation.
fn numbered(prefix: &str) -> Option<u64> {
    let body = prefix.trim_start_matches([' ', '\t']);
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    body[..digits].parse().ok()
}

/// The text from `from` (inside the line of the new item, after the caret)
/// down to the last numbered item that needed a new number, with the
/// numbers of the items at `indent` counting on from `first`, and where
/// that text ends. `None` if no number changes. Lines indented further
/// belong to an item and are passed over; anything else ends the list.
fn renumber(text: &Rope, from: usize, indent: usize, first: u64) -> Option<(String, usize)> {
    let mut line = text.byte_to_line(from) + 1;
    let mut expected = first;
    let mut changes: Vec<(Range<usize>, String)> = Vec::new();
    let mut blank_before = false;
    while line < text.len_lines() {
        let start = text.line_to_byte(line);
        if start - from > RENUMBER_REACH {
            break;
        }
        let end = line_span(text, &(start..start)).end;
        let head = slice(text, start..ceil(text, (start + PREFIX_REACH).min(end)));
        line += 1;
        if head.trim().is_empty() {
            // Two blank lines in a row end a list.
            if blank_before {
                break;
            }
            blank_before = true;
            continue;
        }
        blank_before = false;
        let own = head.len() - head.trim_start_matches([' ', '\t']).len();
        if own > indent {
            continue;
        }
        let (parsed, kind) = parse_block(&head);
        if own < indent || kind != Some(Block::Numbered) {
            break;
        }
        let digits = parsed.prefix.len() - 2;
        let number = &parsed.prefix[..digits];
        if number == expected.to_string() {
            break;
        }
        let at = start + parsed.indent.len();
        changes.push((at..at + digits, expected.to_string()));
        expected += 1;
    }
    let until = changes.last()?.0.end;
    let mut out = String::new();
    let mut copied = from;
    for (range, number) in changes {
        out.push_str(&slice(text, copied..range.start));
        out.push_str(&number);
        copied = range.end;
    }
    Some((out, until))
}

/// How far above an edit the lists around it are read, to know what the
/// items below should be numbered.
const LIST_CONTEXT_REACH: usize = 64 * 1024;

/// The first bytes of the line starting at `start`, enough for its markers.
fn head_at(text: &Rope, start: usize) -> String {
    let end = line_span(text, &(start..start)).end;
    slice(text, start..ceil(text, (start + PREFIX_REACH).min(end)))
}

/// Whether the line starting at `start`, `len` bytes of it read, is in a
/// code block by the highlighting, if there is highlighting to go by.
fn in_code_block(highlighter: Option<&Highlighter>, start: usize, len: usize) -> bool {
    highlighter.is_some_and(|highlighter| {
        highlighter
            .spans(start..start + len.max(1))
            .iter()
            .any(|span| span.marks.contains(Marks::CODE_BLOCK))
    })
}

/// A list item for numbering: how far it is indented, and for a numbered
/// item its number, its delimiter (`.` or `)`) and how many digits it has.
struct Numbering {
    indent: usize,
    number: Option<(u64, u8, usize)>,
}

impl Numbering {
    fn of(line: &str) -> Option<Self> {
        let (parsed, kind) = parse_block(line);
        let number = match kind? {
            Block::Quote => return None,
            Block::Numbered => {
                let digits = parsed.prefix.len() - 2;
                let number = parsed.prefix[..digits].parse().ok()?;
                Some((number, parsed.prefix.as_bytes()[digits], digits))
            }
            Block::Bullet | Block::Task => None,
        };
        Some(Self {
            indent: parsed.indent.len(),
            number,
        })
    }

    /// Items with the same delimiter belong to one list; any bullet goes on
    /// a bulleted list.
    fn delimiter(&self) -> Option<u8> {
        self.number.map(|(_, delimiter, _)| delimiter)
    }
}

/// A list met while putting numbers right.
#[derive(Clone, Copy, Debug)]
struct Level {
    indent: usize,
    /// `None` for a bulleted list.
    delimiter: Option<u8>,
    /// The number of the last item seen.
    last: u64,
    /// Every item has the same number (`1.`, `1.`, `1.`): keep it so.
    same: bool,
}

/// The lists that the line `line` could go on, outermost first: for each
/// level, the nearest item above at that level.
fn lists_above(text: &Rope, highlighter: Option<&Highlighter>, line: usize) -> Vec<Level> {
    let origin = text.line_to_byte(line);
    // Innermost first, each with the number of the item before its last.
    let mut found: Vec<(Level, Option<u64>, bool)> = Vec::new();
    // Items indented as far as this or further are cut off from below.
    let mut threshold = usize::MAX;
    let mut blank_below = false;
    let mut line = line;
    while line > 0 {
        line -= 1;
        let start = text.line_to_byte(line);
        if origin - start > LIST_CONTEXT_REACH {
            break;
        }
        let head = head_at(text, start);
        if head.trim().is_empty() {
            // Two blank lines end a list.
            if blank_below {
                break;
            }
            blank_below = true;
            continue;
        }
        blank_below = false;
        let own = head.len() - head.trim_start_matches([' ', '\t']).len();
        if in_code_block(highlighter, start, head.len()) {
            if own == 0 {
                break;
            }
            continue;
        }
        let Some(item) = Numbering::of(&head) else {
            // Text that is not part of a list ends it.
            if own == 0 {
                break;
            }
            threshold = threshold.min(own);
            continue;
        };
        if item.indent < threshold {
            let level = Level {
                indent: item.indent,
                delimiter: item.delimiter(),
                last: item.number.map_or(0, |(number, ..)| number),
                same: false,
            };
            found.push((level, None, true));
            threshold = item.indent;
        } else if item.indent == threshold
            && let Some((level, before, open)) = found.last_mut()
            && level.indent == threshold
            && *open
        {
            // The item before the nearest one, unless it is another list.
            if level.delimiter == item.delimiter() {
                *before = item.number.map(|(number, ..)| number);
            }
            *open = false;
        }
        if found
            .last()
            .is_some_and(|(level, _, open)| level.indent == 0 && !open)
        {
            break;
        }
    }
    found
        .into_iter()
        .rev()
        .map(|(mut level, before, _)| {
            level.same = level.delimiter.is_some() && before == Some(level.last);
            level
        })
        .collect()
}

/// After an edit that took out, put in or moved list items (deleting an
/// item, pasting items, Tab and Shift+Tab, making lines a list), put the
/// numbers of the numbered lists from the edit down right: each item counts
/// on from the item before it in its list. The first item of a list keeps
/// its number, except where the edit deleted the list's first item from the
/// start of its line: the item that is first now takes that number. A list
/// whose items all have one number keeps it. `changed` is what the edit put
/// in (offsets after the edit), `removed` what it took out, and `selection`
/// the selection after it, moved along with the numbers. `None` if every
/// number is already right, or the edit is too large to look through.
pub fn renumber_lists(
    text: &Rope,
    highlighter: Option<&Highlighter>,
    changed: Range<usize>,
    removed: &str,
    selection: Selection,
) -> Option<Edit> {
    if changed.len() > RENUMBER_REACH {
        return None;
    }
    let first_line = text.byte_to_line(changed.start);
    let mut levels = lists_above(text, highlighter, first_line);
    // A deletion from the start of a line that took a numbered item with it.
    let mut takes_over = None;
    if changed.is_empty() && changed.start == text.line_to_byte(first_line) {
        let end = removed.len().min(PREFIX_REACH);
        let end = (0..=end)
            .rev()
            .find(|&at| removed.is_char_boundary(at))
            .unwrap_or(0);
        let line = removed[..end].split('\n').next().unwrap_or("");
        takes_over = Numbering::of(line).filter(|item| item.number.is_some());
    }

    let mut changes: Vec<(Range<usize>, String)> = Vec::new();
    let mut renumber = |start: usize, item: &Numbering, to: u64| {
        if let Some((number, _, digits)) = item.number
            && number != to
        {
            let at = start + item.indent;
            changes.push((at..at + digits, to.to_string()));
        }
    };
    let mut line = first_line;
    let mut blank_before = false;
    let mut first = true;
    while line < text.len_lines() {
        let start = text.line_to_byte(line);
        line += 1;
        if start > changed.end + RENUMBER_REACH {
            break;
        }
        // Below the edit, the first item of the outermost list that needs
        // nothing ends the work: what follows did not move.
        let past = start > changed.end;
        let head = head_at(text, start);
        if head.trim().is_empty() {
            if blank_before {
                break;
            }
            blank_before = true;
            continue;
        }
        blank_before = false;
        let own = head.len() - head.trim_start_matches([' ', '\t']).len();
        let was_first = std::mem::replace(&mut first, false);
        if in_code_block(highlighter, start, head.len()) {
            if own == 0 {
                break;
            }
            continue;
        }
        let Some(item) = Numbering::of(&head) else {
            if own == 0 {
                break;
            }
            // Text inside an item ends the lists nested deeper.
            levels.retain(|level| level.indent < own);
            continue;
        };
        levels.retain(|level| level.indent <= item.indent);
        let goes_on = levels.last().is_some_and(|level| {
            level.indent == item.indent && level.delimiter == item.delimiter()
        });
        if goes_on {
            let level = levels.last_mut().expect("a level");
            let mut settled = true;
            if item.number.is_some() {
                let expected = if level.same {
                    level.last
                } else {
                    level.last + 1
                };
                level.last = expected;
                settled = item.number.is_some_and(|(number, ..)| number == expected);
                renumber(start, &item, expected);
            }
            if past && settled && levels.len() == 1 {
                break;
            }
        } else {
            if levels
                .last()
                .is_some_and(|level| level.indent == item.indent)
            {
                levels.pop();
            }
            let mut last = item.number.map_or(0, |(number, ..)| number);
            if was_first
                && let Some(gone) = takes_over.take()
                && gone.indent == item.indent
                && gone.delimiter() == item.delimiter()
                && let Some((number, ..)) = gone.number
            {
                renumber(start, &item, number);
                last = number;
            }
            levels.push(Level {
                indent: item.indent,
                delimiter: item.delimiter(),
                last,
                same: false,
            });
            if past && levels.len() == 1 {
                break;
            }
        }
    }

    let from = changes.first()?.0.start;
    let until = changes.last()?.0.end;
    let mut out = String::new();
    let mut copied = from;
    for (range, number) in &changes {
        out.push_str(&slice(text, copied..range.start));
        out.push_str(number);
        copied = range.end;
    }
    // Offsets after a number that got longer or shorter move with it; inside
    // one they stay within it.
    let shift = |offset: usize| -> usize {
        let mut moved = offset as isize;
        for (range, number) in &changes {
            let grown = number.len() as isize - range.len() as isize;
            if range.end <= offset {
                moved += grown;
            } else if range.start < offset {
                let within = (offset - range.start).min(number.len());
                moved += within as isize - (offset - range.start) as isize;
            }
        }
        moved as usize
    };
    Some(Edit {
        range: from..until,
        text: out,
        selection: Selection {
            anchor: shift(selection.anchor),
            focus: shift(selection.focus),
        },
    })
}

/// How far up Tab looks for the item above, to line the new level up with
/// it.
const INDENT_REACH: usize = 64 * 1024;

/// A list item's line: how far it is indented, how long its marker is with
/// the space after it, and its kind. Items in quotes are not counted. A task's
/// box is the item's text, not its marker: `- [ ] ` is a `- ` marker.
fn list_item(line: &str) -> Option<(usize, usize, Block)> {
    let (parsed, kind) = parse_block(line);
    let marker = match kind? {
        Block::Quote => return None,
        Block::Task => 2,
        _ => parsed.prefix.len(),
    };
    Some((parsed.indent.len(), marker, kind?))
}

/// Tab and Shift+Tab on list items: move the items of the selection one
/// level in or out. In, the first item lines up under the text of the item
/// above it at its level (two spaces under `- `, three under `1. `), and a
/// numbered item that starts a new list there is numbered 1; the first item
/// of a list cannot go in. Out, it lines up with the item it belonged to,
/// and numbered items that land in a numbered list count on from that item,
/// as do the items after them there. The other lines of the selection move as far. `None` where the first line
/// of the selection is not a list item: Tab is then an ordinary key. An item
/// that cannot move gives an edit that changes nothing.
pub fn indent(text: &Rope, selection: Selection, outdent: bool) -> Option<Edit> {
    let range = selection.range();
    let first_line = text.byte_to_line(range.start);
    let start = text.line_to_byte(first_line);
    let end = line_span(text, &(start..start)).end;
    let head = slice(text, start..ceil(text, (start + PREFIX_REACH).min(end)));
    let (own, _, kind) = list_item(&head)?;
    let unchanged = Edit {
        range: range.start..range.start,
        text: String::new(),
        selection,
    };

    // The lines above, nearest first, as far as they are list content.
    let mut above = Vec::new();
    let mut line = first_line;
    while line > 0 {
        line -= 1;
        let at = text.line_to_byte(line);
        if start - at > INDENT_REACH {
            break;
        }
        let end = line_span(text, &(at..at)).end;
        let head = slice(text, at..ceil(text, (at + PREFIX_REACH).min(end)));
        if head.trim().is_empty() {
            continue;
        }
        let indent = head.len() - head.trim_start_matches([' ', '\t']).len();
        let item = list_item(&head);
        // Text that is not part of a list ends it.
        if item.is_none() && indent == 0 {
            break;
        }
        above.push((indent, item, numbered(&head)));
    }

    // Out: the level the items land on, and the number the next numbered
    // item there takes when that level is a numbered list.
    let mut target = 0;
    let mut next_number = None;
    let (delta, restart) = if outdent {
        if own == 0 {
            return Some(unchanged);
        }
        let parent = above.iter().find_map(|(indent, item, number)| {
            item.filter(|_| *indent < own).map(|_| (*indent, *number))
        });
        if let Some((indent, number)) = parent {
            target = indent;
            next_number = number.map(|number| number + 1);
        }
        (own - target, false)
    } else {
        // The item above at the same level; lines indented further belong to
        // it. One less indented means this is the first item of its list.
        let mut deeper_numbered = None;
        let mut sibling = None;
        for (indent, item, _) in &above {
            if *indent < own {
                break;
            }
            if *indent == own {
                sibling = *item;
                break;
            }
            if deeper_numbered.is_none() {
                deeper_numbered = Some((*indent, matches!(item, Some((_, _, Block::Numbered)))));
            }
        }
        let Some((_, sibling_marker, _)) = sibling else {
            return Some(unchanged);
        };
        let new_indent = own + sibling_marker;
        // A numbered item starts a list at its new level unless numbered
        // items are already there.
        let continues = deeper_numbered == Some((new_indent, true));
        (sibling_marker, kind == Block::Numbered && !continues)
    };

    let mut first = true;
    let mut edit = rewrite_lines(text, selection, true, |line, _| {
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let (old_prefix, new_line, new_prefix) = if outdent {
            let take = indent.min(delta);
            let landed = indent - take;
            match next_number.as_mut() {
                Some(number)
                    if landed == target
                        && matches!(list_item(line), Some((_, _, Block::Numbered))) =>
                {
                    let body = &line[indent..];
                    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
                    let new_digits = number.to_string();
                    let new = format!("{}{new_digits}{}", &line[take..indent], &body[digits..]);
                    *number += 1;
                    (indent + digits, new, landed + new_digits.len())
                }
                _ => (take, line[take..].to_owned(), 0),
            }
        } else {
            let pad = " ".repeat(delta);
            let restart_here = first && restart;
            let body = &line[indent..];
            if restart_here {
                let digits = body.bytes().take_while(u8::is_ascii_digit).count();
                let new = format!("{}{pad}1{}", &line[..indent], &body[digits..]);
                (indent + digits, new, indent + delta + 1)
            } else {
                (0, format!("{pad}{line}"), delta)
            }
        };
        first = false;
        (new_line, old_prefix, new_prefix)
    });
    // The items after the moved ones at the level they landed on.
    if let Some(number) = next_number
        && let Some((renumbered, until)) = renumber(text, edit.range.end, target, number)
    {
        edit.text.push_str(&renumbered);
        edit.range.end = until;
    }
    Some(edit)
}

/// The parts of a line: indentation, the block prefix, and the rest.
struct Parsed<'a> {
    indent: &'a str,
    prefix: &'a str,
    rest: &'a str,
}

fn heading_prefix(line: &str) -> usize {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return 0;
    }
    let hashes = run_at_start(&line[indent..], '#');
    if !(1..=6).contains(&hashes) {
        return 0;
    }
    let after = &line[indent + hashes..];
    if after.is_empty() {
        return line.len();
    }
    if !after.starts_with([' ', '\t']) {
        return 0;
    }
    indent + hashes + (after.len() - after.trim_start_matches([' ', '\t']).len())
}

/// The heading level of the line the caret is on; 0 for body text. Reads
/// only the start of the line: it runs on every caret move, and a line can
/// be a whole 100 MB file.
pub fn heading_level(text: &Rope, caret: usize) -> u8 {
    let span = line_span(
        text,
        &(caret.min(text.len_bytes())..caret.min(text.len_bytes())),
    );
    let end = ceil(text, (span.start + 16).min(span.end));
    let line = slice(text, span.start..end);
    let prefix = heading_prefix(&line);
    if prefix == 0 {
        return 0;
    }
    run_at_start(line.trim_start_matches(' '), '#') as u8
}

/// How many bytes at the start of a line are its heading, list or quote
/// marker (with the indentation before it), which emphasis must not cover.
fn line_marker(line: &str) -> usize {
    let heading = heading_prefix(line);
    if heading > 0 {
        return heading.min(line.len());
    }
    let (parsed, found) = parse_block(line);
    if found.is_some() {
        parsed.indent.len() + parsed.prefix.len()
    } else {
        0
    }
}

/// The list or quote marker at the start of a line, after its indentation.
fn parse_block(line: &str) -> (Parsed<'_>, Option<Block>) {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, body) = line.split_at(indent_len);
    let found = |len: usize, block: Block| {
        (
            Parsed {
                indent,
                prefix: &body[..len],
                rest: &body[len..],
            },
            Some(block),
        )
    };
    for task in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "+ [ ] "] {
        if body.starts_with(task) {
            return found(task.len(), Block::Task);
        }
    }
    for bullet in ["- ", "* ", "+ "] {
        if body.starts_with(bullet) {
            return found(bullet.len(), Block::Bullet);
        }
    }
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &body[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return found(digits + 2, Block::Numbered);
        }
    }
    if let Some(after) = body.strip_prefix('>') {
        let len = if after.starts_with(' ') { 2 } else { 1 };
        return found(len, Block::Quote);
    }
    (
        Parsed {
            indent,
            prefix: "",
            rest: body,
        },
        None,
    )
}

/// Rewrite each line of the selection's lines with `rewrite`, which gets a
/// line and its index among the non-blank lines and returns the new line and
/// how many bytes of the old line's start became how many of the new one's
/// (so the selection can follow). One [`Edit`] for all of them.
fn rewrite_lines(
    text: &Rope,
    selection: Selection,
    skip_blank: bool,
    mut rewrite: impl FnMut(&str, usize) -> (String, usize, usize),
) -> Edit {
    let range = selection.range();
    let span = line_span(text, &range);
    let old = slice(text, span.clone());
    let mut new = String::with_capacity(old.len() + 16);
    // Old line start, old prefix length, new line start, new prefix length.
    let mut map: Vec<(usize, usize, usize, usize)> = Vec::new();
    let several = old.contains('\n');
    let mut counted = 0;
    let mut old_at = span.start;
    for (index, line) in old.split('\n').enumerate() {
        if index > 0 {
            new.push('\n');
        }
        let new_at = span.start + new.len();
        if skip_blank && several && line.trim().is_empty() {
            new.push_str(line);
            map.push((old_at, 0, new_at, 0));
        } else {
            let (rewritten, old_prefix, new_prefix) = rewrite(line, counted);
            counted += 1;
            new.push_str(&rewritten);
            map.push((old_at, old_prefix, new_at, new_prefix));
        }
        old_at += line.len() + 1;
    }
    // A selection that starts at the very start of a line keeps the whole
    // line, marker included; a caret there moves past the marker.
    let selecting = !selection.is_caret();
    let first = range.start;
    let shift = |offset: usize| -> usize {
        let index = map
            .partition_point(|(start, ..)| *start <= offset)
            .saturating_sub(1);
        let (old_start, old_prefix, new_start, new_prefix) = map[index];
        let within = offset - old_start;
        if selecting && offset == first && within == 0 {
            new_start
        } else if within <= old_prefix {
            new_start + new_prefix
        } else {
            new_start + new_prefix + (within - old_prefix)
        }
    };
    Edit {
        range: span,
        text: new,
        selection: Selection {
            anchor: shift(selection.anchor),
            focus: shift(selection.focus),
        },
    }
}

/// Make the selection's lines headings of `level` (1 to 6), or body text
/// with level 0. A line keeps its text; only the `#` prefix changes.
pub fn set_heading(text: &Rope, selection: Selection, level: u8) -> Edit {
    let level = level.min(6) as usize;
    rewrite_lines(text, selection, true, |line, _| {
        let old = heading_prefix(line);
        let body = &line[old..];
        let prefix = if level == 0 {
            String::new()
        } else {
            format!("{} ", "#".repeat(level))
        };
        (format!("{prefix}{body}"), old, prefix.len())
    })
}

/// Turn the selection's lines into list items or a quote, or back into plain
/// lines if they all already are. A list replaces another kind of list on
/// the same lines; numbered items are numbered from 1. Indentation is kept.
pub fn toggle_block(text: &Rope, selection: Selection, kind: Block) -> Edit {
    let span = line_span(text, &selection.range());
    let old = slice(text, span);
    let lines: Vec<&str> = old.split('\n').collect();
    let several = lines.len() > 1;
    let all = lines
        .iter()
        .filter(|line| !(several && line.trim().is_empty()))
        .all(|line| parse_block(line).1 == Some(kind));
    rewrite_lines(text, selection, true, |line, index| {
        let (parsed, found) = parse_block(line);
        let old_prefix = parsed.indent.len() + parsed.prefix.len();
        if all {
            let new = format!("{}{}", parsed.indent, parsed.rest);
            return (new, old_prefix, parsed.indent.len());
        }
        let marker = match kind {
            Block::Bullet => "- ".to_owned(),
            Block::Numbered => format!("{}. ", index + 1),
            Block::Task => "- [ ] ".to_owned(),
            Block::Quote => "> ".to_owned(),
        };
        // A quote goes around whatever the line is; a list replaces another
        // list's marker but not a quote's.
        let (keep, old_prefix) = match (kind, found) {
            (Block::Quote, _) | (_, None | Some(Block::Quote)) => (line, 0),
            _ => (parsed.rest, old_prefix),
        };
        let (indent, keep) = if kind == Block::Quote || keep == line {
            ("", keep)
        } else {
            (parsed.indent, keep)
        };
        let new = format!("{indent}{marker}{keep}");
        let new_prefix = indent.len() + marker.len();
        (new, old_prefix, new_prefix)
    })
}

/// Whether a line is a code fence: three or more backticks or tildes.
fn is_fence(line: &str) -> bool {
    let body = line.trim_start_matches(' ');
    line.len() - body.len() <= 3 && (body.starts_with("```") || body.starts_with("~~~"))
}

/// The line before or after a byte range of whole lines, with its range
/// (without the line break), if there is one.
fn line_before(text: &Rope, start: usize) -> Option<(Range<usize>, String)> {
    if start == 0 {
        return None;
    }
    let line = text.byte_to_line(start - 1);
    let begin = text.line_to_byte(line);
    Some((begin..start - 1, slice(text, begin..start - 1)))
}

fn line_after(text: &Rope, end: usize) -> Option<(Range<usize>, String)> {
    if end >= text.len_bytes() {
        return None;
    }
    let range = line_span(text, &(end + 1..end + 1));
    Some((range.clone(), slice(text, range)))
}

/// Code block: put the selection's lines between ``` fences, or take the
/// fences away if the lines are exactly the inside of a fenced block. On an
/// empty line the block is empty with the caret inside. The fence is longer
/// than any run of backticks in the lines.
pub fn toggle_code_block(text: &Rope, selection: Selection) -> Edit {
    let range = selection.range();
    let span = line_span(text, &range);
    if let (Some((above, above_text)), Some((below, below_text))) =
        (line_before(text, span.start), line_after(text, span.end))
        && is_fence(&above_text)
        && is_fence(&below_text)
    {
        let inner = slice(text, span.clone());
        let shift = |offset: usize| offset - (span.start - above.start);
        return Edit {
            range: above.start..below.end,
            text: inner,
            selection: Selection {
                anchor: shift(selection.anchor),
                focus: shift(selection.focus),
            },
        };
    }
    let body = slice(text, span.clone());
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    if body.trim().is_empty() {
        let out = format!("{fence}\n\n{fence}");
        return Edit {
            range: span.clone(),
            text: out,
            selection: Selection::caret(span.start + fence.len() + 1),
        };
    }
    let shift = |offset: usize| offset + fence.len() + 1;
    Edit {
        range: span.clone(),
        text: format!("{fence}\n{body}\n{fence}"),
        selection: Selection {
            anchor: shift(selection.anchor),
            focus: shift(selection.focus),
        },
    }
}

/// A horizontal rule (`---`) after the caret's line, on a paragraph of its
/// own: right after text a `---` would turn the text into a heading. The
/// caret goes to the line after the rule.
pub fn insert_rule(text: &Rope, selection: Selection) -> Edit {
    let at = selection.range().end;
    let line = line_span(text, &(at..at));
    let current = slice(text, line.clone());
    if current.trim().is_empty() {
        let after_text = line_before(text, line.start).is_some_and(|(_, l)| !l.trim().is_empty());
        let out = format!("{}---\n", if after_text { "\n" } else { "" });
        let caret = line.start + out.len();
        return Edit {
            range: line,
            text: out,
            selection: Selection::caret(caret),
        };
    }
    let out = "\n\n---\n".to_owned();
    Edit {
        range: line.end..line.end,
        selection: Selection::caret(line.end + out.len()),
        text: out,
    }
}

/// A new footnote: `[^n]` at the caret, with `n` one more than the
/// largest numbered footnote in the document, and its note `[^n]: ` at the
/// end of the document, after a blank line. Two edits, the reference first;
/// the caret ends up after the note's colon, ready for its text. Footnotes
/// with names (`[^why]`) are left alone.
pub fn insert_footnote(text: &Rope, selection: Selection) -> Option<(Edit, Edit)> {
    if text.len_bytes() > 16 * 1024 * 1024 {
        return None;
    }
    let whole = text.to_string();
    let mut largest = 0u32;
    let mut rest = whole.as_str();
    while let Some(at) = rest.find("[^") {
        rest = &rest[at + 2..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty()
            && rest[digits.len()..].starts_with(']')
            && let Ok(n) = digits.parse::<u32>()
        {
            largest = largest.max(n);
        }
    }
    let n = largest + 1;
    let at = selection.range().end;
    let reference = format!("[^{n}]");
    let end = whole.len();
    // After the reference goes in, the end has moved by its length.
    let moved = end + reference.len();
    let lead = if whole.is_empty() || whole.ends_with("\n\n") {
        ""
    } else if whole.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let note = format!("{lead}[^{n}]: ");
    let first = Edit {
        range: at..at,
        text: reference.clone(),
        selection: Selection::caret(at + reference.len()),
    };
    let second = Edit {
        range: moved..moved,
        selection: Selection::caret(moved + note.len()),
        text: note,
    };
    Some((first, second))
}

/// The closing half of a pair typed in Markdown: brackets and backticks.
fn closer_of(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '`' => Some('`'),
        _ => None,
    }
}

/// What typing `typed` does when it opens or closes a pair, with `pending`
/// the offset of a closing half put in automatically that the caret is
/// still in front of. Returns the edit and the new pending offset.
///
/// - An opening bracket or backtick with text selected wraps it.
/// - With only a caret, one followed by the end of the line, a space or a
///   closing bracket gets its closing half, and the caret goes between.
///   A backtick right after another one does not, so fences (```) can be
///   typed.
/// - Typing a closing half the editor put in, right before it, steps over
///   it instead of typing another.
///
/// `None`: type it as usual.
pub fn typed_pair(
    text: &Rope,
    selection: Selection,
    typed: &str,
    pending: Option<usize>,
) -> Option<(Edit, Option<usize>)> {
    let mut chars = typed.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let range = selection.range();
    let len = text.len_bytes();
    let next = (range.end < len).then(|| text.byte(range.end) as char);
    // Stepping over a closing half the editor typed.
    if range.is_empty()
        && pending == Some(range.start)
        && next == Some(c)
        && matches!(c, ')' | ']' | '}' | '`')
    {
        return Some((
            Edit {
                range: range.clone(),
                text: String::new(),
                selection: Selection::caret(range.start + 1),
            },
            None,
        ));
    }
    let close = closer_of(c)?;
    if !range.is_empty() {
        // Wrapping a selection on one line; a longer one is typed over.
        let selected = slice(text, range.clone());
        if selected.contains('\n') || range.len() > 4096 {
            return None;
        }
        let selection = Selection {
            anchor: range.start + 1,
            focus: range.end + 1,
        };
        return Some((
            Edit {
                range: range.clone(),
                text: format!("{c}{selected}{close}"),
                selection,
            },
            None,
        ));
    }
    let at = range.start;
    let previous = (at > 0).then(|| text.byte(at - 1) as char);
    let fits = match next {
        None => true,
        Some(n) => n == '\n' || n == ' ' || n == '\t' || matches!(n, ')' | ']' | '}'),
    };
    if !fits || (c == '`' && previous == Some('`')) {
        return None;
    }
    Some((
        Edit {
            range: at..at,
            text: format!("{c}{close}"),
            selection: Selection::caret(at + 1),
        },
        Some(at + 1),
    ))
}

/// Backspace between an empty pair the editor completed takes both halves.
pub fn backspace_pair(text: &Rope, selection: Selection, pending: Option<usize>) -> Option<Edit> {
    let at = selection.range();
    if !at.is_empty() || pending != Some(at.start) || at.start == 0 || at.start >= text.len_bytes()
    {
        return None;
    }
    let open = text.byte(at.start - 1) as char;
    let close = text.byte(at.start) as char;
    (closer_of(open) == Some(close)).then(|| Edit {
        range: at.start - 1..at.start + 1,
        text: String::new(),
        selection: Selection::caret(at.start - 1),
    })
}

/// Tick or untick the task box of the list item starting at `item` (a
/// byte offset into the source, as the preview records it): `[ ]` becomes
/// `[x]` and `[x]` or `[X]` becomes `[ ]`. The text keeps its length, so
/// the selection stays where it is. `None` when there is no task box there.
pub fn toggle_task(text: &Rope, item: usize, selection: Selection) -> Option<Edit> {
    // The offset comes from a link in the preview, which a document can
    // also write by hand: it must be a character boundary to be an item.
    if item >= text.len_bytes() || floor(text, item) != item {
        return None;
    }
    let line = text.byte_to_line(item);
    let end = if line + 1 < text.len_lines() {
        text.line_to_byte(line + 1)
    } else {
        text.len_bytes()
    };
    // The box follows the item's marker on its first line.
    // At most 64 bytes, cut back to a character boundary (a line of Chinese
    // or Japanese is longer than that and would be cut inside a character).
    let rest = slice(text, item..floor(text, end.min(item + 64)));
    let marker = rest.trim_start_matches([' ', '>']);
    let skipped = rest.len() - marker.len();
    let after = marker.strip_prefix(['-', '*', '+']).or_else(|| {
        let digits = marker.len()
            - marker
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        (digits > 0)
            .then(|| marker[digits..].strip_prefix(['.', ')']))
            .flatten()
    })?;
    let spaces = after.len() - after.trim_start_matches(' ').len();
    if spaces == 0 {
        return None;
    }
    let boxed = &after[spaces..];
    let ticked = if boxed.starts_with("[ ]") {
        false
    } else if boxed.starts_with("[x]") || boxed.starts_with("[X]") {
        true
    } else {
        return None;
    };
    let inside = item + skipped + (marker.len() - after.len()) + spaces + 1;
    Some(Edit {
        range: inside..inside + 1,
        text: if ticked { " " } else { "x" }.to_owned(),
        selection,
    })
}

/// A table of contents linking to every heading, put in as its own block
/// after the caret's line (in place of it when it is empty). `None` when
/// the document has no headings, or is too large to read them all here.
pub fn insert_toc(text: &Rope, selection: Selection) -> Option<Edit> {
    if text.len_bytes() > 16 * 1024 * 1024 {
        return None;
    }
    let toc = crate::structure::table_of_contents(&text.to_string())?;
    let at = selection.range().end;
    let line = line_span(text, &(at..at));
    let current = slice(text, line.clone());
    if current.trim().is_empty() {
        let after_text = line_before(text, line.start).is_some_and(|(_, l)| !l.trim().is_empty());
        let out = format!("{}{toc}", if after_text { "\n" } else { "" });
        let caret = line.start + out.len();
        return Some(Edit {
            range: line,
            text: out,
            selection: Selection::caret(caret),
        });
    }
    // Before the line break that ends the caret's line: that break then
    // ends the last entry.
    let out = format!("\n\n{}", toc.trim_end());
    Some(Edit {
        range: line.end..line.end,
        selection: Selection::caret(line.end + out.len()),
        text: out,
    })
}

/// Pasting a web or mail address over selected text on one line makes that
/// text a link to the address. `None` for any other paste.
pub fn paste_link(text: &Rope, selection: Selection, pasted: &str) -> Option<Edit> {
    let range = selection.range();
    if range.is_empty() || range.len() > 2048 {
        return None;
    }
    let address = pasted.trim();
    let is_web = ["http://", "https://", "mailto:"]
        .iter()
        .any(|prefix| address.starts_with(prefix))
        && address.len() > 8
        && !address.contains(char::is_whitespace);
    if !is_web {
        return None;
    }
    let selected = slice(text, range.clone());
    if selected.contains('\n') || is_address(&selected) {
        return None;
    }
    let out = format!("[{selected}]({address})");
    Some(Edit {
        selection: Selection::caret(range.start + out.len()),
        range,
        text: out,
    })
}

/// A link or image written inline on one line: `[text](address)` or
/// `![text](address)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkAt {
    /// Where it is written, `!` included for an image.
    pub range: Range<usize>,
    pub label: String,
    pub address: String,
    pub image: bool,
}

/// The inline link or image the selection is in, if any: what the link
/// dialog edits instead of inserting a new one. Only the selection's line
/// is read, and only if it is not huge.
pub fn link_around(text: &Rope, selection: Selection) -> Option<LinkAt> {
    let range = selection.range();
    let line = line_span(text, &(range.start..range.start));
    if range.end > line.end || line.len() > 16 * 1024 {
        return None;
    }
    let source = slice(text, line.clone());
    let bytes = source.as_bytes();
    let mut at = 0;
    while let Some(open) = source[at..].find('[').map(|i| at + i) {
        at = open + 1;
        if open > 0 && bytes[open - 1] == b'\\' {
            continue;
        }
        // The label: up to the matching `]`, skipping escaped brackets.
        let mut depth = 0;
        let mut close = None;
        let mut i = open + 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 1,
                b'[' => depth += 1,
                b']' if depth == 0 => {
                    close = Some(i);
                    break;
                }
                b']' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let Some(close) = close else {
            continue;
        };
        if bytes.get(close + 1) != Some(&b'(') {
            continue;
        }
        // The address: up to the matching `)`.
        let mut depth = 0;
        let mut end = None;
        let mut j = close + 2;
        while j < bytes.len() {
            match bytes[j] {
                b'\\' => j += 1,
                b'(' => depth += 1,
                b')' if depth == 0 => {
                    end = Some(j);
                    break;
                }
                b')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let Some(end) = end else {
            continue;
        };
        let image = open > 0 && bytes[open - 1] == b'!';
        let start = line.start + if image { open - 1 } else { open };
        let stop = line.start + end + 1;
        if range.start >= start && range.end <= stop {
            let address = source[close + 2..end].trim();
            let address = address
                .strip_prefix('<')
                .and_then(|a| a.strip_suffix('>'))
                .unwrap_or(address);
            return Some(LinkAt {
                range: start..stop,
                label: source[open + 1..close]
                    .replace("\\[", "[")
                    .replace("\\]", "]"),
                address: address.to_owned(),
                image,
            });
        }
        at = end + 1;
    }
    None
}

/// Write a link or image over `range`, as the link dialog does: brackets in
/// the label are escaped, and an address with spaces or unbalanced
/// parentheses is written between `<` and `>`. The caret goes after it.
pub fn write_link(range: Range<usize>, label: &str, address: &str, image: bool) -> Edit {
    let label = label
        .replace('\n', " ")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let address = address.trim();
    let balanced = address.matches('(').count() == address.matches(')').count();
    let address = if address.contains(char::is_whitespace) || !balanced || address.contains('<') {
        format!("<{}>", address.replace(['<', '>'], ""))
    } else {
        address.to_owned()
    };
    let text = format!("{}[{label}]({address})", if image { "!" } else { "" });
    Edit {
        selection: Selection::caret(range.start + text.len()),
        range,
        text,
    }
}

/// Whether text looks like a link target rather than link text.
pub fn is_address(text: &str) -> bool {
    let text = text.trim();
    !text.contains(char::is_whitespace)
        && ["http://", "https://", "www.", "mailto:", "./", "../", "/"]
            .iter()
            .any(|prefix| text.starts_with(prefix))
}

/// Make the selection a link (or with `image`, an image). Selected text
/// becomes the link text and the caret waits in the empty address; a
/// selected address becomes the address and the caret waits in the text.
pub fn insert_link(text: &Rope, selection: Selection, image: bool) -> Edit {
    let range = selection.range();
    let selected = slice(text, range.clone());
    let bang = if image { "!" } else { "" };
    let (out, caret) = if is_address(&selected) {
        let out = format!("{bang}[]({})", selected.trim());
        let caret = bang.len() + 1;
        (out, caret)
    } else {
        let out = format!("{bang}[{selected}]()");
        let caret = out.len() - 1;
        (out, caret)
    };
    Edit {
        range: range.clone(),
        text: out,
        selection: Selection::caret(range.start + caret),
    }
}

/// Put an empty two-column table on lines of its own at the caret, with a
/// blank line before it, and the caret in its first heading cell. A
/// selection is replaced.
pub fn insert_table(text: &Rope, selection: Selection) -> Edit {
    let range = selection.range();
    let (before, after) = around(text, &range, 2);
    let mut out = String::new();
    // Start on a fresh line, after a blank one.
    if !before.is_empty() {
        if !before.ends_with('\n') {
            out.push_str("\n\n");
        } else if !before.ends_with("\n\n") && range.start > 1 {
            out.push('\n');
        }
    }
    let caret = out.len() + 2;
    out.push_str("|  |  |\n| --- | --- |\n|  |  |\n");
    if !after.is_empty() && !after.starts_with('\n') {
        out.push('\n');
    }
    Edit {
        range: range.clone(),
        text: out,
        selection: Selection::caret(range.start + caret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_footnote_gets_the_next_number_and_its_note_at_the_end() {
        let apply = |text: &str, at: usize| -> (String, Selection) {
            let (first, second) =
                insert_footnote(&Rope::from_str(text), Selection::caret(at)).unwrap();
            let mut out = text.to_owned();
            out.replace_range(first.range, &first.text);
            out.replace_range(second.range, &second.text);
            (out, second.selection)
        };
        let (out, sel) = apply("正文。\n", 9);
        assert_eq!(out, "正文。[^1]\n\n[^1]: ");
        assert_eq!(sel, Selection::caret(out.len()));
        let (out, _) = apply("一[^1] 二[^why]\n\n[^1]: 注\n[^why]: 因为\n", 0);
        assert!(out.starts_with("[^2]一"), "{out}");
        assert!(out.ends_with("[^why]: 因为\n\n[^2]: "), "{out}");
    }

    fn typed(
        text: &str,
        anchor: usize,
        focus: usize,
        c: &str,
        pending: Option<usize>,
    ) -> Option<(String, Selection, Option<usize>)> {
        let rope = Rope::from_str(text);
        let (edit, pending) = typed_pair(&rope, Selection { anchor, focus }, c, pending)?;
        let mut out = text.to_owned();
        out.replace_range(edit.range, &edit.text);
        Some((out, edit.selection, pending))
    }

    #[test]
    fn pairs_are_completed_wrapped_and_stepped_over() {
        // Completed at the end of a line, with the caret between.
        let (out, sel, pending) = typed("看 ", 4, 4, "(", None).unwrap();
        assert_eq!(
            (out.as_str(), sel, pending),
            ("看 ()", Selection::caret(5), Some(5))
        );
        // Stepped over when typed again right before it.
        let (out, sel, pending) = typed("看 ()", 5, 5, ")", Some(5)).unwrap();
        assert_eq!(
            (out.as_str(), sel, pending),
            ("看 ()", Selection::caret(6), None)
        );
        // Not when the closing half was typed by hand.
        assert_eq!(typed("()", 1, 1, ")", None), None);
        // Not in front of a word.
        assert_eq!(typed("word", 0, 0, "[", None), None);
        // A selection is wrapped and stays selected.
        let (out, sel, _) = typed("a link here", 2, 6, "[", None).unwrap();
        assert_eq!(
            (out.as_str(), sel),
            (
                "a [link] here",
                Selection {
                    anchor: 3,
                    focus: 7
                }
            )
        );
        let (out, _, _) = typed("code", 0, 4, "`", None).unwrap();
        assert_eq!(out, "`code`");
        // Fences can be typed: `, step over, then a third ` stays single.
        let (out, _, pending) = typed("", 0, 0, "`", None).unwrap();
        assert_eq!((out.as_str(), pending), ("``", Some(1)));
        let (out, sel, _) = typed("``", 1, 1, "`", Some(1)).unwrap();
        assert_eq!((out.as_str(), sel), ("``", Selection::caret(2)));
        assert_eq!(typed("``", 2, 2, "`", None), None);
        // Other characters, and full-width brackets, are not paired.
        assert_eq!(typed("", 0, 0, "（", None), None);
        assert_eq!(typed("", 0, 0, "ab", None), None);
        // Backspace in an empty completed pair takes both halves.
        let rope = Rope::from_str("x()");
        let edit = backspace_pair(&rope, Selection::caret(2), Some(2)).unwrap();
        assert_eq!((edit.range, edit.selection), (1..3, Selection::caret(1)));
        assert!(backspace_pair(&rope, Selection::caret(2), None).is_none());
    }

    #[test]
    fn task_boxes_are_ticked_and_unticked_in_place() {
        let apply = |text: &str, item: usize| -> Option<String> {
            let rope = Rope::from_str(text);
            let edit = toggle_task(&rope, item, Selection::caret(0))?;
            let mut out = text.to_owned();
            out.replace_range(edit.range, &edit.text);
            Some(out)
        };
        assert_eq!(apply("- [ ] 买菜\n", 0).unwrap(), "- [x] 买菜\n");
        assert_eq!(apply("- [X] 买菜\n", 0).unwrap(), "- [ ] 买菜\n");
        let nested = "- 一\n  * [x] 二\n";
        assert_eq!(apply(nested, 7).unwrap(), "- 一\n  * [ ] 二\n");
        assert_eq!(apply("12. [ ] 十二\n", 0).unwrap(), "12. [x] 十二\n");
        assert_eq!(apply("> - [ ] 引用里\n", 2).unwrap(), "> - [x] 引用里\n");
        assert_eq!(apply("- 普通\n", 0), None);
        assert_eq!(apply("-[ ] 没有空格\n", 0), None);
        assert_eq!(apply("- [ ]", 99), None);
        // A long line of Chinese: 64 bytes in falls inside a character.
        let long = format!("- [ ] {}\n", "买".repeat(40));
        assert!(apply(&long, 0).unwrap().starts_with("- [x] 买"));
        // An offset inside a character, as a hand-written link could give.
        assert_eq!(apply("买\n- [ ] 菜\n", 1), None);
    }

    #[test]
    fn a_table_of_contents_goes_in_as_a_block_of_its_own() {
        let text = Rope::from_str("# 标题\n\n## 一\n\n## 二\n");
        let edit = insert_toc(&text, Selection::caret(8)).unwrap();
        let mut done = text.clone();
        done.remove(text.byte_to_char(edit.range.start)..text.byte_to_char(edit.range.end));
        done.insert(text.byte_to_char(edit.range.start), &edit.text);
        assert_eq!(
            done.to_string(),
            "# 标题\n\n- [标题](#标题)\n  - [一](#一)\n  - [二](#二)\n\n## 一\n\n## 二\n"
        );
        assert!(insert_toc(&Rope::from_str("没有标题"), Selection::caret(0)).is_none());
    }

    /// Apply an edit to plain text; returns the text and the selected part
    /// marked with `[` and `]` (a caret as `|`).
    fn run(text: &str, selection: Range<usize>, edit: impl Fn(&Rope, Selection) -> Edit) -> String {
        let rope = Rope::from_str(text);
        let edit = edit(
            &rope,
            Selection {
                anchor: selection.start,
                focus: selection.end,
            },
        );
        let mut out = String::from(text);
        out.replace_range(edit.range.clone(), &edit.text);
        let range = edit.selection.range();
        if range.is_empty() {
            out.insert(range.start, '|');
        } else {
            out.insert(range.end, ']');
            out.insert(range.start, '[');
        }
        out
    }

    fn at(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).expect("needle in text");
        start..start + needle.len()
    }

    fn inline(text: &str, needle: &str, kind: Inline) -> String {
        run(text, at(text, needle), |rope, sel| {
            toggle_inline(rope, sel, kind)
        })
    }

    #[test]
    fn emphasis_wraps_and_unwraps() {
        let text = "一段中文文字。";
        let bold = inline(text, "中文", Inline::Bold);
        assert_eq!(bold, "一段**[中文]**文字。");
        // Toggling again with the same selection takes it away.
        let unbolded = run(
            "一段**中文**文字。",
            at("一段**中文**文字。", "中文"),
            |r, s| toggle_inline(r, s, Inline::Bold),
        );
        assert_eq!(unbolded, "一段[中文]文字。");
        // So does selecting the marks with the text.
        assert_eq!(
            inline("一段**中文**文字。", "**中文**", Inline::Bold),
            "一段[中文]文字。"
        );
        assert_eq!(
            inline("日本語の文", "日本語", Inline::Italic),
            "*[日本語]*の文"
        );
        assert_eq!(inline("x ~~y~~ z", "y", Inline::Strike), "x [y] z");
        assert_eq!(
            inline("emoji 👍 here", "👍", Inline::Strike),
            "emoji ~~[👍]~~ here"
        );
    }

    #[test]
    fn bold_and_italic_tell_their_stars_apart() {
        // Italic inside bold-italic comes off, bold stays.
        assert_eq!(inline("***x***", "x", Inline::Italic), "**[x]**");
        assert_eq!(inline("***x***", "x", Inline::Bold), "*[x]*");
        // Bold text is not mistaken for italic: italic is added.
        assert_eq!(inline("**x**", "x", Inline::Italic), "***[x]***");
    }

    #[test]
    fn an_empty_selection_gets_both_marks_and_the_caret_between() {
        assert_eq!(
            run("ab", 1..1, |r, s| toggle_inline(r, s, Inline::Bold)),
            "a**|**b"
        );
        assert_eq!(
            run("", 0..0, |r, s| toggle_inline(r, s, Inline::Code)),
            "`|`"
        );
    }

    #[test]
    fn white_space_stays_outside_and_lines_are_wrapped_one_by_one() {
        assert_eq!(
            inline("a  word  b", "  word  ", Inline::Bold),
            "a  **[word]**  b"
        );
        let text = "第一行\n\n第二行";
        let out = run(text, 0..text.len(), |r, s| {
            toggle_inline(r, s, Inline::Italic)
        });
        assert_eq!(out, "[*第一行*\n\n*第二行*]");
    }

    #[test]
    fn marks_go_inside_heading_and_list_markers() {
        let text = "# 会议纪要\n\n1. 第二项\n> 引用\n- [ ] 任务";
        let out = run(text, 0..text.len(), |r, s| {
            toggle_inline(r, s, Inline::Bold)
        });
        assert_eq!(
            out,
            "[# **会议纪要**\n\n1. **第二项**\n> **引用**\n- [ ] **任务**]"
        );
        // One line selected from its start.
        assert_eq!(inline("## 标题", "## 标题", Inline::Italic), "## *[标题]*");
        // A selection inside the text is not affected.
        assert_eq!(
            inline("- 一项文字", "文字", Inline::Bold),
            "- 一项**[文字]**"
        );
    }

    #[test]
    fn code_with_backticks_gets_a_longer_fence() {
        assert_eq!(
            inline("use a`b here", "a`b", Inline::Code),
            "use ``[a`b]`` here"
        );
        assert_eq!(inline("x `y` z", "`y`", Inline::Code), "x [y] z");
        assert_eq!(inline("x ``a`b`` z", "a`b", Inline::Code), "x [a`b] z");
    }

    #[test]
    fn headings_replace_their_prefix_and_keep_the_caret_in_the_text() {
        let text = "标题文字\n下一行";
        assert_eq!(
            run(text, 3..3, |r, s| set_heading(r, s, 2)),
            "## 标|题文字\n下一行"
        );
        assert_eq!(
            run("## 标题文字", 6..6, |r, s| set_heading(r, s, 1)),
            "# 标|题文字"
        );
        assert_eq!(run("### 标题", 4..4, |r, s| set_heading(r, s, 0)), "|标题");
        // A caret inside the old prefix lands at the start of the text.
        assert_eq!(run("###### x", 2..2, |r, s| set_heading(r, s, 3)), "### |x");
        assert_eq!(heading_level(&Rope::from_str("前\n## 二级\n后"), 5), 2);
        assert_eq!(heading_level(&Rope::from_str("#不是标题"), 1), 0);
        assert_eq!(heading_level(&Rope::from_str("    # 代码"), 5), 0);
    }

    #[test]
    fn lists_toggle_over_several_lines_and_skip_blank_ones() {
        let text = "苹果\n香蕉\n\nりんご";
        let all = 0..text.len();
        assert_eq!(
            run(text, all.clone(), |r, s| toggle_block(r, s, Block::Bullet)),
            "[- 苹果\n- 香蕉\n\n- りんご]"
        );
        assert_eq!(
            run(text, all.clone(), |r, s| toggle_block(
                r,
                s,
                Block::Numbered
            )),
            "[1. 苹果\n2. 香蕉\n\n3. りんご]"
        );
        let listed = "- 苹果\n- 香蕉";
        // All already bullets: they become plain lines.
        assert_eq!(
            run(listed, 0..listed.len(), |r, s| toggle_block(
                r,
                s,
                Block::Bullet
            )),
            "[苹果\n香蕉]"
        );
        // Another kind of list is replaced, indentation kept.
        let nested = "  - 子项\n  * 另一项";
        assert_eq!(
            run(nested, 0..nested.len(), |r, s| toggle_block(
                r,
                s,
                Block::Task
            )),
            "[  - [ ] 子项\n  - [ ] 另一项]"
        );
        // A partly numbered selection is numbered throughout.
        let mixed = "1. 一\n二";
        assert_eq!(
            run(mixed, 0..mixed.len(), |r, s| toggle_block(
                r,
                s,
                Block::Numbered
            )),
            "[1. 一\n2. 二]"
        );
    }

    #[test]
    fn quotes_go_around_lists_and_come_off_again() {
        let text = "- 项目";
        let quoted = run(text, 2..2, |r, s| toggle_block(r, s, Block::Quote));
        assert_eq!(quoted, "> - |项目");
        let back = run("> - 项目", 4..4, |r, s| toggle_block(r, s, Block::Quote));
        assert_eq!(back, "- |项目");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_leaves_that_line_alone() {
        let text = "一\n二\n三";
        // From the start of 一 to the start of 三.
        assert_eq!(
            run(text, 0..4 + 4, |r, s| toggle_block(r, s, Block::Bullet)),
            "[- 一\n- 二\n]三"
        );
    }

    #[test]
    fn links_and_images() {
        assert_eq!(
            run("见文档。", at("见文档。", "文档"), |r, s| {
                insert_link(r, s, false)
            }),
            "见[文档](|)。"
        );
        let url = "https://example.com/页";
        assert_eq!(
            run(url, 0..url.len(), |r, s| insert_link(r, s, false)),
            "[|](https://example.com/页)"
        );
        assert_eq!(run("x", 1..1, |r, s| insert_link(r, s, true)), "x![](|)");
    }

    #[test]
    fn tables_go_on_lines_of_their_own() {
        // The caret shows as `|`, here in the first cell: `| | |  |`.
        assert_eq!(
            run("段落", 6..6, insert_table),
            "段落\n\n| | |  |\n| --- | --- |\n|  |  |\n"
        );
        assert_eq!(
            run("", 0..0, insert_table),
            "| | |  |\n| --- | --- |\n|  |  |\n"
        );
        // On an empty line between two paragraphs.
        assert_eq!(
            run("上\n\n下", 4..4, insert_table),
            "上\n\n| | |  |\n| --- | --- |\n|  |  |\n\n下"
        );
    }

    #[test]
    fn the_heading_level_of_a_huge_line_is_found_from_its_start() {
        let text = format!("## {}", "长".repeat(1_000_000));
        let rope = Rope::from_str(&text);
        assert_eq!(heading_level(&rope, text.len() / 2), 2);
        assert_eq!(heading_level(&rope, text.len() + 10), 2);
    }

    #[test]
    fn only_the_selected_lines_are_read() {
        // A long text: the edit covers one line, wherever it is.
        let mut text = "填充\n".repeat(50_000);
        let start = text.len();
        text.push_str("目标行\n");
        text.push_str(&"填充\n".repeat(50_000));
        let rope = Rope::from_str(&text);
        let edit = set_heading(&rope, Selection::caret(start + 3), 1);
        assert_eq!(edit.range, start..start + "目标行".len());
        assert_eq!(edit.text, "# 目标行");
    }

    /// Text with a caret written `|` or a selection written `«…»`, as the
    /// text and the range.
    fn marked(input: &str) -> (String, Range<usize>) {
        if let Some(at) = input.find('|') {
            return (input.replacen('|', "", 1), at..at);
        }
        let start = input.find('«').expect("a caret or a selection");
        let text = input.replacen('«', "", 1);
        let end = text.find('»').expect("the end of the selection");
        (text.replacen('»', "", 1), start..end)
    }

    fn styled(input: &str, kind: Inline) -> String {
        let (text, range) = marked(input);
        run(&text, range, |rope, sel| {
            toggle_inline_in(rope, &Highlighter::new(rope), sel, kind)
        })
    }

    fn styles_of(input: &str) -> Styles {
        let (text, range) = marked(input);
        let rope = Rope::from_str(&text);
        let selection = Selection {
            anchor: range.start,
            focus: range.end,
        };
        styles(&rope, Some(&Highlighter::new(&rope)), selection)
    }

    #[test]
    fn caret_inside_styled_text_takes_the_style_off() {
        assert_eq!(
            styled("一段**中|文**文字。", Inline::Bold),
            "一段中|文文字。"
        );
        assert_eq!(styled("一段**|中文**。", Inline::Bold), "一段|中文。");
        assert_eq!(styled("a __bo|ld__ b", Inline::Bold), "a bo|ld b");
        assert_eq!(styled("x *斜|体* y", Inline::Italic), "x 斜|体 y");
        assert_eq!(styled("x ~~删|除~~ y", Inline::Strike), "x 删|除 y");
        assert_eq!(styled("run `co|de` now", Inline::Code), "run co|de now");
        // Bold and italic at once: each takes its own marks.
        assert_eq!(styled("***b|i***", Inline::Bold), "*b|i*");
        assert_eq!(styled("***b|i***", Inline::Italic), "**b|i**");
        // Just outside the marks is not inside: the marks go in as before.
        assert_eq!(styled("**粗体**|后", Inline::Bold), "**粗体****|**后");
        assert_eq!(styled("前|**粗体**", Inline::Bold), "前**|****粗体**");
        // Plain text is wrapped as before.
        assert_eq!(styled("一段«中文»。", Inline::Bold), "一段**[中文]**。");
    }

    #[test]
    fn a_selection_inside_styled_text_comes_out_of_it() {
        assert_eq!(
            styled("**一二«三»四五**", Inline::Bold),
            "**一二**[三]**四五**"
        );
        assert_eq!(styled("**«一二»三四五**", Inline::Bold), "[一二]**三四五**");
        assert_eq!(styled("**一二三«四五»**", Inline::Bold), "**一二三**[四五]");
        // Underscores would not work between the pieces; stars do.
        assert_eq!(styled("__一«二»三__", Inline::Bold), "**一**[二]**三**");
        assert_eq!(styled("_一«二»三_", Inline::Italic), "*一*[二]*三*");
        // White space stays outside the marks on both sides.
        assert_eq!(
            styled("**one «two» three**", Inline::Bold),
            "**one** [two] **three**"
        );
        // The whole text, with or without its marks: the style comes off.
        assert_eq!(styled("**«一二三»**", Inline::Bold), "[一二三]");
        assert_eq!(styled("«**一二三**»", Inline::Bold), "[一二三]");
    }

    #[test]
    fn styles_report_what_a_click_would_take_away() {
        let bold = styles_of("一段**中|文**文字。");
        assert!(bold.bold && !bold.italic && !bold.code);
        assert!(!styles_of("一段**中文**|文字。").bold);
        assert!(!styles_of("一段|**中文**文字。").bold);
        assert!(styles_of("**一«二»三**").bold);
        // Part of the selection is plain.
        assert!(!styles_of("**一«二**三»").bold);
        let both = styles_of("***b|i***");
        assert!(both.bold && both.italic);
        assert!(styles_of("a `c|ode` b").code);
        assert!(styles_of("a ~~s|trike~~ b").strike);
        assert_eq!(styles_of("plain |text"), Styles::default());
        // Without the highlighting, an exact selection still counts.
        let rope = Rope::from_str("x **bold** y");
        let exact = Selection {
            anchor: 4,
            focus: 8,
        };
        assert!(styles(&rope, None, exact).bold);
        assert!(!styles(&rope, None, Selection::caret(6)).bold);
    }

    #[test]
    fn styles_report_the_block_of_the_lines() {
        assert_eq!(styles_of("- it|em").block, Some(Block::Bullet));
        assert_eq!(styles_of("  1. 第|一").block, Some(Block::Numbered));
        assert_eq!(styles_of("> 引|用").block, Some(Block::Quote));
        let task = "- [ ] 待办";
        let rope = Rope::from_str(task);
        let caret = Selection::caret(task.len());
        assert_eq!(styles(&rope, None, caret).block, Some(Block::Task));
        assert_eq!(
            styles_of("1. «a\n\n2. b»").block,
            Some(Block::Numbered),
            "blank lines among several do not count"
        );
        assert_eq!(styles_of("- «a\nb»").block, None);
        assert_eq!(styles_of("- «a\n1. b»").block, None);
        assert_eq!(styles_of("pla|in").block, None);
    }

    #[test]
    fn styles_of_a_huge_line_read_only_its_start() {
        let long = format!("- {}", "字".repeat(1 << 20));
        let rope = Rope::from_str(&long);
        let caret = Selection::caret(long.len());
        assert_eq!(styles(&rope, None, caret).block, Some(Block::Bullet));
        let all = Selection {
            anchor: 0,
            focus: long.len(),
        };
        assert_eq!(styles(&rope, None, all), Styles::default());
    }

    /// Enter at the caret or selection; `None` where it is a plain break.
    fn enter(input: &str) -> Option<String> {
        let (text, range) = marked(input);
        let rope = Rope::from_str(&text);
        let selection = Selection {
            anchor: range.start,
            focus: range.end,
        };
        line_break(&rope, Some(&Highlighter::new(&rope)), selection)?;
        Some(run(&text, range, |rope, sel| {
            line_break(rope, Some(&Highlighter::new(rope)), sel).expect("an edit")
        }))
    }

    #[test]
    fn enter_carries_list_and_quote_markers_on() {
        assert_eq!(enter("- 苹果|").as_deref(), Some("- 苹果\n- |"));
        assert_eq!(enter("* a|").as_deref(), Some("* a\n* |"));
        assert_eq!(enter("  + 缩进|").as_deref(), Some("  + 缩进\n  + |"));
        assert_eq!(enter("1. 第一|").as_deref(), Some("1. 第一\n2. |"));
        assert_eq!(enter("9) nine|").as_deref(), Some("9) nine\n10) |"));
        // A ticked box starts an unticked one.
        assert_eq!(enter("- [x] 完成|").as_deref(), Some("- [x] 完成\n- [ ] |"));
        assert_eq!(enter("> 引用|").as_deref(), Some("> 引用\n> |"));
        assert_eq!(enter(">紧挨|").as_deref(), Some(">紧挨\n> |"));
        assert_eq!(
            enter("> - 引用里的列表|").as_deref(),
            Some("> - 引用里的列表\n> - |")
        );
        // In the middle of an item, the rest moves to the new one.
        assert_eq!(enter("- 前|后").as_deref(), Some("- 前\n- |后"));
        // A selection is replaced.
        assert_eq!(enter("- 留«删掉»").as_deref(), Some("- 留\n- |"));
    }

    #[test]
    fn enter_on_an_empty_item_ends_the_list() {
        assert_eq!(enter("- a\n- |").as_deref(), Some("- a\n|"));
        assert_eq!(enter("1. a\n2.  |").as_deref(), Some("1. a\n|"));
        assert_eq!(enter("- [ ] |").as_deref(), Some("|"));
        assert_eq!(enter("> |").as_deref(), Some("|"));
    }

    #[test]
    fn enter_is_a_plain_break_elsewhere() {
        assert_eq!(enter("正文|"), None);
        assert_eq!(enter("# 标题|"), None);
        // Inside the marker.
        assert_eq!(enter("-| a"), None);
        assert_eq!(enter("12|. a"), None);
        // In a code block, a line that looks like an item is code.
        assert_eq!(enter("```\n- 代码|\n```"), None);
        assert_eq!(enter("    - 缩进代码|"), None);
        // Indented as far inside a list, it is an item.
        assert_eq!(
            enter("- a\n    - 子项|").as_deref(),
            Some("- a\n    - 子项\n    - |")
        );
    }

    #[test]
    fn enter_renumbers_the_items_below() {
        assert_eq!(
            enter("1. a|\n2. b\n3. c").as_deref(),
            Some("1. a\n2. |\n3. b\n4. c")
        );
        // Nested lines and one blank line stay in the list; numbers that
        // are already right stop the work.
        assert_eq!(
            enter("1. a|\n2. b\n   - 子项\n\n3. c\n\n\n4. 另一个列表").as_deref(),
            Some("1. a\n2. |\n3. b\n   - 子项\n\n4. c\n\n\n4. 另一个列表")
        );
        assert_eq!(
            enter("1. a|\n3. b\n4. c").as_deref(),
            Some("1. a\n2. |\n3. b\n4. c"),
            "already counting on"
        );
        // A less indented line ends the nested list.
        assert_eq!(
            enter("1. x\n   1. a|\n   2. b\n2. y").as_deref(),
            Some("1. x\n   1. a\n   2. |\n   3. b\n2. y")
        );
        // Text after the caret moves along.
        assert_eq!(
            enter("1. 一|二\n2. 三").as_deref(),
            Some("1. 一\n2. |二\n3. 三")
        );
    }

    fn tab(input: &str, outdent: bool) -> Option<String> {
        let (text, range) = marked(input);
        let rope = Rope::from_str(&text);
        let selection = Selection {
            anchor: range.start,
            focus: range.end,
        };
        indent(&rope, selection, outdent)?;
        Some(run(&text, range, |rope, sel| {
            indent(rope, sel, outdent).expect("an edit")
        }))
    }

    #[test]
    fn tab_moves_an_item_under_the_one_above() {
        assert_eq!(tab("- a\n- b|", false).as_deref(), Some("- a\n  - b|"));
        assert_eq!(tab("1. a\n2. b|", false).as_deref(), Some("1. a\n   1. b|"));
        assert_eq!(
            tab("- [ ] a\n- [ ] |b", false).as_deref(),
            Some("- [ ] a\n  - [ ] |b")
        );
        // Further in, past the items already there.
        assert_eq!(
            tab("- a\n  - b\n  - c|", false).as_deref(),
            Some("- a\n  - b\n    - c|")
        );
        // Numbered items already at the new level: the number stays.
        assert_eq!(
            tab("1. a\n   1. b\n2. c|", false).as_deref(),
            Some("1. a\n   1. b\n   2. c|")
        );
        // The first item of a list has nothing to go under.
        assert_eq!(tab("- a|", false).as_deref(), Some("- a|"));
        assert_eq!(tab("- a\n  - b|", false).as_deref(), Some("- a\n  - b|"));
        // Several items move together.
        assert_eq!(
            tab("- a\n- «b\n- c»", false).as_deref(),
            Some("- a\n  - [b\n  - c]")
        );
    }

    #[test]
    fn shift_tab_moves_an_item_out() {
        assert_eq!(tab("- a\n  - b|", true).as_deref(), Some("- a\n- b|"));
        // A numbered item counts on from the item it belonged to, and so do
        // the items after it at that level; its own items stay below it.
        assert_eq!(tab("1. a\n   1. b|", true).as_deref(), Some("1. a\n2. b|"));
        assert_eq!(
            tab("1. a\n   1. b|\n      - x\n2. c\n3. d\n\n- e", true).as_deref(),
            Some("1. a\n2. b|\n      - x\n3. c\n4. d\n\n- e")
        );
        assert_eq!(
            tab("1. a\n   1. «b\n   2. c»", true).as_deref(),
            Some("1. a\n2. [b\n3. c]")
        );
        // Under a bulleted item a number stays as it is.
        assert_eq!(tab("- a\n  1. b|", true).as_deref(), Some("- a\n1. b|"));
        assert_eq!(tab("1) a\n   1) b|", true).as_deref(), Some("1) a\n2) b|"));
        assert_eq!(
            tab("- a\n  - b\n    - c|", true).as_deref(),
            Some("- a\n  - b\n  - c|")
        );
        // Already at the left edge.
        assert_eq!(tab("- a|", true).as_deref(), Some("- a|"));
    }

    #[test]
    fn tab_outside_a_list_is_an_ordinary_key() {
        assert_eq!(tab("正文|", false), None);
        assert_eq!(tab("> - 引用里的项|", false), None);
        assert_eq!(tab("# 标题|", true), None);
    }

    /// Replace `range` of `before` with `inserted`, as an edit by hand, then
    /// put the list numbers right as the editor does. The caret, after the
    /// inserted text, is marked `|`.
    fn renumbered(before: &str, range: Range<usize>, inserted: &str, highlight: bool) -> String {
        let removed = &before[range.clone()];
        let mut after = String::from(before);
        after.replace_range(range.clone(), inserted);
        let changed = range.start..range.start + inserted.len();
        let rope = Rope::from_str(&after);
        let highlighter = Highlighter::new(&rope);
        let edit = renumber_lists(
            &rope,
            highlight.then_some(&highlighter),
            changed.clone(),
            removed,
            Selection::caret(changed.end),
        );
        let caret = match edit {
            Some(edit) => {
                after.replace_range(edit.range, &edit.text);
                edit.selection.focus
            }
            None => changed.end,
        };
        after.insert(caret, '|');
        after
    }

    fn delete(before: &str, needle: &str) -> String {
        renumbered(before, at(before, needle), "", true)
    }

    #[test]
    fn deleting_an_item_renumbers_the_ones_below() {
        assert_eq!(
            delete("1. a\n2. b\n3. c\n4. d", "2. b\n"),
            "1. a\n|2. c\n3. d"
        );
        // Joining two items with Backspace.
        assert_eq!(delete("1. a\n2. b\n3. c", "\n2. b"), "1. a|\n2. c");
        // The first item: the one after it takes its number.
        assert_eq!(delete("1. a\n2. b\n3. c", "1. a\n"), "|1. b\n2. c");
        assert_eq!(delete("5. a\n6. b", "5. a\n"), "|5. b");
        // Items under one that went join the list above them.
        assert_eq!(
            delete("1. a\n   1. x\n2. b\n   1. y\n3. c", "2. b\n"),
            "1. a\n   1. x\n|   2. y\n2. c"
        );
        // Other lists nested in the items stay as they are.
        assert_eq!(
            delete("1. a\n   1. x\n   2. y\n2. b\n3. c", "2. b\n"),
            "1. a\n   1. x\n   2. y\n|2. c"
        );
        // `1)` items are a list of their own.
        assert_eq!(
            delete("1. a\n2. b\n1) c\n2) d", "2. b\n"),
            "1. a\n|1) c\n2) d"
        );
    }

    #[test]
    fn pasting_items_renumbers_them_and_the_ones_below() {
        let before = "1. a\n2. b";
        assert_eq!(
            renumbered(before, 5..5, "1. x\n2. y\n", true),
            "1. a\n2. x\n3. y\n|4. b"
        );
        // The caret moves with numbers that grow.
        let before = "9. a\n";
        assert_eq!(
            renumbered(before, 5..5, "1. x\n1. y", true),
            "9. a\n10. x\n11. y|"
        );
    }

    #[test]
    fn numbers_the_edit_did_not_move_stay() {
        // A list numbered 1, 1, 1 keeps that.
        let before = "1. a\n1. b\n1. c\n1. d";
        assert_eq!(delete(before, "1. c\n"), "1. a\n1. b\n|1. d");
        // A blank line put between items moves no number; further down the
        // numbers stay even if they were not in order.
        let before = "1. a\n2. b\n9. c";
        assert_eq!(renumbered(before, 4..4, "\n", true), "1. a\n|\n2. b\n9. c");
        // Lines in a code block are not items.
        let before = "```\n1. x\n2. y\n3. z\n```";
        assert_eq!(delete(before, "2. y\n"), "```\n1. x\n|3. z\n```");
        // Bulleted lists have nothing to renumber.
        assert_eq!(delete("- a\n- b\n- c", "- b\n"), "- a\n|- c");
        // Text that is not in the list ends it.
        assert_eq!(
            delete("1. a\n2. b\n\n正文\n3. c", "2. b\n"),
            "1. a\n|\n正文\n3. c"
        );
    }

    /// Tab or Shift+Tab, then the renumbering the editor does after it.
    fn tab_renumbered(input: &str, outdent: bool) -> String {
        let (text, range) = marked(input);
        let rope = Rope::from_str(&text);
        let selection = Selection {
            anchor: range.start,
            focus: range.end,
        };
        let edit = indent(&rope, selection, outdent).expect("a list item");
        let mut after = text.clone();
        after.replace_range(edit.range.clone(), &edit.text);
        let removed = &text[edit.range.clone()];
        let changed = edit.range.start..edit.range.start + edit.text.len();
        let rope = Rope::from_str(&after);
        let fixed = renumber_lists(&rope, None, changed, removed, edit.selection);
        let caret = match fixed {
            Some(fixed) => {
                after.replace_range(fixed.range, &fixed.text);
                fixed.selection.focus
            }
            None => edit.selection.focus,
        };
        after.insert(caret, '|');
        after
    }

    #[test]
    fn tab_and_shift_tab_keep_both_levels_in_order() {
        assert_eq!(
            tab_renumbered("1. a\n2. b|\n3. c\n4. d", false),
            "1. a\n   1. b|\n2. c\n3. d"
        );
        assert_eq!(
            tab_renumbered("1. a\n   1. b|\n2. c", true),
            "1. a\n2. b|\n3. c"
        );
        // Going in after items already there continues their numbers.
        assert_eq!(
            tab_renumbered("1. a\n   1. x\n2. b|\n3. c", false),
            "1. a\n   1. x\n   2. b|\n2. c"
        );
    }

    #[test]
    fn making_lines_a_list_after_a_list_continues_it() {
        let text = "1. a\n2. b\nc\nd";
        let rope = Rope::from_str(text);
        let selection = Selection {
            anchor: at(text, "c").start,
            focus: text.len(),
        };
        let edit = toggle_block(&rope, selection, Block::Numbered);
        let mut after = String::from(text);
        after.replace_range(edit.range.clone(), &edit.text);
        assert_eq!(after, "1. a\n2. b\n1. c\n2. d");
        let changed = edit.range.start..edit.range.start + edit.text.len();
        let rope = Rope::from_str(&after);
        let fixed = renumber_lists(&rope, None, changed, &text[edit.range], edit.selection)
            .expect("new numbers");
        after.replace_range(fixed.range, &fixed.text);
        assert_eq!(after, "1. a\n2. b\n3. c\n4. d");
    }

    #[test]
    fn ending_a_list_in_the_middle_keeps_the_rest_in_order() {
        // Enter on an empty item takes its marker away; one blank line does
        // not end a list, so the item below counts on from the one above.
        let before = "1. a\n2. \n3. b";
        assert_eq!(delete(before, "2. "), "1. a\n|\n2. b");
    }

    #[test]
    fn code_blocks_go_around_lines_and_come_off_again() {
        let wrap = |input: &str| {
            let (text, range) = marked(input);
            run(&text, range, toggle_code_block)
        };
        assert_eq!(
            wrap("前\nfn «main»() {}\n后"),
            "前\n```\nfn [main]() {}\n```\n后"
        );
        assert_eq!(wrap("a «b\nc» d"), "```\na [b\nc] d\n```");
        // Back again.
        assert_eq!(wrap("x\n```\nfn «main»()\n```\ny"), "x\nfn [main]()\ny");
        // An empty line: an empty block, caret inside.
        assert_eq!(wrap("正文\n|\n"), "正文\n```\n|\n```\n");
        // Backticks inside need a longer fence.
        assert_eq!(wrap("|用 ``` 围起来"), "````\n|用 ``` 围起来\n````");
    }

    #[test]
    fn rules_go_on_a_paragraph_of_their_own() {
        let rule = |input: &str| {
            let (text, range) = marked(input);
            run(&text, range, insert_rule)
        };
        assert_eq!(rule("正文|\n下一段"), "正文\n\n---\n|\n下一段");
        assert_eq!(rule("正文|"), "正文\n\n---\n|");
        // On an empty line right after text, a blank line first.
        assert_eq!(rule("正文\n|"), "正文\n\n---\n|");
        assert_eq!(rule("正文\n\n|\n后文"), "正文\n\n---\n|\n后文");
    }

    #[test]
    fn pasting_an_address_over_text_makes_a_link() {
        let paste = |input: &str, pasted: &str| {
            let (text, range) = marked(input);
            let rope = Rope::from_str(&text);
            let selection = Selection {
                anchor: range.start,
                focus: range.end,
            };
            paste_link(&rope, selection, pasted)?;
            Some(run(&text, range, |r, s| paste_link(r, s, pasted).unwrap()))
        };
        assert_eq!(
            paste("看«官网»吧", "https://example.com/a?b=1").as_deref(),
            Some("看[官网](https://example.com/a?b=1)|吧")
        );
        assert_eq!(
            paste("看«官网»吧", "  mailto:a@b.c\n").as_deref(),
            Some("看[官网](mailto:a@b.c)|吧")
        );
        // Not an address, nothing selected, several lines, or an address
        // selected: an ordinary paste.
        assert_eq!(paste("看«官网»吧", "普通文字"), None);
        assert_eq!(paste("看|吧", "https://example.com"), None);
        assert_eq!(paste("«一\n二»", "https://example.com"), None);
        assert_eq!(paste("«https://old.example»", "https://example.com"), None);
    }

    #[test]
    fn the_link_around_the_caret_is_found_and_rewritten() {
        let found = |input: &str| {
            let (text, range) = marked(input);
            link_around(
                &Rope::from_str(&text),
                Selection {
                    anchor: range.start,
                    focus: range.end,
                },
            )
        };
        let link = found("看 [官|网](https://example.com/a(b)) 吧").unwrap();
        assert_eq!(link.label, "官网");
        assert_eq!(link.address, "https://example.com/a(b)");
        assert!(!link.image);
        assert_eq!(
            &"看 [官网](https://example.com/a(b)) 吧"[link.range.clone()],
            "[官网](https://example.com/a(b))"
        );
        let image = found("![图|片](<my pic.png>)").unwrap();
        assert!(image.image);
        assert_eq!(image.address, "my pic.png");
        assert_eq!(image.range.start, 0);
        // Escaped brackets in the label; outside any link; a reference link.
        assert_eq!(found("[a \\] |b](x)").unwrap().label, "a ] b");
        assert_eq!(found("前 [a](x) 后|"), None);
        assert_eq!(found("[only|brackets]"), None);
        // Writing one back.
        let edit = write_link(2..5, "官[网]", "https://example.com/a b", false);
        assert_eq!(edit.text, "[官\\[网\\]](<https://example.com/a b>)");
        assert_eq!(edit.selection, Selection::caret(2 + edit.text.len()));
        assert_eq!(
            write_link(0..0, "图", "pic.png", true).text,
            "![图](pic.png)"
        );
        assert_eq!(write_link(0..0, "x", "a(b", false).text, "[x](<a(b>)");
    }
}
