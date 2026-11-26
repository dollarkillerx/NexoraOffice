//! Saving pictures that were moved, resized, wrapped differently, copied or
//! put in the line or out of it: the element says so, and what the model
//! has no field for (the graphic and what is under it) is the file's bytes.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;
#[path = "common/docx_sample.rs"]
mod docx_sample;

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::Arc;

use aster_ooxml::docx_save::save;
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{Document, FloatAt, FloatFrom, Inline, OBJECT, Path, Pos, Wrap};

fn parts(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        out.insert(entry.name().to_owned(), data);
    }
    out
}

fn main_xml(bytes: &[u8]) -> String {
    String::from_utf8(parts(bytes)["word/document.xml"].clone()).unwrap()
}

fn read(bytes: &[u8]) -> Document {
    Docx.read(bytes.to_vec()).unwrap()
}

/// The graphic of the pictures, with things the model knows nothing about:
/// an adjustment of the picture and a shadow.
const GRAPHIC: &str = r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="pic.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rIdImg"><a:lum bright="10"/></a:blip><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:effectLst><a:outerShdw blurRad="50800" dist="38100"/></a:effectLst></pic:spPr></pic:pic></a:graphicData></a:graphic>"#;

fn inline_run() -> String {
    format!(
        r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Picture 1" descr="d"/><wp:cNvGraphicFramePr/>{GRAPHIC}</wp:inline></w:drawing></w:r>"#
    )
}

fn anchor_run() -> String {
    format!(
        r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="5" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="leftMargin"><wp:posOffset>100000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:align>center</wp:align></wp:positionV><wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapSquare wrapText="largest"/><wp:docPr id="2" name="Picture 2"/><wp:cNvGraphicFramePr/>{GRAPHIC}</wp:anchor></w:drawing></w:r>"#
    )
}

fn file(first: &str) -> Vec<u8> {
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r>{first}<w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p>"#
    );
    let mut d = docx_doc::doc(&body).rel("rIdImg", "image", "media/pic.png");
    d.binary
        .push(("word/media/pic.png".to_owned(), docx_sample::png(24, 12)));
    d.bytes()
}

/// Where the picture of the first paragraph is.
fn at() -> Pos {
    Pos::new(Path::body(0), "before ".len())
}

/// Save, and check what holds for every save here: the file reads back as
/// the model, and the graphic is the file's bytes.
fn saved(source: &[u8], doc: &Document) -> (Vec<u8>, String) {
    let bytes = save(source.to_vec(), doc).expect("the document saves");
    let back = read(&bytes);
    assert!(same_content(doc, &back), "{}", main_xml(&bytes));
    let xml = main_xml(&bytes);
    if !xml.contains("Picture 3") {
        assert!(xml.contains(GRAPHIC), "the graphic changed\n{xml}");
    }
    (bytes, xml)
}

fn float_of(doc: &Document) -> aster_writer::Float {
    drawing_at(doc, &at()).unwrap().float.unwrap()
}

#[test]
fn a_picture_in_the_line_can_float_and_come_back() {
    let bytes = file(&inline_run());
    let mut doc = read(&bytes);
    assert!(set_picture_layout(&mut doc, &at(), PictureLayout::Square));
    let (bytes2, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<wp:anchor ") && !xml.contains("<wp:inline"));
    assert!(xml.contains(
        r#"<wp:positionH relativeFrom="character"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="line"><wp:posOffset>0</wp:posOffset></wp:positionV>"#
    ));
    assert!(xml.contains(r#"<wp:wrapSquare wrapText="bothSides"/>"#));
    assert!(xml.contains(r#"distL="114300""#));
    // The properties of the drawing stay.
    assert!(xml.contains(r#"<wp:docPr id="1" name="Picture 1" descr="d"/>"#));
    assert!(xml.contains(r#"<wp:extent cx="914400" cy="457200"/>"#));
    let back = read(&bytes2);
    assert_eq!(
        picture_layout(drawing_at(&back, &at()).unwrap()),
        PictureLayout::Square
    );
    // And back into the line.
    let mut doc = back;
    assert!(set_picture_layout(&mut doc, &at(), PictureLayout::Inline));
    let (_, xml) = saved(&bytes2, &doc);
    assert!(xml.contains("<wp:inline ") && !xml.contains("<wp:anchor"));
    assert!(xml.contains(r#"<wp:docPr id="1" name="Picture 1" descr="d"/>"#));
}

#[test]
fn every_wrap_is_written_and_read_back() {
    let bytes = file(&inline_run());
    for layout in [
        PictureLayout::Square,
        PictureLayout::Tight,
        PictureLayout::TopAndBottom,
        PictureLayout::BehindText,
        PictureLayout::InFrontOfText,
    ] {
        let mut doc = read(&bytes);
        set_picture_layout(&mut doc, &at(), layout);
        let (bytes2, xml) = saved(&bytes, &doc);
        assert_eq!(
            picture_layout(drawing_at(&read(&bytes2), &at()).unwrap()),
            layout
        );
        assert_eq!(
            xml.contains("behindDoc=\"1\""),
            layout == PictureLayout::BehindText
        );
        // Changing the wrap of the floating one keeps its position bytes.
        let mut doc = read(&bytes2);
        set_picture_layout(&mut doc, &at(), PictureLayout::Square);
        saved(&bytes2, &doc);
    }
}

#[test]
fn a_floating_picture_moved_keeps_every_other_byte_of_its_element() {
    let bytes = file(&anchor_run());
    let mut doc = read(&bytes);
    let before = float_of(&doc);
    assert_eq!(before.x.at, FloatAt::Offset(100_000));
    assert_eq!(before.y.at, FloatAt::Center);
    assert!(move_floating_drawing(
        &mut doc,
        &at(),
        (50_000, 20_000),
        (0, 3_000_000)
    ));
    let (bytes2, xml) = saved(&bytes, &doc);
    // The reference the file named stays; the offsets are the new ones.
    assert!(xml.contains(
        r#"<wp:positionH relativeFrom="leftMargin"><wp:posOffset>150000</wp:posOffset></wp:positionH>"#
    ));
    assert!(xml.contains(
        r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>1291400</wp:posOffset></wp:positionV>"#
    ));
    // What did not change is as it was.
    assert!(xml.contains(r#"<wp:wrapSquare wrapText="largest"/>"#));
    assert!(xml.contains(r#"relativeHeight="5""#));
    assert!(xml.contains(r#"<wp:docPr id="2" name="Picture 2"/>"#));
    assert!(xml.contains(r#"distL="114300" distR="114300""#));
    let after = float_of(&read(&bytes2));
    assert_eq!(after.x.at, FloatAt::Offset(150_000));
    assert_eq!(after.y.from, FloatFrom::Paragraph);
    assert_eq!(after.wrap, Wrap::Square);
}

#[test]
fn a_floating_picture_resized_keeps_its_place_and_wrap() {
    let bytes = file(&anchor_run());
    let mut doc = read(&bytes);
    resize_drawing(&mut doc, &at(), (1_828_800, 914_400));
    let bytes2 = save(bytes.clone(), &doc).unwrap();
    assert!(same_content(&doc, &read(&bytes2)));
    let xml = main_xml(&bytes2);
    assert!(xml.contains(r#"<wp:extent cx="1828800" cy="914400"/>"#));
    assert!(xml.contains(r#"<a:ext cx="1828800" cy="914400"/>"#));
    assert!(xml.contains(r#"<wp:wrapSquare wrapText="largest"/>"#));
    assert!(xml.contains(r#"<wp:posOffset>100000</wp:posOffset>"#));
    assert!(xml.contains("<a:outerShdw blurRad=\"50800\" dist=\"38100\"/>"));
}

#[test]
fn a_floating_picture_can_go_behind_the_text_or_into_the_line() {
    let bytes = file(&anchor_run());
    let mut doc = read(&bytes);
    set_picture_layout(&mut doc, &at(), PictureLayout::BehindText);
    let (bytes2, xml) = saved(&bytes, &doc);
    assert!(xml.contains(r#"behindDoc="1""#) && xml.contains("<wp:wrapNone/>"));
    assert!(xml.contains(r#"<wp:positionV relativeFrom="paragraph"><wp:align>center</wp:align>"#));
    assert_eq!(
        picture_layout(drawing_at(&read(&bytes2), &at()).unwrap()),
        PictureLayout::BehindText
    );
    let mut doc = read(&bytes);
    set_picture_layout(&mut doc, &at(), PictureLayout::Inline);
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<wp:inline ") && !xml.contains("<wp:anchor"));
    assert!(!xml.contains("positionH") && !xml.contains("wrapSquare"));
    assert!(xml.contains(r#"<wp:docPr id="2" name="Picture 2"/>"#));
}

#[test]
fn a_picture_moved_in_the_text_is_written_where_it_is_now() {
    let bytes = file(&inline_run());
    let mut doc = read(&bytes);
    let now = move_drawing(&mut doc, &at(), &Pos::new(Path::body(1), 3)).unwrap();
    assert_eq!(now, Pos::new(Path::body(1), 3));
    let (_, xml) = saved(&bytes, &doc);
    assert_eq!(xml.matches("<wp:inline").count(), 1);
    let first = xml.find("</w:p>").unwrap();
    assert!(
        xml.find("<wp:inline").unwrap() > first,
        "it is in the second paragraph"
    );
    assert_eq!(
        doc.paragraph(&Path::body(1)).unwrap().text(),
        format!("sec{OBJECT}ond")
    );
    // Within its paragraph.
    let mut doc = read(&bytes);
    move_drawing(&mut doc, &at(), &Pos::new(Path::body(0), 0)).unwrap();
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.find("<wp:inline").unwrap() < xml.find("before ").unwrap());
}

#[test]
fn a_pasted_duplicate_has_its_own_id_and_shares_the_image() {
    let bytes = file(&inline_run());
    let mut doc = read(&bytes);
    let from = Pos::new(Path::body(0), "before ".len());
    let to = Pos::new(Path::body(0), "before ".len() + OBJECT.len());
    let fragment = fragment(&doc, &from, &to);
    insert_fragment(&mut doc, &Pos::new(Path::body(1), 3), &fragment);
    insert_fragment(&mut doc, &Pos::new(Path::body(1), 0), &fragment);
    let (bytes2, xml) = saved(&bytes, &doc);
    assert_eq!(xml.matches("<wp:inline").count(), 3);
    assert_eq!(xml.matches(GRAPHIC).count(), 3);
    let mut ids: Vec<&str> = xml
        .split("<wp:docPr id=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "{ids:?}");
    // No image part was added.
    let before = parts(&bytes);
    let after = parts(&bytes2);
    let media =
        |m: &BTreeMap<String, Vec<u8>>| m.keys().filter(|k| k.starts_with("word/media/")).count();
    assert_eq!(media(&before), media(&after));
    assert_eq!(
        before["word/_rels/document.xml.rels"],
        after["word/_rels/document.xml.rels"]
    );
    assert_eq!(before["[Content_Types].xml"], after["[Content_Types].xml"]);
}

#[test]
fn a_picture_with_the_bytes_the_file_has_uses_that_image() {
    let bytes = file(&inline_run());
    let mut doc = read(&bytes);
    let image: Arc<[u8]> = Arc::from(docx_sample::png(24, 12));
    insert_picture(
        &mut doc,
        &Pos::new(Path::body(1), 0),
        image.clone(),
        (1000, 500),
        None,
    )
    .unwrap();
    insert_picture(
        &mut doc,
        &Pos::new(Path::body(1), 0),
        image,
        (1000, 500),
        None,
    )
    .unwrap();
    let (bytes2, xml) = saved(&bytes, &doc);
    assert_eq!(xml.matches("r:embed=\"rIdImg\"").count(), 3);
    assert_eq!(
        parts(&bytes2)
            .keys()
            .filter(|k| k.starts_with("word/media/"))
            .count(),
        1
    );
}

#[test]
fn pictures_made_here_are_stored_once_and_can_float() {
    let bytes = file(&inline_run());
    let mut doc = read(&bytes);
    let other: Arc<[u8]> = Arc::from(docx_sample::png(10, 10));
    let p = Pos::new(Path::body(1), 0);
    let end = insert_picture(
        &mut doc,
        &p,
        other.clone(),
        (3000, 2000),
        Some("new".into()),
    )
    .unwrap();
    insert_picture(&mut doc, &end, other, (3000, 2000), None).unwrap();
    assert!(set_picture_layout(
        &mut doc,
        &p,
        PictureLayout::TopAndBottom
    ));
    move_floating_drawing(&mut doc, &p, (10, 20), (0, 0));
    let (bytes2, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<wp:anchor "));
    assert!(xml.contains("<wp:wrapTopAndBottom/>"));
    assert!(xml.contains("<wp:posOffset>10</wp:posOffset>"));
    let added: Vec<_> = parts(&bytes2)
        .keys()
        .filter(|k| k.starts_with("word/media/"))
        .cloned()
        .collect();
    assert_eq!(added.len(), 2, "{added:?}");
    let back = read(&bytes2);
    let first = drawing_at(&back, &p).unwrap();
    assert_eq!(picture_layout(first), PictureLayout::TopAndBottom);
    assert_eq!(first.alt.as_deref(), Some("new"));
    assert!(matches!(
        back.paragraph(&Path::body(1)).unwrap().runs[0].kind,
        Inline::Drawing(_)
    ));
}

#[test]
fn a_picture_pasted_from_another_file_is_that_picture_not_the_one_at_its_place() {
    // Two files laid out alike, with different images at the same place.
    let a = file(&inline_run());
    let mut d = docx_doc::doc(&format!(
        r#"<w:p><w:r><w:t xml:space="preserve">before </w:t></w:r>{}<w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p>"#,
        inline_run()
    ))
    .rel("rIdImg", "image", "media/pic.png");
    d.binary
        .push(("word/media/pic.png".to_owned(), docx_sample::png(40, 40)));
    let b = d.bytes();
    let doc_a = read(&a);
    let mut doc_b = read(&b);
    let from = Pos::new(Path::body(0), "before ".len());
    let to = Pos::new(Path::body(0), "before ".len() + OBJECT.len());
    let fragment = fragment(&doc_a, &from, &to);
    insert_fragment(&mut doc_b, &Pos::new(Path::body(1), 3), &fragment);
    let bytes = save(b.clone(), &doc_b).unwrap();
    let back = read(&bytes);
    let image_of = |doc: &Document, block: usize, offset: usize| match &drawing_at(
        doc,
        &Pos::new(Path::body(block), offset),
    )
    .unwrap()
    .content
    {
        aster_writer::DrawingContent::Picture(p) => p.image.clone(),
        other => panic!("{other:?}"),
    };
    let mine: Arc<[u8]> = Arc::from(docx_sample::png(24, 12));
    assert_eq!(image_of(&back, 1, 3), mine);
    assert_eq!(
        &*image_of(&back, 0, "before ".len()),
        &*docx_sample::png(40, 40)
    );
}

#[test]
fn a_picture_that_was_not_changed_is_the_file_it_was() {
    for run in [inline_run(), anchor_run()] {
        let bytes = file(&run);
        let doc = read(&bytes);
        assert_eq!(save(bytes.clone(), &doc).unwrap(), bytes);
    }
}
