//! A part's elements as a tree, each with the byte range it has in the
//! file.
//!
//! WordprocessingML is read by looking around: a paragraph needs its
//! properties before its runs, a table needs all its rows before its grid
//! is known, a text box holds whole paragraphs. A tree makes that simple,
//! and the byte ranges are what saving needs. Attributes stay in the
//! start tag and are looked up when asked for.

use std::ops::Range;

use crate::error::{Error, Result};
use crate::xml::{Ev, Tag, XmlReader};

/// Elements a part may have before it is refused.
const MAX_NODES: usize = 3_000_000;

pub struct Node<'a> {
    tag: Tag<'a>,
    /// From the `<` of the start tag to just past the end tag, in the part's
    /// raw bytes (a byte order mark counts).
    pub start: usize,
    pub end: usize,
    pub children: Vec<Node<'a>>,
    /// Text directly inside; whitespace between elements is left out.
    pub text: String,
}

impl<'a> Node<'a> {
    pub fn local(&self) -> &[u8] {
        self.tag.local()
    }

    pub fn is(&self, name: &str) -> bool {
        self.tag.is(name)
    }

    pub fn attr(&self, name: &str) -> Option<String> {
        self.tag.attr(name)
    }

    /// Every attribute with this local name, whatever its prefix.
    pub fn attrs(&self, name: &str) -> Vec<String> {
        self.tag.attrs(name)
    }

    /// `w:val`, or whatever the element's value attribute is.
    pub fn val(&self) -> Option<String> {
        self.tag.attr("val")
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.tag.attr_i64(name)
    }

    pub fn float(&self, name: &str) -> Option<f64> {
        self.tag.attr_f64(name)
    }

    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    pub fn child(&self, name: &str) -> Option<&Node<'a>> {
        self.children.iter().find(|child| child.is(name))
    }

    pub fn each<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s Node<'a>> + 's {
        self.children.iter().filter(move |child| child.is(name))
    }

    /// A toggle element: present without a value or with a true value is
    /// on, `0`, `false` and `off` are off. `None` when it is not there.
    pub fn toggle(&self, name: &str) -> Option<bool> {
        self.child(name).map(Node::on)
    }

    /// This element as a toggle.
    pub fn on(&self) -> bool {
        !matches!(
            self.val().as_deref().map(str::trim),
            Some("0" | "false" | "off")
        )
    }

    /// The text of every `t` element under this one.
    pub fn collect_text(&self, out: &mut String) {
        if self.is("t") {
            out.push_str(&self.text);
        }
        for child in &self.children {
            child.collect_text(out);
        }
    }
}

/// Elements whose text counts even when it is only white space.
fn keeps_space(name: &[u8]) -> bool {
    matches!(
        name,
        b"t" | b"instrText" | b"delText" | b"delInstrText" | b"sym"
    )
}

/// The root element of a part.
pub fn parse<'a>(bytes: &'a [u8], part: &'a str) -> Result<Node<'a>> {
    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let mut reader = XmlReader::new(bytes, part);
    let mut stack: Vec<Node<'a>> = Vec::new();
    let mut count = 0usize;
    loop {
        match reader.next()? {
            Ev::Open(tag) => {
                count += 1;
                if count > MAX_NODES {
                    return Err(Error::TooLarge(format!("{part}: too many elements")));
                }
                stack.push(Node {
                    tag,
                    start: reader.opened_at() + bom,
                    end: 0,
                    children: Vec::new(),
                    text: String::new(),
                });
            }
            Ev::Close => {
                let Some(mut node) = stack.pop() else {
                    continue;
                };
                node.end = reader.position() + bom;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => return Ok(node),
                }
            }
            Ev::Text(text) => {
                if let Some(top) = stack.last_mut()
                    && (keeps_space(top.tag.local()) || !text.trim().is_empty())
                {
                    top.text.push_str(&text);
                }
            }
            Ev::Eof => return Err(Error::broken(part, "ends inside an element")),
        }
    }
}

/// A child to read, and the range it stands for: its own, or that of the
/// `AlternateContent` it was chosen from.
pub struct Item<'n, 'a> {
    pub node: &'n Node<'a>,
    pub range: Range<usize>,
}

/// `AlternateContent` names a few ways to say the same thing. The choice is
/// the first one whose required namespaces we understand, else the
/// fallback, else the first choice.
fn chosen<'n, 'a>(content: &'n Node<'a>) -> Option<&'n Node<'a>> {
    const UNDERSTOOD: [&str; 10] = [
        "wps", "wpg", "wp14", "w14", "w15", "w16se", "w16cid", "w16", "wpc", "a14",
    ];
    let understood = |choice: &Node<'_>| {
        choice
            .attr("Requires")
            .is_some_and(|req| req.split_whitespace().all(|p| UNDERSTOOD.contains(&p)))
    };
    content
        .each("Choice")
        .find(|choice| understood(choice))
        .or_else(|| content.child("Fallback"))
        .or_else(|| content.child("Choice"))
}

/// The children of `children` to look at, with `AlternateContent` replaced
/// by what it chose, and (when `descend` says so) containers such as
/// content controls replaced by their content.
pub fn flatten<'n, 'a>(
    children: &'n [Node<'a>],
    descend: &dyn Fn(&Node<'a>) -> bool,
    out: &mut Vec<Item<'n, 'a>>,
) {
    flatten_in(children, descend, out, None, 0);
}

fn flatten_in<'n, 'a>(
    children: &'n [Node<'a>],
    descend: &dyn Fn(&Node<'a>) -> bool,
    out: &mut Vec<Item<'n, 'a>>,
    outer: Option<Range<usize>>,
    depth: usize,
) {
    for child in children {
        if depth < 24 {
            if child.is("AlternateContent") {
                if let Some(branch) = chosen(child) {
                    let outer = outer.clone().or_else(|| Some(child.range()));
                    flatten_in(&branch.children, descend, out, outer, depth + 1);
                }
                continue;
            }
            if descend(child) {
                // A content control keeps its content in `sdtContent`.
                let inner = if child.is("sdt") {
                    child.child("sdtContent").map_or(&[][..], |c| &c.children)
                } else {
                    &child.children[..]
                };
                flatten_in(inner, descend, out, outer.clone(), depth + 1);
                continue;
            }
        }
        out.push(Item {
            node: child,
            range: outer.clone().unwrap_or_else(|| child.range()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_the_elements_bytes() {
        let xml = "\u{feff}<?xml version=\"1.0\"?>\n<w:a xmlns:w=\"u\">\n <w:b x=\"1\"/><w:c>t&amp;u</w:c>\n</w:a>";
        let bytes = xml.as_bytes();
        let root = parse(bytes, "t.xml").unwrap();
        assert!(root.is("a"));
        assert_eq!(&xml[root.start..root.end], &xml[xml.find("<w:a").unwrap()..]);
        let b = root.child("b").unwrap();
        assert_eq!(&xml[b.start..b.end], "<w:b x=\"1\"/>");
        let c = root.child("c").unwrap();
        assert_eq!(&xml[c.start..c.end], "<w:c>t&amp;u</w:c>");
        assert_eq!(c.text, "t&u");
        assert!(root.text.is_empty());
    }

    #[test]
    fn a_part_that_ends_early_is_an_error() {
        assert!(parse(b"<a><b>", "t.xml").is_err());
    }

    #[test]
    fn alternate_content_picks_what_we_understand() {
        let xml = r#"<p xmlns:mc="m"><mc:AlternateContent><mc:Choice Requires="wps"><x/></mc:Choice><mc:Fallback><y/></mc:Fallback></mc:AlternateContent><mc:AlternateContent><mc:Choice Requires="foo"><x/></mc:Choice><mc:Fallback><y/></mc:Fallback></mc:AlternateContent></p>"#;
        let root = parse(xml.as_bytes(), "t.xml").unwrap();
        let mut out = Vec::new();
        flatten(&root.children, &|_| false, &mut out);
        let names: Vec<_> = out.iter().map(|i| i.node.local().to_vec()).collect();
        assert_eq!(names, vec![b"x".to_vec(), b"y".to_vec()]);
        assert_eq!(&xml[out[0].range.clone()][..24], "<mc:AlternateContent><mc");
    }
}
