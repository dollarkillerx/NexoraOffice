//! The look of a link in a saved file: the character style alone when the
//! document has it, direct colour and underline alone when it does not.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;

use std::io::{Cursor, Read};

use aster_ooxml::docx_save::save;
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{Document, Link, Path, Pos, UnderlineStyle};

const WITH_STYLE: &str = r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="160"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>"#;
const WITHOUT_STYLE: &str = r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="160"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#;

const BODY: &str = r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>this thing</w:t></w:r><w:r><w:t xml:space="preserve"> now</w:t></w:r></w:p>"#;

fn file(styles: &str) -> Vec<u8> {
    docx_doc::doc(BODY).styles(styles).bytes()
}

fn read(bytes: &[u8]) -> Document {
    Docx.read(bytes.to_vec()).unwrap()
}

fn main_xml(bytes: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut text = String::new();
    archive
        .by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn target() -> Link {
    Link {
        target: "https://example.org/".to_owned(),
        tooltip: None,
    }
}

fn range(doc: &Document) -> (Pos, Pos) {
    let text = doc.paragraph(&Path::body(0)).unwrap().text();
    let at = text.find("this").unwrap();
    (
        Pos::new(Path::body(0), at),
        Pos::new(Path::body(0), at + "this thing".len()),
    )
}

fn linked_run(doc: &Document) -> aster_writer::Run {
    doc.paragraph(&Path::body(0))
        .unwrap()
        .runs
        .iter()
        .find(|r| r.link.is_some())
        .expect("a linked run")
        .clone()
}

#[test]
fn a_link_uses_the_character_style_alone_when_the_document_has_it() {
    let bytes = file(WITH_STYLE);
    let mut doc = read(&bytes);
    let (from, to) = range(&doc);
    set_link(&mut doc, &from, &to, Some(target()));
    let saved = save(bytes.clone(), &doc).unwrap();
    let xml = main_xml(&saved);
    assert!(xml.contains("<w:rStyle w:val=\"Hyperlink\"/>"));
    assert!(!xml.contains("<w:color"), "{xml}");
    assert!(!xml.contains("<w:u "), "{xml}");
    // The bold it had stays.
    assert!(xml.contains("<w:b/>"));
    // Read back, the run looks like a link all the same.
    let run = linked_run(&read(&saved));
    assert_eq!(run.style.as_deref(), Some("Hyperlink"));
    assert_eq!(run.look.color, Some([0x05, 0x63, 0xC1, 0xFF]));
    assert_eq!(run.look.underline.unwrap().style, UnderlineStyle::Single);
    assert!(run.look.bold);
}

#[test]
fn a_link_has_direct_colour_and_underline_alone_when_there_is_no_style() {
    let bytes = file(WITHOUT_STYLE);
    let mut doc = read(&bytes);
    let (from, to) = range(&doc);
    set_link(&mut doc, &from, &to, Some(target()));
    let saved = save(bytes.clone(), &doc).unwrap();
    let xml = main_xml(&saved);
    assert!(!xml.contains("rStyle"), "{xml}");
    assert!(xml.contains("<w:color w:val=\"0563C1\"/>"));
    assert!(xml.contains("<w:u w:val=\"single\"/>"));
    let run = linked_run(&read(&saved));
    assert_eq!(run.style, None);
    assert_eq!(run.look.color, Some([0x05, 0x63, 0xC1, 0xFF]));
}

#[test]
fn taking_a_link_off_takes_off_whichever_look_it_had() {
    for styles in [WITH_STYLE, WITHOUT_STYLE] {
        let bytes = file(styles);
        let mut doc = read(&bytes);
        let (from, to) = range(&doc);
        set_link(&mut doc, &from, &to, Some(target()));
        let linked = save(bytes, &doc).unwrap();
        let mut doc = read(&linked);
        let (from, to) = range(&doc);
        set_link(&mut doc, &from, &to, None);
        let saved = save(linked, &doc).unwrap();
        let xml = main_xml(&saved);
        assert!(!xml.contains("hyperlink"), "{xml}");
        assert!(!xml.contains("rStyle"), "{xml}");
        assert!(!xml.contains("<w:color"), "{xml}");
        assert!(!xml.contains("<w:u "), "{xml}");
        assert!(xml.contains("<w:b/>"));
        let back = read(&saved);
        let run = back
            .paragraph(&Path::body(0))
            .unwrap()
            .runs
            .iter()
            .find(|r| r.text == "this thing")
            .unwrap()
            .clone();
        assert_eq!(run.look.color, None);
        assert_eq!(run.look.underline, None);
        assert!(run.look.bold && run.style.is_none() && run.link.is_none());
    }
}

#[test]
fn typed_link_text_has_the_look_a_link_has() {
    let bytes = file(WITH_STYLE);
    let mut doc = read(&bytes);
    let at = Pos::new(Path::body(0), 0);
    insert_link(&mut doc, &at, "new", target());
    let xml = main_xml(&save(bytes, &doc).unwrap());
    assert!(xml.contains("<w:rStyle w:val=\"Hyperlink\"/>"));
    assert!(!xml.contains("<w:color"), "{xml}");
    assert!(!xml.contains("<w:u "), "{xml}");
}

#[test]
fn a_run_with_a_character_style_of_its_own_gets_the_direct_look() {
    // A run has one character style: the link cannot have it as well.
    let body = r#"<w:p><w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>loud</w:t></w:r></w:p>"#;
    let styles = format!(
        "{WITH_STYLE}<w:style w:type=\"character\" w:styleId=\"Strong\"><w:name w:val=\"Strong\"/><w:rPr><w:b/></w:rPr></w:style>"
    );
    let bytes = docx_doc::doc(body).styles(&styles).bytes();
    let mut doc = read(&bytes);
    set_link(
        &mut doc,
        &Pos::new(Path::body(0), 0),
        &Pos::new(Path::body(0), 4),
        Some(target()),
    );
    let xml = main_xml(&save(bytes, &doc).unwrap());
    assert!(xml.contains("<w:rStyle w:val=\"Strong\"/>"));
    assert!(!xml.contains("Hyperlink"));
    assert!(xml.contains("<w:color w:val=\"0563C1\"/>"));
}
