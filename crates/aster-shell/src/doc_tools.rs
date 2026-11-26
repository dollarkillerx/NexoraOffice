//! The assistant's and MCP clients' tools on Word documents: reading the
//! body by block, and changing it. Like `assistant` (Markdown) and
//! `sheet_tools` (workbooks), these run on the interface thread on a tab the
//! policy has already let the caller act on; they only know the document.
//!
//! **Addresses.** The body is a list of blocks, each a paragraph or a
//! table, numbered from 1 as lines are in the other tools. A table cell is
//! `block` plus `row` and `cell`, both from 1: `cell` counts the cells of
//! the row as the file lists them (a merged cell is one). Every address is
//! checked against the document as it is when the call runs, and a bad one
//! is answered with the range that is valid.
//!
//! **Changes.** Every write is one call to [`DocTab::edit_document`]: the
//! change is made on a copy of the document and put in as one step of undo,
//! so a call that fails halfway changes nothing, and the user undoes a whole
//! call with one Cmd+Z. Text is changed with the editing functions of
//! `aster_writer::edit`, the ones the keyboard uses, so a change keeps the
//! look of the text it replaces and the markers (bookmarks, comment ranges)
//! it passes.
//!
//! **What is not done.** Pictures and other objects, headers and footers,
//! footnotes and endnotes are not read or edited. A block that holds a
//! picture, a footnote reference, a page number or a ruby is never replaced
//! or deleted whole, and a match that touches one is left alone, so that the
//! assistant never destroys what it cannot see.

use aster_writer::edit::{self, ListKind, Selection};
use aster_writer::{
    Align, Block, Document, Inline, Link, NumberFormat, OBJECT, Paragraph, Path, Pos, Rgba, Run,
    RunLook, RunPatch, StoryId, Table, Underline, UnderlineStyle,
};
use serde_json::{Map, Value, json};

use crate::assistant;
use crate::i18n::{self, Strings};
use crate::writer_tab::DocTab;

/// The longest text one argument may carry, in characters.
pub const MAX_TEXT: usize = 100_000;
/// At most this many paragraphs one text argument may make.
const MAX_LINES: usize = 2_000;
/// The longest phrase to look for or to format.
const MAX_PHRASE: usize = 500;
/// The longest link address.
const MAX_URL: usize = 2_000;
/// A table made by `edit_table` has at most this many cells.
pub const MAX_CELLS: usize = 2_000;
const MAX_TABLE_ROWS: usize = 200;
const MAX_TABLE_COLUMNS: usize = 20;
/// Rows or columns added in one call.
const MAX_COUNT: usize = 50;

/// Blocks `read_word` gives when not told, and the most it gives.
const DEFAULT_BLOCKS: usize = 60;
const MAX_BLOCKS: usize = 300;
/// A read stops after the block that brings its text past this many
/// characters (but always gives one block).
const READ_BUDGET: usize = 30_000;
/// A paragraph longer than this is cut when read, a table cell at this.
const PARAGRAPH_READ: usize = 6_000;
const CELL_READ: usize = 400;
/// Rows of a table `read_word` gives when not told, and the most.
const DEFAULT_ROWS: usize = 50;
const MAX_ROWS_READ: usize = 200;

/// What a write is answered while the user's input method is composing.
pub const COMPOSING: &str = "the user is typing with an input method; try again in a moment";

/// The names of the tools this module carries out on Word documents.
pub const WRITE_TOOLS: &[&str] = &[
    "replace_paragraphs",
    "insert_paragraphs",
    "delete_blocks",
    "find_and_replace",
    "format_text",
    "edit_table",
];

/// Run a tool on a Word document. Returns the result for the caller and
/// whether the document changed.
pub fn run(tab: &mut DocTab, tool: &str, args: &Value) -> Result<(String, bool), String> {
    match tool {
        "read_word" => read(tab.view.document(), args).map(|text| (text, false)),
        "read_document" | "search_document" | "get_selection" | "get_outline" => {
            let text = tab.view.document().plain_text().replace(OBJECT, "");
            let headings = outline(tab);
            assistant::run_on_word(&text, &tab.view.selected_text(), headings, tool, args)
                .map(|text| (text, false))
        }
        name if WRITE_TOOLS.contains(&name) => write(tab, name, args),
        other => Err(format!(
            "`{other}` works on Markdown documents; on a Word document use read_word, \
             replace_paragraphs, insert_paragraphs, delete_blocks, find_and_replace, \
             format_text and edit_table"
        )),
    }
}

/// The headings of the outline, with the block each is.
fn outline(tab: &DocTab) -> Vec<Value> {
    tab.view
        .outline()
        .iter()
        .map(|e| {
            json!({
                "level": u32::from(e.level) + 1,
                "text": e.text,
                "page": e.page + 1,
                "block": e.pos.path.top() + 1,
            })
        })
        .collect()
}

// --- Arguments ----------------------------------------------------------------

/// A whole number from 1, or `None` when the argument is not given.
fn count_arg(args: &Value, name: &str) -> Result<Option<usize>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| *n >= 1)
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("`{name}` must be a whole number from 1")),
    }
}

fn need(args: &Value, name: &str) -> Result<usize, String> {
    count_arg(args, name)?.ok_or_else(|| format!("`{name}` is needed"))
}

/// A text argument: not longer than `max` characters, without the object
/// character (which stands for pictures), line breaks as `\n`.
fn text_arg(args: &Value, name: &str, max: usize) -> Result<String, String> {
    let text = args[name]
        .as_str()
        .ok_or_else(|| format!("`{name}` must be text"))?;
    if text.chars().nth(max).is_some() {
        return Err(format!("`{name}` is too long: at most {max} characters"));
    }
    if text.contains(OBJECT) {
        return Err(format!(
            "`{name}` holds U+FFFC, which stands for a picture or another object; leave it out"
        ));
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn optional_text(args: &Value, name: &str, max: usize) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text_arg(args, name, max).map(Some),
    }
}

fn lines_of(text: &str) -> usize {
    text.matches('\n').count() + 1
}

/// The first and last block (0-based) of the range `block` to `end_block`.
fn block_range(doc: &Document, args: &Value) -> Result<(usize, usize), String> {
    let total = doc.body.blocks.len();
    let first = need(args, "block")?;
    let last = count_arg(args, "end_block")?.unwrap_or(first);
    if first > total {
        return Err(outside(first, total));
    }
    if last > total {
        return Err(outside(last, total));
    }
    if last < first {
        return Err(format!(
            "`end_block` {last} comes before `block` {first}; give them in document order"
        ));
    }
    Ok((first - 1, last - 1))
}

fn outside(block: usize, total: usize) -> String {
    if total == 0 {
        format!("block {block} is outside the document: it has no blocks")
    } else {
        format!("block {block} is outside the document (blocks 1 to {total})")
    }
}

/// The paragraph at body block `index`, or why it is not one.
fn paragraph_block(doc: &Document, index: usize) -> Result<&Paragraph, String> {
    match doc.body.blocks.get(index) {
        Some(Block::Paragraph(p)) => Ok(p),
        Some(Block::Table(table)) => Err(format!(
            "block {} is a table ({} rows); use edit_table for its cells or delete_blocks to remove it",
            index + 1,
            table.rows.len()
        )),
        None => Err(outside(index + 1, doc.body.blocks.len())),
    }
}

fn table_block(doc: &Document, index: usize) -> Result<&Table, String> {
    match doc.body.blocks.get(index) {
        Some(Block::Table(table)) => Ok(table),
        Some(Block::Paragraph(_)) => {
            Err(format!("block {} is a paragraph, not a table", index + 1))
        }
        None => Err(outside(index + 1, doc.body.blocks.len())),
    }
}

/// Whether a run is something the tools cannot see or edit, which they
/// therefore never remove.
fn is_object(run: &Run) -> bool {
    matches!(
        run.kind,
        Inline::Drawing(_)
            | Inline::Footnote(_)
            | Inline::Endnote(_)
            | Inline::PageNumber(_)
            | Inline::Ruby(_)
    )
}

fn objects_in(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(p) => p.runs.iter().filter(|run| is_object(run)).count(),
            Block::Table(table) => table
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .map(|cell| objects_in(&cell.blocks))
                .sum(),
        })
        .sum()
}

/// The byte ranges of a paragraph's text that are objects.
fn object_ranges(p: &Paragraph) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for run in &p.runs {
        let end = start + run.text.len();
        if is_object(run) && end > start {
            out.push((start, end));
        }
        start = end;
    }
    out
}

/// A paragraph's text as the tools show it: objects left out.
fn shown(p: &Paragraph) -> String {
    p.text().replace(OBJECT, "")
}

fn cut(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (format!("{}…", &text[..at]), true),
        None => (text.to_owned(), false),
    }
}

// --- Reading ------------------------------------------------------------------

/// The name of a paragraph's style, as the file gives it.
fn style_name(doc: &Document, p: &Paragraph) -> String {
    let id = p
        .props
        .style
        .as_deref()
        .or(doc.styles.default_style.as_deref());
    id.and_then(|id| doc.styles.get(id))
        .map_or_else(|| "Normal".to_owned(), |style| style.name.clone())
}

fn list_kind(doc: &Document, p: &Paragraph) -> Option<&'static str> {
    let list = p.props.list.as_ref()?;
    let level = doc.numbering.get(list.list)?.levels.first()?;
    match level.format {
        NumberFormat::Bullet => Some("bullet"),
        NumberFormat::None => None,
        _ => Some("numbered"),
    }
}

fn align_name(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "justify",
        Align::Distribute => "distribute",
    }
}

/// The links of a paragraph: the text each covers, and where it goes.
fn links_of(p: &Paragraph) -> Vec<Value> {
    let mut out: Vec<(String, std::sync::Arc<Link>)> = Vec::new();
    for run in &p.runs {
        let Some(link) = &run.link else { continue };
        match out.last_mut() {
            Some((text, last)) if **last == **link => text.push_str(&run.text),
            _ => out.push((run.text.clone(), link.clone())),
        }
    }
    out.into_iter()
        .map(|(text, link)| json!({ "text": text, "target": link.target }))
        .collect()
}

fn paragraph_json(doc: &Document, index: usize, p: &Paragraph) -> Value {
    let mut out = Map::new();
    out.insert("block".into(), json!(index + 1));
    out.insert("type".into(), json!("paragraph"));
    out.insert("style".into(), json!(style_name(doc, p)));
    let (text, truncated) = cut(&shown(p), PARAGRAPH_READ);
    out.insert("text".into(), json!(text));
    if truncated {
        out.insert("truncated".into(), json!(true));
    }
    if let Some(kind) = list_kind(doc, p) {
        out.insert("list".into(), json!(kind));
        if let Some(list) = &p.props.list
            && list.level > 0
        {
            out.insert("list_level".into(), json!(list.level + 1));
        }
        if let Some(label) = &p.label {
            out.insert("label".into(), json!(label.text));
        }
    }
    if p.props.align != Align::Left {
        out.insert("align".into(), json!(align_name(p.props.align)));
    }
    let objects = p.runs.iter().filter(|run| is_object(run)).count();
    if objects > 0 {
        out.insert("objects".into(), json!(objects));
    }
    let links = links_of(p);
    if !links.is_empty() {
        out.insert("links".into(), Value::Array(links));
    }
    Value::Object(out)
}

/// The text of a table cell: its paragraphs one per line.
fn cell_text(blocks: &[Block]) -> String {
    aster_writer::paragraphs_of(blocks)
        .iter()
        .map(|(_, p)| shown(p))
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_json(index: usize, table: &Table, first_row: usize, max_rows: usize) -> Value {
    let rows = table.rows.len();
    let from = first_row.min(rows);
    let to = (from + max_rows).min(rows);
    let mut merged = false;
    let cells: Vec<Value> = table.rows[from..to]
        .iter()
        .map(|row| {
            Value::Array(
                row.cells
                    .iter()
                    .map(|cell| {
                        if cell.span > 1 || cell.merge != aster_writer::VMerge::None {
                            merged = true;
                        }
                        json!(cut(&cell_text(&cell.blocks), CELL_READ).0)
                    })
                    .collect(),
            )
        })
        .collect();
    let mut out = Map::new();
    out.insert("block".into(), json!(index + 1));
    out.insert("type".into(), json!("table"));
    out.insert("rows".into(), json!(rows));
    out.insert("columns".into(), json!(table.columns.len()));
    out.insert("first_row".into(), json!(from + 1));
    out.insert("cells".into(), Value::Array(cells));
    if merged {
        out.insert(
            "note".into(),
            json!("some cells are merged: a row lists the cells the file has, so it can be shorter than `columns`"),
        );
    }
    if to < rows {
        out.insert(
            "more_rows".into(),
            json!(format!(
                "rows {}-{} of {rows} are shown; read the rest with start_row {}",
                from + 1,
                to,
                to + 1
            )),
        );
    }
    Value::Object(out)
}

/// `read_word`: the body by block.
fn read(doc: &Document, args: &Value) -> Result<String, String> {
    let total = doc.body.blocks.len();
    let start = count_arg(args, "start_block")?.unwrap_or(1);
    let wanted = count_arg(args, "max_blocks")?
        .unwrap_or(DEFAULT_BLOCKS)
        .min(MAX_BLOCKS);
    let start_row = count_arg(args, "start_row")?.unwrap_or(1) - 1;
    let max_rows = count_arg(args, "max_rows")?
        .unwrap_or(DEFAULT_ROWS)
        .min(MAX_ROWS_READ);
    if total > 0 && start > total {
        return Err(outside(start, total));
    }
    let mut blocks = Vec::new();
    let mut size = 0;
    let mut next = start - 1;
    while next < total && blocks.len() < wanted {
        let value = match &doc.body.blocks[next] {
            Block::Paragraph(p) => paragraph_json(doc, next, p),
            Block::Table(table) => {
                let from = if blocks.is_empty() { start_row } else { 0 };
                table_json(next, table, from, max_rows)
            }
        };
        size += value.to_string().chars().count();
        if !blocks.is_empty() && size > READ_BUDGET {
            break;
        }
        blocks.push(value);
        next += 1;
    }
    let mut out = Map::new();
    out.insert("total_blocks".into(), json!(total));
    out.insert(
        "first".into(),
        json!(if blocks.is_empty() { 0 } else { start }),
    );
    out.insert("last".into(), json!(next));
    out.insert("blocks".into(), Value::Array(blocks));
    if next < total {
        out.insert(
            "more".into(),
            json!(format!(
                "{} more blocks; read on with start_block {}",
                total - next,
                next + 1
            )),
        );
    }
    out.insert(
        "styles".into(),
        json!(
            doc.styles
                .paragraph_styles()
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        ),
    );
    other_stories(doc, &mut out);
    out.insert(
        "note".into(),
        json!(
            "The blocks are the body, and only the body can be edited. Headers, footers, footnotes, endnotes and comments are listed for reading; pictures (objects) are not shown."
        ),
    );
    Ok(Value::Object(out).to_string())
}

/// Most notes and comments `read_word` lists; the rest are counted.
const MAX_LISTED: usize = 50;

/// The text of a story's paragraphs on lines of their own, objects left
/// out and cut short.
fn story_text(doc: &Document, story: StoryId) -> String {
    let text = doc
        .paragraphs_in(story)
        .iter()
        .map(|(_, p)| shown(p))
        .collect::<Vec<_>>()
        .join("\n");
    cut(text.trim(), CELL_READ).0
}

/// What `read_word` says besides the body, in a compact form: the headers
/// and footers the sections use, the notes, the comments, and whether the
/// document has tracked changes. Empty things are left out. All read only.
fn other_stories(doc: &Document, out: &mut Map<String, Value>) {
    let sections = doc.sections();
    for (key, footer) in [("headers", false), ("footers", true)] {
        let mut used: Vec<usize> = Vec::new();
        for (_, section) in &sections {
            let set = if footer {
                &section.footers
            } else {
                &section.headers
            };
            for index in [set.default, set.first, set.even].into_iter().flatten() {
                if !used.contains(&index) {
                    used.push(index);
                }
            }
        }
        let texts: Vec<String> = used
            .into_iter()
            .map(|index| story_text(doc, StoryId::Marginal(index)))
            .filter(|text| !text.is_empty())
            .collect();
        if !texts.is_empty() {
            out.insert(key.into(), json!(texts));
        }
    }
    for (key, notes, endnote) in [
        ("footnotes", &doc.footnotes.notes, false),
        ("endnotes", &doc.endnotes.notes, true),
    ] {
        let listed: Vec<Value> = notes
            .iter()
            .map(|note| {
                let story = if endnote {
                    StoryId::Endnote(note.id)
                } else {
                    StoryId::Footnote(note.id)
                };
                (note.id, story_text(doc, story))
            })
            .filter(|(_, text)| !text.is_empty())
            .take(MAX_LISTED)
            .map(|(id, text)| json!(format!("{id}: {text}")))
            .collect();
        if !listed.is_empty() {
            out.insert(key.into(), Value::Array(listed));
        }
    }
    if !doc.comments.is_empty() {
        let listed: Vec<Value> = doc
            .comments
            .iter()
            .take(MAX_LISTED)
            .map(|c| {
                json!({
                    "author": c.author,
                    "text": cut(&c.text(), CELL_READ).0,
                })
            })
            .collect();
        out.insert("comments".into(), Value::Array(listed));
        if doc.comments.len() > MAX_LISTED {
            out.insert(
                "more_comments".into(),
                json!(doc.comments.len() - MAX_LISTED),
            );
        }
    }
    if doc.has_changes() {
        out.insert(
            "tracked_changes".into(),
            json!("the document has tracked insertions or deletions; the text shown has the insertions and leaves out the deleted text"),
        );
    }
}

// --- Writing ------------------------------------------------------------------

/// What a change did: the answer for the caller and where the caret goes
/// (`None`: the user's selection stays).
struct Applied {
    result: Map<String, Value>,
    caret: Option<Pos>,
}

fn applied(result: Value, caret: Option<Pos>) -> Result<Applied, String> {
    let Value::Object(result) = result else {
        return Err("internal error: a result is an object".to_owned());
    };
    Ok(Applied { result, caret })
}

fn write(tab: &mut DocTab, tool: &str, args: &Value) -> Result<(String, bool), String> {
    let mut outcome: Option<Result<Applied, String>> = None;
    let changed = tab.edit_document(|doc, selection| {
        let done = apply(doc, tool, args);
        let next = match &done {
            Ok(applied) => Some(match &applied.caret {
                Some(caret) => Selection::caret(caret.clone()),
                None => selection.clone(),
            }),
            Err(_) => None,
        };
        outcome = Some(done);
        next
    })?;
    match outcome {
        Some(Ok(mut applied)) => {
            applied.result.insert("changed".into(), json!(changed));
            Ok((Value::Object(applied.result).to_string(), changed))
        }
        Some(Err(error)) => Err(error),
        None => Err("the document could not be edited just now; try again".to_owned()),
    }
}

fn apply(doc: &mut Document, tool: &str, args: &Value) -> Result<Applied, String> {
    match tool {
        "replace_paragraphs" => replace_paragraphs(doc, args),
        "insert_paragraphs" => insert_paragraphs(doc, args),
        "delete_blocks" => delete_blocks(doc, args),
        "find_and_replace" => find_and_replace(doc, args),
        "format_text" => format_text(doc, args),
        "edit_table" => edit_table(doc, args),
        other => Err(format!("`{other}` is not a Word editing tool")),
    }
}

/// The look text typed in place of a paragraph's text gets: that of its
/// first text that is not a link, else its paragraph mark's.
fn replacement_look(p: &Paragraph) -> RunLook {
    p.runs
        .iter()
        .find(|run| run.is_text() && !run.text.is_empty() && run.link.is_none())
        .map_or_else(|| p.mark.clone(), |run| run.look.clone())
}

fn end_of(doc: &Document, index: usize) -> Pos {
    let path = Path::body(index);
    Pos::new(path.clone(), doc.paragraph(&path).map_or(0, Paragraph::len))
}

fn replace_paragraphs(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let (first, last) = block_range(doc, args)?;
    for index in first..=last {
        let p = paragraph_block(doc, index)?;
        if p.runs.iter().any(is_object) {
            return Err(format!(
                "block {} holds a picture, a note reference or another object that would be lost; \
                 change its words with find_and_replace instead",
                index + 1
            ));
        }
    }
    let text = text_arg(args, "text", MAX_TEXT)?;
    if lines_of(&text) > MAX_LINES {
        return Err(format!(
            "at most {MAX_LINES} paragraphs can be written at once"
        ));
    }
    let look = replacement_look(paragraph_block(doc, first)?);
    let from = Pos::new(Path::body(first), 0);
    let to = end_of(doc, last);
    edit::delete(doc, &from, &to);
    let caret = edit::insert_text(doc, &from, &text, Some(&look));
    let end = caret.path.top();
    applied(
        json!({
            "ok": true,
            "replaced_blocks": last - first + 1,
            "first_block": first + 1,
            "last_block": end + 1,
            "total_blocks": doc.body.blocks.len(),
        }),
        Some(caret),
    )
}

/// A style the document has, by its name (any case) or id.
fn style_id(doc: &Document, name: &str) -> Result<std::sync::Arc<str>, String> {
    let wanted = name.trim();
    let paragraph_styles = || {
        doc.styles
            .styles
            .iter()
            .filter(|style| style.kind == aster_writer::StyleKind::Paragraph)
    };
    paragraph_styles()
        .find(|s| s.name.eq_ignore_ascii_case(wanted) || s.id.eq_ignore_ascii_case(wanted))
        .map(|s| s.id.clone())
        .ok_or_else(|| {
            let names: Vec<String> = doc
                .styles
                .paragraph_styles()
                .iter()
                .map(|s| s.name.clone())
                .collect();
            format!("no paragraph style named {wanted:?}; the styles are {names:?}")
        })
}

/// An empty paragraph in the document's default style.
fn default_paragraph(doc: &Document) -> Paragraph {
    let (props, base) = doc.styles.paragraph_look(None);
    Paragraph {
        props: props.clone(),
        base: base.clone(),
        mark: base.clone(),
        ..Paragraph::default()
    }
}

/// The empty paragraph that goes next to paragraph `p` (at `path`): after
/// it, what pressing Enter at its end gives (a heading is followed by body
/// text); before it, one just like it.
fn blank_next_to(doc: &Document, path: &Path, after: bool) -> Paragraph {
    let Some(p) = doc.paragraph(path) else {
        return default_paragraph(doc);
    };
    let mut blank = Paragraph {
        props: p.props.clone(),
        base: p.base.clone(),
        mark: p.mark.clone(),
        ..Paragraph::default()
    };
    if after {
        // Let the editor's own rule for Enter say what follows.
        let mut scratch = doc.clone();
        let at = edit::split_paragraph(&mut scratch, &Pos::new(path.clone(), p.len()));
        if at.path != *path
            && let Some(next) = scratch.paragraph(&at.path)
        {
            blank.props = next.props.clone();
            blank.base = next.base.clone();
            blank.mark = next.mark.clone();
        }
    }
    blank
}

fn insert_paragraphs(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let total = doc.body.blocks.len();
    let anchor = need(args, "block")?;
    if anchor > total {
        return Err(outside(anchor, total));
    }
    let anchor = anchor - 1;
    let after = match args["position"].as_str().unwrap_or("after") {
        "after" => true,
        "before" => false,
        other => {
            return Err(format!(
                "`position` must be \"before\" or \"after\", not {other:?}"
            ));
        }
    };
    let text = text_arg(args, "text", MAX_TEXT)?;
    if lines_of(&text) > MAX_LINES {
        return Err(format!(
            "at most {MAX_LINES} paragraphs can be written at once"
        ));
    }
    let style = optional_text(args, "style", 200)?
        .map(|name| style_id(doc, &name))
        .transpose()?;
    let index = if after { anchor + 1 } else { anchor };
    let blank = match &doc.body.blocks[anchor] {
        Block::Paragraph(_) => blank_next_to(doc, &Path::body(anchor), after),
        Block::Table(_) => default_paragraph(doc),
    };
    doc.body.blocks.insert(index, Block::paragraph(blank));
    doc.renumber();
    let from = Pos::new(Path::body(index), 0);
    let caret = edit::insert_text(doc, &from, &text, None);
    if let Some(id) = style {
        edit::set_style(doc, &from, &caret, &id);
    }
    applied(
        json!({
            "ok": true,
            "first_block": index + 1,
            "last_block": caret.path.top() + 1,
            "total_blocks": doc.body.blocks.len(),
        }),
        Some(caret),
    )
}

/// Every marker under `block`.
fn markers_of(block: &Block, out: &mut Vec<Run>) {
    match block {
        Block::Paragraph(p) => out.extend(
            p.runs
                .iter()
                .filter(|run| matches!(run.kind, Inline::Marker))
                .cloned(),
        ),
        Block::Table(table) => {
            for cell in table.rows.iter().flat_map(|row| &row.cells) {
                for block in &cell.blocks {
                    markers_of(block, out);
                }
            }
        }
    }
}

fn delete_blocks(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let (first, last) = block_range(doc, args)?;
    for index in first..=last {
        if objects_in(&doc.body.blocks[index..=index]) > 0 {
            return Err(format!(
                "block {} holds a picture, a note reference or another object that would be lost; \
                 the assistant cannot remove those yet",
                index + 1
            ));
        }
    }
    let blank = match doc.body.blocks.get(first) {
        Some(Block::Paragraph(_)) => blank_next_to(doc, &Path::body(first), false),
        _ => default_paragraph(doc),
    };
    let removed: Vec<Block> = doc.body.blocks.drain(first..=last).collect();
    let mut markers = Vec::new();
    for block in &removed {
        markers_of(block, &mut markers);
    }
    // A section break on a removed paragraph goes with it, as in Word: the
    // text before takes the formatting of the section after.
    let blocks = &mut doc.body.blocks;
    let tables = |blocks: &Vec<Block>, at: usize| {
        blocks
            .get(at)
            .is_some_and(|block| block.as_table().is_some())
    };
    if blocks.is_empty()
        || blocks
            .last()
            .is_some_and(|block| block.as_table().is_some())
        || (first > 0 && tables(blocks, first - 1) && tables(blocks, first))
    {
        // A body has a paragraph last, and two tables do not touch.
        let at = if blocks.is_empty() || first >= blocks.len() {
            blocks.len()
        } else {
            first
        };
        blocks.insert(at, Block::paragraph(blank));
    }
    if !markers.is_empty() {
        let target = (first..blocks.len())
            .find(|i| matches!(blocks[*i], Block::Paragraph(_)))
            .or_else(|| {
                (0..first.min(blocks.len()))
                    .rev()
                    .find(|i| matches!(blocks[*i], Block::Paragraph(_)))
            });
        if let Some(Block::Paragraph(p)) = target.map(|i| &mut blocks[i]) {
            let p = std::sync::Arc::make_mut(p);
            if target.is_some_and(|i| i >= first) {
                p.runs.splice(0..0, markers);
            } else {
                p.runs.extend(markers);
            }
        }
    }
    doc.renumber();
    let at = first.min(doc.body.blocks.len().saturating_sub(1));
    let caret = (at..doc.body.blocks.len())
        .chain((0..at).rev())
        .find(|i| matches!(doc.body.blocks[*i], Block::Paragraph(_)))
        .map(|i| Pos::new(Path::body(i), 0));
    applied(
        json!({
            "ok": true,
            "deleted_blocks": last - first + 1,
            "total_blocks": doc.body.blocks.len(),
        }),
        caret,
    )
}

// --- Finding text ---------------------------------------------------------------

/// The text lowercased one character at a time (a character whose lowercase
/// is more than one character stays), and for each byte of the result the
/// byte offset of its character in the original, plus the original's length.
fn fold(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    for (at, c) in text.char_indices() {
        let mut lower = c.to_lowercase();
        let c = match (lower.next(), lower.next()) {
            (Some(one), None) => one,
            _ => c,
        };
        for _ in 0..c.len_utf8() {
            map.push(at);
        }
        folded.push(c);
    }
    map.push(text.len());
    (folded, map)
}

/// Where `needle` is in `text`: byte ranges, not overlapping, in order.
fn find_in(text: &str, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return text
            .match_indices(needle)
            .map(|(at, found)| (at, at + found.len()))
            .collect();
    }
    let (folded, map) = fold(text);
    let needle = fold(needle).0;
    folded
        .match_indices(&needle)
        .map(|(at, found)| (map[at], map[at + found.len()]))
        .collect()
}

/// The matches in the paragraphs `paths` (all of them with `None`): where
/// they are, and how many were left out because they touch an object.
fn matches_in(
    doc: &Document,
    needle: &str,
    case_sensitive: bool,
    within: Option<&dyn Fn(&Path) -> bool>,
    cap: usize,
) -> (Vec<(Path, usize, usize)>, usize) {
    let mut found = Vec::new();
    let mut skipped = 0;
    'paragraphs: for (path, p) in doc.paragraphs() {
        if within.is_some_and(|within| !within(&path)) {
            continue;
        }
        let objects = object_ranges(p);
        for (a, b) in find_in(&p.text(), needle, case_sensitive) {
            if objects.iter().any(|(s, e)| a < *e && b > *s) {
                skipped += 1;
                continue;
            }
            if found.len() >= cap {
                break 'paragraphs;
            }
            found.push((path.clone(), a, b));
        }
    }
    (found, skipped)
}

/// The look of the first character at `at`, so that a replacement looks like
/// what it replaces.
fn look_for(doc: &Document, at: &Pos) -> Option<RunLook> {
    let p = doc.paragraph(&at.path)?;
    let text = p.text();
    let mut start = at.offset.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let first = text[start..].chars().next().map_or(0, char::len_utf8);
    Some(p.look_at(start + first).clone())
}

fn find_and_replace(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    if args["regex"].as_bool().unwrap_or(false) {
        return Err(
            "regular expressions are not supported in Word documents; give the text as it is"
                .to_owned(),
        );
    }
    let find = text_arg(args, "find", MAX_PHRASE)?;
    if find.is_empty() {
        return Err("`find` is empty".to_owned());
    }
    let replace = text_arg(args, "replace", MAX_TEXT)?;
    if replace.contains('\n') {
        return Err(
            "`replace` cannot hold line breaks in a Word document; use replace_paragraphs or insert_paragraphs for new paragraphs"
                .to_owned(),
        );
    }
    let case = args["case_sensitive"].as_bool().unwrap_or(true);
    let mut cap = usize::MAX;
    if !args["all"].as_bool().unwrap_or(true) {
        cap = 1;
    }
    if let Some(limit) = count_arg(args, "limit")? {
        cap = cap.min(limit);
    }
    let (hits, skipped) = matches_in(doc, &find, case, None, cap);
    // Each paragraph from its end, so that a change does not move the
    // places before it.
    for (path, a, b) in hits.iter().rev() {
        let from = Pos::new(path.clone(), *a);
        let look = look_for(doc, &from);
        edit::delete(doc, &from, &Pos::new(path.clone(), *b));
        edit::insert_text(doc, &from, &replace, look.as_ref());
    }
    let caret = hits
        .first()
        .map(|(path, a, _)| Pos::new(path.clone(), a + replace.len()));
    let mut result = json!({ "replaced": hits.len() });
    if skipped > 0 {
        result["skipped"] = json!(skipped);
        result["note"] = json!(format!(
            "{skipped} matches touch a picture or another object and were left alone"
        ));
    }
    applied(result, caret)
}

// --- Formatting -------------------------------------------------------------------

/// Typefaces for East Asian text: choosing one sets that face too, as the
/// toolbar's font box does.
const EAST_ASIAN: [&str; 12] = [
    "SimSun",
    "SimHei",
    "KaiTi",
    "FangSong",
    "Microsoft YaHei",
    "宋体",
    "黑体",
    "楷体",
    "仿宋",
    "MS Mincho",
    "MS Gothic",
    "Yu Mincho",
];

/// `#RRGGBB`, `RRGGBB` or a common colour name; `auto`, `none` and empty
/// are no colour.
fn parse_color(text: &str) -> Result<Option<Rgba>, String> {
    let wanted = text.trim().to_ascii_lowercase();
    let named = |rgb: u32| Ok(Some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0xFF]));
    match wanted.as_str() {
        "" | "auto" | "none" | "automatic" => return Ok(None),
        "black" => return named(0x000000),
        "white" => return named(0xFFFFFF),
        "red" => return named(0xFF0000),
        "green" => return named(0x00B050),
        "blue" => return named(0x0070C0),
        "yellow" => return named(0xFFFF00),
        "orange" => return named(0xFFC000),
        "purple" => return named(0x7030A0),
        "gray" | "grey" => return named(0x808080),
        "cyan" => return named(0x00FFFF),
        "magenta" => return named(0xFF00FF),
        _ => {}
    }
    let digits = wanted.strip_prefix('#').unwrap_or(&wanted);
    if digits.len() == 6
        && digits.chars().all(|c| c.is_ascii_hexdigit())
        && let Ok(rgb) = u32::from_str_radix(digits, 16)
    {
        return named(rgb);
    }
    Err(format!(
        "{text:?} is not a colour; write #RRGGBB, a name like red, or \"none\""
    ))
}

/// A link address the tools may set: web, e-mail and bookmarks in the
/// document. Nothing that opens a file or runs something.
fn parse_link(text: &str) -> Result<Option<Link>, String> {
    let target = text.trim();
    if target.is_empty() {
        return Ok(None);
    }
    if target.chars().count() > MAX_URL {
        return Err(format!("`link` is too long: at most {MAX_URL} characters"));
    }
    if target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("`link` cannot hold spaces or control characters".to_owned());
    }
    let lower = target.to_ascii_lowercase();
    let allowed = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
        || (lower.starts_with('#') && lower.len() > 1);
    if !allowed {
        return Err(
            "`link` must be a web address (http or https), an e-mail address (mailto:) or a bookmark (#name); other kinds of link are only set by the user"
                .to_owned(),
        );
    }
    Ok(Some(Link {
        target: target.to_owned(),
        tooltip: None,
    }))
}

/// What a `format_text` call asks for.
struct Formatting {
    patch: RunPatch,
    link: Option<Option<Link>>,
    style: Option<std::sync::Arc<str>>,
    align: Option<Align>,
    list: Option<Option<ListKind>>,
}

impl Formatting {
    fn parse(doc: &Document, args: &Value) -> Result<Self, String> {
        let flag = |name: &str| -> Result<Option<bool>, String> {
            match args.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(*b)),
                Some(_) => Err(format!("`{name}` must be true or false")),
            }
        };
        let mut patch = RunPatch {
            bold: flag("bold")?,
            italic: flag("italic")?,
            strike: flag("strikethrough")?,
            underline: flag("underline")?.map(|on| {
                on.then_some(Underline {
                    style: UnderlineStyle::Single,
                    color: None,
                })
            }),
            ..RunPatch::default()
        };
        if let Some(color) = optional_text(args, "color", 40)? {
            patch.color = Some(parse_color(&color)?);
        }
        if let Some(color) = optional_text(args, "highlight", 40)? {
            patch.highlight = Some(parse_color(&color)?);
        }
        if let Some(size) = args.get("size").filter(|v| !v.is_null()) {
            let size = size
                .as_f64()
                .filter(|s| (1.0..=1000.0).contains(s))
                .ok_or("`size` is a number of points from 1 to 1000")?;
            patch.size = Some(size as f32);
        }
        if let Some(font) = optional_text(args, "font", 100)? {
            let font = font.trim();
            if font.is_empty() || font.eq_ignore_ascii_case("default") {
                patch.font = Some(None);
            } else {
                patch.font = Some(Some(font.into()));
                if EAST_ASIAN.contains(&font) {
                    patch.east_asian_font = Some(Some(font.into()));
                }
            }
        }
        let link = optional_text(args, "link", MAX_URL + 1)?
            .map(|text| parse_link(&text))
            .transpose()?;
        let style = optional_text(args, "style", 200)?
            .map(|name| style_id(doc, &name))
            .transpose()?;
        let align = match args["align"].as_str() {
            None => None,
            Some("left") => Some(Align::Left),
            Some("center") => Some(Align::Center),
            Some("right") => Some(Align::Right),
            Some("justify") => Some(Align::Justify),
            Some(other) => {
                return Err(format!(
                    "`align` must be left, center, right or justify, not {other:?}"
                ));
            }
        };
        let list = match args["list"].as_str() {
            None => None,
            Some("bullet") => Some(Some(ListKind::Bullet)),
            Some("numbered") => Some(Some(ListKind::Numbered)),
            Some("none") => Some(None),
            Some(other) => {
                return Err(format!(
                    "`list` must be bullet, numbered or none, not {other:?}"
                ));
            }
        };
        let formatting = Self {
            patch,
            link,
            style,
            align,
            list,
        };
        if formatting.is_empty() {
            return Err(
                "nothing to change: give at least one of bold, italic, underline, strikethrough, color, highlight, size, font, link, style, align, list"
                    .to_owned(),
            );
        }
        Ok(formatting)
    }

    fn is_empty(&self) -> bool {
        self.patch.is_empty()
            && self.link.is_none()
            && self.style.is_none()
            && self.align.is_none()
            && self.list.is_none()
    }

    fn for_paragraphs(&self) -> bool {
        self.style.is_some() || self.align.is_some() || self.list.is_some()
    }
}

/// The paragraphs of the cell at `row`, `cell` of the table at block
/// `block`, as paths, and the cell itself.
fn cell_paragraphs(
    doc: &Document,
    block: usize,
    row: usize,
    cell: usize,
) -> Result<Vec<Path>, String> {
    let table = table_block(doc, block)?;
    let rows = table.rows.len();
    let Some(r) = table.rows.get(row) else {
        return Err(format!(
            "row {} is outside the table at block {} (rows 1 to {rows})",
            row + 1,
            block + 1
        ));
    };
    let Some(c) = r.cells.get(cell) else {
        return Err(format!(
            "cell {} is outside row {} of the table at block {} (cells 1 to {})",
            cell + 1,
            row + 1,
            block + 1,
            r.cells.len()
        ));
    };
    let base = Path::body(block);
    let paths: Vec<Path> = c
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, Block::Paragraph(_)))
        .map(|(k, _)| base.cell_block(row, cell, k))
        .collect();
    if paths.is_empty() {
        return Err(format!(
            "cell {} of row {} holds no paragraph to work on",
            cell + 1,
            row + 1
        ));
    }
    Ok(paths)
}

fn cell_args(args: &Value) -> Result<Option<(usize, usize)>, String> {
    match (count_arg(args, "row")?, count_arg(args, "cell")?) {
        (None, None) => Ok(None),
        (Some(row), cell) => Ok(Some((row - 1, cell.unwrap_or(1) - 1))),
        (None, Some(_)) => Err("`cell` needs `row`".to_owned()),
    }
}

fn format_text(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let formatting = Formatting::parse(doc, args)?;
    let phrase = optional_text(args, "phrase", MAX_PHRASE)?.filter(|p| !p.is_empty());
    if phrase.is_some() && formatting.for_paragraphs() {
        return Err(
            "style, align and list apply to whole paragraphs; leave out `phrase` to set them"
                .to_owned(),
        );
    }
    // Which paragraphs: a cell's, or the blocks' (those in tables among
    // them too).
    let (first_block, last_block) = block_range(doc, args)?;
    let paths: Vec<Path> = match cell_args(args)? {
        Some((row, cell)) => {
            if first_block != last_block {
                return Err("`end_block` cannot be used with `row` and `cell`".to_owned());
            }
            cell_paragraphs(doc, first_block, row, cell)?
        }
        None => {
            if let Some(Block::Table(_)) = doc.body.blocks.get(first_block)
                && first_block == last_block
            {
                return Err(format!(
                    "block {} is a table; give `row` and `cell` for the cell to format",
                    first_block + 1
                ));
            }
            doc.paragraphs()
                .into_iter()
                .filter(|(path, _)| (first_block..=last_block).contains(&path.top()))
                .map(|(path, _)| path)
                .collect()
        }
    };
    let (Some(first), Some(last)) = (paths.first(), paths.last()) else {
        return Err("there is no text in that place to format".to_owned());
    };
    let from = Pos::new(first.clone(), 0);
    let to = Pos::new(last.clone(), doc.paragraph(last).map_or(0, Paragraph::len));
    let mut touched = paths.len();
    let mut phrases = 0;
    if let Some(style) = &formatting.style {
        edit::set_style(doc, &from, &to, style);
    }
    if let Some(align) = formatting.align {
        edit::set_align(doc, &from, &to, align);
    }
    if let Some(kind) = formatting.list {
        edit::set_list(doc, &from, &to, kind);
    }
    let char_level = !formatting.patch.is_empty() || formatting.link.is_some();
    if char_level {
        let ranges: Vec<(Pos, Pos)> = match &phrase {
            None => vec![(from.clone(), to.clone())],
            Some(phrase) => {
                let case = args["case_sensitive"].as_bool().unwrap_or(true);
                let inside = |path: &Path| paths.contains(path);
                let (hits, _) = matches_in(doc, phrase, case, Some(&inside), usize::MAX);
                if hits.is_empty() {
                    return Err(format!(
                        "the phrase {phrase:?} is not in {}; read the block again for its exact text",
                        match (args.get("row"), first_block == last_block) {
                            (Some(_), _) => format!("block {}'s cell", first_block + 1),
                            (None, true) => format!("block {}", first_block + 1),
                            (None, false) =>
                                format!("blocks {} to {}", first_block + 1, last_block + 1),
                        }
                    ));
                }
                phrases = hits.len();
                touched = hits
                    .iter()
                    .map(|(path, _, _)| path)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                hits.into_iter()
                    .map(|(path, a, b)| (Pos::new(path.clone(), a), Pos::new(path, b)))
                    .collect()
            }
        };
        for (start, end) in &ranges {
            edit::apply_run_patch(doc, start, end, &formatting.patch);
            if let Some(link) = &formatting.link {
                edit::set_link(doc, start, end, link.clone());
            }
        }
    }
    let mut result = json!({ "ok": true, "paragraphs": touched });
    if phrases > 0 {
        result["phrases"] = json!(phrases);
    }
    applied(result, None)
}

// --- Tables -----------------------------------------------------------------------

/// Cell texts from `cells`: rows of strings (numbers and booleans as
/// typed, `null` empty).
pub fn table_cells(args: &Value) -> Result<Vec<Vec<String>>, String> {
    let rows = args["cells"]
        .as_array()
        .ok_or("`cells` must be an array of rows, each an array of cell texts")?;
    let mut out = Vec::with_capacity(rows.len());
    let mut count = 0usize;
    for row in rows {
        let row = row
            .as_array()
            .ok_or("each row of `cells` must be an array of cell texts")?;
        let mut cells = Vec::with_capacity(row.len());
        for value in row {
            cells.push(match value {
                Value::String(text) => {
                    if text.contains(OBJECT) {
                        return Err("a cell text holds U+FFFC; leave it out".to_owned());
                    }
                    text.replace("\r\n", "\n").replace('\r', "\n")
                }
                Value::Number(number) => number.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                _ => return Err("a cell text must be a string, number or boolean".to_owned()),
            });
        }
        count += cells.iter().map(|text| text.chars().count()).sum::<usize>();
        out.push(cells);
    }
    if count > MAX_TEXT {
        return Err(format!("the cells hold more than {MAX_TEXT} characters"));
    }
    if out.is_empty() || out.iter().all(Vec::is_empty) {
        return Err("`cells` has no rows".to_owned());
    }
    let columns = out.iter().map(Vec::len).max().unwrap_or(0);
    if out.len() > MAX_TABLE_ROWS || columns > MAX_TABLE_COLUMNS {
        return Err(format!(
            "a table made here has at most {MAX_TABLE_ROWS} rows and {MAX_TABLE_COLUMNS} columns; \
             these cells make {} by {columns}",
            out.len()
        ));
    }
    if out.len() * columns > MAX_CELLS {
        return Err(format!(
            "a table made here has at most {MAX_CELLS} cells; these make {}",
            out.len() * columns
        ));
    }
    Ok(out)
}

/// The width of the text area where block `block` is, in twips.
fn text_width(doc: &Document, block: usize) -> i32 {
    let section = doc
        .sections()
        .into_iter()
        .find(|(last, _)| *last >= block)
        .map_or_else(|| doc.last_section.clone(), |(_, s)| s.clone());
    let width =
        section.page.0 - section.margins.left - section.margins.right - section.margins.gutter;
    if width >= 1440 { width } else { 9000 }
}

fn insert_table(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let total = doc.body.blocks.len();
    let anchor = need(args, "block")?;
    if anchor > total {
        return Err(outside(anchor, total));
    }
    let anchor = anchor - 1;
    let after = match args["position"].as_str().unwrap_or("after") {
        "after" => true,
        "before" => false,
        other => {
            return Err(format!(
                "`position` must be \"before\" or \"after\", not {other:?}"
            ));
        }
    };
    let cells = table_cells(args)?;
    let columns = cells.iter().map(Vec::len).max().unwrap_or(1);
    let width = text_width(doc, anchor);
    // The table goes before a paragraph (at its start), or after the
    // paragraph at the end of it.
    let at = match (&doc.body.blocks[anchor], after) {
        (Block::Paragraph(_), false) => Pos::new(Path::body(anchor), 0),
        (Block::Paragraph(p), true) => match doc.body.blocks.get(anchor + 1) {
            Some(Block::Paragraph(_)) => Pos::new(Path::body(anchor + 1), 0),
            _ => Pos::new(Path::body(anchor), p.len()),
        },
        (Block::Table(_), true) => {
            if !matches!(doc.body.blocks.get(anchor + 1), Some(Block::Paragraph(_))) {
                let blank = default_paragraph(doc);
                doc.body.blocks.insert(anchor + 1, Block::paragraph(blank));
            }
            Pos::new(Path::body(anchor + 1), 0)
        }
        (Block::Table(_), false) => {
            let blank = default_paragraph(doc);
            doc.body.blocks.insert(anchor, Block::paragraph(blank));
            Pos::new(Path::body(anchor), 0)
        }
    };
    let mut caret = edit::insert_table(doc, &at, cells.len(), columns, width);
    let mut table = Path::body(caret.path.top());
    if caret.path.steps.len() < 4 || doc.block(&table).and_then(Block::as_table).is_none() {
        return Err("the table could not be made at that place".to_owned());
    }
    // Two tables with nothing between them are one table to Word: a
    // paragraph keeps them apart.
    let index = table.top();
    if index > 0 && doc.body.blocks[index - 1].as_table().is_some() {
        let blank = default_paragraph(doc);
        doc.body.blocks.insert(index, Block::paragraph(blank));
        table = Path::body(index + 1);
        caret = Pos::new(table.cell_block(0, 0, 0), 0);
    }
    for (r, row) in cells.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            if !text.is_empty() {
                let path = table.cell_block(r, c, 0);
                edit::insert_text(doc, &Pos::new(path, 0), text, None);
            }
        }
    }
    applied(
        json!({
            "ok": true,
            "block": table.top() + 1,
            "rows": cells.len(),
            "columns": columns,
            "total_blocks": doc.body.blocks.len(),
        }),
        Some(caret),
    )
}

fn set_cell(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let block = need(args, "block")? - 1;
    let (row, cell) = (need(args, "row")? - 1, need(args, "cell")? - 1);
    let text = text_arg(args, "text", MAX_TEXT)?;
    let paths = cell_paragraphs(doc, block, row, cell)?;
    let table = table_block(doc, block)?;
    let content = &table.rows[row].cells[cell].blocks;
    if content.iter().any(|b| b.as_table().is_some()) {
        return Err(format!(
            "cell {} of row {} holds a nested table; the assistant cannot edit those yet",
            cell + 1,
            row + 1
        ));
    }
    if objects_in(content) > 0 {
        return Err(format!(
            "cell {} of row {} holds a picture or another object that would be lost; use find_and_replace for its words",
            cell + 1,
            row + 1
        ));
    }
    if lines_of(&text) > MAX_LINES {
        return Err(format!(
            "at most {MAX_LINES} paragraphs can be written at once"
        ));
    }
    let (first, last) = (&paths[0], &paths[paths.len() - 1]);
    let look = doc.paragraph(first).map(replacement_look);
    let from = Pos::new(first.clone(), 0);
    let to = Pos::new(last.clone(), doc.paragraph(last).map_or(0, Paragraph::len));
    edit::delete(doc, &from, &to);
    let caret = edit::insert_text(doc, &from, &text, look.as_ref());
    applied(
        json!({ "ok": true, "block": block + 1, "row": row + 1, "cell": cell + 1 }),
        Some(caret),
    )
}

/// The position at the start of the cell a row/column call names, which is
/// the "selection" the table operations take.
fn anchor_cell(doc: &Document, args: &Value, cell_needed: bool) -> Result<(usize, Pos), String> {
    let block = need(args, "block")? - 1;
    table_block(doc, block)?;
    let row = if cell_needed {
        count_arg(args, "row")?.unwrap_or(1)
    } else {
        need(args, "row")?
    } - 1;
    let cell = if cell_needed {
        need(args, "cell")?
    } else {
        count_arg(args, "cell")?.unwrap_or(1)
    } - 1;
    let paths = cell_paragraphs(doc, block, row, cell)?;
    Ok((block, Pos::new(paths[0].clone(), 0)))
}

fn edit_table(doc: &mut Document, args: &Value) -> Result<Applied, String> {
    let action = args["action"].as_str().unwrap_or_default();
    match action {
        "insert" => return insert_table(doc, args),
        "set_cell" => return set_cell(doc, args),
        "insert_row" | "delete_row" | "insert_column" | "delete_column" => {}
        other => {
            return Err(format!(
                "`action` must be insert, set_cell, insert_row, delete_row, insert_column or delete_column, not {other:?}"
            ));
        }
    }
    let columns = action.ends_with("column");
    let (block, at) = anchor_cell(doc, args, columns)?;
    let table = table_block(doc, block)?;
    let count = count_arg(args, "count")?.unwrap_or(1);
    if count > MAX_COUNT {
        return Err(format!("`count` is at most {MAX_COUNT}"));
    }
    let caret = match action {
        "insert_row" | "insert_column" => {
            let before = match args["position"].as_str().unwrap_or("after") {
                "before" => true,
                "after" => false,
                other => {
                    return Err(format!(
                        "`position` must be \"before\" or \"after\", not {other:?}"
                    ));
                }
            };
            if (table.rows.len() + if action == "insert_row" { count } else { 0 }) > MAX_TABLE_ROWS
                || (table.columns.len() + if columns { count } else { 0 }) > MAX_TABLE_COLUMNS
            {
                return Err(format!(
                    "a table made here has at most {MAX_TABLE_ROWS} rows and {MAX_TABLE_COLUMNS} columns"
                ));
            }
            let mut caret = at.clone();
            for _ in 0..count {
                caret = if columns {
                    edit::insert_columns(doc, &at, &at, before)
                } else {
                    edit::insert_rows(doc, &at, &at, before)
                };
            }
            caret
        }
        _ => {
            let whole = if columns {
                table.columns.len() <= 1
            } else {
                table.rows.len() <= 1
            };
            if whole {
                return Err(format!(
                    "that would delete the last {} and with it the whole table; use delete_blocks to remove the table",
                    if columns { "column" } else { "row" }
                ));
            }
            if columns {
                edit::delete_columns(doc, &at, &at)
            } else {
                edit::delete_rows(doc, &at, &at)
            }
        }
    };
    let Some(table) = doc.block(&Path::body(block)).and_then(Block::as_table) else {
        return Err("the table is gone after the change".to_owned());
    };
    edit::check_table(table).map_err(|why| format!("the change would break the table: {why}"))?;
    applied(
        json!({
            "ok": true,
            "block": block + 1,
            "rows": table.rows.len(),
            "columns": table.columns.len(),
        }),
        Some(caret),
    )
}

// --- What the user is told ------------------------------------------------------

/// What a call would do, for the card the user decides on: a line of facts,
/// the text it would replace or delete, and what it would write. Read from
/// the document as it is now, in the interface language. The texts are not
/// cut; the caller shortens them for the card.
pub fn card(tab: &DocTab, tool: &str, args: &Value, strings: &Strings) -> (String, String, String) {
    let doc = tab.view.document();
    let total = doc.body.blocks.len();
    let blocks = |first: usize, last: usize| {
        if first == last {
            i18n::fill(
                &i18n::fill(
                    strings.approval_word_block,
                    "block",
                    &(first + 1).to_string(),
                ),
                "total",
                &total.to_string(),
            )
        } else {
            i18n::fill(
                &i18n::fill(
                    &i18n::fill(
                        strings.approval_word_blocks,
                        "start",
                        &(first + 1).to_string(),
                    ),
                    "end",
                    &(last + 1).to_string(),
                ),
                "total",
                &total.to_string(),
            )
        }
    };
    let old_text = |first: usize, last: usize| -> String {
        doc.body.blocks[first..=last]
            .iter()
            .map(|block| match block {
                Block::Paragraph(p) => shown(p),
                Block::Table(table) => table
                    .rows
                    .iter()
                    .map(|row| {
                        row.cells
                            .iter()
                            .map(|cell| cell_text(&cell.blocks))
                            .collect::<Vec<_>>()
                            .join("\t")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
    let none = (String::new(), String::new(), String::new());
    match tool {
        "replace_paragraphs" => match block_range(doc, args) {
            Ok((first, last)) => (blocks(first, last), old_text(first, last), text("text")),
            Err(_) => (String::new(), String::new(), text("text")),
        },
        "delete_blocks" => match block_range(doc, args) {
            Ok((first, last)) => (blocks(first, last), old_text(first, last), String::new()),
            Err(_) => none,
        },
        "insert_paragraphs" => {
            let Ok(block) = need(args, "block") else {
                return (String::new(), String::new(), text("text"));
            };
            let template = if args["position"].as_str() == Some("before") {
                strings.approval_word_before
            } else {
                strings.approval_word_after
            };
            let facts = i18n::fill(
                &i18n::fill(template, "block", &block.to_string()),
                "total",
                &total.to_string(),
            );
            (facts, String::new(), text("text"))
        }
        "find_and_replace" => {
            let find = text("find");
            let case = args["case_sensitive"].as_bool().unwrap_or(true);
            let cap = match (
                args["all"].as_bool().unwrap_or(true),
                count_arg(args, "limit"),
            ) {
                (false, _) => 1,
                (true, Ok(Some(limit))) => limit,
                _ => usize::MAX,
            };
            let all = matches_in(doc, &find, case, None, usize::MAX).0.len();
            let count = all.min(cap).to_string();
            let scope = if args["all"].as_bool().unwrap_or(true) {
                i18n::fill(strings.approval_all, "count", &count)
            } else {
                i18n::fill(strings.approval_first, "count", &all.to_string())
            };
            let facts = i18n::fill(
                &i18n::fill(
                    &i18n::fill(
                        strings.approval_find,
                        "pattern",
                        &format!("“{}”", cut(&find, 200).0),
                    ),
                    "kind",
                    strings.approval_plain,
                ),
                "scope",
                &scope,
            );
            (facts, String::new(), text("replace"))
        }
        "format_text" => {
            let place = match block_range(doc, args) {
                Ok((first, last)) => blocks(first, last),
                Err(_) => String::new(),
            };
            let place = match args["phrase"].as_str().filter(|p| !p.is_empty()) {
                Some(phrase) => format!(
                    "{place} — {}",
                    i18n::fill(strings.approval_word_phrase, "phrase", &cut(phrase, 100).0)
                ),
                None => place,
            };
            (place, String::new(), formatting_lines(args, strings))
        }
        "edit_table" => table_card(doc, args, strings),
        _ => none,
    }
}

fn table_card(doc: &Document, args: &Value, strings: &Strings) -> (String, String, String) {
    let action = args["action"].as_str().unwrap_or_default();
    let block = count_arg(args, "block").ok().flatten().unwrap_or(0);
    let cell_ref = |row: usize, cell: usize| {
        i18n::fill(
            &i18n::fill(
                &i18n::fill(strings.approval_word_cell, "block", &block.to_string()),
                "row",
                &row.to_string(),
            ),
            "cell",
            &cell.to_string(),
        )
    };
    let before = args["position"].as_str() == Some("before");
    let row = count_arg(args, "row").ok().flatten();
    let cell = count_arg(args, "cell").ok().flatten();
    match action {
        "insert" => {
            let cells = table_cells(args).unwrap_or_default();
            let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
            let facts = i18n::fill(
                &i18n::fill(
                    &i18n::fill(
                        strings.approval_word_table_new,
                        "rows",
                        &cells.len().to_string(),
                    ),
                    "columns",
                    &columns.to_string(),
                ),
                "block",
                &block.to_string(),
            );
            let rows = cells
                .iter()
                .map(|row| row.join("\t"))
                .collect::<Vec<_>>()
                .join("\n");
            (facts, String::new(), rows)
        }
        "set_cell" => {
            let (row, cell) = (row.unwrap_or(1), cell.unwrap_or(1));
            let old = block
                .checked_sub(1)
                .and_then(|b| cell_paragraphs(doc, b, row - 1, cell - 1).ok())
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| doc.paragraph(path))
                        .map(shown)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            (
                format!(
                    "{} — {}",
                    cell_ref(row, cell),
                    strings.approval_word_set_cell
                ),
                old,
                args["text"].as_str().unwrap_or_default().to_owned(),
            )
        }
        "insert_row" | "insert_column" | "delete_row" | "delete_column" => {
            let what = match (action, before) {
                ("insert_row", true) => strings.table_row_above,
                ("insert_row", false) => strings.table_row_below,
                ("insert_column", true) => strings.table_column_left,
                ("insert_column", false) => strings.table_column_right,
                ("delete_row", _) => strings.table_delete_row,
                _ => strings.table_delete_column,
            };
            let count = count_arg(args, "count").ok().flatten().unwrap_or(1);
            let times = if count > 1 {
                format!(" ×{count}")
            } else {
                String::new()
            };
            (
                format!(
                    "{} — {what}{times}",
                    cell_ref(row.unwrap_or(1), cell.unwrap_or(1))
                ),
                String::new(),
                String::new(),
            )
        }
        _ => (String::new(), String::new(), String::new()),
    }
}

/// What a `format_text` call sets, one line each, in the interface language.
fn formatting_lines(args: &Value, strings: &Strings) -> String {
    let mark = |on: bool| if on { "✓" } else { "✗" };
    let mut lines = Vec::new();
    for (name, label) in [
        ("bold", strings.format_bold),
        ("italic", strings.format_italic),
        ("underline", strings.format_underline),
        ("strikethrough", strings.format_strike),
    ] {
        if let Some(on) = args[name].as_bool() {
            lines.push(format!("{label}: {}", mark(on)));
        }
    }
    for (name, label) in [
        ("color", strings.approval_word_color),
        ("highlight", strings.approval_word_highlight),
        ("font", strings.approval_word_font),
        ("style", strings.approval_word_style),
        ("align", strings.approval_word_align),
        ("link", strings.format_link),
    ] {
        if let Some(value) = args[name].as_str() {
            lines.push(format!("{label}: {}", cut(value, 200).0));
        }
    }
    if let Some(size) = args["size"].as_f64() {
        lines.push(format!("{}: {size}", strings.approval_word_size));
    }
    match args["list"].as_str() {
        Some("bullet") => lines.push(format!("{}: ✓", strings.format_bullet)),
        Some("numbered") => lines.push(format!("{}: ✓", strings.format_numbered)),
        Some("none") => lines.push(format!(
            "{}, {}: ✗",
            strings.format_bullet, strings.format_numbered
        )),
        _ => {}
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests;
