//! Small edits of XML as text, for writing back parts with as few bytes
//! changed as possible: an element's children and their places, a start
//! tag's attributes, escaping. Prefixes are kept as written.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// An element found in a part: its local name and byte range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

fn local_name(e: &BytesStart<'_>) -> String {
    let name = e.name().into_inner().to_string();
    match name.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => name,
    }
}

/// The child elements of the element that starts at `0` in `xml`, with
/// their ranges.
pub fn children(xml: &str) -> Vec<Span> {
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().check_end_names = false;
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut name = String::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => {
                if depth == 1 {
                    start = before;
                    name = local_name(&e);
                }
                depth += 1;
            }
            Event::Empty(e) if depth == 1 => out.push(Span {
                name: local_name(&e),
                start: before,
                end: after,
            }),
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    out.push(Span {
                        name: std::mem::take(&mut name),
                        start,
                        end: after,
                    });
                }
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The end of the start tag and the start of the end tag of an element
/// (equal at the end for an empty one, `<a/>`).
pub fn inner(xml: &str) -> (usize, usize) {
    let open = xml.find('>').map_or(xml.len(), |i| i + 1);
    if xml[..open].ends_with("/>") {
        return (open, open);
    }
    let close = xml.rfind("</").unwrap_or(xml.len());
    (open, close.max(open))
}

/// The element's qualified name (`a:rPr`).
pub fn qualified_name(xml: &str) -> &str {
    let rest = xml.strip_prefix('<').unwrap_or(xml);
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The prefix of an element's name with its colon (`a:`), or empty.
pub fn prefix_of(xml: &str) -> String {
    match qualified_name(xml).split_once(':') {
        Some((prefix, _)) => format!("{prefix}:"),
        None => String::new(),
    }
}

/// The same element under another local name, prefix kept: `<a:endParaRPr
/// sz="1"/>` as `<a:rPr sz="1"/>`.
pub fn rename(xml: &str, local: &str) -> String {
    let qualified = qualified_name(xml).to_owned();
    let new = match qualified.split_once(':') {
        Some((prefix, _)) => format!("{prefix}:{local}"),
        None => local.to_owned(),
    };
    let mut out = format!("<{new}{}", &xml[1 + qualified.len()..]);
    let close = format!("</{qualified}>");
    if out.ends_with(&close) {
        out.truncate(out.len() - close.len());
        out.push_str(&format!("</{new}>"));
    }
    out
}

/// Where an attribute sits in a start tag: the whole attribute (from the
/// whitespace before it to the end of its closing quote) and its value.
struct AttributeSpan {
    whole: std::ops::Range<usize>,
    value: std::ops::Range<usize>,
}

fn is_xml_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Finds attribute `name` in a start tag by walking its attributes, so text
/// inside another attribute's value never matches and `xid`/`r:id` are not
/// `id`. Whitespace may surround `=`.
fn find_attribute(tag: &str, name: &str) -> Option<AttributeSpan> {
    let bytes = tag.as_bytes();
    let skip_space = |mut i: usize| {
        while i < bytes.len() && is_xml_space(bytes[i]) {
            i += 1;
        }
        i
    };
    // Skip the element name.
    let mut i = 1;
    while i < bytes.len() && !is_xml_space(bytes[i]) && !matches!(bytes[i], b'>' | b'/') {
        i += 1;
    }
    loop {
        let before = i;
        i = skip_space(i);
        if i == before || i >= bytes.len() || matches!(bytes[i], b'>' | b'/') {
            return None;
        }
        let name_start = i;
        while i < bytes.len() && !is_xml_space(bytes[i]) && bytes[i] != b'=' {
            if matches!(bytes[i], b'>' | b'/') {
                return None;
            }
            i += 1;
        }
        let found = &tag[name_start..i];
        i = skip_space(i);
        if bytes.get(i) != Some(&b'=') {
            return None;
        }
        i = skip_space(i + 1);
        let quote = match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => q,
            _ => return None,
        };
        let start = i + 1;
        let end = start + tag[start..].find(quote as char)?;
        i = end + 1;
        if found == name {
            return Some(AttributeSpan {
                whole: before..i,
                value: start..end,
            });
        }
    }
}

/// The value of attribute `name` (as written, entities not resolved).
pub fn attribute(xml: &str, name: &str) -> Option<String> {
    let (open, _) = inner(xml);
    let tag = &xml[..open];
    let span = find_attribute(tag, name)?;
    Some(tag[span.value].to_owned())
}

/// The element with attribute `name` set to `value` (escaped here), added at
/// the end of its start tag if missing.
pub fn set_attribute(xml: &str, name: &str, value: &str) -> String {
    let (open, _) = inner(xml);
    let tag = &xml[..open];
    let value = escape_attribute(value);
    if let Some(span) = find_attribute(tag, name) {
        return format!(
            "{}{value}{}",
            &xml[..span.value.start],
            &xml[span.value.end..]
        );
    }
    let end = if tag.ends_with("/>") {
        open - 2
    } else {
        open - 1
    };
    format!("{} {name}=\"{value}\"{}", &xml[..end], &xml[end..])
}

/// The element without attribute `name`.
pub fn remove_attribute(xml: &str, name: &str) -> String {
    let (open, _) = inner(xml);
    match find_attribute(&xml[..open], name) {
        Some(span) => format!("{}{}", &xml[..span.whole.start], &xml[span.whole.end..]),
        None => xml.to_owned(),
    }
}

/// The element with its children named in `remove` taken out and `insert`
/// put in before the first child named in `before` (at the end when there
/// is none). An empty element is opened up as needed.
pub fn edit_children(xml: &str, remove: &[&str], insert: &str, before: &[&str]) -> String {
    let spans = children(xml);
    let (open, close) = inner(xml);
    let empty = open == close && xml[..open].ends_with("/>");
    let mut kept = String::new();
    let mut placed = insert.is_empty();
    let mut at = open;
    for span in &spans {
        kept.push_str(&xml[at..span.start]);
        at = span.end;
        if !placed && before.contains(&span.name.as_str()) {
            kept.push_str(insert);
            placed = true;
        }
        if !remove.contains(&span.name.as_str()) {
            kept.push_str(&xml[span.start..span.end]);
        }
    }
    kept.push_str(&xml[at..close]);
    if !placed {
        kept.push_str(insert);
    }
    if empty {
        if kept.is_empty() {
            return xml.to_owned();
        }
        let tag = &xml[..open - 2];
        let name = qualified_name(xml);
        return format!("{tag}>{kept}</{name}>");
    }
    format!("{}{kept}{}", &xml[..open], &xml[close..])
}

/// Whether the element has neither attributes nor children.
pub fn is_bare(xml: &str) -> bool {
    let (open, close) = inner(xml);
    let tag = &xml[..open];
    let name = qualified_name(xml);
    let rest = tag[1 + name.len()..]
        .trim_end_matches('>')
        .trim_end_matches('/');
    rest.trim().is_empty() && xml[open..close].trim().is_empty()
}

/// Text for XML content and attribute values: the reserved characters, and
/// characters XML cannot hold dropped.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

pub fn escape_attribute(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

/// The part's text and its byte order mark apart.
pub fn split_bom(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (&bytes[..3], rest),
        None => (&bytes[..0], bytes),
    }
}

/// The text of a part, after any byte order mark.
pub fn text_of(bytes: &[u8]) -> (Vec<u8>, String) {
    let (bom, xml) = split_bom(bytes);
    (bom.to_vec(), String::from_utf8_lossy(xml).into_owned())
}

/// Every element named `name` (local name) in `xml`, at any depth, with
/// its range.
pub fn find_all(xml: &str, name: &str) -> Vec<Span> {
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().check_end_names = false;
    let mut out = Vec::new();
    let mut open: Vec<(String, usize)> = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        let Ok(event) = reader.read_event() else {
            break;
        };
        let after = reader.buffer_position() as usize;
        match event {
            Event::Start(e) => open.push((local_name(&e), before)),
            Event::Empty(e) if local_name(&e) == name => out.push(Span {
                name: name.to_owned(),
                start: before,
                end: after,
            }),
            Event::End(_) => {
                if let Some((found, start)) = open.pop()
                    && found == name
                {
                    out.push(Span {
                        name: found,
                        start,
                        end: after,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out.sort_by_key(|span| span.start);
    out
}

/// Put `items` just before the root element's end tag.
pub fn append_to_root(bytes: &[u8], items: &str) -> Vec<u8> {
    let (bom, text) = text_of(bytes);
    let at = text.rfind("</").unwrap_or(text.len());
    [
        bom.as_slice(),
        &text.as_bytes()[..at],
        items.as_bytes(),
        &text.as_bytes()[at..],
    ]
    .concat()
}

/// The first `rId` number not used in a relationships part.
pub fn free_rel_number(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut max = 0;
    for (at, _) in text.match_indices("Id") {
        if at == 0 || !is_xml_space(bytes[at - 1]) {
            continue;
        }
        let mut i = at + 2;
        while i < bytes.len() && is_xml_space(bytes[i]) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < bytes.len() && is_xml_space(bytes[i]) {
            i += 1;
        }
        if !matches!(bytes.get(i), Some(b'"' | b'\'')) {
            continue;
        }
        let Some(rest) = text[i + 1..].strip_prefix("rId") else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u32>() {
            max = max.max(n);
        }
    }
    max + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_text_inside_values_never_matches() {
        let tag = r#"<p:cNvPr descr="see id='4' here" id="5" name="x"/>"#;
        assert_eq!(attribute(tag, "id").as_deref(), Some("5"));
        assert_eq!(
            set_attribute(tag, "id", "9"),
            r#"<p:cNvPr descr="see id='4' here" id="9" name="x"/>"#
        );
        assert_eq!(
            remove_attribute(tag, "id"),
            r#"<p:cNvPr descr="see id='4' here" name="x"/>"#
        );
    }

    #[test]
    fn attributes_split_by_any_whitespace() {
        let tag = "<a:rPr\n  lang=\"en-US\"\n\tsz='1800'/>";
        assert_eq!(attribute(tag, "lang").as_deref(), Some("en-US"));
        assert_eq!(attribute(tag, "sz").as_deref(), Some("1800"));
        let bigger = set_attribute(tag, "sz", "2400");
        assert_eq!(bigger, "<a:rPr\n  lang=\"en-US\"\n\tsz='2400'/>");
        assert_eq!(bigger.matches("sz=").count(), 1);
        assert_eq!(remove_attribute(tag, "lang"), "<a:rPr\n\tsz='1800'/>");
        let spaced = "<a:x id = \"7\"/>";
        assert_eq!(attribute(spaced, "id").as_deref(), Some("7"));
    }

    #[test]
    fn attribute_names_match_whole_names_only() {
        let tag = r#"<p:cNvPr xid="1" r:id="rId2" id="3"/>"#;
        assert_eq!(attribute(tag, "id").as_deref(), Some("3"));
        assert_eq!(
            set_attribute(tag, "id", "9"),
            r#"<p:cNvPr xid="1" r:id="rId2" id="9"/>"#
        );
        assert_eq!(
            remove_attribute(tag, "id"),
            r#"<p:cNvPr xid="1" r:id="rId2"/>"#
        );
    }

    #[test]
    fn free_rel_number_reads_both_quotes() {
        let rels = "<Relationships><Relationship Id=\"rId1\"/>\
            <Relationship\n Id='rId7'/><Relationship Id = \"rId3\"/></Relationships>";
        assert_eq!(free_rel_number(rels), 8);
        assert_eq!(free_rel_number("<Relationships/>"), 1);
    }

    #[test]
    fn attributes_and_children_change_in_place() {
        let rpr = r#"<a:rPr lang="ja-JP" sz="1800"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="Arial"/></a:rPr>"#;
        assert_eq!(attribute(rpr, "sz").as_deref(), Some("1800"));
        let bigger = set_attribute(rpr, "sz", "2400");
        assert!(bigger.starts_with(r#"<a:rPr lang="ja-JP" sz="2400">"#));
        let bold = set_attribute(rpr, "b", "1");
        assert!(bold.starts_with(r#"<a:rPr lang="ja-JP" sz="1800" b="1">"#));
        assert_eq!(
            remove_attribute(rpr, "lang"),
            rpr.replace(r#" lang="ja-JP""#, "")
        );
        let names: Vec<_> = children(rpr).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["solidFill", "latin"]);
        let red = edit_children(
            rpr,
            &["solidFill"],
            r#"<a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>"#,
            &["latin"],
        );
        assert_eq!(
            red,
            r#"<a:rPr lang="ja-JP" sz="1800"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill><a:latin typeface="Arial"/></a:rPr>"#
        );
        let empty = r#"<a:pPr/>"#;
        assert_eq!(
            edit_children(empty, &[], "<a:buNone/>", &[]),
            "<a:pPr><a:buNone/></a:pPr>"
        );
        assert_eq!(set_attribute(empty, "lvl", "1"), r#"<a:pPr lvl="1"/>"#);
        assert!(is_bare("<a:pPr/>"));
        assert!(!is_bare(r#"<a:pPr lvl="1"/>"#));
        assert_eq!(
            rename(r#"<a:endParaRPr lang="en-US"/>"#, "rPr"),
            r#"<a:rPr lang="en-US"/>"#
        );
        assert_eq!(
            rename(r#"<a:endParaRPr><a:latin/></a:endParaRPr>"#, "rPr"),
            r#"<a:rPr><a:latin/></a:rPr>"#
        );
        assert_eq!(prefix_of("<p:sp>"), "p:");
        assert_eq!(free_rel_number(r#"Id="rId3" Id="rId12""#), 13);
    }
}
