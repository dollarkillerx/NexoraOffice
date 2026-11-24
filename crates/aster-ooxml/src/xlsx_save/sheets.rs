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
