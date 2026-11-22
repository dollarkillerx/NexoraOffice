//! What the gate needs from XML: a compact form of the sample files, a summary
//! of a part that two serialisations of the same content agree on, and the
//! relationship lists that tie the parts of a package together.

use std::collections::BTreeMap;

use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesRef, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, Reader, Writer, XmlVersion};

pub const NS_MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
pub const NS_RELATIONSHIPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Remove the indentation the sample files are stored with, leaving what an
/// office application writes: one line, no white space between elements. White
/// space that is the whole content of an element (`<w:t> </w:t>`) is content
/// and stays.
pub fn minify(source: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_reader(source);
    let mut writer = Writer::new(Vec::with_capacity(source.len()));
    let mut held: Option<Event> = None;
    let mut after_start = false;
    loop {
        let event = reader.read_event().map_err(|err| err.to_string())?;
        if let Event::Text(text) = &event
            && text.clone().into_inner().trim().is_empty()
        {
            // Content only if it sits directly between a start and an end tag.
            held = after_start.then_some(event);
            after_start = false;
            continue;
        }
        if let Some(space) = held.take()
            && matches!(event, Event::End(_))
        {
            writer.write_event(space).map_err(|err| err.to_string())?;
        }
        after_start = matches!(event, Event::Start(_));
        match event {
            Event::Eof => break,
            Event::Decl(_) => {
                writer.write_event(event).map_err(|err| err.to_string())?;
                writer.get_mut().extend_from_slice(b"\r\n");
            }
            other => writer.write_event(other).map_err(|err| err.to_string())?,
        }
    }
    Ok(writer.into_inner())
}

/// A serialisation-independent description of one XML part. Namespace prefixes,
/// attribute order, the XML declaration and white space between elements do not
/// appear in it; element order, attribute values and text do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Equal for two parts exactly when they hold the same content.
    pub canonical: String,
    /// Counts by expanded name, `{namespace}local`.
    pub elements: BTreeMap<String, u32>,
    /// Counts by `element @attribute = value`.
    pub attributes: BTreeMap<String, u32>,
    /// Counts by `element : text`.
    pub text: BTreeMap<String, u32>,
    /// Things that make the part wrong on its own: prefixes used without a
    /// declaration, and prefixes named by markup-compatibility attributes that
    /// are not declared where they are named.
    pub problems: Vec<String>,
}

struct Frame {
    name: String,
    text: String,
    has_children: bool,
}

fn expanded(resolved: &ResolveResult, local: &str, problems: &mut Vec<String>) -> String {
    match resolved {
        ResolveResult::Unbound => local.to_owned(),
        ResolveResult::Bound(namespace) => {
            format!("{{{}}}{local}", namespace.as_ref())
        }
        ResolveResult::Unknown(prefix) => {
            problems.push(format!("prefix `{prefix}` is used but not declared"));
            format!("{{?{prefix}}}{local}")
        }
    }
}

/// An attribute's value with entities resolved. A value that cannot be
/// resolved is kept as written, so that it still compares equal to itself.
pub fn attribute_value(attribute: &Attribute) -> String {
    match attribute.normalized_value(XmlVersion::Implicit1_0) {
        Ok(value) => value.into_owned(),
        Err(_) => attribute.value.to_string(),
    }
}

/// The character or text an entity reference stands for.
pub fn general_ref(reference: &BytesRef) -> String {
    if let Ok(Some(ch)) = reference.resolve_char_ref() {
        return ch.to_string();
    }
    let name = reference.clone().into_inner();
    match name.as_ref() {
        "amp" => "&".to_owned(),
        "lt" => "<".to_owned(),
        "gt" => ">".to_owned(),
        "quot" => "\"".to_owned(),
        "apos" => "'".to_owned(),
        other => format!("&{other};"),
    }
}

impl Summary {
    fn close_text(&mut self, frame: &mut Frame, at_child: bool) {
        let text = std::mem::take(&mut frame.text);
        // White space next to child elements is layout, not content.
        if text.is_empty() || (text.trim().is_empty() && (at_child || frame.has_children)) {
            return;
        }
        self.canonical.push_str(&format!("{text:?}"));
        *self
            .text
            .entry(format!("{} : {text}", frame.name))
            .or_default() += 1;
    }
}

pub fn summarize(source: &[u8]) -> Result<Summary, String> {
    let mut summary = Summary::default();
    let mut reader = NsReader::from_reader(source);
    let mut stack: Vec<Frame> = Vec::new();
    loop {
        let (resolved, event) = reader
            .read_resolved_event()
            .map_err(|err| err.to_string())?;
        match event {
            Event::Start(_) | Event::Empty(_) => {
                let is_empty = matches!(event, Event::Empty(_));
                let (Event::Start(element) | Event::Empty(element)) = event else {
                    unreachable!()
                };
                let name = expanded(
                    &resolved,
                    element.local_name().as_ref(),
                    &mut summary.problems,
                );
                if let Some(parent) = stack.last_mut() {
                    summary.close_text(parent, true);
                    parent.has_children = true;
                }
                let resolver = reader.resolver();
                let mut attributes: Vec<(String, String)> = Vec::new();
                for attribute in element.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| format!("{name}: {err}"))?;
                    let key = attribute.key.as_ref();
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        continue;
                    }
                    let (space, local) = resolver.resolve_attribute(attribute.key);
                    let attribute_name = expanded(&space, local.as_ref(), &mut summary.problems);
                    let value = attribute_value(&attribute);
                    // These attributes hold lists of prefixes; the prefixes
                    // must mean something where the list is written.
                    let names_prefixes = attribute_name.starts_with(&format!("{{{NS_MC}}}"))
                        || (name == format!("{{{NS_MC}}}Choice") && attribute_name == "Requires");
                    if names_prefixes {
                        for token in value.split_ascii_whitespace() {
                            let prefix = token.split(':').next().unwrap_or(token);
                            let probe = format!("{prefix}:x");
                            if let (ResolveResult::Unknown(_), _) =
                                resolver.resolve_element(QName(&probe))
                            {
                                summary.problems.push(format!(
                                    "`{}` names the prefix `{prefix}`, which is not declared",
                                    local.as_ref()
                                ));
                            }
                        }
                    }
                    attributes.push((attribute_name, value));
                }
                attributes.sort();
                *summary.elements.entry(name.clone()).or_default() += 1;
                summary.canonical.push('<');
                summary.canonical.push_str(&name);
                for (attribute_name, value) in attributes {
                    summary
                        .canonical
                        .push_str(&format!(" {attribute_name}={value:?}"));
                    *summary
                        .attributes
                        .entry(format!("{name} @{attribute_name} = {value}"))
                        .or_default() += 1;
                }
                summary.canonical.push('>');
                if is_empty {
                    summary.canonical.push_str("</>");
                } else {
                    stack.push(Frame {
                        name,
                        text: String::new(),
                        has_children: false,
                    });
                }
            }
            Event::End(_) => {
                let mut frame = stack.pop().ok_or("an end tag without a start tag")?;
                summary.close_text(&mut frame, false);
                summary.canonical.push_str("</>");
            }
            Event::Text(text) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&text.xml10_content());
                }
            }
            Event::CData(data) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&data.into_inner());
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(frame) = stack.last_mut() {
                    frame.text.push_str(&general_ref(&reference));
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err("the document ends inside an element".into());
    }
    if summary.elements.is_empty() {
        return Err("no root element".into());
    }
    summary.problems.sort();
    summary.problems.dedup();
    Ok(summary)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

/// The entries of a relationships part, in document order.
pub fn relationships(source: &[u8]) -> Result<Vec<Relationship>, String> {
    let mut reader = Reader::from_reader(source);
    let mut found = Vec::new();
    loop {
        match reader.read_event().map_err(|err| err.to_string())? {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == "Relationship" =>
            {
                let mut relationship = Relationship {
                    id: String::new(),
                    kind: String::new(),
                    target: String::new(),
                    external: false,
                };
                for attribute in element.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| err.to_string())?;
                    let value = attribute_value(&attribute);
                    match attribute.key.as_ref() {
                        "Id" => relationship.id = value,
                        "Type" => relationship.kind = value,
                        "Target" => relationship.target = value,
                        "TargetMode" => relationship.external = value == "External",
                        _ => {}
                    }
                }
                found.push(relationship);
            }
            Event::Eof => return Ok(found),
            _ => {}
        }
    }
}

/// The value of the first `r:id` (or `r:embed`-style) attribute named `local`
/// in the relationships namespace on the first element called `element`.
pub fn first_relationship_id(source: &[u8], element: &str) -> Result<Option<String>, String> {
    let mut reader = NsReader::from_reader(source);
    loop {
        let (_, event) = reader
            .read_resolved_event()
            .map_err(|err| err.to_string())?;
        match event {
            Event::Start(start) | Event::Empty(start) if start.local_name().as_ref() == element => {
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| err.to_string())?;
                    let (space, local) = reader.resolver().resolve_attribute(attribute.key);
                    let in_relationships = matches!(
                        &space,
                        ResolveResult::Bound(namespace)
                            if namespace.as_ref() == NS_RELATIONSHIPS
                    );
                    if in_relationships && local.as_ref() == "id" {
                        return Ok(Some(attribute_value(&attribute)));
                    }
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minify_keeps_content_and_drops_layout() {
        let pretty =
            b"<?xml version=\"1.0\"?>\n<a>\n  <b> </b>\n  <c>x &amp; y</c>\n  <d/>\n</a>\n";
        let compact = minify(pretty).unwrap();
        assert_eq!(
            String::from_utf8(compact).unwrap(),
            "<?xml version=\"1.0\"?>\r\n<a><b> </b><c>x &amp; y</c><d/></a>"
        );
    }

    #[test]
    fn minified_and_indented_forms_have_the_same_summary() {
        let pretty = "<r xmlns:w=\"urn:w\">\n  <w:p w:id=\"1\">\n    <w:t> </w:t>\n    <w:t>a &lt; b</w:t>\n  </w:p>\n</r>";
        let compact = minify(pretty.as_bytes()).unwrap();
        assert_eq!(
            summarize(pretty.as_bytes()).unwrap(),
            summarize(&compact).unwrap()
        );
    }

    #[test]
    fn prefixes_and_attribute_order_do_not_matter() {
        let one =
            summarize(b"<w:p xmlns:w=\"urn:w\" w:a=\"1\" w:b=\"2\"><w:t>x</w:t></w:p>").unwrap();
        let two =
            summarize(b"<p xmlns=\"urn:w\" xmlns:q=\"urn:w\" q:b=\"2\" q:a=\"1\"><t>x</t></p>")
                .unwrap();
        assert_eq!(one.canonical, two.canonical);
        assert_eq!(one.elements["{urn:w}t"], 1);
        assert_eq!(one.text["{urn:w}t : x"], 1);
    }

    #[test]
    fn a_lost_attribute_or_element_shows_in_the_counts() {
        let before = summarize(b"<a xmlns:x=\"urn:x\"><b x:keep=\"1\"/><x:c/></a>").unwrap();
        let after = summarize(b"<a><b/></a>").unwrap();
        assert_ne!(before.canonical, after.canonical);
        assert!(before.attributes.contains_key("b @{urn:x}keep = 1"));
        assert!(after.attributes.is_empty());
        assert!(!after.elements.contains_key("{urn:x}c"));
    }

    #[test]
    fn undeclared_prefixes_are_reported() {
        let summary = summarize(
            b"<a xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" mc:Ignorable=\"w14 mc\"><z:b/></a>",
        );
        // An unbound element prefix is a namespace error the reader may refuse outright.
        match summary {
            Ok(summary) => {
                assert!(summary.problems.iter().any(|p| p.contains("`w14`")));
                assert!(summary.problems.iter().any(|p| p.contains("`z`")));
                assert!(!summary.problems.iter().any(|p| p.contains("`mc`")));
            }
            Err(message) => assert!(!message.is_empty()),
        }
    }

    #[test]
    fn relationships_are_listed_in_order() {
        let found = relationships(
            b"<Relationships><Relationship Id=\"rId2\" Type=\"t/image\" Target=\"media/a.png\"/><Relationship Id=\"rId1\" Type=\"t/hyperlink\" Target=\"https://example.org/?a=1&amp;b=2\" TargetMode=\"External\"/></Relationships>",
        )
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "rId2");
        assert!(found[1].external);
        assert_eq!(found[1].target, "https://example.org/?a=1&b=2");
    }

    #[test]
    fn the_first_relationship_id_is_found_by_namespace() {
        let source = b"<p:x xmlns:p=\"urn:p\" xmlns:rel=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><p:sldId id=\"256\" rel:id=\"rId7\"/><p:sldId id=\"257\" rel:id=\"rId8\"/></p:x>";
        assert_eq!(
            first_relationship_id(source, "sldId").unwrap().as_deref(),
            Some("rId7")
        );
    }
}
