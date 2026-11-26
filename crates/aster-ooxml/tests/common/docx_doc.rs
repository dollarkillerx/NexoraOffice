//! Building Word documents from XML strings in tests.

#![allow(dead_code)]

use aster_ooxml::{Docx, FormatCodec, Package};
use aster_writer::*;

use super::common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

pub const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:w10="urn:schemas-microsoft-com:office:word""#;

pub const MAIN_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

/// A document made of parts written as XML strings.
#[derive(Default, Clone)]
pub struct Doc {
    pub main: String,
    pub body: String,
    styles: Option<String>,
    numbering: Option<String>,
    settings: Option<String>,
    theme: Option<String>,
    footnotes: Option<String>,
    endnotes: Option<String>,
    /// (part name, xml), related from the main part as headers or footers.
    pub parts: Vec<(String, String)>,
    /// (id, kind, target) of the main part's relationships.
    pub rels: Vec<(String, String, String)>,
    /// Parts that are not XML.
    pub binary: Vec<(String, Vec<u8>)>,
    pub bom: bool,
}

pub fn doc(body: &str) -> Doc {
    Doc {
        main: "word/document.xml".to_owned(),
        body: body.to_owned(),
        ..Doc::default()
    }
}

impl Doc {
    pub fn styles(mut self, inner: &str) -> Self {
        self.styles = Some(format!(r#"<w:styles {NS}>{inner}</w:styles>"#));
        self
    }
    pub fn numbering(mut self, inner: &str) -> Self {
        self.numbering = Some(format!(r#"<w:numbering {NS}>{inner}</w:numbering>"#));
        self
    }
    pub fn settings(mut self, inner: &str) -> Self {
        self.settings = Some(format!(r#"<w:settings {NS}>{inner}</w:settings>"#));
        self
    }
    pub fn theme(mut self, xml: &str) -> Self {
        self.theme = Some(xml.to_owned());
        self
    }
    pub fn footnotes(mut self, inner: &str) -> Self {
        self.footnotes = Some(format!(r#"<w:footnotes {NS}>{inner}</w:footnotes>"#));
        self
    }
    pub fn endnotes(mut self, inner: &str) -> Self {
        self.endnotes = Some(format!(r#"<w:endnotes {NS}>{inner}</w:endnotes>"#));
        self
    }
    /// A header or footer part, related with `id`.
    pub fn marginal(mut self, id: &str, kind: &str, name: &str, inner: &str) -> Self {
        let root = if kind == "header" { "hdr" } else { "ftr" };
        self.parts.push((
            format!("word/{name}"),
            format!(r#"<w:{root} {NS}>{inner}</w:{root}>"#),
        ));
        self.rels
            .push((id.to_owned(), kind.to_owned(), name.to_owned()));
        self
    }
    pub fn rel(mut self, id: &str, kind: &str, target: &str) -> Self {
        self.rels
            .push((id.to_owned(), kind.to_owned(), target.to_owned()));
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NS}><w:body>{}</w:body></w:document>"#,
            self.body
        )
        .into_bytes();
        if self.bom {
            let mut with = b"\xEF\xBB\xBF".to_vec();
            with.append(&mut document);
            document = with;
        }
        let mut rel_list: Vec<(String, String, String)> = self.rels.clone();
        let mut parts: Vec<(String, Vec<u8>)> = vec![(self.main.clone(), document)];
        let mut optional = |present: &Option<String>, id: &str, kind: &str, name: &str| {
            if let Some(xml) = present {
                rel_list.push((id.to_owned(), kind.to_owned(), name.to_owned()));
                parts.push((format!("word/{name}"), xml.clone().into_bytes()));
            }
        };
        optional(&self.styles, "rIdStyles", "styles", "styles.xml");
        optional(
            &self.numbering,
            "rIdNumbering",
            "numbering",
            "numbering.xml",
        );
        optional(&self.settings, "rIdSettings", "settings", "settings.xml");
        optional(&self.theme, "rIdTheme", "theme", "theme/theme1.xml");
        optional(
            &self.footnotes,
            "rIdFootnotes",
            "footnotes",
            "footnotes.xml",
        );
        optional(&self.endnotes, "rIdEndnotes", "endnotes", "endnotes.xml");
        for (name, xml) in &self.parts {
            parts.push((name.clone(), xml.clone().into_bytes()));
        }
        for (name, bytes) in &self.binary {
            parts.push((name.clone(), bytes.clone()));
        }
        let rel_refs: Vec<(&str, &str, &str)> = rel_list
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect();
        let main_dir = self.main.rsplit_once('/').map_or("", |(d, _)| d);
        let rels_name = match self.main.rsplit_once('/') {
            Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
            None => format!("_rels/{}.rels", self.main),
        };
        let _ = main_dir;
        let mut rels_xml = rels(&rel_refs);
        // External targets are written by hand.
        for (id, kind, target) in &self.rels {
            if kind == "hyperlink" {
                let plain = format!(
                    r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{target}"/>"#
                );
                rels_xml = rels_xml.replace(&plain, &format!(
                    r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{target}" TargetMode="External"/>"#
                ));
            }
        }
        let content_types = format!(
            r#"{CONTENT_TYPES_HEAD}<Override PartName="/{}" ContentType="{MAIN_CT}"/></Types>"#,
            self.main
        );
        let root = root_rels(&self.main);
        let mut all: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", root.as_bytes()),
            (rels_name.as_str(), rels_xml.as_bytes()),
        ];
        for (name, bytes) in &parts {
            all.push((name.as_str(), bytes.as_slice()));
        }
        package(&all)
    }

    pub fn read(&self) -> Document {
        Docx.read(self.bytes()).expect("the document reads")
    }
}

pub fn read(body: &str) -> Document {
    doc(body).read()
}

pub fn p(d: &Document, i: usize) -> &Paragraph {
    d.body.blocks[i].as_paragraph().expect("a paragraph")
}

pub fn t(d: &Document, i: usize) -> &Table {
    d.body.blocks[i].as_table().expect("a table")
}

pub fn run_texts(paragraph: &Paragraph) -> Vec<String> {
    paragraph.runs.iter().map(|r| r.text.clone()).collect()
}

pub fn kinds(paragraph: &Paragraph) -> Vec<&'static str> {
    paragraph
        .runs
        .iter()
        .map(|r| match &r.kind {
            Inline::Text => "text",
            Inline::Tab => "tab",
            Inline::Break(Break::Line) => "line",
            Inline::Break(Break::Page) => "page",
            Inline::Break(Break::Column) => "column",
            Inline::Drawing(_) => "drawing",
            Inline::Footnote(_) => "footnote",
            Inline::Endnote(_) => "endnote",
            Inline::PageNumber(_) => "pagenumber",
            Inline::Ruby(_) => "ruby",
            Inline::Marker => "marker",
        })
        .collect()
}

pub fn rgb(hex: u32) -> Rgba {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255]
}

pub fn raw<'a>(bytes: &'a [u8], range: &std::ops::Range<usize>) -> &'a str {
    std::str::from_utf8(&bytes[range.clone()]).expect("a range of whole characters")
}

/// A part of a package, as the package returns it.
pub fn common_part(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut package = Package::open(bytes.to_vec()).unwrap();
    package.read(name).unwrap()
}
