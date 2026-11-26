//! Tracked changes in saved paragraphs: the `w:ins` wrapper around inserted
//! runs, and the records in a paragraph's properties.
//!
//! Text inside an insertion that is still as it was stays in the wrapper the
//! file had (the bytes between runs are copied). What cannot is made from the
//! model: a run made here that is inserted (a split piece, text typed into an
//! insertion) gets a wrapper of its own with the insertion's id, author and
//! date; a run that was accepted leaves its wrapper. A deletion is a marker
//! whose element (`w:del` with its `w:delText`) is copied as it was, or is
//! left out when the deletion was accepted; a rejected one comes back as
//! ordinary runs.

use aster_writer::{Change, ParaChanges};

use crate::xml_edit::{children, escape_attribute, find_all};

/// The attributes a change record has.
fn attributes(w: &str, change: &Change) -> String {
    let mut out = format!(
        " {w}id=\"{}\" {w}author=\"{}\"",
        change.id,
        escape_attribute(&change.author)
    );
    if let Some(date) = &change.date {
        out.push_str(&format!(" {w}date=\"{}\"", escape_attribute(date)));
    }
    out
}

/// `<w:ins ...>` (or `<w:moveTo ...>`) for the runs of an insertion.
pub fn wrapper_open(w: &str, change: &Change) -> String {
    let name = if change.moved { "moveTo" } else { "ins" };
    format!("<{w}{name}{}>", attributes(w, change))
}

pub fn wrapper_close(w: &str, change: &Change) -> String {
    let name = if change.moved { "moveTo" } else { "ins" };
    format!("</{w}{name}>")
}

/// `bytes`, a run as written, inside the wrapper of `change`. A page-number
/// field written as `w:fldSimple` cannot be inside one: its run is.
pub fn wrap(w: &str, change: &Change, bytes: &str) -> String {
    let field = format!("<{w}fldSimple");
    if bytes.starts_with(&field)
        && let Some(open) = bytes.find('>')
        && let Some(close) = bytes.rfind(&format!("</{w}fldSimple>"))
    {
        return format!(
            "{}{}{}{}",
            &bytes[..=open],
            wrapper_open(w, change),
            &bytes[open + 1..close],
            wrapper_close(w, change)
        ) + &bytes[close..];
    }
    format!(
        "{}{bytes}{}",
        wrapper_open(w, change),
        wrapper_close(w, change)
    )
}

/// The wrapper opened by `bytes` is for the change with this id.
pub fn wrapper_is_for(w: &str, bytes: &str, id: i64) -> bool {
    bytes.contains(&format!("{w}id=\"{id}\""))
}

/// `xml` without its children (at the first level) named in `names`.
fn without_children(xml: &str, names: &[&str]) -> String {
    let mut out = xml.to_owned();
    for span in children(xml).into_iter().rev() {
        if names.contains(&span.name.as_str()) {
            out.replace_range(span.start..span.end, "");
        }
    }
    out
}

/// `xml` without any element named `name`, at any depth.
pub fn without_elements(xml: &str, name: &str) -> String {
    let mut spans = find_all(xml, name);
    spans.sort_by_key(|s| s.start);
    let mut out = xml.to_owned();
    let mut last_start = usize::MAX;
    for span in spans.into_iter().rev() {
        if span.end <= last_start {
            out.replace_range(span.start..span.end, "");
            last_start = span.start;
        }
    }
    out
}

/// A paragraph's `pPr` (or nothing) taken from the records it had to the
/// ones it has: records that are gone are removed; one that is new is
/// written into the mark's `rPr`, which is made if there is none.
pub fn edit_marks(w: &str, ppr: &str, then: &ParaChanges, now: &ParaChanges) -> String {
    if then == now {
        return ppr.to_owned();
    }
    let mut out = ppr.to_owned();
    // `<w:pPr/>` becomes an element with room for children.
    if out.trim_end().ends_with("/>") && out.trim_start().starts_with('<') {
        let end = out.trim_end().len() - 2;
        let name = crate::xml_edit::qualified_name(&out).to_owned();
        out = format!("{}></{name}>", &out[..end]);
    }
    let drop_inserted = then.inserted_mark.is_some() && now.inserted_mark.is_none();
    let drop_deleted = then.deleted_mark.is_some() && now.deleted_mark.is_none();
    let drop_formatting = then.formatting && !now.formatting;
    if drop_inserted || drop_deleted || drop_formatting {
        if drop_formatting {
            out = without_children(&out, &["pPrChange"]);
        }
        if let Some(rpr) = children(&out).into_iter().find(|c| c.name == "rPr") {
            let mut names: Vec<&str> = Vec::new();
            if drop_inserted {
                names.extend(["ins", "moveTo"]);
            }
            if drop_deleted {
                names.extend(["del", "moveFrom"]);
            }
            if drop_formatting {
                names.push("rPrChange");
            }
            let cleaned = without_children(&out[rpr.start..rpr.end], &names);
            let replacement = if children(&cleaned).is_empty() {
                String::new()
            } else {
                cleaned
            };
            out.replace_range(rpr.start..rpr.end, &replacement);
        }
    }
    // A properties element with nothing left in it is not written.
    if !out.trim().is_empty() && children(&out).is_empty() && !out.trim_end().ends_with("/>") {
        out.clear();
    }
    let mut records = String::new();
    if let Some(change) = now.inserted_mark.as_ref()
        && then.inserted_mark.is_none()
    {
        let name = if change.moved { "moveTo" } else { "ins" };
        records.push_str(&format!("<{w}{name}{}/>", attributes(w, change)));
    }
    if let Some(change) = now.deleted_mark.as_ref()
        && then.deleted_mark.is_none()
    {
        let name = if change.moved { "moveFrom" } else { "del" };
        records.push_str(&format!("<{w}{name}{}/>", attributes(w, change)));
    }
    if !records.is_empty() {
        out = add_to_mark(w, &out, &records);
    }
    out
}

/// Put `records` first in the `rPr` of a `pPr`, which comes before `sectPr`
/// and `pPrChange` and after everything else.
fn add_to_mark(w: &str, ppr: &str, records: &str) -> String {
    if ppr.trim().is_empty() {
        return format!("<{w}pPr><{w}rPr>{records}</{w}rPr></{w}pPr>");
    }
    let spans = children(ppr);
    if let Some(rpr) = spans.iter().find(|c| c.name == "rPr") {
        let element = &ppr[rpr.start..rpr.end];
        let at = if element.trim_end().ends_with("/>") {
            // `<w:rPr/>`: now has content.
            let name_end = element
                .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .unwrap_or(element.len());
            let rebuilt = format!(
                "{}>{records}</{}>",
                &element[..name_end],
                &element[1..name_end]
            );
            let mut out = ppr.to_owned();
            out.replace_range(rpr.start..rpr.end, &rebuilt);
            return out;
        } else {
            rpr.start + element.find('>').map_or(0, |i| i + 1)
        };
        let mut out = ppr.to_owned();
        out.insert_str(at, records);
        return out;
    }
    // After the last child that comes before the mark's place.
    let stop = spans
        .iter()
        .find(|c| matches!(c.name.as_str(), "sectPr" | "pPrChange"))
        .map(|c| c.start);
    let mut out = ppr.to_owned();
    let at = stop.unwrap_or_else(|| out.rfind("</").unwrap_or(out.len()));
    out.insert_str(at, &format!("<{w}rPr>{records}</{w}rPr>"));
    out
}

/// `out` without insertion wrappers, and hyperlinks, that nothing is left in
/// (the runs were rejected or moved out).
pub fn without_empty_wrappers(w: &str, out: &str) -> String {
    let mut out = out.to_owned();
    for name in ["hyperlink", "ins", "moveTo"] {
        let open = format!("<{w}{name} ");
        let close = format!("</{w}{name}>");
        let mut from = 0;
        while let Some(i) = out[from..].find(&open) {
            let start = from + i;
            let Some(gt) = out[start..].find('>') else {
                break;
            };
            let after = start + gt + 1;
            // A self-closing one is a record, not a wrapper.
            if out[..after].ends_with("/>") {
                from = after;
            } else if out[after..].starts_with(&close) {
                out.replace_range(start..after + close.len(), "");
                from = start;
            } else {
                from = after;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn change(id: i64) -> Arc<Change> {
        Arc::new(Change {
            id,
            author: Arc::from("Ann"),
            date: Some(Arc::from("2024-01-01T00:00:00Z")),
            moved: false,
        })
    }

    #[test]
    fn a_page_number_in_an_insertion_is_wrapped_inside_the_field() {
        let field = "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple>";
        assert_eq!(
            wrap("w:", &change(3), field),
            "<w:fldSimple w:instr=\" PAGE \"><w:ins w:id=\"3\" w:author=\"Ann\" w:date=\"2024-01-01T00:00:00Z\"><w:r><w:t>1</w:t></w:r></w:ins></w:fldSimple>"
        );
        assert_eq!(
            wrap("w:", &change(3), "<w:r/>"),
            "<w:ins w:id=\"3\" w:author=\"Ann\" w:date=\"2024-01-01T00:00:00Z\"><w:r/></w:ins>"
        );
    }

    #[test]
    fn a_wrapper_has_the_changes_attributes() {
        assert_eq!(
            wrapper_open("w:", &change(4)),
            "<w:ins w:id=\"4\" w:author=\"Ann\" w:date=\"2024-01-01T00:00:00Z\">"
        );
        assert!(wrapper_is_for("w:", &wrapper_open("w:", &change(4)), 4));
        assert!(!wrapper_is_for("w:", &wrapper_open("w:", &change(4)), 5));
    }

    #[test]
    fn a_record_is_added_to_the_marks_properties_and_taken_away() {
        let none = ParaChanges::default();
        let ins = ParaChanges {
            inserted_mark: Some(change(1)),
            ..ParaChanges::default()
        };
        let made = edit_marks("w:", "", &none, &ins);
        assert_eq!(
            made,
            "<w:pPr><w:rPr><w:ins w:id=\"1\" w:author=\"Ann\" w:date=\"2024-01-01T00:00:00Z\"/></w:rPr></w:pPr>"
        );
        assert_eq!(edit_marks("w:", &made, &ins, &none), "");
        // Next to other things: before `sectPr`, after `jc`.
        let with = edit_marks(
            "w:",
            "<w:pPr><w:jc w:val=\"left\"/><w:sectPr/></w:pPr>",
            &none,
            &ins,
        );
        assert!(with.find("<w:rPr>").unwrap() > with.find("<w:jc").unwrap());
        assert!(with.find("<w:rPr>").unwrap() < with.find("<w:sectPr").unwrap());
        // An existing mark `rPr` keeps its other children.
        let kept = edit_marks(
            "w:",
            "<w:pPr><w:rPr><w:b/><w:ins w:id=\"1\"/></w:rPr></w:pPr>",
            &ins,
            &none,
        );
        assert_eq!(kept, "<w:pPr><w:rPr><w:b/></w:rPr></w:pPr>");
    }

    #[test]
    fn formatting_records_go_everywhere() {
        let then = ParaChanges {
            formatting: true,
            ..ParaChanges::default()
        };
        let ppr = "<w:pPr><w:rPr><w:rPrChange w:id=\"1\"><w:rPr/></w:rPrChange></w:rPr><w:pPrChange w:id=\"2\"><w:pPr/></w:pPrChange></w:pPr>";
        let out = edit_marks("w:", ppr, &then, &ParaChanges::default());
        assert!(!out.contains("Change"), "{out}");
        let runs = "<w:r><w:rPr><w:b/><w:rPrChange w:id=\"3\"><w:rPr/></w:rPrChange></w:rPr><w:t>x</w:t></w:r>";
        assert_eq!(
            without_elements(runs, "rPrChange"),
            "<w:r><w:rPr><w:b/></w:rPr><w:t>x</w:t></w:r>"
        );
    }
}
