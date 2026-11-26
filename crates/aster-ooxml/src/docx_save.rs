//! Saving an edited Word document back into its package (ADR 0001).
//!
//! **The rule: bytes that belong to something unchanged are written back
//! exactly; only what changed is rewritten.** The file as first read is the
//! base. It is read again here, and the model is compared with what that
//! gives (the byte ranges of every block and run):
//!
//! - every part but the main document (and `numbering.xml`, `.rels` and the
//!   content types when a list or a link is added) is copied as it was,
//!   compressed bytes and all; a document with no change is returned as the
//!   file it was read from, byte for byte;
//! - the body is spliced. Between the original blocks the bytes stay as they
//!   are (content-control wrappers, the final `sectPr`, anything unknown). A
//!   block that is as read and in its place is copied; one that is gone has
//!   its element removed; one with no source, or out of its order (moved, or
//!   the second half of a split) is written where it now is. A table whose
//!   structure is as read is spliced cell by cell; a table made here is
//!   written from the model.
//! - a changed paragraph keeps its start tag and its `pPr` with only the
//!   changed properties set; the runs are spliced the same way: between the
//!   original runs the bytes (hyperlink, insertion, smart tag and content
//!   control wrappers, white space) stay, a run that is as read and in order
//!   is copied, a removed run's element is removed, a changed run keeps its
//!   `rPr` with only the changed properties set and its text is written
//!   again. A run made here is written where it now is, outside the wrapper
//!   it does not belong to.
//! - a paragraph made here has a `pPr` with the style, what differs from the
//!   style, the list and the mark's `rPr`; its runs have an `rPr` with what
//!   differs from the paragraph's base look.
//!
//! What cannot be written from the model, because it only exists as the
//! bytes of a file, is kept by copying those bytes: drawings, notes,
//! markers and sections. A drawing or reference that has no source (one
//! pasted from elsewhere) is left out.
//!
//! Not updated, as with PPTX: `docProps/app.xml` statistics.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use aster_writer::edit::{image_format, rebase_look};
use aster_writer::{
    Align, Block, Break, CellAlign, Document, Drawing, DrawingContent, Inline, Link, List,
    ListLevel, NumberFormat, ParaSource, Paragraph, Picture, Row, RowHeight, RunLook, RunSource,
    Section, SectionStart, Suffix, Table, VMerge,
};

use crate::docx;
use crate::error::{Error, Result};
use crate::package::{Kind, Package, rels_name};
use crate::xml_edit::{
    append_to_root, attribute, children, escape, escape_attribute, find_all, free_rel_number,
    inner, qualified_name, set_attribute,
};

mod blank;
mod props;
mod xml;

pub use blank::blank;
use props::{SectionEdit, border_element, borders_element, hex, ppr_patch, rpr_patch};
use xml::{Tok, is_balanced, is_element, leading_closes, local, tokens};

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NUMBERING_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const TYPES: &str = "[Content_Types].xml";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";

type RunKey = (usize, usize, Option<(usize, usize)>);

fn run_key(source: &RunSource) -> RunKey {
    (
        source.range.start,
        source.range.end,
        source.parent.as_ref().map(|p| (p.start, p.end)),
    )
}

fn range_key(range: &Range<usize>) -> (usize, usize) {
    (range.start, range.end)
}

/// Save `document` over `source`, the file it was read from.
pub fn save(source: Vec<u8>, document: &Document) -> Result<Vec<u8>> {
    let mut package = Package::open(source.clone())?;
    let main = package.expect(Kind::Docx)?;
    let original = docx::read(source.clone())?;
    let bytes = package.read(&main)?;
    let text = String::from_utf8(bytes).map_err(|_| Error::broken(&main, "not UTF-8 text"))?;

    let rels_part = rels_name(&main);
    let rels_text = if package.has(&rels_part) {
        String::from_utf8_lossy(&package.read(&rels_part)?).into_owned()
    } else {
        EMPTY_RELS.to_owned()
    };
    let relationships = package.relationships(&main)?;

    let taken: HashSet<String> = package
        .part_names()
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let docpr_floor = other_docpr_max(&mut package, &main, document);
    let mut saver = Saver::new(&text, &original, document, rels_text);
    saver.media = Media::new(&main, taken);
    saver.next_docpr = saver.next_docpr.max(docpr_floor);
    let new_text = saver.document_text()?;

    let mut replaced: HashMap<String, Vec<u8>> = HashMap::new();
    let mut added: Vec<(String, Vec<u8>)> = Vec::new();
    let mut types_edit: Vec<String> = Vec::new();
    if new_text != text {
        replaced.insert(main.to_ascii_lowercase(), new_text.into_bytes());
    }

    // Lists made since reading.
    let new_lists: Vec<&List> = document
        .numbering
        .lists
        .iter()
        .filter(|list| original.numbering.get(list.id).is_none())
        .collect();
    if !new_lists.is_empty() {
        let existing = relationships
            .iter()
            .find(|r| r.kind == "numbering")
            .and_then(|r| r.target.clone());
        match existing.filter(|name| package.has(name)) {
            Some(name) => {
                let old = String::from_utf8_lossy(&package.read(&name)?).into_owned();
                let new = numbering_with(&old, &new_lists);
                replaced.insert(name.to_ascii_lowercase(), new.into_bytes());
            }
            None => {
                let name = free_part(&package, "word/numbering", "xml");
                let target = name.rsplit('/').next().unwrap_or("numbering.xml");
                saver.rels.add("numbering", target, false);
                added.push((name.clone(), numbering_new(&new_lists).into_bytes()));
                types_edit.push(format!(
                    "<Override PartName=\"/{}\" ContentType=\"{NUMBERING_CT}\"/>",
                    escape_attribute(&name)
                ));
            }
        }
    }
    if !saver.media.items.is_empty() {
        let types = package.read(TYPES)?;
        let types_text = String::from_utf8_lossy(&types).into_owned();
        let mut seen: HashSet<&str> = HashSet::new();
        for item in &saver.media.items {
            added.push((item.part.clone(), item.bytes.to_vec()));
            if seen.insert(item.extension) && !has_default(&types_text, item.extension) {
                types_edit.push(format!(
                    "<Default Extension=\"{}\" ContentType=\"{}\"/>",
                    item.extension, item.content_type
                ));
            }
        }
    }
    if saver.rels.changed {
        let bytes = saver.rels.text.clone().into_bytes();
        if package.has(&rels_part) {
            replaced.insert(rels_part.to_ascii_lowercase(), bytes);
        } else {
            added.push((rels_part.clone(), bytes));
        }
    }
    if !types_edit.is_empty() {
        let types = package.read(TYPES)?;
        let items = types_edit.concat();
        replaced.insert(TYPES.to_ascii_lowercase(), append_to_root(&types, &items));
    }
    if replaced.is_empty() && added.is_empty() {
        // Nothing changed: the file as it was.
        return Ok(source);
    }
    package.rewrite_adding(&replaced, &std::collections::HashSet::new(), &added)
}

const EMPTY_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>";

fn free_part(package: &Package, stem: &str, extension: &str) -> String {
    let first = format!("{stem}.{extension}");
    if !package.has(&first) {
        return first;
    }
    (1..)
        .map(|n| format!("{stem}{n}.{extension}"))
        .find(|name| !package.has(name))
        .expect("some number is free")
}

/// Whether the content types part has a default type for `extension`.
fn has_default(types: &str, extension: &str) -> bool {
    find_all(types, "Default").iter().any(|span| {
        attribute(&types[span.start..span.end], "Extension")
            .is_some_and(|found| found.eq_ignore_ascii_case(extension))
    })
}

/// The highest `docPr` id in the document's other parts (headers, footers,
/// notes) when the document has a picture to write that was not read, else 0:
/// ids of new pictures start above it and above the main part's own.
fn other_docpr_max(package: &mut Package, main: &str, document: &Document) -> u32 {
    let has_new = document.paragraphs().iter().any(|(_, p)| {
        p.runs.iter().any(|r| {
            r.source.is_none() && matches!(&r.kind, Inline::Drawing(d) if matches!(d.content, DrawingContent::Picture(_)))
        })
    });
    if !has_new {
        return 0;
    }
    let mut max = 0;
    for name in package.part_names() {
        let lower = name.to_ascii_lowercase();
        if name == main || !lower.starts_with("word/") || !lower.ends_with(".xml") {
            continue;
        }
        let Ok(bytes) = package.read(&name) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        max = max.max(max_docpr(&text));
    }
    max
}

fn max_docpr(text: &str) -> u32 {
    find_all(text, "docPr")
        .iter()
        .filter_map(|span| attribute(&text[span.start..span.end], "id"))
        .filter_map(|id| id.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Pictures made here: the image parts to add
// ---------------------------------------------------------------------------

/// An image part to add to the package.
struct NewImage {
    /// The part's name (`word/media/image1.png`).
    part: String,
    bytes: Arc<[u8]>,
    extension: &'static str,
    content_type: &'static str,
    /// The relationship from the main part.
    rel_id: String,
}

/// The image parts a save adds: each distinct image once.
#[derive(Default)]
struct Media {
    /// The folder next to the main part (`word/media/`).
    dir: String,
    /// The names of the parts in the package, lower case.
    taken: HashSet<String>,
    items: Vec<NewImage>,
    by_bytes: HashMap<Arc<[u8]>, usize>,
}

impl Media {
    fn new(main: &str, taken: HashSet<String>) -> Self {
        let dir = match main.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/media/"),
            None => "media/".to_owned(),
        };
        Self {
            dir,
            taken,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Relationships of the main part
// ---------------------------------------------------------------------------

struct Rels {
    text: String,
    changed: bool,
}

impl Rels {
    fn add(&mut self, kind: &str, target: &str, external: bool) -> String {
        let id = format!("rId{}", free_rel_number(&self.text));
        let mode = if external {
            " TargetMode=\"External\""
        } else {
            ""
        };
        let item = format!(
            "<Relationship Id=\"{id}\" Type=\"{REL_BASE}/{kind}\" Target=\"{}\"{mode}/>",
            escape_attribute(target)
        );
        self.text =
            String::from_utf8_lossy(&append_to_root(self.text.as_bytes(), &item)).into_owned();
        self.changed = true;
        id
    }

    /// The id of the relationship to an external address, made if there is
    /// none.
    fn hyperlink(&mut self, target: &str) -> String {
        let wanted = escape_attribute(target);
        for span in find_all(&self.text, "Relationship") {
            let xml = &self.text[span.start..span.end];
            let is_link = attribute(xml, "Type").is_some_and(|t| t.ends_with("/hyperlink"));
            if is_link
                && attribute(xml, "Target").as_deref() == Some(wanted.as_str())
                && let Some(id) = attribute(xml, "Id")
            {
                return id;
            }
        }
        self.add("hyperlink", target, true)
    }
}

// ---------------------------------------------------------------------------
// The saver
// ---------------------------------------------------------------------------

struct Saver<'a> {
    text: &'a str,
    original: &'a Document,
    document: &'a Document,
    /// The prefix of the WordprocessingML namespace, with its colon.
    w: String,
    /// The prefix of the relationships namespace, with its colon.
    r: String,
    needs_rel_namespace: bool,
    paras: HashMap<(usize, usize), &'a Paragraph>,
    tables: HashMap<(usize, usize), &'a Table>,
    runs: HashMap<RunKey, &'a RunSource>,
    /// The base look of the paragraph each original run was in.
    run_bases: HashMap<RunKey, &'a RunLook>,
    /// The drawings of the original runs.
    drawings: HashMap<RunKey, &'a Arc<Drawing>>,
    rels: Rels,
    /// Table cells being written (paragraphs in them are written with their
    /// layout spelled out).
    in_cell: usize,
    media: Media,
    /// The prefixes of the drawing namespaces, with their colons, and which
    /// of them the root element has to declare.
    wp: String,
    a: String,
    pic: String,
    undeclared: Vec<(String, &'static str)>,
    wrote_picture: bool,
    /// The last `docPr` id given out (or found in the file).
    next_docpr: u32,
    /// The opening tags of hyperlinks written here: two of them with
    /// nothing between become one.
    openers: HashSet<String>,
}

/// What the original document has, by where it was read from.
#[derive(Default)]
struct Index<'a> {
    paras: HashMap<(usize, usize), &'a Paragraph>,
    tables: HashMap<(usize, usize), &'a Table>,
    runs: HashMap<RunKey, &'a RunSource>,
    /// The base look of the paragraph each run was in.
    bases: HashMap<RunKey, &'a RunLook>,
    drawings: HashMap<RunKey, &'a Arc<Drawing>>,
}

fn index_blocks<'a>(blocks: &'a [Block], index: &mut Index<'a>) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                if let Some(source) = &p.source {
                    index.paras.insert(range_key(&source.range), p);
                    for run in &source.runs {
                        index.runs.insert(run_key(run), run);
                        index.bases.insert(run_key(run), &p.base);
                    }
                    for run in &p.runs {
                        if let (Some(source), Inline::Drawing(drawing)) = (&run.source, &run.kind) {
                            index.drawings.insert(run_key(source), drawing);
                        }
                    }
                }
            }
            Block::Table(t) => {
                if let Some(range) = &t.source {
                    index.tables.insert(range_key(range), t);
                }
                for row in &t.rows {
                    for cell in &row.cells {
                        index_blocks(&cell.blocks, index);
                    }
                }
            }
        }
    }
}

/// The prefix bound to `namespace` in the root element, with its colon.
fn prefix_for(text: &str, namespace: &str) -> Option<String> {
    for (at, _) in text.match_indices("xmlns:") {
        let rest = &text[at + 6..];
        let name_end = rest.find(['=', ' ', '>'])?;
        let after = rest[name_end..].trim_start_matches('=');
        let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let value = &after[1..];
        let end = value.find(quote)?;
        if &value[..end] == namespace {
            return Some(format!("{}:", &rest[..name_end]));
        }
        if at > 4096 {
            break;
        }
    }
    None
}

/// The start tag of the root element.
fn root_tag(text: &str) -> &str {
    let mut at = 0;
    while let Some(i) = text[at..].find('<') {
        let start = at + i;
        let next = text.as_bytes().get(start + 1).copied().unwrap_or(b'>');
        if next.is_ascii_alphabetic() || next == b'_' {
            return match text[start..].find('>') {
                Some(end) => &text[start..start + end + 1],
                None => &text[start..],
            };
        }
        at = start + 1;
    }
    ""
}

impl<'a> Saver<'a> {
    fn new(
        text: &'a str,
        original: &'a Document,
        document: &'a Document,
        rels_text: String,
    ) -> Self {
        let mut index: Index<'a> = Default::default();
        index_blocks(&original.body.blocks, &mut index);
        let Index {
            paras,
            tables,
            runs,
            bases: run_bases,
            drawings,
        } = index;
        let mut undeclared = Vec::new();
        let mut prefix = |uri: &'static str, preferred: &str| -> String {
            if let Some(found) = prefix_for(text, uri) {
                return found;
            }
            let root = root_tag(text);
            let name = std::iter::once(preferred.to_owned())
                .chain((1..).map(|n| format!("{preferred}{n}")))
                .find(|name| !root.contains(&format!("xmlns:{name}=")))
                .expect("some name is free");
            undeclared.push((name.clone(), uri));
            format!("{name}:")
        };
        let wp = prefix(WP_NS, "wp");
        let a = prefix(A_NS, "a");
        let pic = prefix(PIC_NS, "pic");
        let w = prefix_for(
            text,
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        )
        .unwrap_or_else(|| "w:".to_owned());
        let r = prefix_for(text, REL_NS);
        Self {
            text,
            original,
            document,
            w,
            needs_rel_namespace: r.is_none(),
            r: r.unwrap_or_else(|| "r:".to_owned()),
            paras,
            tables,
            runs,
            run_bases,
            drawings,
            rels: Rels {
                text: rels_text,
                changed: false,
            },
            in_cell: 0,
            media: Media::default(),
            wp,
            a,
            pic,
            undeclared,
            wrote_picture: false,
            next_docpr: max_docpr(text),
            openers: HashSet::new(),
        }
    }

    /// What an original run's look comes to in a paragraph whose base look is
    /// `base`: what it had from the base of its own paragraph follows.
    fn effective(&self, origin: &RunSource, base: &RunLook) -> RunLook {
        let mut look = origin.look.clone();
        if let Some(old) = self.run_bases.get(&run_key(origin))
            && **old != *base
        {
            rebase_look(&mut look, old, base);
        }
        look
    }

    /// The run is what the file had, read in a paragraph with base `base`.
    fn unchanged(&self, run: &aster_writer::Run, origin: &RunSource, base: &RunLook) -> bool {
        run.text == origin.text
            && run.style == origin.style
            && run.link == origin.link
            && run.look == self.effective(origin, base)
            && self.resized(run, origin).is_none()
    }

    /// For a drawing whose size is not what the file had: the size it had
    /// and the size it has.
    fn resized(
        &self,
        run: &aster_writer::Run,
        origin: &RunSource,
    ) -> Option<((i64, i64), (i64, i64))> {
        let (Inline::Drawing(now), Some(then)) = (&run.kind, self.drawings.get(&run_key(origin)))
        else {
            return None;
        };
        (now.size != then.size).then_some((then.size, now.size))
    }

    fn slice(&self, range: &Range<usize>) -> Option<&'a str> {
        self.text.get(range.clone())
    }

    /// The main part's text with the body written again where it changed.
    fn document_text(&mut self) -> Result<String> {
        let text = self.text;
        let body = find_all(text, "body")
            .into_iter()
            .next()
            .ok_or_else(|| Error::broken("document", "no body"))?;
        let element = &text[body.start..body.end];
        let (open, close) = inner(element);
        let mut region = body.start + open..body.start + close;
        // The final section properties stay after everything.
        let last = children(element)
            .into_iter()
            .rev()
            .find(|c| !c.name.is_empty());
        if let Some(last) = last
            && last.name == "sectPr"
        {
            region.end = body.start + last.start;
        }
        let blocks = self.container(
            region.clone(),
            &self.original.body.blocks,
            &self.document.body.blocks,
        );
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..region.start]);
        out.push_str(&blocks);
        out.push_str(&text[region.end..]);
        if self.needs_rel_namespace && self.rels.changed {
            out = add_namespace(&out, "r", REL_NS);
        }
        if self.wrote_picture {
            for (prefix, uri) in &self.undeclared {
                out = add_namespace(&out, prefix, uri);
            }
        }
        Ok(out)
    }

    // -- blocks -----------------------------------------------------------

    /// The text inside a body or a table cell, in `region` of the original,
    /// where `orig` were the blocks and `model` are the blocks now.
    fn container(&mut self, region: Range<usize>, orig: &[Block], model: &[Block]) -> String {
        // The original blocks, in order and inside the region.
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for block in orig {
            let range = match block {
                Block::Paragraph(p) => p.source.as_ref().map(|s| s.range.clone()),
                Block::Table(t) => t.source.clone(),
            };
            if let Some(range) = range
                && range.start >= ranges.last().map_or(region.start, |r| r.end)
                && range.end <= region.end
                && range.start < range.end
            {
                ranges.push(range);
            }
        }
        let index: HashMap<(usize, usize), usize> = ranges
            .iter()
            .enumerate()
            .map(|(i, r)| (range_key(r), i))
            .collect();
        let n = ranges.len();
        let gap = |g: usize| -> Range<usize> {
            let start = if g == 0 {
                region.start
            } else {
                ranges[g - 1].end
            };
            let end = if g == n { region.end } else { ranges[g].start };
            start..end
        };
        let mut out = String::new();
        let mut next_gap = 1;
        out.push_str(&self.text[gap(0)]);
        let mut last: isize = -1;
        for block in model {
            let key = match block {
                Block::Paragraph(p) => p.source.as_ref().map(|s| range_key(&s.range)),
                Block::Table(t) => t.source.as_ref().map(range_key),
            };
            let matched = key
                .and_then(|k| index.get(&k).copied())
                .filter(|&i| i as isize > last);
            match matched {
                Some(i) => {
                    for g in next_gap..=i {
                        out.push_str(&self.text[gap(g)]);
                    }
                    next_gap = next_gap.max(i + 1);
                    last = i as isize;
                    out.push_str(&self.block_in_place(block, &ranges[i]));
                }
                None => out.push_str(&self.block_elsewhere(block)),
            }
        }
        for g in next_gap..=n {
            out.push_str(&self.text[gap(g)]);
        }
        out
    }

    /// A block that is where it was.
    fn block_in_place(&mut self, block: &Block, range: &Range<usize>) -> String {
        match block {
            Block::Paragraph(p) => {
                if let Some(op) = self.paras.get(&range_key(range)).copied() {
                    if para_unchanged(p, op) {
                        return self.text[range.clone()].to_owned();
                    }
                    if let Some(out) = self.rewrite_in_place(p, op) {
                        return out;
                    }
                }
                self.fresh_paragraph(p)
            }
            Block::Table(t) => self.table_in_place(t, range),
        }
    }

    /// A block with no place of its own in the original: new, moved, the
    /// second half of a split.
    fn block_elsewhere(&mut self, block: &Block) -> String {
        match block {
            Block::Paragraph(p) => {
                if let Some(source) = &p.source
                    && let Some(op) = self.paras.get(&range_key(&source.range)).copied()
                    && para_unchanged(p, op)
                    && let Some(bytes) = self.slice(&source.range)
                {
                    return bytes.to_owned();
                }
                self.fresh_paragraph(p)
            }
            Block::Table(t) => {
                if let Some(range) = &t.source
                    && self.tables.contains_key(&range_key(range))
                    && self.slice(range).is_some()
                {
                    return self.table_in_place(t, range);
                }
                self.table_from_model(t)
            }
        }
    }

    // -- tables -----------------------------------------------------------

    /// A table that is where it was. What is kept of the original bytes
    /// depends on what changed:
    ///
    /// - only the content of cells: the bytes of the table, with the content
    ///   of the cells that changed written again;
    /// - rows added or removed, columns as they were: the table's start
    ///   (`tblPr`, `tblGrid`) and every row that is as read are copied, the
    ///   rows made here are written from the model;
    /// - columns added or removed: the table's `tblPr` is kept (its width
    ///   follows the grid where it was the grid's sum), the grid and every
    ///   row are written from the model, with each cell's content as for
    ///   any block (paragraphs that are as read are copied).
    fn table_in_place(&mut self, model: &Table, range: &Range<usize>) -> String {
        let Some(ot) = self.tables.get(&range_key(range)).copied() else {
            return self.table_from_model(model);
        };
        if model.props != ot.props {
            return self.table_from_model(model);
        }
        if same_structure(model, ot) {
            let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
            for (row, orow) in model.rows.iter().zip(&ot.rows) {
                match self.cell_replacements(row, orow) {
                    Some(found) => replacements.extend(found),
                    None => return self.table_from_model(model),
                }
            }
            return self.apply(range, replacements);
        }
        self.table_rewritten(model, ot, range)
    }

    /// The original bytes of `range` with the replacements put in.
    fn apply(&self, range: &Range<usize>, replacements: Vec<(Range<usize>, String)>) -> String {
        let mut out = String::new();
        let mut at = range.start;
        for (region, content) in replacements {
            out.push_str(&self.text[at..region.start]);
            out.push_str(&content);
            at = region.end;
        }
        out.push_str(&self.text[at..range.end]);
        out
    }

    /// The content of the cells of `row` that changed, written again, with
    /// the part of the original bytes it replaces. `None` if the bytes do
    /// not fit the model.
    fn cell_replacements(
        &mut self,
        row: &aster_writer::Row,
        orow: &aster_writer::Row,
    ) -> Option<Vec<(Range<usize>, String)>> {
        let mut out = Vec::new();
        for (cell, ocell) in row.cells.iter().zip(&orow.cells) {
            if cell_unchanged(cell, ocell) {
                continue;
            }
            let crange = ocell.source.as_ref()?;
            let element = self.slice(crange)?;
            let (open, close) = inner(element);
            let region = crange.start + open..crange.start + close;
            self.in_cell += 1;
            let content = self.container(region.clone(), &ocell.blocks, &cell.blocks);
            self.in_cell -= 1;
            out.push((region, content));
        }
        Some(out)
    }

    /// A table whose rows or columns changed, over the original's bytes.
    fn table_rewritten(&mut self, model: &Table, ot: &Table, range: &Range<usize>) -> String {
        let Some(element) = self.slice(range) else {
            return self.table_from_model(model);
        };
        let kids = children(element);
        let Some(grid) = kids.iter().find(|c| c.name == "tblGrid") else {
            return self.table_from_model(model);
        };
        let (_, close) = inner(element);
        if grid.end > close {
            return self.table_from_model(model);
        }
        let region = range.start + grid.end..range.start + close;
        let columns_changed = model.columns != ot.columns;

        // The start: kept, with the table's width following the grid.
        let mut out = String::new();
        match kids.iter().find(|c| c.name == "tblPr") {
            Some(pr) if columns_changed && pr.end <= grid.start => {
                out.push_str(&element[..pr.start]);
                out.push_str(&self.patch_table_width(
                    &element[pr.start..pr.end],
                    ot.columns.iter().sum(),
                    model.columns.iter().sum(),
                ));
                out.push_str(&element[pr.end..grid.start]);
            }
            _ => out.push_str(&element[..grid.start]),
        }
        if columns_changed {
            out.push_str(&self.grid_xml(&model.columns));
        } else {
            out.push_str(&element[grid.start..grid.end]);
        }

        // The rows of the original, in order and inside the region.
        let mut ranges: Vec<(Range<usize>, usize)> = Vec::new();
        for (i, row) in ot.rows.iter().enumerate() {
            if let Some(r) = &row.source
                && r.start >= ranges.last().map_or(region.start, |(r, _)| r.end)
                && r.end <= region.end
                && r.start < r.end
            {
                ranges.push((r.clone(), i));
            }
        }
        let index: HashMap<(usize, usize), usize> = ranges
            .iter()
            .enumerate()
            .map(|(i, (r, _))| (range_key(r), i))
            .collect();
        let n = ranges.len();
        let gap = |g: usize| -> Range<usize> {
            let start = if g == 0 {
                region.start
            } else {
                ranges[g - 1].0.end
            };
            let end = if g == n {
                region.end
            } else {
                ranges[g].0.start
            };
            start..end
        };
        out.push_str(&self.text[gap(0)]);
        let mut next_gap = 1;
        let mut last: isize = -1;
        for row in &model.rows {
            let matched = row
                .source
                .as_ref()
                .and_then(|r| index.get(&range_key(r)).copied())
                .filter(|&i| i as isize > last);
            let Some(i) = matched else {
                out.push_str(&self.row_xml(&model.columns, row, None));
                continue;
            };
            for g in next_gap..=i {
                out.push_str(&self.text[gap(g)]);
            }
            next_gap = next_gap.max(i + 1);
            last = i as isize;
            let (rrange, oi) = (&ranges[i].0, ranges[i].1);
            let orow = &ot.rows[oi];
            let text = if !columns_changed && same_row(row, orow) {
                match self.cell_replacements(row, orow) {
                    Some(found) => self.apply(rrange, found),
                    None => self.row_xml(&model.columns, row, None),
                }
            } else {
                let kept = if row_props_equal(row, orow) {
                    self.kept_row_start(rrange)
                } else {
                    None
                };
                self.row_xml(&model.columns, row, kept)
            };
            out.push_str(&text);
        }
        for g in next_gap..=n {
            out.push_str(&self.text[gap(g)]);
        }
        out.push_str(&element[close..]);
        out
    }

    /// The row element's start tag, its `trPr` (and `tblPrEx`) as they were
    /// and its end tag: what a row written again keeps. `None` if the row
    /// does not start with them.
    fn kept_row_start(&self, range: &Range<usize>) -> Option<(String, String, String)> {
        let element = self.slice(range)?;
        if !is_element(element, "tr") {
            return None;
        }
        let (open, _) = inner(element);
        if element[..open].ends_with("/>") {
            return None;
        }
        let kids = children(element);
        let first = kids.iter().position(|c| c.name == "tc")?;
        if !kids[..first]
            .iter()
            .all(|c| c.name == "trPr" || c.name == "tblPrEx")
        {
            return None;
        }
        Some((
            element[..open].to_owned(),
            element[open..kids[first].start].to_owned(),
            format!("</{}>", qualified_name(element)),
        ))
    }

    /// `tblPr` with the width following the grid: where it is in twips and
    /// was the sum of the old columns, it is the sum of the new ones.
    fn patch_table_width(&self, pr: &str, old: i32, new: i32) -> String {
        let Some(width) = children(pr).into_iter().find(|c| c.name == "tblW") else {
            return pr.to_owned();
        };
        let element = &pr[width.start..width.end];
        let is_dxa =
            attribute(element, &format!("{}type", self.w)).is_none_or(|kind| kind == "dxa");
        let value = format!("{}w", self.w);
        if is_dxa && attribute(element, &value).as_deref() == Some(old.to_string().as_str()) {
            return format!(
                "{}{}{}",
                &pr[..width.start],
                set_attribute(element, &value, &new.to_string()),
                &pr[width.end..]
            );
        }
        pr.to_owned()
    }

    fn grid_xml(&self, columns: &[i32]) -> String {
        let w = &self.w;
        let mut out = format!("<{w}tblGrid>");
        for column in columns {
            out.push_str(&format!("<{w}gridCol {w}w=\"{column}\"/>"));
        }
        out.push_str(&format!("</{w}tblGrid>"));
        out
    }

    /// A table written from the model: grid, rows, cells, spans, merges,
    /// borders and shading.
    fn table_from_model(&mut self, table: &Table) -> String {
        let w = self.w.clone();
        let mut out = format!("<{w}tbl><{w}tblPr>");
        if let Some(style) = &table.props.style {
            out.push_str(&format!(
                "<{w}tblStyle {w}val=\"{}\"/>",
                escape_attribute(style)
            ));
        }
        let total: i32 = table.columns.iter().sum();
        out.push_str(&format!("<{w}tblW {w}w=\"{total}\" {w}type=\"dxa\"/>"));
        match table.props.align {
            Align::Center => out.push_str(&format!("<{w}jc {w}val=\"center\"/>")),
            Align::Right => out.push_str(&format!("<{w}jc {w}val=\"right\"/>")),
            _ => {}
        }
        out.push_str(&format!(
            "<{w}tblInd {w}w=\"{}\" {w}type=\"dxa\"/>",
            table.props.indent
        ));
        // Borders that every cell has alike are the table's too.
        if let Some(sides) = common_borders(table) {
            let mut borders = borders_element(
                &w,
                "tblBorders",
                &sides,
                None,
                ["left", "top", "right", "bottom"],
            );
            let inside = format!(
                "{}{}</{w}tblBorders>",
                border_element(&w, "insideH", sides[1].as_ref()),
                border_element(&w, "insideV", sides[0].as_ref())
            );
            borders = borders.replace(&format!("</{w}tblBorders>"), &inside);
            out.push_str(&borders);
        }
        if table.props.fixed_layout {
            out.push_str(&format!("<{w}tblLayout {w}type=\"fixed\"/>"));
        }
        let [left, top, right, bottom] = table.props.cell_margins;
        out.push_str(&format!(
            "<{w}tblCellMar><{w}top {w}w=\"{top}\" {w}type=\"dxa\"/><{w}left {w}w=\"{left}\" {w}type=\"dxa\"/><{w}bottom {w}w=\"{bottom}\" {w}type=\"dxa\"/><{w}right {w}w=\"{right}\" {w}type=\"dxa\"/></{w}tblCellMar>"
        ));
        // What Word asks of a table style by default.
        out.push_str(&format!(
            "<{w}tblLook {w}val=\"04A0\" {w}firstRow=\"1\" {w}lastRow=\"0\" {w}firstColumn=\"1\" {w}lastColumn=\"0\" {w}noHBand=\"0\" {w}noVBand=\"1\"/>"
        ));
        out.push_str(&format!("</{w}tblPr>"));
        out.push_str(&self.grid_xml(&table.columns));
        for row in &table.rows {
            out.push_str(&self.row_xml(&table.columns, row, None));
        }
        out.push_str(&format!("</{w}tbl>"));
        out
    }

    /// The `trPr` of a row, from the model.
    fn row_pr(&self, row: &aster_writer::Row) -> String {
        let w = &self.w;
        let mut pr = String::new();
        if row.grid_before > 0 {
            pr.push_str(&format!("<{w}gridBefore {w}val=\"{}\"/>", row.grid_before));
        }
        if row.grid_after > 0 {
            pr.push_str(&format!("<{w}gridAfter {w}val=\"{}\"/>", row.grid_after));
        }
        if row.cant_split {
            pr.push_str(&format!("<{w}cantSplit/>"));
        }
        match row.height {
            RowHeight::Auto => {}
            RowHeight::AtLeast(h) => pr.push_str(&format!(
                "<{w}trHeight {w}val=\"{h}\" {w}hRule=\"atLeast\"/>"
            )),
            RowHeight::Exactly(h) => {
                pr.push_str(&format!("<{w}trHeight {w}val=\"{h}\" {w}hRule=\"exact\"/>"))
            }
        }
        if row.header {
            pr.push_str(&format!("<{w}tblHeader/>"));
        }
        if pr.is_empty() {
            pr
        } else {
            format!("<{w}trPr>{pr}</{w}trPr>")
        }
    }

    /// A row from the model: its cells with their widths from `columns`,
    /// spans, merges, borders, shading, margins and alignment, and their
    /// content as for any block. With `kept` (the start tag, the row
    /// properties and the end tag it had) the row keeps those bytes.
    fn row_xml(
        &mut self,
        columns: &[i32],
        row: &aster_writer::Row,
        kept: Option<(String, String, String)>,
    ) -> String {
        let w = self.w.clone();
        let (start, end) = match kept {
            Some((tag, pr, end)) => (format!("{tag}{pr}"), end),
            None => (format!("<{w}tr>{}", self.row_pr(row)), format!("</{w}tr>")),
        };
        let mut out = start;
        let mut column = row.grid_before as usize;
        for cell in &row.cells {
            let span = cell.span.max(1) as usize;
            let width: i32 = columns.iter().skip(column).take(span).sum();
            column += span;
            out.push_str(&format!(
                "<{w}tc><{w}tcPr><{w}tcW {w}w=\"{width}\" {w}type=\"dxa\"/>"
            ));
            if span > 1 {
                out.push_str(&format!("<{w}gridSpan {w}val=\"{span}\"/>"));
            }
            match cell.merge {
                VMerge::None => {}
                VMerge::Start => out.push_str(&format!("<{w}vMerge {w}val=\"restart\"/>")),
                VMerge::Continue => out.push_str(&format!("<{w}vMerge/>")),
            }
            if cell.borders.iter().any(Option::is_some) {
                out.push_str(&borders_element(
                    &w,
                    "tcBorders",
                    &cell.borders,
                    None,
                    ["left", "top", "right", "bottom"],
                ));
            }
            if let Some(fill) = cell.shading {
                out.push_str(&format!(
                    "<{w}shd {w}val=\"clear\" {w}color=\"auto\" {w}fill=\"{}\"/>",
                    hex(fill)
                ));
            }
            let [l, t, r, b] = cell.margins;
            out.push_str(&format!(
                "<{w}tcMar><{w}top {w}w=\"{t}\" {w}type=\"dxa\"/><{w}left {w}w=\"{l}\" {w}type=\"dxa\"/><{w}bottom {w}w=\"{b}\" {w}type=\"dxa\"/><{w}right {w}w=\"{r}\" {w}type=\"dxa\"/></{w}tcMar>"
            ));
            match cell.align {
                CellAlign::Top => {}
                CellAlign::Center => out.push_str(&format!("<{w}vAlign {w}val=\"center\"/>")),
                CellAlign::Bottom => out.push_str(&format!("<{w}vAlign {w}val=\"bottom\"/>")),
            }
            out.push_str(&format!("</{w}tcPr>"));
            self.in_cell += 1;
            for block in &cell.blocks {
                out.push_str(&self.block_elsewhere(block));
            }
            self.in_cell -= 1;
            if cell.blocks.last().is_none_or(|b| b.as_table().is_some()) {
                out.push_str(&format!("<{w}p/>"));
            }
            out.push_str(&format!("</{w}tc>"));
        }
        out.push_str(&end);
        out
    }

    // -- paragraphs -------------------------------------------------------

    fn section_xml(&self, section: &Section) -> String {
        if let Some(range) = &section.source
            && let Some(bytes) = self.slice(range)
            && is_element(bytes, "sectPr")
        {
            return bytes.to_owned();
        }
        let w = &self.w;
        let mut out = format!("<{w}sectPr>");
        match section.start {
            SectionStart::NextPage => {}
            SectionStart::Continuous => out.push_str(&format!("<{w}type {w}val=\"continuous\"/>")),
            SectionStart::EvenPage => out.push_str(&format!("<{w}type {w}val=\"evenPage\"/>")),
            SectionStart::OddPage => out.push_str(&format!("<{w}type {w}val=\"oddPage\"/>")),
            SectionStart::NextColumn => out.push_str(&format!("<{w}type {w}val=\"nextColumn\"/>")),
        }
        let (width, height) = section.page;
        let orient = if width > height {
            format!(" {w}orient=\"landscape\"")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<{w}pgSz {w}w=\"{width}\" {w}h=\"{height}\"{orient}/>"
        ));
        let m = &section.margins;
        out.push_str(&format!(
            "<{w}pgMar {w}top=\"{}\" {w}right=\"{}\" {w}bottom=\"{}\" {w}left=\"{}\" {w}header=\"{}\" {w}footer=\"{}\" {w}gutter=\"{}\"/>",
            m.top, m.right, m.bottom, m.left, m.header, m.footer, m.gutter
        ));
        let separator = if section.columns.separator {
            format!(" {w}sep=\"1\"")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<{w}cols {w}num=\"{}\" {w}space=\"{}\"{separator}/>",
            section.columns.count, section.columns.space
        ));
        if section.title_page {
            out.push_str(&format!("<{w}titlePg/>"));
        }
        out.push_str(&format!("</{w}sectPr>"));
        out
    }

    /// The section edit that takes a paragraph's `pPr` from what it carried
    /// to what it carries.
    fn section_edit(&self, now: &Option<Arc<Section>>, then: &Option<Arc<Section>>) -> SectionEdit {
        match (now, then) {
            (None, None) => SectionEdit::Keep,
            (None, Some(_)) => SectionEdit::Remove,
            (Some(a), Some(b)) if a == b => SectionEdit::Keep,
            (Some(a), _) => SectionEdit::Set(self.section_xml(a)),
        }
    }

    /// A paragraph that was where it was and has changed: its start tag, its
    /// `pPr` with the changed properties and the runs spliced. `None` if
    /// the bytes do not fit the model (the caller writes it from the model).
    fn rewrite_in_place(&mut self, model: &Paragraph, op: &Paragraph) -> Option<String> {
        let osrc = op.source.as_ref()?;
        let range = osrc.range.clone();
        let element = self.slice(&range)?;
        let (open_end, close_start) = inner(element);
        let empty = open_end == close_start && element[..open_end].ends_with("/>");
        let qname = qualified_name(element).to_owned();
        let start_tag = if empty {
            format!("{}>", &element[..open_end - 2])
        } else {
            element[..open_end].to_owned()
        };
        let ppr_span = children(element).into_iter().find(|c| c.name == "pPr");
        let content_start = ppr_span.as_ref().map_or(open_end, |c| c.end);
        let content_end = close_start;
        let ppr_old = ppr_span.as_ref().map(|c| &element[c.start..c.end]);

        // The runs of the original, as units of bytes with gaps between.
        let mut units: Vec<Unit> = Vec::new();
        let mut at = range.start + content_start;
        let limit = range.start + content_end;
        for (i, run) in osrc.runs.iter().enumerate() {
            let span = run.parent.clone().unwrap_or_else(|| run.range.clone());
            if let Some(last) = units.last_mut()
                && run.parent.is_some()
                && last.range == span
            {
                last.children.push(i);
                continue;
            }
            if span.start < at || span.end > limit || span.start >= span.end {
                return None;
            }
            if run.parent.is_some() && (run.range.start < span.start || run.range.end > span.end) {
                return None;
            }
            at = span.end;
            units.push(Unit {
                range: span,
                children: vec![i],
                group: run.parent.is_some(),
            });
        }

        let section = self.section_edit(&model.section, &osrc.section);
        let mut src_mark = osrc.mark.clone();
        if op.base != model.base {
            rebase_look(&mut src_mark, &op.base, &model.base);
        }
        let changed = model.props != osrc.props
            || model.mark != src_mark
            || !matches!(section, SectionEdit::Keep);
        let ppr = if changed {
            ppr_patch(
                &self.w,
                ppr_old,
                &osrc.props,
                &model.props,
                Some(
                    self.document
                        .styles
                        .paragraph_look(model.props.style.as_deref())
                        .0,
                ),
                &src_mark,
                &model.mark,
                &model.base,
                section,
                false,
            )
            .unwrap_or_default()
        } else {
            ppr_old.unwrap_or_default().to_owned()
        };

        let mut gaps: Vec<Range<usize>> = Vec::with_capacity(units.len() + 1);
        let mut from = range.start + content_start;
        for unit in &units {
            gaps.push(from..unit.range.start);
            from = unit.range.end;
        }
        gaps.push(from..limit);
        let content = self.rewrite_runs(model, osrc, &units, gaps)?;
        let end_tag = format!("</{qname}>");
        Some(format!("{start_tag}{ppr}{content}{end_tag}"))
    }

    /// The content of a changed paragraph: the original bytes between the
    /// runs, the runs as read, changed or gone, and the runs made here.
    fn rewrite_runs(
        &mut self,
        model: &Paragraph,
        osrc: &ParaSource,
        units: &[Unit],
        gaps: Vec<Range<usize>>,
    ) -> Option<String> {
        let m = units.len();
        let mut unit_of = vec![0usize; osrc.runs.len()];
        for (u, unit) in units.iter().enumerate() {
            for &i in &unit.children {
                unit_of[i] = u;
            }
        }
        let index_of: HashMap<RunKey, usize> = osrc
            .runs
            .iter()
            .enumerate()
            .map(|(i, r)| (run_key(r), i))
            .collect();

        // Which runs stand for which original runs: the first claim in
        // order; anything else is made here or a piece of a split.
        #[derive(Clone, Copy)]
        enum Act {
            InPlace(usize),
            Elsewhere,
        }
        let mut acts = Vec::with_capacity(model.runs.len());
        let mut last: isize = -1;
        for run in &model.runs {
            let claim = run
                .source
                .as_ref()
                .and_then(|s| {
                    index_of
                        .get(&run_key(s))
                        .copied()
                        .filter(|&i| *osrc.runs[i] == **s)
                })
                .filter(|&i| i as isize > last);
            match claim {
                Some(i) => {
                    last = i as isize;
                    acts.push(Act::InPlace(i));
                }
                None => acts.push(Act::Elsewhere),
            }
        }

        // Runs that came out of one `<w:r>` and are all there, in order and
        // as read, are copied as that element.
        let mut skip = vec![false; model.runs.len()];
        let mut whole = vec![false; model.runs.len()];
        for (u, unit) in units.iter().enumerate() {
            if !unit.group {
                continue;
            }
            let members: Vec<(usize, usize)> = acts
                .iter()
                .enumerate()
                .filter_map(|(j, a)| match a {
                    Act::InPlace(i) if unit_of[*i] == u => Some((j, *i)),
                    _ => None,
                })
                .collect();
            let intact = members.len() == unit.children.len()
                && members.iter().enumerate().all(|(n, (j, i))| {
                    *i == unit.children[n]
                        && (n == 0 || *j == members[n - 1].0 + 1)
                        && self.unchanged(&model.runs[*j], &osrc.runs[*i], &model.base)
                });
            if intact {
                whole[members[0].0] = true;
                for (j, _) in &members[1..] {
                    skip[*j] = true;
                }
            }
        }

        let wrapper_links: Vec<Option<Arc<Link>>> = (0..=m)
            .map(|g| {
                units
                    .get(g)
                    .and_then(|u| osrc.runs.get(u.children[0]))
                    .and_then(|r| r.link.clone())
            })
            .collect();
        let mut em = Em {
            text: self.text,
            gaps,
            consumed: vec![0; m + 1],
            out: String::new(),
            open: Vec::new(),
            suspended: Vec::new(),
            wrapper_links,
        };
        let mut k = 0usize;
        let mut after: Option<Option<Arc<Link>>> = None;
        for (j, run) in model.runs.iter().enumerate() {
            if skip[j] {
                continue;
            }
            match acts[j] {
                Act::InPlace(i) => {
                    let u = unit_of[i];
                    if u + 1 != k {
                        for g in k..=u {
                            em.gap(g);
                        }
                        k = u + 1;
                    }
                    let original = &osrc.runs[i];
                    let relinked = run.link != original.link;
                    if !relinked {
                        em.reopen();
                    }
                    let bytes = if whole[j] {
                        self.slice(&units[u].range)?.to_owned()
                    } else if original.parent.is_none()
                        && self.unchanged(run, original, &model.base)
                    {
                        self.slice(&original.range)?.to_owned()
                    } else {
                        self.run_from_origin(run, original, &model.base)?
                    };
                    if relinked {
                        let w = self.w.clone();
                        let mut opener = |link: &Link| self.hyperlink_open(link);
                        em.place(&run.link, &bytes, &w, &mut opener);
                    } else {
                        em.out.push_str(&bytes);
                    }
                    after = Some(run.link.clone());
                }
                Act::Elsewhere => {
                    let Some(bytes) = self.run_xml(run, &model.base) else {
                        continue;
                    };
                    let w = self.w.clone();
                    let mut opener = |link: &Link| self.hyperlink_open(link);
                    em.elsewhere(&run.link, &after, k, &bytes, &w, &mut opener);
                    after = None;
                }
            }
        }
        for g in k..=m {
            em.gap(g);
        }
        Some(self.join_links(em.out))
    }

    /// Hyperlinks written here that follow each other with nothing between
    /// them, to the same place, are one.
    fn join_links(&self, mut out: String) -> String {
        let open = format!("<{}hyperlink", self.w);
        let close = format!("</{}hyperlink>", self.w);
        for opener in &self.openers {
            let joint = format!("{close}{opener}");
            let mut from = 0;
            while let Some(i) = out[from..].find(&joint) {
                let at = from + i;
                // The hyperlink that closes there has the same opening tag.
                let same = out[..at]
                    .rfind(&open)
                    .is_some_and(|start| out[start..].starts_with(opener.as_str()));
                if same {
                    out.replace_range(at..at + joint.len(), "");
                    from = at;
                } else {
                    from = at + joint.len();
                }
            }
        }
        out
    }

    /// A paragraph written from the model, with no element to start from
    /// except what its runs carry.
    fn fresh_paragraph(&mut self, p: &Paragraph) -> String {
        let styles = self.document.styles.clone();
        let mut style_props = styles.paragraph_look(p.props.style.as_deref()).0.clone();
        style_props.style = None;
        let section = match &p.section {
            Some(section) => SectionEdit::Set(self.section_xml(section)),
            None => SectionEdit::Keep,
        };
        let ppr = ppr_patch(
            &self.w,
            None,
            &style_props,
            &p.props,
            None,
            &p.base,
            &p.mark,
            &p.base,
            section,
            self.in_cell > 0,
        )
        .unwrap_or_default();
        let w = self.w.clone();
        let mut out = format!("<{w}p>{ppr}");
        let mut i = 0;
        while i < p.runs.len() {
            let link = p.runs[i].link.clone();
            let mut j = i;
            let mut group = String::new();
            while j < p.runs.len() && p.runs[j].link == link {
                if let Some(bytes) = self.run_xml(&p.runs[j], &p.base) {
                    group.push_str(&bytes);
                }
                j += 1;
            }
            match link {
                Some(link) if !group.is_empty() => {
                    out.push_str(&self.hyperlink_open(&link));
                    out.push_str(&group);
                    out.push_str(&format!("</{w}hyperlink>"));
                }
                _ => out.push_str(&group),
            }
            i = j;
        }
        out.push_str(&format!("</{w}p>"));
        out
    }

    // -- runs -------------------------------------------------------------

    /// The opening tag of a hyperlink element for `link`.
    fn hyperlink_open(&mut self, link: &Link) -> String {
        let w = self.w.clone();
        let mut tag = format!("<{w}hyperlink");
        if let Some(anchor) = link.target.strip_prefix('#') {
            tag.push_str(&format!(" {w}anchor=\"{}\"", escape_attribute(anchor)));
        } else {
            let id = self.rels.hyperlink(&link.target);
            tag.push_str(&format!(" {}id=\"{id}\"", self.r));
        }
        if let Some(tip) = &link.tooltip {
            tag.push_str(&format!(" {w}tooltip=\"{}\"", escape_attribute(tip)));
        }
        tag.push_str(&format!(" {w}history=\"1\">"));
        self.openers.insert(tag.clone());
        tag
    }

    /// A run written wherever the paragraph now has it: from the bytes it
    /// was read from if it has any, otherwise from the model.
    fn run_xml(&mut self, run: &aster_writer::Run, base: &RunLook) -> Option<String> {
        let origin = run
            .source
            .as_ref()
            .and_then(|s| self.runs.get(&run_key(s)).copied().filter(|o| **o == **s));
        match origin {
            Some(origin) => self.run_from_origin(run, origin, base),
            None => self.run_from_model(run, base),
        }
    }

    /// A run read from the file: its element with the look patched and the
    /// text written again, or its bytes as they were.
    fn run_from_origin(
        &self,
        run: &aster_writer::Run,
        origin: &RunSource,
        base: &RunLook,
    ) -> Option<String> {
        let element = self.slice(&origin.range)?;
        if matches!(run.kind, Inline::Marker) {
            return (!element.is_empty() && is_balanced(element)).then(|| element.to_owned());
        }
        let same = self.unchanged(run, origin, base);
        let src_look = self.effective(origin, base);
        let (start_tag, qname, rpr_old, content) = match &origin.parent {
            None => {
                if !is_element(element, "r") {
                    return None;
                }
                let (open, close) = inner(element);
                let kids = children(element);
                let rpr = kids
                    .iter()
                    .find(|c| c.name == "rPr")
                    .map(|c| &element[c.start..c.end]);
                let content: String = kids
                    .iter()
                    .filter(|c| c.name != "rPr" && c.name != "lastRenderedPageBreak")
                    .map(|c| &element[c.start..c.end])
                    .collect();
                if same {
                    return Some(element.to_owned());
                }
                let _ = close;
                (
                    element[..open].to_owned(),
                    qualified_name(element).to_owned(),
                    rpr,
                    content,
                )
            }
            Some(parent) => {
                let pelement = self.slice(parent)?;
                if !is_element(pelement, "r") {
                    return None;
                }
                let (open, _) = inner(pelement);
                let rpr = children(pelement)
                    .into_iter()
                    .find(|c| c.name == "rPr")
                    .map(|c| &pelement[c.start..c.end]);
                (
                    pelement[..open].to_owned(),
                    qualified_name(pelement).to_owned(),
                    rpr,
                    element.to_owned(),
                )
            }
        };
        let rpr = if same {
            rpr_old.unwrap_or_default().to_owned()
        } else {
            rpr_patch(
                &self.w,
                rpr_old,
                &src_look,
                &origin.style,
                &run.look,
                &run.style,
                base,
            )
        };
        let content = if run.is_text() {
            self.text_element(&run.text)
        } else if let Some((then, now)) = self.resized(run, origin) {
            patch_extent(&content, then, now)
        } else {
            content
        };
        Some(format!("{start_tag}{rpr}{content}</{qname}>"))
    }

    /// What a run in a paragraph with base look `base` looks like with the
    /// character style `style` and nothing of its own: the style sets what
    /// differs from the document defaults.
    fn styled(&self, base: &RunLook, style: &Option<Arc<str>>) -> RunLook {
        let styles = &self.document.styles;
        let mut look = base.clone();
        let Some(style) = style.as_deref().and_then(|id| styles.get(id)) else {
            return look;
        };
        let (set, default) = (&style.run, &styles.default_run);
        macro_rules! field {
            ($($f:ident),+) => {
                $( if set.$f != default.$f { look.$f = set.$f.clone(); } )+
            };
        }
        field!(
            font,
            east_asian_font,
            size,
            bold,
            italic,
            underline,
            strike,
            double_strike,
            color,
            highlight,
            shading,
            position,
            caps,
            small_caps,
            hidden,
            spacing,
            scale,
            raise
        );
        look
    }

    fn text_element(&self, text: &str) -> String {
        let w = &self.w;
        format!("<{w}t xml:space=\"preserve\">{}</{w}t>", escape(text))
    }

    /// A run that was not read: written from the model. Things that need
    /// their original bytes (a drawing, a ruby) cannot be.
    fn run_from_model(&mut self, run: &aster_writer::Run, base: &RunLook) -> Option<String> {
        let w = self.w.clone();
        let w = &w;
        let content = match &run.kind {
            Inline::Text => self.text_element(&run.text),
            Inline::Tab => format!("<{w}tab/>"),
            Inline::Break(Break::Line) => format!("<{w}br/>"),
            Inline::Break(Break::Page) => format!("<{w}br {w}type=\"page\"/>"),
            Inline::Break(Break::Column) => format!("<{w}br {w}type=\"column\"/>"),
            Inline::Footnote(id) => format!("<{w}footnoteReference {w}id=\"{id}\"/>"),
            Inline::Endnote(id) => format!("<{w}endnoteReference {w}id=\"{id}\"/>"),
            Inline::Drawing(drawing) => match &drawing.content {
                DrawingContent::Picture(picture) => self.picture_xml(drawing, picture)?,
                _ => return None,
            },
            Inline::Marker | Inline::PageNumber(_) | Inline::Ruby(_) => {
                return None;
            }
        };
        let src = self.styled(base, &run.style);
        let rpr = rpr_patch(w, None, &src, &None, &run.look, &run.style, base);
        Some(format!("<{w}r>{rpr}{content}</{w}r>"))
    }
}

// ---------------------------------------------------------------------------
// Pictures made here
// ---------------------------------------------------------------------------

impl Saver<'_> {
    /// The relationship and part name for `image`, made on first use.
    /// `None` for bytes that are not in a format Word opens.
    fn image_rel(&mut self, image: &Arc<[u8]>) -> Option<(String, String)> {
        let format = image_format(image)?;
        if let Some(&at) = self.media.by_bytes.get(image) {
            let item = &self.media.items[at];
            return Some((item.rel_id.clone(), item.part.clone()));
        }
        let extension = format.extension();
        // The first number no part of any extension has.
        let part = (1..)
            .map(|n| format!("{}image{n}", self.media.dir))
            .find(|stem| {
                let stem = stem.to_ascii_lowercase();
                !self.media.taken.iter().any(|t| {
                    t.strip_prefix(&stem)
                        .is_some_and(|rest| rest.starts_with('.'))
                })
            })
            .map(|stem| format!("{stem}.{extension}"))
            .expect("some number is free");
        let target = part.rsplit('/').next().unwrap_or(&part);
        let target = format!("media/{target}");
        let rel_id = self.rels.add("image", &target, false);
        self.media.taken.insert(part.to_ascii_lowercase());
        self.media
            .by_bytes
            .insert(image.clone(), self.media.items.len());
        self.media.items.push(NewImage {
            part: part.clone(),
            bytes: image.clone(),
            extension,
            content_type: format.content_type(),
            rel_id: rel_id.clone(),
        });
        Some((rel_id, part))
    }

    /// `<w:drawing>` for a picture made here, in the line. The image is
    /// stored once however often it is used; a floating picture is written
    /// in the line (where it floats is not kept).
    fn picture_xml(&mut self, drawing: &Drawing, picture: &Picture) -> Option<String> {
        let (rel_id, part) = self.image_rel(&picture.image)?;
        self.next_docpr += 1;
        let id = self.next_docpr;
        self.wrote_picture = true;
        let (w, wp, a, pic, r) = (
            self.w.clone(),
            self.wp.clone(),
            self.a.clone(),
            self.pic.clone(),
            self.r.clone(),
        );
        let (cx, cy) = (drawing.size.0.max(0), drawing.size.1.max(0));
        let file = part.rsplit('/').next().unwrap_or("image");
        let descr = drawing.alt.as_deref().map_or(String::new(), |alt| {
            format!(" descr=\"{}\"", escape_attribute(alt))
        });
        let mut crop = String::new();
        if picture.crop.iter().any(|c| *c > 0.0) {
            crop.push_str(&format!("<{a}srcRect"));
            for (name, share) in ["l", "t", "r", "b"].iter().zip(picture.crop) {
                let value = (share.clamp(0.0, 1.0) * 100_000.0).round() as i64;
                if value > 0 {
                    crop.push_str(&format!(" {name}=\"{value}\""));
                }
            }
            crop.push_str("/>");
        }
        let file = escape_attribute(file);
        Some(format!(
            "<{w}drawing><{wp}inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<{wp}extent cx=\"{cx}\" cy=\"{cy}\"/><{wp}effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
<{wp}docPr id=\"{id}\" name=\"Picture {id}\"{descr}/>\
<{wp}cNvGraphicFramePr><{a}graphicFrameLocks noChangeAspect=\"1\"/></{wp}cNvGraphicFramePr>\
<{a}graphic><{a}graphicData uri=\"{PIC_NS}\"><{pic}pic><{pic}nvPicPr>\
<{pic}cNvPr id=\"0\" name=\"{file}\"/><{pic}cNvPicPr/></{pic}nvPicPr>\
<{pic}blipFill><{a}blip {r}embed=\"{rel_id}\"/>{crop}<{a}stretch><{a}fillRect/></{a}stretch></{pic}blipFill>\
<{pic}spPr><{a}xfrm><{a}off x=\"0\" y=\"0\"/><{a}ext cx=\"{cx}\" cy=\"{cy}\"/></{a}xfrm>\
<{a}prstGeom prst=\"rect\"><{a}avLst/></{a}prstGeom></{pic}spPr></{pic}pic>\
</{a}graphicData></{a}graphic></{wp}inline></{w}drawing>"
        ))
    }
}

/// `content` with the size `then` of a drawing (in `wp:extent` and in the
/// `a:ext` of its transform) changed to `now`; other elements are as they
/// were.
fn patch_extent(content: &str, then: (i64, i64), now: (i64, i64)) -> String {
    let mut out = String::with_capacity(content.len());
    let mut at = 0;
    while let Some(i) = content[at..].find('<') {
        let start = at + i;
        let Some(len) = content[start..].find('>') else {
            break;
        };
        let tag = &content[start..start + len + 1];
        out.push_str(&content[at..start]);
        let name = qualified_name(tag);
        let local = name.rsplit(':').next().unwrap_or(name);
        let matches = (local == "extent" || local == "ext")
            && attribute(tag, "cx").as_deref() == Some(then.0.to_string().as_str())
            && attribute(tag, "cy").as_deref() == Some(then.1.to_string().as_str());
        if matches {
            let tag = set_attribute(tag, "cx", &now.0.to_string());
            out.push_str(&set_attribute(&tag, "cy", &now.1.to_string()));
        } else {
            out.push_str(tag);
        }
        at = start + len + 1;
    }
    out.push_str(&content[at..]);
    out
}

// ---------------------------------------------------------------------------
// Comparing the model with what was read
// ---------------------------------------------------------------------------

struct Unit {
    range: Range<usize>,
    /// Indexes into the paragraph's original runs.
    children: Vec<usize>,
    /// The runs are children of this `<w:r>` (the range is the `<w:r>`).
    group: bool,
}

/// The paragraph is what the file had: the same properties, mark, section
/// and runs, each as read.
fn para_unchanged(model: &Paragraph, op: &Paragraph) -> bool {
    let Some(osrc) = &op.source else {
        return false;
    };
    model.props == osrc.props
        && model.base == op.base
        && model.mark == osrc.mark
        && model.section == osrc.section
        && model.runs.len() == osrc.runs.len()
        && model.runs.iter().zip(&op.runs).all(|(run, orig)| {
            run.kind == orig.kind
                && run_same_run(run, orig)
                && run.source.as_ref().map(|s| run_key(s))
                    == orig.source.as_ref().map(|s| run_key(s))
        })
}

fn run_same_run(a: &aster_writer::Run, b: &aster_writer::Run) -> bool {
    a.text == b.text && a.look == b.look && a.style == b.style && a.link == b.link
}

/// The table has the rows and cells it was read with, as they were: the
/// same grid and properties, and in every row the same cells with the same
/// spans, merges, borders, shading, margins and alignment.
fn same_structure(model: &Table, ot: &Table) -> bool {
    model.columns == ot.columns
        && model.props == ot.props
        && model.rows.len() == ot.rows.len()
        && model.rows.iter().zip(&ot.rows).all(|(r, o)| same_row(r, o))
}

fn row_props_equal(row: &Row, orow: &Row) -> bool {
    row.height == orow.height
        && row.header == orow.header
        && row.cant_split == orow.cant_split
        && row.grid_before == orow.grid_before
        && row.grid_after == orow.grid_after
}

fn same_row(row: &Row, orow: &Row) -> bool {
    row.source.is_some()
        && row.source == orow.source
        && row_props_equal(row, orow)
        && row.cells.len() == orow.cells.len()
        && row.cells.iter().zip(&orow.cells).all(|(c, oc)| {
            c.source.is_some()
                && c.source == oc.source
                && c.span == oc.span
                && c.merge == oc.merge
                && c.borders == oc.borders
                && c.shading == oc.shading
                && c.margins == oc.margins
                && c.align == oc.align
        })
}

/// Borders every cell has on all four sides, alike.
fn common_borders(table: &Table) -> Option<[Option<aster_writer::Border>; 4]> {
    let first = table.rows.first()?.cells.first()?.borders;
    let same = first[0].is_some()
        && first.iter().all(|b| *b == first[0])
        && table
            .rows
            .iter()
            .all(|r| r.cells.iter().all(|c| c.borders == first));
    same.then_some(first)
}

fn table_unchanged(model: &Table, ot: &Table) -> bool {
    same_structure(model, ot)
        && model.source == ot.source
        && model.rows.iter().zip(&ot.rows).all(|(r, o)| {
            r.cells
                .iter()
                .zip(&o.cells)
                .all(|(c, oc)| cell_unchanged(c, oc))
        })
}

fn cell_unchanged(cell: &aster_writer::Cell, ocell: &aster_writer::Cell) -> bool {
    cell.blocks.len() == ocell.blocks.len()
        && cell
            .blocks
            .iter()
            .zip(&ocell.blocks)
            .all(|(b, ob)| match (b, ob) {
                (Block::Paragraph(p), Block::Paragraph(op)) => {
                    p.source.as_ref().map(|s| range_key(&s.range))
                        == op.source.as_ref().map(|s| range_key(&s.range))
                        && para_unchanged(p, op)
                }
                (Block::Table(t), Block::Table(ot)) => table_unchanged(t, ot),
                _ => false,
            })
}

// ---------------------------------------------------------------------------
// Writing the bytes between runs
// ---------------------------------------------------------------------------

struct Wrapper {
    name: String,
    bytes: String,
    /// Where in the output its opening bytes start, and where its content
    /// does: a wrapper with nothing written since can be dropped.
    out_at: usize,
    body_at: usize,
    /// A hyperlink (or a field that is one): what runs inside have as link.
    link: Option<Arc<Link>>,
}

/// The output of a paragraph's content, with the wrappers the original bytes
/// have open at each point.
struct Em<'t> {
    text: &'t str,
    gaps: Vec<Range<usize>>,
    /// How much of each gap has been written already.
    consumed: Vec<usize>,
    out: String,
    open: Vec<Wrapper>,
    /// Wrappers closed for a run that does not belong inside them and not
    /// yet opened again.
    suspended: Vec<Wrapper>,
    /// For a hyperlink opened in a gap: the link of the run that follows.
    wrapper_links: Vec<Option<Arc<Link>>>,
}

impl Em<'_> {
    fn reopen(&mut self) {
        for mut wrapper in std::mem::take(&mut self.suspended) {
            wrapper.out_at = self.out.len();
            self.out.push_str(&wrapper.bytes);
            wrapper.body_at = self.out.len();
            self.open.push(wrapper);
        }
    }

    fn close(&mut self, bytes: &str) {
        if self.suspended.pop().is_some() {
            return;
        }
        self.open.pop();
        self.out.push_str(bytes);
    }

    /// The bytes before original run `g` that have not been written.
    fn gap(&mut self, g: usize) {
        let Some(range) = self.gaps.get(g).cloned() else {
            return;
        };
        let start = range.start + self.consumed[g];
        let Some(bytes) = self.text.get(start..range.end) else {
            return;
        };
        let Some(toks) = tokens(bytes) else {
            self.reopen();
            self.out.push_str(bytes);
            return;
        };
        for tok in toks {
            match tok {
                Tok::Open(name, bytes) => {
                    self.reopen();
                    let bearing = matches!(local(name), "hyperlink" | "fldSimple");
                    let link = if bearing {
                        self.wrapper_links.get(g).cloned().flatten()
                    } else {
                        None
                    };
                    let out_at = self.out.len();
                    self.out.push_str(bytes);
                    self.open.push(Wrapper {
                        name: name.to_owned(),
                        bytes: bytes.to_owned(),
                        out_at,
                        body_at: self.out.len(),
                        link,
                    });
                }
                Tok::Close(_, bytes) => self.close(bytes),
                Tok::Other(bytes) => {
                    self.reopen();
                    self.out.push_str(bytes);
                }
                Tok::Space(bytes) => self.out.push_str(bytes),
            }
        }
        self.consumed[g] = range.end - range.start;
    }

    /// A run that has no place among the original runs, written after the
    /// last thing written. Wrappers it does not belong to are left first.
    fn elsewhere(
        &mut self,
        link: &Option<Arc<Link>>,
        after: &Option<Option<Arc<Link>>>,
        next: usize,
        bytes: &str,
        w: &str,
        opener: &mut dyn FnMut(&Link) -> String,
    ) {
        self.leave_after(link, after, next);
        self.place(link, bytes, w, opener);
    }

    /// Leave the wrappers an original run ends inside, before a run that
    /// follows it and does not have its link.
    fn leave_after(
        &mut self,
        link: &Option<Arc<Link>>,
        after: &Option<Option<Arc<Link>>>,
        next: usize,
    ) {
        // It follows an original run that ends inside wrappers: leave them,
        // unless it has the same link.
        if let Some(previous) = after
            && previous != link
            && let Some(range) = self.gaps.get(next).cloned()
        {
            let start = range.start + self.consumed[next];
            if let Some(rest) = self.text.get(start..range.end) {
                let n = leading_closes(rest);
                if n > 0
                    && let Some(toks) = tokens(&rest[..n])
                {
                    for tok in toks {
                        match tok {
                            Tok::Close(_, b) => self.close(b),
                            Tok::Space(b) => self.out.push_str(b),
                            _ => {}
                        }
                    }
                    self.consumed[next] += n;
                }
            }
        }
    }

    /// Write a run with link `link` where the output is: out of a hyperlink
    /// that is not its own, in a hyperlink of its own if it has a link. A
    /// hyperlink left with nothing in it is not written.
    fn place(
        &mut self,
        link: &Option<Arc<Link>>,
        bytes: &str,
        w: &str,
        opener: &mut dyn FnMut(&Link) -> String,
    ) {
        // Inside a hyperlink that is not its own: step out of it.
        let in_suspended = self.suspended.iter().rposition(|w| w.link.is_some());
        let in_open = self.open.iter().rposition(|w| w.link.is_some());
        let mut wrap = link.is_some();
        match (in_suspended, in_open) {
            (Some(i), _) => {
                if &self.suspended[i].link == link {
                    self.reopen();
                    wrap = false;
                }
            }
            (None, Some(i)) => {
                if &self.open[i].link == link {
                    wrap = false;
                } else {
                    let popped: Vec<Wrapper> = self.open.drain(i..).collect();
                    for wrapper in popped.iter().rev() {
                        if self.out.len() == wrapper.body_at {
                            self.out.truncate(wrapper.out_at);
                        } else {
                            self.out.push_str(&format!("</{}>", wrapper.name));
                        }
                    }
                    self.suspended.splice(0..0, popped);
                }
            }
            (None, None) => {}
        }
        match (wrap, link) {
            (true, Some(link)) => {
                self.out.push_str(&opener(link));
                self.out.push_str(bytes);
                self.out.push_str(&format!("</{w}hyperlink>"));
            }
            _ => self.out.push_str(bytes),
        }
    }
}

// ---------------------------------------------------------------------------
// Namespaces, lists
// ---------------------------------------------------------------------------

/// Declare a namespace on the root element.
fn add_namespace(text: &str, prefix: &str, uri: &str) -> String {
    let mut at = 0;
    while let Some(i) = text[at..].find('<') {
        let start = at + i;
        let next = text.as_bytes().get(start + 1).copied().unwrap_or(b'>');
        if next.is_ascii_alphabetic() || next == b'_' {
            let Some(end) = text[start..].find('>') else {
                break;
            };
            let end = start + end;
            let insert = if text[..end].ends_with('/') {
                end - 1
            } else {
                end
            };
            return format!(
                "{} xmlns:{prefix}=\"{uri}\"{}",
                &text[..insert],
                &text[insert..]
            );
        }
        at = start + 1;
    }
    text.to_owned()
}

fn format_name(format: &NumberFormat) -> String {
    match format {
        NumberFormat::Decimal => "decimal",
        NumberFormat::DecimalZero => "decimalZero",
        NumberFormat::LowerLetter => "lowerLetter",
        NumberFormat::UpperLetter => "upperLetter",
        NumberFormat::LowerRoman => "lowerRoman",
        NumberFormat::UpperRoman => "upperRoman",
        NumberFormat::Bullet => "bullet",
        NumberFormat::None => "none",
        NumberFormat::DecimalEnclosedCircle => "decimalEnclosedCircle",
        NumberFormat::DecimalFullWidth => "decimalFullWidth",
        NumberFormat::ChineseCounting => "chineseCounting",
        NumberFormat::ChineseLegal => "chineseLegalSimplified",
        NumberFormat::IdeographDigital => "ideographDigital",
        NumberFormat::IdeographTraditional => "ideographTraditional",
        NumberFormat::IdeographZodiac => "ideographZodiac",
        NumberFormat::Aiueo => "aiueoFullWidth",
        NumberFormat::Iroha => "irohaFullWidth",
        NumberFormat::Ordinal => "ordinal",
        NumberFormat::Other(name) => name,
    }
    .to_owned()
}

fn level_xml(index: usize, level: &ListLevel) -> String {
    let suffix = match level.suffix {
        Suffix::Tab => "",
        Suffix::Space => "<w:suff w:val=\"space\"/>",
        Suffix::Nothing => "<w:suff w:val=\"nothing\"/>",
    };
    let legal = if level.legal { "<w:isLgl/>" } else { "" };
    let jc = match level.align {
        Align::Center => "center",
        Align::Right => "right",
        _ => "left",
    };
    let indent = if level.first_line < 0 {
        format!(
            "<w:ind w:left=\"{}\" w:hanging=\"{}\"/>",
            level.indent_left, -level.first_line
        )
    } else {
        format!(
            "<w:ind w:left=\"{}\" w:firstLine=\"{}\"/>",
            level.indent_left, level.first_line
        )
    };
    format!(
        "<w:lvl w:ilvl=\"{index}\"><w:start w:val=\"{}\"/><w:numFmt w:val=\"{}\"/>{legal}{suffix}<w:lvlText w:val=\"{}\"/><w:lvlJc w:val=\"{jc}\"/><w:pPr>{indent}</w:pPr></w:lvl>",
        level.start,
        escape_attribute(&format_name(&level.format)),
        escape_attribute(&level.text)
    )
}

/// The `abstractNum` and `num` elements for new lists; the abstract
/// numbers are `free` where the file already has them.
fn list_elements(text: &str, lists: &[&List]) -> (String, String) {
    let mut taken: Vec<u32> = find_all(text, "abstractNum")
        .iter()
        .filter_map(|s| attribute(&text[s.start..s.end], "w:abstractNumId"))
        .filter_map(|v| v.parse().ok())
        .collect();
    let mut abstracts = String::new();
    let mut nums = String::new();
    let mut assigned: HashMap<u32, u32> = HashMap::new();
    for list in lists {
        let id = *assigned.entry(list.definition).or_insert_with(|| {
            let id = if taken.contains(&list.definition) {
                taken.iter().max().map_or(0, |m| m + 1)
            } else {
                list.definition
            };
            taken.push(id);
            abstracts.push_str(&format!(
                "<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>"
            ));
            for (i, level) in list.levels.iter().enumerate() {
                abstracts.push_str(&level_xml(i, level));
            }
            abstracts.push_str("</w:abstractNum>");
            id
        });
        nums.push_str(&format!(
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{id}\"/></w:num>",
            list.id
        ));
    }
    (abstracts, nums)
}

fn numbering_new(lists: &[&List]) -> String {
    let (abstracts, nums) = list_elements("", lists);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{abstracts}{nums}</w:numbering>"
    )
}

/// `numbering.xml` with the new lists added: abstract definitions after the
/// last one, lists after the last list.
fn numbering_with(text: &str, lists: &[&List]) -> String {
    let (abstracts, nums) = list_elements(text, lists);
    let root = text
        .match_indices('<')
        .map(|(i, _)| i)
        .find(|&i| text[i + 1..].starts_with(|c: char| c.is_ascii_alphabetic()))
        .unwrap_or(0);
    let (root_open, _) = inner(&text[root..]);
    let root_open = root + root_open;
    let end = text.rfind("</").unwrap_or(text.len());
    let abstract_at = find_all(text, "abstractNum")
        .last()
        .map_or(root_open, |s| s.end);
    let num_at = find_all(text, "num")
        .last()
        .map_or(end.max(abstract_at), |s| s.end)
        .max(abstract_at);
    let mut out = String::with_capacity(text.len() + abstracts.len() + nums.len());
    out.push_str(&text[..abstract_at]);
    out.push_str(&abstracts);
    out.push_str(&text[abstract_at..num_at]);
    out.push_str(&nums);
    out.push_str(&text[num_at..]);
    out
}
