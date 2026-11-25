//! Notes written into a workbook: the comments part (who and what) and
//! the VML drawing Excel draws their boxes from.
//!
//! A sheet whose notes changed gets its comments part written again from
//! the model; a note the file already had, unchanged, keeps its bytes (its
//! bold author line, its fonts). Threaded comments are not changed here:
//! their notes stay as the file has them. In the VML drawing, the box of
//! a note that went is taken out and a new note gets a box of Excel's
//! default size; the other shapes keep their bytes. A sheet without notes
//! before gets both parts, their relationships, their content types and a
//! `<legacyDrawing>`. Inserted and deleted rows and columns move the
//! notes' cells and boxes ([`move_notes`], [`move_boxes`]).

use std::collections::{BTreeMap, HashMap};

use aster_sheets::structure::Lines;
use aster_sheets::{CellRef, Comment};

use super::{children, escape, set_attribute, split_bom, start_tag, top_level};
use crate::error::Result;
use crate::package::Package;

const COMMENTS_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml";
const VML_TYPE: &str = "application/vnd.openxmlformats-officedocument.vmlDrawing";
const REL_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

/// What writing a sheet's notes changes outside the sheet's own part.
#[derive(Default)]
pub(super) struct Parts {
    /// Parts written again (comments, drawing, the sheet's relationships).
    pub replaced: Vec<(String, Vec<u8>)>,
    /// New parts.
    pub added: Vec<(String, Vec<u8>)>,
    /// Content types to add: `(true, extension, type)` for a default,
    /// `(false, part, type)` for an override.
    pub types: Vec<(bool, String, String)>,
}

/// Write the notes of a sheet: its part is `part` and its XML so far
/// `sheet` (returned, with a `<legacyDrawing>` if one had to be added).
/// `current` reads a part as this save has it so far; `taken` says
/// whether a part name is used (in the package or added by this save).
pub(super) fn write(
    package: &mut Package,
    current: &dyn Fn(&mut Package, &str) -> Result<Vec<u8>>,
    taken: &dyn Fn(&Package, &str) -> bool,
    part: &str,
    sheet: Vec<u8>,
    comments: &BTreeMap<CellRef, Comment>,
    out: &mut Parts,
) -> Result<Vec<u8>> {
    let rels_part = crate::package::rels_name(part);
    let rels = if package.has(&rels_part) {
        package.relationships(part)?
    } else {
        Vec::new()
    };
    let folder = part.rsplit_once('/').map_or("", |(dir, _)| dir);
    let base = folder.rsplit_once('/').map_or("", |(dir, _)| dir);
    let target_of = |kind: &str| {
        rels.iter()
            .find(|rel| rel.kind == kind)
            .and_then(|rel| rel.target.clone())
            .filter(|target| package.has(target))
    };
    let notes_part = target_of("comments");
    let boxes_part = target_of("vmlDrawing");
    let mut new_rels = String::new();
    let mut rels_bytes = if package.has(&rels_part) {
        Some(current(package, &rels_part)?)
    } else {
        None
    };
    let mut next_id = rels_bytes
        .as_deref()
        .map_or(1, super::sheets::free_rel_number);

    // The comments part.
    match notes_part {
        Some(target) => {
            let old = current(package, &target)?;
            let new = comments_xml(Some(&old), &target, comments)?;
            out.replaced.push((target, new));
        }
        None => {
            let target = free_name(package, taken, &format!("{base}/comments"), ".xml");
            let new = comments_xml(None, &target, comments)?;
            let relative = relative_to(folder, &target);
            new_rels.push_str(&format!(
                r#"<Relationship Id="rId{next_id}" Type="{REL_BASE}/comments" Target="{relative}"/>"#
            ));
            next_id += 1;
            out.types
                .push((false, format!("/{target}"), COMMENTS_TYPE.to_owned()));
            out.added.push((target, new));
        }
    }

    // The boxes.
    let mut sheet = sheet;
    match boxes_part {
        Some(target) => {
            let old = current(package, &target)?;
            out.replaced.push((target, boxes(&old, comments, 1)));
        }
        None => {
            let target = free_name(
                package,
                taken,
                &format!("{base}/drawings/vmlDrawing"),
                ".vml",
            );
            let number: u32 = target
                .rsplit_once("vmlDrawing")
                .and_then(|(_, n)| n.trim_end_matches(".vml").parse().ok())
                .unwrap_or(1);
            let blank = format!(
                r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel"><o:shapelayout v:ext="edit"><o:idmap v:ext="edit" data="{number}"/></o:shapelayout><v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype></xml>"#
            );
            let id = format!("rId{next_id}");
            let relative = relative_to(folder, &target);
            new_rels.push_str(&format!(
                r#"<Relationship Id="{id}" Type="{REL_BASE}/vmlDrawing" Target="{relative}"/>"#
            ));
            out.types
                .push((true, "vml".to_owned(), VML_TYPE.to_owned()));
            out.added
                .push((target, boxes(blank.as_bytes(), comments, number)));
            sheet = legacy_drawing(&sheet, &id);
        }
    }

    if !new_rels.is_empty() {
        match rels_bytes.take() {
            Some(bytes) => out
                .replaced
                .push((rels_part, super::sheets::append_to_root(&bytes, &new_rels))),
            None => {
                let rels = format!(
                    "{}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{new_rels}</Relationships>",
                    super::XML_DECLARATION
                );
                out.added.push((rels_part, rels.into_bytes()));
            }
        }
    }
    Ok(sheet)
}

/// The first `{stem}{n}{extension}` not taken.
fn free_name(
    package: &Package,
    taken: &dyn Fn(&Package, &str) -> bool,
    stem: &str,
    extension: &str,
) -> String {
    (1..)
        .map(|n| format!("{}{n}{extension}", stem.trim_start_matches('/')))
        .find(|name| !taken(package, name))
        .unwrap_or_default()
}

/// A part's name relative to a folder (both from the package root).
fn relative_to(folder: &str, target: &str) -> String {
    let from: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_owned(); from.len() - common];
    parts.extend(to[common..].iter().map(|s| (*s).to_owned()));
    parts.join("/")
}

/// The comments part for `comments`. `old` is the file's: its authors
/// stay first, and its notes that did not change keep their bytes.
fn comments_xml(
    old: Option<&[u8]>,
    part: &str,
    comments: &BTreeMap<CellRef, Comment>,
) -> Result<Vec<u8>> {
    let mut authors: Vec<String> = Vec::new();
    let mut kept: HashMap<CellRef, String> = HashMap::new();
    let mut root = format!(r#"<comments xmlns="{MAIN}">"#);
    let mut prefix = String::new();
    if let Some(old) = old {
        let (_, xml) = split_bom(old);
        let text = String::from_utf8_lossy(xml).into_owned();
        let read: HashMap<CellRef, Comment> = crate::xlsx::notes(old, part)?.into_iter().collect();
        for (name, start, end) in top_level(xml) {
            let element = &text[start..end];
            match name.as_str() {
                "authors" => {
                    let (tag, _) = start_tag(element);
                    let name = tag[1..].split([' ', '>', '/']).next().unwrap_or("");
                    prefix = name
                        .rsplit_once(':')
                        .map_or(String::new(), |(p, _)| format!("{p}:"));
                    for (_, raw) in children(element) {
                        authors.push(super::unescape(&inner_text(&raw)));
                    }
                }
                "commentList" => {
                    for (_, raw) in children(element) {
                        let (tag, _) = start_tag(&raw);
                        let Some(at) = super::attribute(tag.as_bytes(), "ref")
                            .as_deref()
                            .and_then(CellRef::parse)
                        else {
                            continue;
                        };
                        let same = match (comments.get(&at), read.get(&at)) {
                            (Some(now), _) if now.threaded => true,
                            (Some(now), Some(was)) => {
                                now.text == was.text && now.author == was.author
                            }
                            _ => false,
                        };
                        if same {
                            kept.insert(at, raw);
                        }
                    }
                }
                _ => {}
            }
        }
        // The root as the file wrote it, with its namespaces.
        if let Some(at) = text.find(&format!("<{prefix}comments"))
            && let Some(len) = text[at..].find('>')
        {
            root = text[at..=at + len].to_owned();
        }
    }
    let p = prefix.as_str();
    let mut list = String::new();
    for (at, comment) in comments {
        if let Some(raw) = kept.get(at) {
            list.push_str(raw);
            continue;
        }
        if comment.threaded {
            continue;
        }
        let author = match authors.iter().position(|a| *a == comment.author) {
            Some(i) => i,
            None => {
                authors.push(comment.author.clone());
                authors.len() - 1
            }
        };
        list.push_str(&format!(
            r#"<{p}comment ref="{at}" authorId="{author}" shapeId="0"><{p}text><{p}r><{p}rPr><{p}sz val="9"/><{p}color indexed="81"/><{p}rFont val="Tahoma"/><{p}family val="2"/></{p}rPr><{p}t xml:space="preserve">{}</{p}t></{p}r></{p}text></{p}comment>"#,
            escape(&comment.text)
        ));
    }
    let authors: String = authors
        .iter()
        .map(|a| format!("<{p}author>{}</{p}author>", escape(a)))
        .collect();
    let root = if root.ends_with("/>") {
        format!("{}>", root.trim_end_matches("/>").trim_end())
    } else {
        root
    };
    Ok(format!(
        "{}{root}<{p}authors>{authors}</{p}authors><{p}commentList>{list}</{p}commentList></{p}comments>",
        super::XML_DECLARATION
    )
    .into_bytes())
}

/// The text inside an element, tags left out.
fn inner_text(raw: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// A note's box in the VML drawing: its shape's span in the text and the
/// cell it is for.
struct Shape {
    start: usize,
    end: usize,
    at: Option<CellRef>,
}

/// The shapes of a VML drawing; `at` for those that are notes.
fn shapes(text: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("<v:shape") {
        let start = from + found;
        // Not `<v:shapetype` nor `<v:shapelayout`.
        let next = text[start + "<v:shape".len()..].chars().next();
        if !matches!(next, Some(' ' | '>' | '\n' | '\r' | '\t')) {
            from = start + 1;
            continue;
        }
        let Some(close) = text[start..].find("</v:shape>") else {
            break;
        };
        let end = start + close + "</v:shape>".len();
        let body = &text[start..end];
        let number = |name: &str| -> Option<u32> {
            let open = format!("<x:{name}>");
            let at = body.find(&open)? + open.len();
            let len = body[at..].find('<')?;
            body[at..at + len].trim().parse().ok()
        };
        let note = body.contains("ObjectType=\"Note\"");
        let at = match (note, number("Row"), number("Column")) {
            (true, Some(row), Some(col)) => Some(CellRef::new(row, col)),
            _ => None,
        };
        out.push(Shape { start, end, at });
        from = end;
    }
    out
}

/// A VML drawing with a box for each comment and none for cells without
/// one: boxes that stay keep their bytes. `block` is the drawing's shape
/// id block (`o:idmap data`), for new shapes' ids.
fn boxes(bytes: &[u8], comments: &BTreeMap<CellRef, Comment>, block: u32) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let text = String::from_utf8_lossy(xml).into_owned();
    let found = shapes(&text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut has: Vec<CellRef> = Vec::new();
    for shape in &found {
        let Some(cell) = shape.at else {
            continue;
        };
        if comments.contains_key(&cell) && !has.contains(&cell) {
            has.push(cell);
            continue;
        }
        // A box for a note that went.
        out.push_str(&text[at..shape.start]);
        at = shape.end;
    }
    out.push_str(&text[at..]);
    // Shape ids go on from the largest the drawing has.
    let first_id = text
        .match_indices("_x0000_s")
        .filter_map(|(i, m)| {
            let digits: String = text[i + m.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(block * 1024)
        + 1;
    let mut new = String::new();
    for (n, cell) in comments
        .keys()
        .filter(|cell| !has.contains(cell))
        .enumerate()
    {
        let next = first_id + n as u32;
        let (row, col) = (cell.row, cell.col);
        new.push_str(&format!(
            r##"<v:shape id="_x0000_s{next}" type="#_x0000_t202" style="position:absolute;margin-left:59.25pt;margin-top:1.5pt;width:108pt;height:59.25pt;z-index:{z};visibility:hidden" fillcolor="#ffffe1" o:insetmode="auto"><v:fill color2="#ffffe1"/><v:shadow on="t" color="black" obscured="t"/><v:path o:connecttype="none"/><v:textbox style="mso-direction-alt:auto"><div style="text-align:left"></div></v:textbox><x:ClientData ObjectType="Note"><x:MoveWithCells/><x:SizeWithCells/><x:Anchor>{left}, 15, {top}, 2, {right}, 15, {bottom}, 16</x:Anchor><x:AutoFill>False</x:AutoFill><x:Row>{row}</x:Row><x:Column>{col}</x:Column></x:ClientData></v:shape>"##,
            z = next % 1024,
            left = col + 1,
            top = row.saturating_sub(1),
            right = col + 3,
            bottom = row + 3,
        ));
    }
    if !new.is_empty() {
        let close = out.rfind("</xml>").unwrap_or(out.len());
        out.insert_str(close, &new);
    }
    [bom, out.as_bytes()].concat()
}

/// The worksheet children that come after `<legacyDrawing>`.
const AFTER_LEGACY: [&str; 8] = [
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

/// A worksheet with a `<legacyDrawing>` naming relationship `id`.
fn legacy_drawing(bytes: &[u8], id: &str) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let text = String::from_utf8_lossy(xml).into_owned();
    let children = top_level(xml);
    let p = children
        .iter()
        .find(|(name, ..)| name == "sheetData")
        .map(|(_, start, _)| {
            let (_, name) = start_tag(&text[*start..]);
            name.rsplit_once(':')
                .map_or(String::new(), |(p, _)| format!("{p}:"))
        })
        .unwrap_or_default();
    // The root's prefix for relationships, or a declaration of our own.
    let root_end = text.find('>').unwrap_or(0);
    let mut root = text[..root_end].to_owned();
    let decl = format!("=\"{REL_BASE}\"");
    let element = match root.find(&decl).and_then(|at| {
        let before = &root[..at];
        let name_at = before.rfind("xmlns:")? + "xmlns:".len();
        Some(before[name_at..].to_owned())
    }) {
        Some(r) => format!("<{p}legacyDrawing {r}:id=\"{id}\"/>"),
        None => {
            root.clear();
            format!("<{p}legacyDrawing xmlns:r=\"{REL_BASE}\" r:id=\"{id}\"/>")
        }
    };
    let at = children
        .iter()
        .find(|(name, ..)| AFTER_LEGACY.contains(&name.as_str()))
        .map(|(_, start, _)| *start)
        .unwrap_or_else(|| text.rfind("</").unwrap_or(text.len()));
    [bom, &xml[..at], element.as_bytes(), &xml[at..]].concat()
}

/// A comments part (or threaded comments part) with its comments' cells
/// moved; those on deleted cells go.
pub(super) fn move_notes(bytes: &[u8], changes: &[Lines]) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let mut text = String::from_utf8_lossy(xml).into_owned();
    for lines in changes {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (start, end, element) in elements_with_ref(&text) {
            out.push_str(&text[at..start]);
            let (tag, _) = start_tag(&element);
            let moved = super::attribute(tag.as_bytes(), "ref")
                .as_deref()
                .and_then(CellRef::parse)
                .map(|cell| moved_cell(cell, lines));
            match moved {
                Some(Some(cell)) => {
                    out.push_str(&set_attribute(tag, "ref", &cell.to_string()));
                    out.push_str(&element[tag.len()..]);
                }
                // On a deleted cell.
                Some(None) => {}
                None => out.push_str(&element),
            }
            at = end;
        }
        out.push_str(&text[at..]);
        text = out;
    }
    [bom, text.as_bytes()].concat()
}

/// The `<comment>` and `<threadedComment>` elements of a part: start, end
/// and text.
fn elements_with_ref(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for (name, start, end) in top_level(text.as_bytes()) {
        let element = &text[start..end];
        match name.as_str() {
            "commentList" => {
                // Its children, found again in the text.
                let mut from = start;
                for (_, raw) in children(element) {
                    if let Some(found) = text[from..end].find(&raw) {
                        let at = from + found;
                        out.push((at, at + raw.len(), raw.clone()));
                        from = at + raw.len();
                    }
                }
            }
            "threadedComment" => out.push((start, end, element.to_owned())),
            _ => {}
        }
    }
    out
}

fn moved_cell(cell: CellRef, lines: &Lines) -> Option<CellRef> {
    Some(if lines.columns {
        CellRef::new(cell.row, lines.moved(cell.col)?)
    } else {
        CellRef::new(lines.moved(cell.row)?, cell.col)
    })
}

/// A VML drawing with its notes' boxes moved with their cells: the cell
/// they belong to and their anchor. A box whose cell is deleted goes.
pub(super) fn move_boxes(bytes: &[u8], changes: &[Lines]) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let mut text = String::from_utf8_lossy(xml).into_owned();
    for lines in changes {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for shape in shapes(&text) {
            let Some(cell) = shape.at else {
                continue;
            };
            out.push_str(&text[at..shape.start]);
            let body = &text[shape.start..shape.end];
            if let Some(moved) = moved_cell(cell, lines) {
                out.push_str(&moved_box(body, moved, lines));
            }
            at = shape.end;
        }
        out.push_str(&text[at..]);
        text = out;
    }
    [bom, text.as_bytes()].concat()
}

/// A note's shape for its cell's new place: `Row`, `Column` and the
/// anchor's lines moved (a line deleted under it becomes the first line
/// after the deleted ones).
fn moved_box(body: &str, cell: CellRef, lines: &Lines) -> String {
    let mut out = body.to_owned();
    let set = |out: &mut String, name: &str, value: String| {
        let open = format!("<x:{name}>");
        if let Some(at) = out.find(&open).map(|i| i + open.len())
            && let Some(len) = out[at..].find('<')
        {
            out.replace_range(at..at + len, &value);
        }
    };
    set(&mut out, "Row", cell.row.to_string());
    set(&mut out, "Column", cell.col.to_string());
    let open = "<x:Anchor>";
    if let Some(at) = out.find(open).map(|i| i + open.len())
        && let Some(len) = out[at..].find('<')
    {
        let numbers: Vec<Option<u32>> = out[at..at + len]
            .split(',')
            .map(|n| n.trim().parse().ok())
            .collect();
        if numbers.len() == 8 && numbers.iter().all(Option::is_some) {
            let mut numbers: Vec<u32> = numbers.into_iter().flatten().collect();
            let moved = |line: u32| lines.moved(line).unwrap_or(lines.at);
            let which = if lines.columns { [0, 4] } else { [2, 6] };
            for i in which {
                numbers[i] = moved(numbers[i]);
            }
            let anchor = numbers
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            out.replace_range(at..at + len, &anchor);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_are_named_relative_to_the_sheet() {
        assert_eq!(
            relative_to("xl/worksheets", "xl/comments1.xml"),
            "../comments1.xml"
        );
        assert_eq!(
            relative_to("xl/worksheets", "xl/drawings/vmlDrawing2.vml"),
            "../drawings/vmlDrawing2.vml"
        );
    }

    #[test]
    fn boxes_are_kept_dropped_added_and_moved() {
        let vml = r##"<xml xmlns:v="urn:schemas-microsoft-com:vml"><v:shapetype id="_x0000_t202"></v:shapetype><v:shape id="_x0000_s1025" type="#_x0000_t202"><x:ClientData ObjectType="Note"><x:Anchor>2, 15, 1, 2, 4, 31, 5, 1</x:Anchor><x:Row>1</x:Row><x:Column>1</x:Column></x:ClientData></v:shape><v:shape id="_x0000_s1026"><x:ClientData ObjectType="Note"><x:Row>9</x:Row><x:Column>0</x:Column></x:ClientData></v:shape></xml>"##;
        let mut comments = BTreeMap::new();
        comments.insert(CellRef::new(1, 1), Comment::note("a", "b"));
        comments.insert(CellRef::new(4, 2), Comment::note("a", "c"));
        let out = String::from_utf8(boxes(vml.as_bytes(), &comments, 1)).unwrap();
        assert!(out.contains("_x0000_s1025"), "B2's box stays");
        assert!(!out.contains("<x:Row>9</x:Row>"), "A10's goes: {out}");
        assert!(out.contains(r#"<v:shape id="_x0000_s1027""#), "{out}");
        assert!(
            out.contains("<x:Row>4</x:Row><x:Column>2</x:Column>"),
            "{out}"
        );
        let lines = Lines {
            columns: false,
            at: 0,
            count: 2,
            insert: true,
        };
        let moved = String::from_utf8(move_boxes(out.as_bytes(), &[lines])).unwrap();
        assert!(
            moved.contains("<x:Anchor>2, 15, 3, 2, 4, 31, 7, 1</x:Anchor>"),
            "{moved}"
        );
        assert!(
            moved.contains("<x:Row>3</x:Row><x:Column>1</x:Column>"),
            "{moved}"
        );
        assert!(
            moved.contains("<x:Row>6</x:Row><x:Column>2</x:Column>"),
            "{moved}"
        );
        let gone = Lines {
            columns: true,
            at: 1,
            count: 1,
            insert: false,
        };
        let moved = String::from_utf8(move_boxes(moved.as_bytes(), &[gone])).unwrap();
        assert!(
            !moved.contains("_x0000_s1025"),
            "B's box goes with column B"
        );
        assert!(
            moved.contains("<x:Row>6</x:Row><x:Column>1</x:Column>"),
            "{moved}"
        );
    }
}
