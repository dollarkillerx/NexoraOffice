//! Saving edited presentations: what changes in the package and what stays
//! byte for byte, and that the file reads back as edited.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::Arc;

use aster_ooxml::pptx_save::{blank, save};
use aster_ooxml::{FormatCodec, Pptx};
use aster_slides::edit::TextPos;
use aster_slides::{Fill, Frame, Geometry, List, Outline, Picture, Presentation, Role, Shape};

const NAMES: [&str; 5] = [
    "Title Slide",
    "Title and Content",
    "Section Header",
    "Title Only",
    "Blank",
];

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

/// The parts that differ (changed, added or removed) between two packages.
fn changed(before: &[u8], after: &[u8]) -> Vec<String> {
    let (a, b) = (parts(before), parts(after));
    let mut out: Vec<String> = a
        .keys()
        .chain(b.keys())
        .filter(|name| a.get(*name) != b.get(*name))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

fn text(bytes: &[u8], part: &str) -> String {
    String::from_utf8(parts(bytes)[part].clone()).unwrap()
}

fn read(bytes: &[u8]) -> Presentation {
    Pptx.read(bytes.to_vec()).unwrap()
}

fn title_of(presentation: &Presentation, slide: usize) -> String {
    presentation.slides[slide]
        .shapes
        .iter()
        .find(|s| {
            s.placeholder
                .as_ref()
                .is_some_and(|p| p.kind.contains("itle"))
        })
        .and_then(|s| s.text.as_ref())
        .map(|b| b.plain_text())
        .unwrap_or_default()
}

/// Type into the first placeholder of kind `kind` on slide `index`.
fn type_into(presentation: &mut Presentation, index: usize, kind: &str, text: &str) {
    let shape = presentation.slides[index]
        .shapes
        .iter_mut()
        .find(|s| s.placeholder.as_ref().is_some_and(|p| p.kind == kind))
        .unwrap();
    let body = shape.text.as_mut().unwrap();
    let end = body.end();
    body.insert(end, text);
}

#[test]
fn a_new_presentation_reads_and_saves_unchanged() {
    let source = blank::presentation(&NAMES);
    let presentation = read(&source);
    assert_eq!(presentation.slides.len(), 1);
    assert_eq!(presentation.size, (12_192_000, 6_858_000));
    let names: Vec<_> = presentation
        .layouts
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(names, NAMES);
    // The first slide is a title slide with two empty placeholders.
    let slide = &presentation.slides[0];
    assert_eq!(slide.layout, Some(0));
    let kinds: Vec<_> = slide
        .shapes
        .iter()
        .filter(|s| !s.inherited)
        .map(|s| s.placeholder.as_ref().unwrap().kind.clone())
        .collect();
    assert_eq!(kinds, ["ctrTitle", "subTitle"]);
    let title = &slide.shapes[0];
    assert_eq!(
        title.frame,
        Frame::new(1_524_000, 1_122_363, 9_144_000, 2_387_600)
    );
    assert_eq!(title.text.as_ref().unwrap().paragraphs[0].end.size, 60.0);
    assert!(title.text.as_ref().unwrap().is_empty());
    let saved = save(source.clone(), &presentation).unwrap();
    assert_eq!(changed(&source, &saved), Vec::<String>::new());
    assert_eq!(saved.len(), source.len());
}

#[test]
fn typing_changes_only_that_slide() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    type_into(&mut presentation, 0, "ctrTitle", "第三季度报告 & <更多>");
    type_into(&mut presentation, 0, "subTitle", "東京オフィス");
    let saved = save(source.clone(), &presentation).unwrap();
    assert_eq!(changed(&source, &saved), ["ppt/slides/slide1.xml"]);
    let xml = text(&saved, "ppt/slides/slide1.xml");
    assert!(xml.contains("第三季度报告 &amp; &lt;更多&gt;"), "{xml}");
    assert!(xml.contains(r#"lang="ja-JP""#), "{xml}");
    let again = read(&saved);
    assert_eq!(title_of(&again, 0), "第三季度报告 & <更多>");
    // The same text in the same size: nothing written explicitly.
    let title = &again.slides[0].shapes[0].text.as_ref().unwrap().paragraphs[0];
    assert_eq!(title.runs[0].size, 60.0);
    assert!(!xml.contains("sz="), "{xml}");
    // Saving again from the file as read gives the same.
    assert_eq!(parts(&save(source, &presentation).unwrap()), parts(&saved));
}

#[test]
fn formatting_keeps_what_did_not_change() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    type_into(&mut presentation, 0, "ctrTitle", "Hello world");
    let first = save(source, &presentation).unwrap();
    // Read what was saved and format a word of it.
    let mut presentation = read(&first);
    let body = presentation.slides[0].shapes[0].text.as_mut().unwrap();
    body.format(TextPos::new(0, 6), TextPos::new(0, 11), |run| {
        run.bold = true;
        run.color = Some([0xC0, 0, 0, 0xFF]);
        run.size = 40.0;
    });
    body.paragraphs[0].align = aster_slides::Align::Right;
    let saved = save(first.clone(), &presentation).unwrap();
    assert_eq!(changed(&first, &saved), ["ppt/slides/slide1.xml"]);
    let xml = text(&saved, "ppt/slides/slide1.xml");
    assert!(xml.contains(r#"b="1""#), "{xml}");
    assert!(xml.contains(r#"sz="4000""#), "{xml}");
    assert!(xml.contains(r#"<a:srgbClr val="C00000"/>"#), "{xml}");
    assert!(xml.contains(r#"algn="r""#), "{xml}");
    // The subtitle, not touched, is as it was.
    let subtitle = |xml: &str| {
        let at = xml.find("Subtitle 2").unwrap();
        xml[at..].to_owned()
    };
    assert_eq!(
        subtitle(&xml),
        subtitle(&text(&first, "ppt/slides/slide1.xml"))
    );
    let again = read(&saved);
    let runs = &again.slides[0].shapes[0].text.as_ref().unwrap().paragraphs[0].runs;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text, "Hello ");
    assert!(!runs[0].bold);
    assert_eq!(
        (runs[1].text.as_str(), runs[1].bold, runs[1].size),
        ("world", true, 40.0)
    );
    assert_eq!(runs[1].color, Some([0xC0, 0, 0, 0xFF]));
}

#[test]
fn slides_are_added_copied_moved_and_deleted() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    type_into(&mut presentation, 0, "ctrTitle", "One");
    // A title-and-content slide, with a title and two list levels.
    let mut slide = presentation.layouts[1].slide.clone();
    {
        let title = slide
            .shapes
            .iter_mut()
            .find(|s| s.role == Role::Title)
            .unwrap();
        title
            .text
            .as_mut()
            .unwrap()
            .insert(TextPos::default(), "Two");
        let body = slide
            .shapes
            .iter_mut()
            .find(|s| s.role == Role::Body && !s.inherited)
            .unwrap()
            .text
            .as_mut()
            .unwrap();
        body.insert(TextPos::default(), "point\nsub point");
        let levels = body.level_looks();
        body.paragraphs[1].set_level(1, &levels);
        body.renumber();
    }
    presentation.slides.push(slide);
    // A copy of the first, then the first deleted: "Two", "One".
    let mut copy = presentation.slides[0].clone();
    let mut source_info = (**copy.source.as_ref().unwrap()).clone();
    source_info.copy = true;
    copy.source = Some(Arc::new(source_info));
    presentation.slides.push(copy);
    presentation.slides.remove(0);
    let saved = save(source.clone(), &presentation).unwrap();
    let again = read(&saved);
    assert_eq!(again.slides.len(), 2);
    assert_eq!(title_of(&again, 0), "Two");
    assert_eq!(title_of(&again, 1), "One");
    let body = again.slides[0]
        .shapes
        .iter()
        .find(|s| s.role == Role::Body && !s.inherited)
        .unwrap()
        .text
        .as_ref()
        .unwrap();
    assert_eq!(body.paragraphs[1].level, 1);
    assert_eq!(body.paragraphs[0].bullet.as_deref(), Some("•"));
    assert_eq!(body.paragraphs[1].runs[0].size, 24.0);
    let names = parts(&saved);
    assert!(!names.contains_key("ppt/slides/slide1.xml"));
    assert!(names.contains_key("ppt/slides/slide2.xml"));
    assert!(names.contains_key("ppt/slides/slide3.xml"));
    let types = text(&saved, "[Content_Types].xml");
    assert!(!types.contains("/ppt/slides/slide1.xml"));
    assert!(types.contains("/ppt/slides/slide2.xml"));
    let pres = text(&saved, "ppt/presentation.xml");
    assert!(pres.contains(r#"<p:sldId id="257""#), "{pres}");
    assert!(!pres.contains(r#"id="256""#), "{pres}");
    // Untouched parts stay as they were.
    for part in [
        "ppt/slideMasters/slideMaster1.xml",
        "ppt/theme/theme1.xml",
        "_rels/.rels",
    ] {
        assert!(
            !changed(&source, &saved).contains(&part.to_owned()),
            "{part}"
        );
    }
}

#[test]
fn shapes_move_and_new_ones_are_written() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    let slide = &mut presentation.slides[0];
    slide.shapes[1].frame.x += 100_000;
    let look = slide.look.clone();
    let mut rect = Shape::new(
        Frame::new(1_000, 2_000, 3_000_000, 1_000_000),
        Geometry::Ellipse,
        Role::Shape,
    );
    rect.fill = Some(Fill::Solid(look.accent));
    rect.line = Some(Outline {
        color: look.accent_line,
        width: 12_700,
    });
    slide.shapes.push(rect);
    let png: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\nnot really"[..]);
    let mut picture = Shape::new(Frame::new(5, 6, 7, 8), Geometry::Rect, Role::Picture);
    picture.picture = Some(Picture {
        image: png.clone(),
        crop: [0.0; 4],
    });
    slide.shapes.push(picture.clone());
    slide.shapes.push(picture);
    let saved = save(source.clone(), &presentation).unwrap();
    let names = parts(&saved);
    // One picture, written once.
    assert_eq!(names["ppt/media/image1.png"], &png[..]);
    assert!(!names.contains_key("ppt/media/image2.png"));
    let rels = text(&saved, "ppt/slides/_rels/slide1.xml.rels");
    assert_eq!(rels.matches("/image\"").count(), 2);
    let again = read(&saved);
    let own: Vec<&Shape> = again.slides[0]
        .shapes
        .iter()
        .filter(|s| !s.inherited)
        .collect();
    assert_eq!(own.len(), 5);
    assert_eq!(own[1].frame.x, 1_524_000 + 100_000);
    assert_eq!(own[2].geometry, Geometry::Ellipse);
    assert_eq!(own[2].fill, Some(Fill::Solid([0x44, 0x72, 0xC4, 0xFF])));
    assert_eq!(own[3].role, Role::Picture);
    assert_eq!(own[3].picture.as_ref().unwrap().image, png);
    // Ids are new and different.
    let xml = text(&saved, "ppt/slides/slide1.xml");
    for id in ["id=\"4\"", "id=\"5\"", "id=\"6\""] {
        assert!(xml.contains(id), "{id}: {xml}");
    }
}

#[test]
fn notes_are_written_with_a_notes_master_when_there_is_none() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    presentation.slides[0].notes = Some("最初に挨拶する。\nThen the numbers.".into());
    let saved = save(source.clone(), &presentation).unwrap();
    let names = parts(&saved);
    assert!(names.contains_key("ppt/notesSlides/notesSlide1.xml"));
    assert!(names.contains_key("ppt/notesMasters/notesMaster1.xml"));
    assert!(text(&saved, "ppt/presentation.xml").contains("notesMasterIdLst"));
    let again = read(&saved);
    assert_eq!(
        again.slides[0].notes.as_deref(),
        Some("最初に挨拶する。\nThen the numbers.")
    );
    // Changed again: only the notes part.
    let mut edited = again.clone();
    edited.slides[0].notes = Some("短く".into());
    let resaved = save(saved.clone(), &edited).unwrap();
    assert_eq!(
        changed(&saved, &resaved),
        ["ppt/notesSlides/notesSlide1.xml"]
    );
    assert_eq!(read(&resaved).slides[0].notes.as_deref(), Some("短く"));
}

#[test]
fn lists_and_levels_are_written() {
    let source = blank::presentation(&NAMES);
    let mut presentation = read(&source);
    let body = presentation.slides[0].shapes[1].text.as_mut().unwrap();
    body.insert(TextPos::default(), "a\nb");
    for paragraph in &mut body.paragraphs {
        paragraph.list = Some(List::Number("arabicPeriod".into(), 1));
    }
    body.renumber();
    let saved = save(source, &presentation).unwrap();
    let xml = text(&saved, "ppt/slides/slide1.xml");
    assert_eq!(
        xml.matches(r#"<a:buAutoNum type="arabicPeriod"/>"#).count(),
        2,
        "{xml}"
    );
    let again = read(&saved);
    let body = again.slides[0].shapes[1].text.as_ref().unwrap();
    assert_eq!(body.paragraphs[1].bullet.as_deref(), Some("2."));
}
