//! Compare a package with what a library made of it, part by part.

use std::collections::{BTreeMap, BTreeSet};

use crate::package::{Package, is_xml};
use crate::xml::{Summary, summarize};

/// What the original had and the result lacks, and the other way round.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delta {
    pub lost_elements: Vec<(String, u32)>,
    pub lost_attributes: Vec<(String, u32)>,
    pub lost_text: Vec<(String, u32)>,
    pub gained_elements: Vec<(String, u32)>,
    pub gained_attributes: Vec<(String, u32)>,
    pub gained_text: Vec<(String, u32)>,
    /// Attributes that are still there with another value that does not mean
    /// the same: `(element @attribute, before, after)`.
    pub changed_attributes: Vec<(String, String, String)>,
    /// Attributes whose value is spelled differently and means the same, such
    /// as `1` and `1.0`. Not a loss.
    pub respelled_attributes: Vec<(String, String, String)>,
}

fn total(items: &[(String, u32)]) -> u32 {
    items.iter().map(|(_, count)| count).sum()
}

impl Delta {
    pub fn lost_element_count(&self) -> u32 {
        total(&self.lost_elements)
    }

    pub fn lost_attribute_count(&self) -> u32 {
        total(&self.lost_attributes)
    }

    pub fn lost_text_count(&self) -> u32 {
        total(&self.lost_text)
    }

    pub fn changed_attribute_count(&self) -> u32 {
        self.changed_attributes.len() as u32
    }

    /// Nothing the original had is gone or altered. Order may differ, things
    /// may be added, values may be respelled.
    #[cfg(test)]
    pub fn loses_nothing(&self) -> bool {
        self.lost_elements.is_empty()
            && self.lost_attributes.is_empty()
            && self.lost_text.is_empty()
            && self.changed_attributes.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The same bytes.
    Identical,
    /// Other bytes, the same content: prefixes, attribute order or white space.
    Equivalent,
    /// The content differs.
    Different(Delta),
    /// A part that is not XML has other bytes.
    BinaryDiffers,
    /// The part is gone.
    Missing,
    /// The part was XML and no longer parses.
    Unreadable(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// One entry per part of the original, in its order.
    pub parts: Vec<(String, Change)>,
    /// Parts of the result that the original did not have.
    pub added: Vec<String>,
}

fn minus(left: &BTreeMap<String, u32>, right: &BTreeMap<String, u32>) -> Vec<(String, u32)> {
    left.iter()
        .filter_map(|(key, count)| {
            let other = right.get(key).copied().unwrap_or(0);
            (*count > other).then(|| (key.clone(), count - other))
        })
        .collect()
}

/// Whether two spellings of an attribute value say the same thing.
fn same_meaning(name: &str, before: &str, after: &str) -> bool {
    if let (Ok(one), Ok(two)) = (before.parse::<f64>(), after.parse::<f64>()) {
        return one == two;
    }
    let truth = |value: &str| match value {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    };
    if let (Some(one), Some(two)) = (truth(before), truth(after)) {
        return one == two;
    }
    // Declaring more namespaces ignorable takes nothing away.
    name.ends_with("}Ignorable")
        && before
            .split_ascii_whitespace()
            .all(|prefix| after.split_ascii_whitespace().any(|other| other == prefix))
}

/// WordprocessingML elements that switch something on. Their `val` attribute
/// defaults to true, so `<w:b w:val="1"/>` and `<w:b/>` say the same.
const ON_OFF_ELEMENTS: [&str; 46] = [
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "rtl",
    "cs",
    "specVanish",
    "oMath",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "widowControl",
    "suppressLineNumbers",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "contextualSpacing",
    "mirrorIndents",
    "cantSplit",
    "tblHeader",
    "hidden",
    "noWrap",
    "tcFitText",
    "hideMark",
    "titlePg",
    "bidiVisual",
    "formProt",
    "noEndnote",
];

/// Whether leaving this attribute out says the same as writing it: the value
/// is what the schema assumes when the attribute is absent. Only the cases met
/// in the samples are listed; anything else that goes missing counts as lost.
fn is_schema_default(key: &str) -> bool {
    const WORD: &str = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}";
    let (name, value) = name_and_value(key);
    let Some((element, attribute)) = name.split_once(" @") else {
        return false;
    };
    let (Some(element), Some("val")) = (element.strip_prefix(WORD), attribute.strip_prefix(WORD))
    else {
        return false;
    };
    match value {
        "1" | "true" | "on" => ON_OFF_ELEMENTS.contains(&element),
        "continue" => element == "vMerge",
        _ => false,
    }
}

/// Split `element @attribute = value` at the value.
fn name_and_value(key: &str) -> (&str, &str) {
    key.split_once(" = ").unwrap_or((key, ""))
}

fn take_one(items: &mut Vec<(String, u32)>, index: usize) {
    items[index].1 -= 1;
    if items[index].1 == 0 {
        items.remove(index);
    }
}

fn delta(before: &Summary, after: &Summary) -> Delta {
    let mut delta = Delta {
        lost_elements: minus(&before.elements, &after.elements),
        lost_attributes: minus(&before.attributes, &after.attributes),
        lost_text: minus(&before.text, &after.text),
        gained_elements: minus(&after.elements, &before.elements),
        gained_attributes: minus(&after.attributes, &before.attributes),
        gained_text: minus(&after.text, &before.text),
        ..Delta::default()
    };
    // An attribute lost with one value and gained with another has changed.
    let mut index = 0;
    while index < delta.lost_attributes.len() {
        let (name, old) = name_and_value(&delta.lost_attributes[index].0);
        let partner = delta
            .gained_attributes
            .iter()
            .position(|(key, _)| name_and_value(key).0 == name);
        let Some(partner) = partner else {
            index += 1;
            continue;
        };
        let new = name_and_value(&delta.gained_attributes[partner].0).1;
        let entry = (name.to_owned(), old.to_owned(), new.to_owned());
        if same_meaning(name, old, new) {
            delta.respelled_attributes.push(entry);
        } else {
            delta.changed_attributes.push(entry);
        }
        take_one(&mut delta.gained_attributes, partner);
        let before_len = delta.lost_attributes.len();
        take_one(&mut delta.lost_attributes, index);
        if delta.lost_attributes.len() == before_len {
            // The same lost value occurs again; look for another partner.
            continue;
        }
    }
    // An attribute left out because it had its default value. If its element
    // is gone as well, the element still counts.
    let (defaults, lost): (Vec<_>, Vec<_>) = std::mem::take(&mut delta.lost_attributes)
        .into_iter()
        .partition(|(key, _)| is_schema_default(key));
    delta.lost_attributes = lost;
    for (key, _) in defaults {
        let (name, old) = name_and_value(&key);
        delta.respelled_attributes.push((
            name.to_owned(),
            old.to_owned(),
            "(left out, the default)".to_owned(),
        ));
    }
    delta
}

pub fn compare_part(name: &str, before: &[u8], after: &[u8]) -> Change {
    if before == after {
        return Change::Identical;
    }
    // A part the original could not be parsed as XML is compared as bytes.
    let Some(before) = is_xml(name).then(|| summarize(before).ok()).flatten() else {
        return Change::BinaryDiffers;
    };
    match summarize(after) {
        Err(why) => Change::Unreadable(why),
        Ok(after) if after.canonical == before.canonical => Change::Equivalent,
        Ok(after) => Change::Different(delta(&before, &after)),
    }
}

pub fn compare(before: &Package, after: &Package) -> Comparison {
    let parts = before
        .parts
        .iter()
        .map(|part| {
            let change = match after.get(&part.name) {
                None => Change::Missing,
                Some(data) => compare_part(&part.name, &part.data, data),
            };
            (part.name.clone(), change)
        })
        .collect();
    let known: BTreeSet<&str> = before.names().collect();
    let added = after
        .names()
        .filter(|name| !known.contains(name))
        .map(str::to_owned)
        .collect();
    Comparison { parts, added }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Part;

    fn package(parts: &[(&str, &[u8])]) -> Package {
        Package {
            parts: parts
                .iter()
                .map(|(name, data)| Part {
                    name: (*name).to_owned(),
                    data: data.to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn every_kind_of_change_is_told_apart() {
        let before = package(&[
            ("same.xml", b"<a/>"),
            ("reprefixed.xml", b"<w:a xmlns:w=\"urn:w\" w:x=\"1\"/>"),
            ("lossy.xml", b"<a><b keep=\"1\">text</b><c/></a>"),
            ("image.png", b"\x89PNG1"),
            ("gone.xml", b"<a/>"),
            ("broken.xml", b"<a/>"),
        ]);
        let after = package(&[
            ("same.xml", b"<a/>"),
            (
                "reprefixed.xml",
                b"<a xmlns=\"urn:w\" xmlns:q=\"urn:w\" q:x=\"1\"></a>",
            ),
            ("lossy.xml", b"<a><b>text</b></a>"),
            ("image.png", b"\x89PNG2"),
            ("broken.xml", b"<a>"),
            ("new.xml", b"<n/>"),
        ]);
        let result = compare(&before, &after);
        let change = |name: &str| {
            &result
                .parts
                .iter()
                .find(|(part, _)| part == name)
                .unwrap()
                .1
        };
        assert_eq!(change("same.xml"), &Change::Identical);
        assert_eq!(change("reprefixed.xml"), &Change::Equivalent);
        assert_eq!(change("image.png"), &Change::BinaryDiffers);
        assert_eq!(change("gone.xml"), &Change::Missing);
        assert!(matches!(change("broken.xml"), Change::Unreadable(_)));
        let Change::Different(delta) = change("lossy.xml") else {
            panic!("expected a difference")
        };
        assert_eq!(delta.lost_element_count(), 1);
        assert_eq!(delta.lost_attribute_count(), 1);
        assert_eq!(delta.lost_text_count(), 0);
        assert!(!delta.loses_nothing());
        assert_eq!(result.added, ["new.xml"]);
    }

    #[test]
    fn a_respelled_value_is_not_a_loss_and_an_altered_one_is() {
        let change = compare_part(
            "a.xml",
            b"<a xmlns:mc=\"urn:mc\"><pane ySplit=\"1\" on=\"true\" name=\"Picture 1\"/><b mc:Ignorable=\"w14\"/></a>",
            b"<a xmlns:mc=\"urn:mc\"><pane ySplit=\"1.0\" on=\"1\" name=\"image1.png\"/><b mc:Ignorable=\"w14 w15\"/></a>",
        );
        let Change::Different(delta) = change else {
            panic!("the bytes differ")
        };
        assert_eq!(delta.respelled_attributes.len(), 3, "{delta:?}");
        assert_eq!(
            delta.changed_attributes,
            [(
                "pane @name".to_owned(),
                "Picture 1".to_owned(),
                "image1.png".to_owned()
            )]
        );
        assert!(
            delta.lost_attributes.is_empty() && delta.gained_attributes.is_empty(),
            "{delta:?}"
        );
        assert!(!delta.loses_nothing());
    }

    #[test]
    fn leaving_out_a_default_value_is_not_a_loss() {
        const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let before = format!(
            "<w:a xmlns:w=\"{W}\"><w:cantSplit w:val=\"1\"/><w:vMerge w:val=\"continue\"/><w:ilvl w:val=\"1\"/><w:b w:val=\"0\"/></w:a>"
        );
        let after = format!("<w:a xmlns:w=\"{W}\"><w:cantSplit/><w:vMerge/><w:ilvl/><w:b/></w:a>");
        let Change::Different(delta) = compare_part("a.xml", before.as_bytes(), after.as_bytes())
        else {
            panic!("the bytes differ")
        };
        assert_eq!(delta.respelled_attributes.len(), 2, "{delta:?}");
        // A list level is not a switch, and switching bold off is not the default.
        assert_eq!(delta.lost_attribute_count(), 2, "{delta:?}");
    }

    #[test]
    fn reordering_loses_nothing_but_is_not_equivalent() {
        let change = compare_part(
            "a.rels",
            b"<r><x id=\"1\"/><x id=\"2\"/></r>",
            b"<r><x id=\"2\"/><x id=\"1\"/></r>",
        );
        let Change::Different(delta) = change else {
            panic!("element order is content")
        };
        assert!(delta.loses_nothing());
    }
}
