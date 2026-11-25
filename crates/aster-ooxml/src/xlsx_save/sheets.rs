//! Sheets added, renamed and deleted, written into `workbook.xml`, its
//! relationships, the content types and the parts that name sheets.
//!
//! The `<sheet>` entries of sheets that stay keep their bytes (with a new
//! `name` when renamed); a deleted sheet's part and relationships go, and
//! its entry, its local names and its references become `#REF!`; an added
//! sheet gets a new part, relationship and content type. The parts a
//! deleted sheet used (its drawing, comments) stay in the package,
//! unreferenced, as Excel leaves stale parts alone too.

use aster_sheets::sheets::retarget;

use super::structure::{map_attributes, map_texts};
use super::{children, escape, set_attribute, split_bom, start_tag, top_level, unescape};

/// Which sheet of the file each sheet of the workbook is.
pub(super) struct Plan {
    /// By workbook sheet: its index in the file, `None` for a new one.
    pub origins: Vec<Option<usize>>,
    /// The file's sheet names, in its order.
    pub file_names: Vec<String>,
    /// The workbook's sheet names, in its order.
    pub names: Vec<String>,
}

impl Plan {
    /// Whether any sheet was added, deleted or renamed.
    pub fn changed(&self) -> bool {
        self.origins.len() != self.file_names.len()
            || self.origins.iter().enumerate().any(|(i, o)| *o != Some(i))
            || !self.renames().is_empty()
    }

    /// Where each sheet of the file is now; `None` if deleted.
    pub fn now(&self) -> Vec<Option<usize>> {
        let mut out = vec![None; self.file_names.len()];
        for (index, origin) in self.origins.iter().enumerate() {
            if let Some(f) = origin
                && let Some(slot) = out.get_mut(*f)
            {
                *slot = Some(index);
            }
        }
        out
    }

    pub fn deleted(&self) -> Vec<usize> {
        self.now()
            .iter()
            .enumerate()
            .filter(|(_, now)| now.is_none())
            .map(|(f, _)| f)
            .collect()
    }

    /// Old and new names of the sheets renamed.
    pub fn renames(&self) -> Vec<(String, String)> {
        self.origins
            .iter()
            .zip(&self.names)
            .filter_map(|(origin, name)| {
                let old = self.file_names.get((*origin)?)?;
                (old != name).then(|| (old.clone(), name.clone()))
            })
            .collect()
    }

    /// A formula of the file with references to deleted sheets `#REF!`
    /// and renamed sheets under their new names. Renames go through
    /// stand-ins, so two sheets can swap names.
    pub fn formula(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for f in self.deleted() {
            out = retarget(&out, &self.file_names[f], None);
        }
        let renames = self.renames();
        for (i, (old, _)) in renames.iter().enumerate() {
            out = retarget(&out, old, Some(&stand_in(i)));
        }
        for (i, (_, new)) in renames.iter().enumerate() {
            out = retarget(&out, &stand_in(i), Some(new));
        }
        out
    }

    /// The index a sheet tab number of the file (`activeTab`) has now: the
    /// sheet's own, or the next one's if it is gone.
    fn tab(&self, f: usize) -> usize {
        let before = self.now()[..f.min(self.file_names.len())]
            .iter()
            .filter(|n| n.is_some())
            .count();
        before.min(self.origins.len().saturating_sub(1))
    }
}

fn stand_in(i: usize) -> String {
    format!("\u{1}{i}")
}

/// The namespace URIs of relationship ids, Transitional and Strict.
const REL_NAMESPACES: [&str; 2] = [
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "http://purl.oclc.org/ooxml/officeDocument/relationships",
];

/// `workbook.xml` for the plan. `added` are the new sheets: their index in
/// the workbook and relationship id.
pub(super) fn workbook_xml(bytes: &[u8], plan: &Plan, added: &[(usize, String)]) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let text = String::from_utf8_lossy(xml).into_owned();
    // The prefix bound to the relationships namespace, if any.
    let rel_prefix = REL_NAMESPACES.iter().find_map(|uri| {
        let pattern = format!("=\"{uri}\"");
        let at = text.find(&pattern)?;
        let declared = &text[..at];
        let start = declared.rfind("xmlns:")? + "xmlns:".len();
        Some(declared[start..].to_owned())
    });
    let mut ops: Vec<(usize, usize, String)> = Vec::new();
    for (child, start, end) in top_level(xml) {
        let element = &text[start..end];
        let new = match child.as_str() {
            "sheets" => Some(sheets(element, plan, added, rel_prefix.as_deref())),
            "definedNames" => Some(defined_names(element, plan)),
            "bookViews" => Some(map_attributes(element, &["activeTab", "firstSheet"], |v| {
                let f: usize = v.parse().ok()?;
                Some(plan.tab(f).to_string())
            })),
            _ => None,
        };
        if let Some(new) = new
            && new != element
        {
            ops.push((start, end, new));
        }
    }
    let mut out = Vec::with_capacity(bytes.len() + 256);
    out.extend_from_slice(bom);
    let mut at = 0;
    for (start, end, new) in ops {
        out.extend_from_slice(&xml[at..start]);
        out.extend_from_slice(new.as_bytes());
        at = end;
    }
    out.extend_from_slice(&xml[at..]);
    out
}

fn sheets(
    element: &str,
    plan: &Plan,
    added: &[(usize, String)],
    rel_prefix: Option<&str>,
) -> String {
    let (tag, name) = start_tag(element);
    let p = name
        .rsplit_once(':')
        .map_or(String::new(), |(p, _)| format!("{p}:"));
    let raw: Vec<String> = children(element)
        .into_iter()
        .filter(|(name, _)| name == "sheet")
        .map(|(_, raw)| raw)
        .collect();
    let mut next_id = raw
        .iter()
        .filter_map(|r| super::attribute(start_tag(r).0.as_bytes(), "sheetId"))
        .filter_map(|v| v.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut items = String::new();
    for (index, origin) in plan.origins.iter().enumerate() {
        let name = escape(&plan.names[index]);
        match origin.and_then(|f| raw.get(f)) {
            Some(raw) => {
                let (open, _) = start_tag(raw);
                let renamed = super::attribute(open.as_bytes(), "name")
                    .is_none_or(|v| unescape(&v) != plan.names[index]);
                if renamed {
                    items.push_str(&set_attribute(open, "name", &name));
                    items.push_str(&raw[open.len()..]);
                } else {
                    items.push_str(raw);
                }
            }
            None => {
                let Some((_, rel)) = added.iter().find(|(i, _)| *i == index) else {
                    continue;
                };
                let id = match rel_prefix {
                    Some(r) => format!("{r}:id=\"{rel}\""),
                    None => format!("xmlns:r=\"{}\" r:id=\"{rel}\"", REL_NAMESPACES[0]),
                };
                items.push_str(&format!(
                    "<{p}sheet name=\"{name}\" sheetId=\"{next_id}\" {id}/>"
                ));
                next_id += 1;
            }
        }
    }
    let close = format!("</{name}>");
    let open = tag.trim_end_matches("/>").trim_end_matches('>');
    format!("{open}>{items}{close}")
}

fn defined_names(element: &str, plan: &Plan) -> String {
    let (tag, name) = start_tag(element);
    if tag.ends_with("/>") {
        return element.to_owned();
    }
    let now = plan.now();
    let mut kept = Vec::new();
    for (child, raw) in children(element) {
        if child != "definedName" {
            kept.push(raw);
            continue;
        }
        let (open, _) = start_tag(&raw);
        let mut open = open.to_owned();
        if let Some(local) = super::attribute(open.as_bytes(), "localSheetId")
            && let Ok(f) = local.parse::<usize>()
        {
            match now.get(f).copied().flatten() {
                // A name of a deleted sheet goes with it.
                None => continue,
                Some(index) if index != f => {
                    open = set_attribute(&open, "localSheetId", &index.to_string());
                }
                Some(_) => {}
            }
        }
        let body = &raw[start_tag(&raw).0.len()..];
        let close_at = body.rfind("</").unwrap_or(body.len());
        let written = unescape(&body[..close_at]);
        let formula = plan.formula(&written);
        let body = if formula == written {
            body.to_owned()
        } else {
            format!("{}{}", escape(&formula), &body[close_at..])
        };
        kept.push(format!("{open}{body}"));
    }
    if kept.is_empty() {
        return String::new();
    }
    format!("{tag}{}</{name}>", kept.concat())
}

/// A chart part with the sheet names in its data references changed.
pub(super) fn chart_xml(bytes: &[u8], plan: &Plan) -> Option<Vec<u8>> {
    let (bom, xml) = split_bom(bytes);
    let text = std::str::from_utf8(xml).ok()?;
    let new = map_texts(text, &["f"], |f| plan.formula(f));
    (new != text).then(|| [bom, new.as_bytes()].concat())
}

/// `docProps/app.xml` with the worksheets' titles as they are now. Excel
/// lists them first in `TitlesOfParts`, counted by the first group of
/// `HeadingPairs`; when the file's list is that, it is written again with
/// the new names and count. Otherwise only renamed titles change.
/// `worksheets` are the worksheet names as the file had them and as they
/// are now.
pub(super) fn app_xml(
    bytes: &[u8],
    plan: &Plan,
    worksheets: &(Vec<String>, Vec<String>),
) -> Option<Vec<u8>> {
    let (bom, xml) = split_bom(bytes);
    let mut text = std::str::from_utf8(xml).ok()?.to_owned();
    let original = text.clone();
    if let Some(listed) = titles(&text, worksheets) {
        return (listed != original).then(|| [bom, listed.as_bytes()].concat());
    }
    for (old, new) in plan.renames() {
        for prefix in ["vt:", ""] {
            let from = format!("<{prefix}lpstr>{}</{prefix}lpstr>", escape(&old));
            if text.contains(&from) {
                let to = format!("<{prefix}lpstr>{}</{prefix}lpstr>", escape(&new));
                text = text.replacen(&from, &to, 1);
                break;
            }
        }
    }
    (text != original).then(|| [bom, text.as_bytes()].concat())
}

/// `app.xml` with the worksheet group of its titles rewritten, if the file
/// lists the worksheets as Excel does.
fn titles(text: &str, (old, new): &(Vec<String>, Vec<String>)) -> Option<String> {
    let pairs_at = text.find("HeadingPairs>")?;
    // The first group's count.
    let i4_open = text[pairs_at..].find("i4>")? + pairs_at + "i4>".len();
    let i4_close = i4_open + text[i4_open..].find('<')?;
    let count: usize = text[i4_open..i4_close].trim().parse().ok()?;
    if count != old.len() {
        return None;
    }
    let titles_at = text.find("TitlesOfParts>")?;
    let vector_tag = titles_at + text[titles_at..].find("vector")?;
    let vector_open = vector_tag + text[vector_tag..].find('>')? + 1;
    // The lpstr items: where each starts and ends, and its text.
    let (prefix, _) = text[vector_open..].split_once("lpstr>")?;
    let prefix = prefix.trim_start().trim_start_matches('<');
    let open = format!("<{prefix}lpstr>");
    let close = format!("</{prefix}lpstr>");
    let mut items = Vec::new();
    let mut at = vector_open;
    while items.len() < count {
        let start = at + text[at..].find(&open)?;
        let body = start + open.len();
        let end = body + text[body..].find(&close)?;
        items.push((start, end + close.len(), super::unescape(&text[body..end])));
        at = end + close.len();
    }
    if items.iter().map(|(_, _, t)| t).ne(old.iter()) {
        return None;
    }
    let first = items.first().map_or(vector_open, |(s, _, _)| *s);
    let last = items.last().map_or(vector_open, |(_, e, _)| *e);
    let listed: String = new
        .iter()
        .map(|name| format!("{open}{}{close}", escape(name)))
        .collect();
    let size_text = &text[vector_tag..vector_open];
    let size: usize = super::attribute(size_text.as_bytes(), "size")?
        .parse()
        .ok()?;
    let new_size = size + new.len() - count;
    let tag = set_attribute(size_text, "size", &new_size.to_string());
    let mut out = String::with_capacity(text.len() + 64);
    out.push_str(&text[..i4_open]);
    out.push_str(&new.len().to_string());
    out.push_str(&text[i4_close..vector_tag]);
    out.push_str(&tag);
    out.push_str(&text[vector_open..first]);
    out.push_str(&listed);
    out.push_str(&text[last..]);
    Some(out)
}

/// A worksheet with its frozen rows and columns: the first `<sheetView>`
/// loses its `<pane>` and selections and gets a frozen pane (if any) with
/// one selection in the pane that scrolls. A sheet without `<sheetViews>`
/// gets one where the schema puts it.
pub(super) fn frozen_xml(bytes: &[u8], (rows, cols): (u32, u32)) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let text = String::from_utf8_lossy(xml).into_owned();
    let children = top_level(xml);
    let root_prefix = children
        .iter()
        .find(|(name, ..)| name == "sheetData")
        .map(|(_, start, _)| {
            let (_, name) = start_tag(&text[*start..]);
            name.rsplit_once(':')
                .map_or(String::new(), |(p, _)| format!("{p}:"))
        })
        .unwrap_or_default();
    let p = root_prefix.as_str();
    let pane = if rows == 0 && cols == 0 {
        String::new()
    } else {
        let active = match (rows > 0, cols > 0) {
            (true, true) => "bottomRight",
            (true, false) => "bottomLeft",
            _ => "topRight",
        };
        let top_left = aster_sheets::CellRef::new(rows, cols).to_string();
        let mut pane = format!("<{p}pane");
        if cols > 0 {
            pane.push_str(&format!(" xSplit=\"{cols}\""));
        }
        if rows > 0 {
            pane.push_str(&format!(" ySplit=\"{rows}\""));
        }
        pane.push_str(&format!(
            " topLeftCell=\"{top_left}\" activePane=\"{active}\" state=\"frozen\"/><{p}selection pane=\"{active}\" activeCell=\"{top_left}\" sqref=\"{top_left}\"/>"
        ));
        pane
    };
    let found = children.iter().find(|(name, ..)| name == "sheetViews");
    let (start, end, new) = match found {
        Some(&(_, start, end)) => {
            let element = &text[start..end];
            let views = children_of_views(element, &pane);
            (start, end, views)
        }
        None => {
            let at = children
                .iter()
                .find(|(name, ..)| !matches!(name.as_str(), "sheetPr" | "dimension"))
                .map_or(xml.len(), |(_, start, _)| *start);
            (
                at,
                at,
                format!(
                    "<{p}sheetViews><{p}sheetView workbookViewId=\"0\">{pane}</{p}sheetView></{p}sheetViews>"
                ),
            )
        }
    };
    [bom, &xml[..start], new.as_bytes(), &xml[end..]].concat()
}

/// `<sheetViews>` with its first `<sheetView>`'s pane and selections
/// replaced by `pane`.
fn children_of_views(element: &str, pane: &str) -> String {
    let (tag, name) = start_tag(element);
    let mut out = String::from(tag);
    let mut first = true;
    for (child, raw) in children(element) {
        if child != "sheetView" || !first {
            out.push_str(&raw);
            continue;
        }
        first = false;
        let (open, view_name) = start_tag(&raw);
        if open.ends_with("/>") {
            out.push_str(open.trim_end_matches("/>").trim_end());
            out.push('>');
            out.push_str(pane);
            out.push_str(&format!("</{view_name}>"));
            continue;
        }
        out.push_str(open);
        // The pane and selections go first in a sheetView; what else it
        // holds (pivot selections) stays after them.
        out.push_str(pane);
        for (inner, inner_raw) in children(&raw) {
            if inner != "pane" && inner != "selection" {
                out.push_str(&inner_raw);
            }
        }
        out.push_str(&format!("</{view_name}>"));
    }
    out.push_str(&format!("</{name}>"));
    out
}

/// The worksheet children that come after `<autoFilter>`, in the schema's
/// order: a new filter goes before the first of them.
const AFTER_FILTER: [&str; 28] = [
    "sortState",
    "dataConsolidate",
    "customSheetViews",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

/// A worksheet with its `<autoFilter>` written from `filter`, or without
/// one. Columns the file filtered some other way keep their elements.
pub(super) fn filter_xml(bytes: &[u8], filter: Option<&aster_sheets::AutoFilter>) -> Vec<u8> {
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
    let found = children.iter().find(|(name, ..)| name == "autoFilter");
    let old = found.map(|&(_, start, end)| &text[start..end]);
    let new = filter.map_or_else(String::new, |filter| filter_element(filter, old, &p));
    let (start, end) = match found {
        Some(&(_, start, end)) => (start, end),
        None if new.is_empty() => return bytes.to_vec(),
        None => {
            let at = children
                .iter()
                .find(|(name, ..)| AFTER_FILTER.contains(&name.as_str()))
                .map(|(_, start, _)| *start)
                .unwrap_or_else(|| text.rfind("</").unwrap_or(text.len()));
            (at, at)
        }
    };
    [bom, &xml[..start], new.as_bytes(), &xml[end..]].concat()
}

/// `<autoFilter>` for a filter; `old` is the file's, whose columns
/// filtered some other way are kept, renumbered from the new left edge.
fn filter_element(filter: &aster_sheets::AutoFilter, old: Option<&str>, p: &str) -> String {
    let first = filter.range.first.col;
    let mut kept = std::collections::BTreeMap::new();
    if let Some(old) = old {
        let (tag, _) = start_tag(old);
        let edge = super::attribute(tag.as_bytes(), "ref")
            .as_deref()
            .and_then(aster_sheets::CellRange::parse)
            .map_or(first, |range| range.first.col);
        for (name, raw) in children(old) {
            if name != "filterColumn" {
                continue;
            }
            let (inner, _) = start_tag(&raw);
            let id = super::attribute(inner.as_bytes(), "colId")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            let col = edge + id;
            if filter.others.contains(&col) && col >= first {
                let id = (col - first).to_string();
                kept.insert(
                    col,
                    format!(
                        "{}{}",
                        set_attribute(inner, "colId", &id),
                        &raw[inner.len()..]
                    ),
                );
            }
        }
    }
    for (&col, shown) in &filter.columns {
        let mut column = format!("<{p}filterColumn colId=\"{}\"><{p}filters", col - first);
        if shown.blanks {
            column.push_str(" blank=\"1\"");
        }
        if shown.values.is_empty() {
            column.push_str("/>");
        } else {
            column.push('>');
            for value in &shown.values {
                column.push_str(&format!("<{p}filter val=\"{}\"/>", escape(value)));
            }
            column.push_str(&format!("</{p}filters>"));
        }
        column.push_str(&format!("</{p}filterColumn>"));
        kept.insert(col, column);
    }
    let range = filter.range.to_string();
    if kept.is_empty() {
        return format!("<{p}autoFilter ref=\"{range}\"/>");
    }
    let columns: String = kept.into_values().collect();
    format!("<{p}autoFilter ref=\"{range}\">{columns}</{p}autoFilter>")
}

/// Text inserted before the closing tag of the root element.
pub(super) fn append_to_root(bytes: &[u8], items: &str) -> Vec<u8> {
    let (bom, xml) = split_bom(bytes);
    let text = String::from_utf8_lossy(xml);
    let at = text.rfind("</").unwrap_or(text.len());
    [
        bom,
        text[..at].as_bytes(),
        items.as_bytes(),
        text[at..].as_bytes(),
    ]
    .concat()
}

/// The first `rId` number free in a relationships part.
pub(super) fn free_rel_number(bytes: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices("Id=\"rId")
        .filter_map(|(at, m)| {
            let digits: String = text[at + m.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// The `Type` URI a worksheet relationship has in this package.
pub(super) fn worksheet_type(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices("Type=\"")
        .find_map(|(at, m)| {
            let rest = &text[at + m.len()..];
            let value = &rest[..rest.find('"')?];
            value.ends_with("/worksheet").then(|| value.to_owned())
        })
        .unwrap_or_else(|| format!("{}/worksheet", REL_NAMESPACES[0]))
}

/// The worksheet children that come after `<dataValidations>`.
const AFTER_VALIDATIONS: [&str; 23] = [
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
    "sortState",
    "dataConsolidate",
];

fn attribute_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A new worksheet with the list rules the model has written into it, as
/// `<dataValidations>` before the elements that follow it.
pub(super) fn validations_xml(bytes: &[u8], rules: &[aster_sheets::Validation]) -> Vec<u8> {
    if rules.is_empty() {
        return bytes.to_vec();
    }
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
    let mut items = String::new();
    for rule in rules {
        let sqref: Vec<String> = rule
            .ranges
            .iter()
            .map(|range| {
                if range.first == range.last {
                    range.first.to_string()
                } else {
                    format!("{}:{}", range.first, range.last)
                }
            })
            .collect();
        let formula = match &rule.source {
            aster_sheets::ListSource::Items(list) => {
                format!("\"{}\"", list.join(",").replace('"', "\"\""))
            }
            aster_sheets::ListSource::Formula(formula) => formula.clone(),
        };
        let style = match rule.style {
            aster_sheets::ErrorStyle::Stop => "",
            aster_sheets::ErrorStyle::Warning => " errorStyle=\"warning\"",
            aster_sheets::ErrorStyle::Information => " errorStyle=\"information\"",
        };
        let mut attributes = String::from(" type=\"list\"");
        attributes.push_str(style);
        if rule.allow_blank {
            attributes.push_str(" allowBlank=\"1\"");
        }
        if !rule.show_arrow {
            attributes.push_str(" showDropDown=\"1\"");
        }
        if rule.show_error {
            attributes.push_str(" showErrorMessage=\"1\"");
        }
        if !rule.error_title.is_empty() {
            attributes.push_str(&format!(
                " errorTitle=\"{}\"",
                attribute_text(&rule.error_title)
            ));
        }
        if !rule.error.is_empty() {
            attributes.push_str(&format!(" error=\"{}\"", attribute_text(&rule.error)));
        }
        items.push_str(&format!(
            "<{p}dataValidation{attributes} sqref=\"{}\"><{p}formula1>{}</{p}formula1></{p}dataValidation>",
            sqref.join(" "),
            attribute_text(&formula).replace("&quot;", "\""),
        ));
    }
    let element = format!(
        "<{p}dataValidations count=\"{}\">{items}</{p}dataValidations>",
        rules.len()
    );
    let at = children
        .iter()
        .find(|(name, ..)| AFTER_VALIDATIONS.contains(&name.as_str()))
        .map(|(_, start, _)| *start)
        .unwrap_or_else(|| text.rfind("</").unwrap_or(text.len()));
    [bom, &xml[..at], element.as_bytes(), &xml[at..]].concat()
}
