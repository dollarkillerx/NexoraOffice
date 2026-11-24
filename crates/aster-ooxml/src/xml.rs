//! A small pull reader over quick-xml for the parts we model.
//!
//! Elements are matched by local name: the prefixes producers choose vary
//! (`x:c`, `c`) and Strict OOXML uses other namespace URIs for the same
//! names. Empty elements come as an open followed by a close, so callers
//! handle one shape. Text has entities and character references resolved.

use std::borrow::Cow;

use quick_xml::XmlVersion;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

use crate::error::{Error, Result};

/// Deeper nesting than this is refused, so a hostile file cannot make the
/// readers recurse without end.
pub const MAX_DEPTH: usize = 256;

pub enum Ev<'a> {
    Open(Tag<'a>),
    Close,
    Text(Cow<'a, str>),
    Eof,
}

pub struct Tag<'a> {
    start: BytesStart<'a>,
}

impl Tag<'_> {
    /// The name without its prefix.
    pub fn local(&self) -> &[u8] {
        local(self.start.name().into_inner()).as_bytes()
    }

    pub fn is(&self, name: &str) -> bool {
        self.local() == name.as_bytes()
    }

    /// An attribute by local name, with entities resolved.
    pub fn attr(&self, name: &str) -> Option<String> {
        self.start
            .attributes()
            .flatten()
            .find(|attribute| local(attribute.key.into_inner()) == name)
            .map(|attribute| value(&attribute))
    }

    /// Every attribute with this local name, whatever its prefix: `id` and
    /// `r:id` on the same element.
    pub fn attrs(&self, name: &str) -> Vec<String> {
        self.start
            .attributes()
            .flatten()
            .filter(|attribute| local(attribute.key.into_inner()) == name)
            .map(|attribute| value(&attribute))
            .collect()
    }

    pub fn attr_i64(&self, name: &str) -> Option<i64> {
        self.attr(name)?.trim().parse().ok()
    }

    pub fn attr_f64(&self, name: &str) -> Option<f64> {
        self.attr(name)?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }

    /// `1`, `true` and `on` are true; `0`, `false` and `off` false.
    pub fn attr_bool(&self, name: &str) -> Option<bool> {
        match self.attr(name)?.trim() {
            "1" | "true" | "on" => Some(true),
            "0" | "false" | "off" => Some(false),
            _ => None,
        }
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// An attribute's value with entities resolved; as written if they cannot be.
fn value(attribute: &Attribute<'_>) -> String {
    match attribute.normalized_value(XmlVersion::Implicit1_0) {
        Ok(value) => value.into_owned(),
        Err(_) => attribute.value.to_string(),
    }
}

pub struct XmlReader<'a> {
    reader: Reader<&'a [u8]>,
    part: &'a str,
    depth: usize,
    /// The part without a byte order mark: what positions count in.
    bytes: &'a [u8],
    /// Where the last element opened began.
    opened_at: usize,
}

impl<'a> XmlReader<'a> {
    pub fn new(bytes: &'a [u8], part: &'a str) -> Self {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let mut reader = Reader::from_reader(bytes);
        let config = reader.config_mut();
        config.expand_empty_elements = true;
        config.check_end_names = false;
        Self {
            reader,
            part,
            depth: 0,
            bytes,
            opened_at: 0,
        }
    }

    pub fn part(&self) -> &str {
        self.part
    }

    /// Where reading has got to, in bytes after any byte order mark.
    pub fn position(&self) -> usize {
        self.reader.buffer_position() as usize
    }

    /// Where the element last opened began.
    pub fn opened_at(&self) -> usize {
        self.opened_at
    }

    /// The part's text between two positions.
    pub fn slice(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.bytes[start.min(end)..end.min(self.bytes.len())]).into_owned()
    }

    pub fn next(&mut self) -> Result<Ev<'a>> {
        loop {
            let before = self.reader.buffer_position() as usize;
            let event = self
                .reader
                .read_event()
                .map_err(|err| Error::broken(self.part, err))?;
            return Ok(match event {
                Event::Start(start) => {
                    self.opened_at = before;
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        return Err(Error::broken(self.part, "elements nested too deeply"));
                    }
                    Ev::Open(Tag { start })
                }
                Event::End(_) => {
                    self.depth = self.depth.saturating_sub(1);
                    Ev::Close
                }
                Event::Text(text) => Ev::Text(text.xml10_content()),
                Event::CData(data) => Ev::Text(data.into_inner()),
                Event::GeneralRef(reference) => Ev::Text(Cow::Owned(general_ref(&reference))),
                Event::Eof => Ev::Eof,
                // Empty elements are expanded; declarations, comments and
                // processing instructions carry nothing we use.
                Event::Empty(_)
                | Event::Decl(_)
                | Event::PI(_)
                | Event::Comment(_)
                | Event::DocType(_) => continue,
            });
        }
    }

    /// After an `Open`: skip to just past its `Close`.
    pub fn skip(&mut self) -> Result<()> {
        let mut depth = 1usize;
        loop {
            match self.next()? {
                Ev::Open(_) => depth += 1,
                Ev::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Ev::Text(_) => {}
                Ev::Eof => return Err(Error::broken(self.part, "ends inside an element")),
            }
        }
    }

    /// After an `Open`: all text inside it, nested elements included, up to
    /// just past its `Close`.
    pub fn text(&mut self) -> Result<String> {
        let mut out = String::new();
        let mut depth = 1usize;
        loop {
            match self.next()? {
                Ev::Open(_) => depth += 1,
                Ev::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out);
                    }
                }
                Ev::Text(text) => out.push_str(&text),
                Ev::Eof => return Err(Error::broken(self.part, "ends inside an element")),
            }
        }
    }

    /// Move to the next element called `name`, at any depth. `None` at the
    /// end of the part.
    pub fn find(&mut self, name: &str) -> Result<Option<Tag<'a>>> {
        loop {
            match self.next()? {
                Ev::Open(tag) if tag.is(name) => return Ok(Some(tag)),
                Ev::Eof => return Ok(None),
                _ => {}
            }
        }
    }
}

/// After an `Open`: hand each child element to `f`, which must consume it
/// (read it to its close or skip it), and return after the parent's close.
pub fn each_child<'a>(
    reader: &mut XmlReader<'a>,
    mut f: impl FnMut(&mut XmlReader<'a>, Tag<'a>) -> Result<()>,
) -> Result<()> {
    loop {
        match reader.next()? {
            Ev::Open(tag) => f(reader, tag)?,
            Ev::Close => return Ok(()),
            Ev::Text(_) => {}
            Ev::Eof => return Err(Error::broken(reader.part(), "ends inside an element")),
        }
    }
}

/// The character or text an entity reference stands for.
fn general_ref(reference: &BytesRef) -> String {
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

/// OOXML escapes characters XML cannot hold as `_xHHHH_` (`_x000D_` for a
/// carriage return); `_x005F_` escapes the underscore itself.
pub fn decode_escapes(text: &str) -> Cow<'_, str> {
    if !text.contains("_x") {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_'
            && bytes.len() >= i + 7
            && bytes[i + 1] == b'x'
            && bytes[i + 6] == b'_'
            && bytes[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit)
        {
            let code = u32::from_str_radix(&text[i + 2..i + 6], 16).expect("hex digits");
            if let Some(ch) = char::from_u32(code) {
                out.push(ch);
                i += 7;
                continue;
            }
        }
        let len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_entities_and_empty_elements() {
        let xml = "\u{feff}<?xml version=\"1.0\"?><x:a xmlns:x=\"u\" v=\"1 &amp; 2\"><x:b/>t&lt;&#x6F22;<![CDATA[<c>]]></x:a>";
        let mut reader = XmlReader::new(xml.as_bytes(), "test.xml");
        let Ev::Open(a) = reader.next().unwrap() else {
            panic!("expected an element")
        };
        assert!(a.is("a"));
        assert_eq!(a.attr("v").as_deref(), Some("1 & 2"));
        let Ev::Open(b) = reader.next().unwrap() else {
            panic!("expected b")
        };
        assert!(b.is("b"));
        assert!(matches!(reader.next().unwrap(), Ev::Close));
        let mut text = String::new();
        while let Ev::Text(t) = reader.next().unwrap() {
            text.push_str(&t);
        }
        assert_eq!(text, "t<漢<c>");
    }

    #[test]
    fn deep_nesting_is_refused() {
        let xml = "<a>".repeat(MAX_DEPTH + 1);
        let mut reader = XmlReader::new(xml.as_bytes(), "deep.xml");
        let result = (0..=MAX_DEPTH).try_for_each(|_| reader.next().map(|_| ()));
        assert!(result.is_err());
    }

    #[test]
    fn ooxml_escapes() {
        assert_eq!(decode_escapes("a_x000D_b"), "a\rb");
        assert_eq!(decode_escapes("_x005F_x0041_"), "_x0041_");
        assert_eq!(decode_escapes("x_y_"), "x_y_");
        assert_eq!(decode_escapes("東_x0009_京"), "東\t京");
        assert_eq!(decode_escapes("_xZZZZ_"), "_xZZZZ_");
    }
}
