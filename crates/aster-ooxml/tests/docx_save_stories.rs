//! Saving edits made in headers, footers, footnotes and endnotes, and the
//! headers, footers and footnotes that did not exist when the file was read.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;
#[path = "common/docx_sample.rs"]
mod docx_sample;

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use aster_ooxml::docx_save::{blank, save};
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{Document, Inline, MarginalKind, MarginalSlot, PageField, Path, Pos, StoryId};

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

fn part(bytes: &[u8], name: &str) -> String {
    String::from_utf8(parts(bytes)[name].clone()).unwrap()
}

fn read(bytes: &[u8]) -> Document {
    Docx.read(bytes.to_vec()).unwrap()
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

/// Every part is well-formed XML with one root.
fn assert_well_formed(bytes: &[u8]) {
    for (name, data) in parts(bytes) {
        if !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        let mut reader = quick_xml::Reader::from_reader(data.as_slice());
        let (mut depth, mut roots) = (0i32, 0);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(_)) => {
                    if depth == 0 {
                        roots += 1;
                    }
                    depth += 1;
                }
                Ok(quick_xml::events::Event::Empty(_)) => {
                    if depth == 0 {
                        roots += 1;
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(err) => panic!("{name} is not well-formed: {err}"),
            }
        }
        assert_eq!((depth, roots), (0, 1), "{name}");
    }
}

/// Save, then check what must hold of every save of these tests: the parts
/// are well-formed, the file reads back as the document edited, exactly
/// `expected` parts differ from the source, and saving what was read back
/// again gives the same bytes. Returns the saved file and the document read
/// back from it.
fn check(source: &[u8], edited: &Document, expected: &[&str]) -> (Vec<u8>, Document) {
    let saved = save(source.to_vec(), edited).expect("the document saves");
    assert_well_formed(&saved);
    let back = read(&saved);
    assert!(
        same_content(edited, &back),
        "different content after saving:\nbody {:?} / {:?}\nmarginals {:?} / {:?}\nfootnotes {:?} / {:?}",
        edited.plain_text(),
        back.plain_text(),
        marginal_texts(edited),
        marginal_texts(&back),
        note_texts(edited),
        note_texts(&back),
    );
    assert_eq!(changed(source, &saved), expected, "the parts that changed");
    let again = save(saved.clone(), &back).expect("saves again");
    assert_eq!(again, saved, "a second save without edits changes bytes");
    (saved, back)
}

fn marginal_texts(doc: &Document) -> Vec<Vec<String>> {
    (0..doc.marginals.len())
        .map(|i| {
            doc.paragraphs_in(StoryId::Marginal(i))
                .iter()
                .map(|(_, p)| p.text())
                .collect()
        })
        .collect()
}

fn note_texts(doc: &Document) -> Vec<(i32, Vec<String>)> {
    doc.footnotes
        .notes
        .iter()
        .map(|n| {
            (
                n.id,
                aster_writer::paragraphs_of(&n.blocks)
                    .iter()
                    .map(|(_, p)| p.text())
                    .collect(),
            )
        })
        .collect()
}

/// The place after `needle` in a paragraph of `story`.
fn after(doc: &Document, story: StoryId, needle: &str) -> Pos {
    let (path, p) = doc
        .paragraphs_in(story)
        .into_iter()
        .find(|(_, p)| p.text().contains(needle))
        .unwrap_or_else(|| panic!("no paragraph with {needle:?} in {story:?}"));
    Pos::new(path, p.text().find(needle).unwrap() + needle.len())
}

fn body_after(doc: &Document, needle: &str) -> Pos {
    after(doc, StoryId::Body, needle)
}

/// Type at the start or at the end of a story.
fn put_start(doc: &mut Document, story: StoryId, text: &str) -> Pos {
    let at = doc.story_start(story).unwrap();
    insert_text(doc, &at, text, None)
}

fn put_end(doc: &mut Document, story: StoryId, text: &str) -> Pos {
    let at = doc.story_end(story).unwrap();
    insert_text(doc, &at, text, None)
}

fn sample() -> Vec<u8> {
    docx_sample::sample()
}

/// A header part's index by its text.
fn marginal_with(doc: &Document, text: &str) -> StoryId {
    let i = (0..doc.marginals.len())
        .find(|&i| {
            doc.paragraphs_in(StoryId::Marginal(i))
                .iter()
                .any(|(_, p)| p.text().contains(text))
        })
        .unwrap_or_else(|| panic!("no header or footer with {text:?}"));
    StoryId::Marginal(i)
}

// ---------------------------------------------------------------------------
// Edits in what was read
// ---------------------------------------------------------------------------

#[test]
fn typing_in_a_header_changes_only_that_part() {
    let source = sample();
    let mut doc = read(&source);
    let story = marginal_with(&doc, "Aster Office");
    let at = after(&doc, story, "Aster Office");
    insert_text(&mut doc, &at, " 2026", None);
    let (saved, back) = check(&source, &doc, &["word/header1.xml"]);
    assert!(part(&saved, "word/header1.xml").contains("Aster Office 2026"));
    // The field after it, the tabs and the other paragraph keep their bytes.
    let xml = part(&saved, "word/header1.xml");
    assert!(xml.contains("<w:instrText xml:space=\"preserve\"> PAGE </w:instrText>"));
    assert_eq!(
        back.paragraphs_in(marginal_with(&back, "2026"))[0].1.text()[..16],
        doc.paragraphs_in(story)[0].1.text()[..16]
    );
    // A second edit of the same file, from the original bytes again.
    let at = after(&doc, story, "2026");
    insert_text(&mut doc, &at, "!", None);
    check(&source, &doc, &["word/header1.xml"]);
}

#[test]
fn typing_in_a_footer_and_the_first_page_header() {
    let source = sample();
    let mut doc = read(&source);
    let footer = marginal_with(&doc, " / ");
    let at = doc.story_end(footer).unwrap();
    insert_text(&mut doc, &at, " end", None);
    let first = marginal_with(&doc, "First page header");
    let at = doc.story_start(first).unwrap();
    insert_text(&mut doc, &at, "The ", None);
    let saved = check(&source, &doc, &["word/footer1.xml", "word/header2.xml"]).0;
    assert!(part(&saved, "word/header2.xml").contains("The First page header"));
}

#[test]
fn editing_a_footnote_changes_only_the_footnotes_part() {
    let source = sample();
    let mut doc = read(&source);
    let at = after(&doc, StoryId::Footnote(2), "The footnote text");
    insert_text(&mut doc, &at, " more", None);
    let (saved, _) = check(&source, &doc, &["word/footnotes.xml"]);
    let xml = part(&saved, "word/footnotes.xml");
    assert!(xml.contains("The footnote text more, 脚注。"));
    // The note's own mark is still the mark, not a reference.
    assert!(xml.contains("<w:footnoteRef/>"));
    assert!(!xml.contains("footnoteReference"));
    // An endnote the same way.
    let mut doc = read(&source);
    let at = after(&doc, StoryId::Endnote(1), "An endnote");
    insert_text(&mut doc, &at, "!", None);
    check(&source, &doc, &["word/endnotes.xml"]);
}

#[test]
fn splitting_deleting_and_formatting_in_a_footnote() {
    let source = sample();
    let mut doc = read(&source);
    let at = after(&doc, StoryId::Footnote(2), "The footnote");
    let second = split_paragraph(&mut doc, &at);
    insert_text(&mut doc, &second, "Second paragraph", None);
    apply_run_patch(
        &mut doc,
        &Pos::new(second.path.clone(), 0),
        &Pos::new(second.path.clone(), 6),
        &aster_writer::RunPatch {
            bold: Some(true),
            ..Default::default()
        },
    );
    let (saved, back) = check(&source, &doc, &["word/footnotes.xml"]);
    assert_eq!(note_texts(&back)[0].1.len(), 2);
    assert!(part(&saved, "word/footnotes.xml").contains("<w:b/>"));
}

#[test]
fn a_link_in_a_header_is_related_from_the_header() {
    let source = sample();
    let mut doc = read(&source);
    let story = marginal_with(&doc, "Aster Office");
    let from = doc.story_start(story).unwrap();
    let to = after(&doc, story, "Aster");
    set_link(
        &mut doc,
        &from,
        &to,
        Some(aster_writer::Link {
            target: "https://example.org/".into(),
            tooltip: None,
        }),
    );
    let (saved, back) = check(
        &source,
        &doc,
        &["word/_rels/header1.xml.rels", "word/header1.xml"],
    );
    assert!(part(&saved, "word/_rels/header1.xml.rels").contains("https://example.org/"));
    let story = marginal_with(&back, "Aster Office");
    assert!(
        back.paragraphs_in(story)[0]
            .1
            .runs
            .iter()
            .any(|r| r.link.is_some())
    );
}

// ---------------------------------------------------------------------------
// Footnotes made here
// ---------------------------------------------------------------------------

#[test]
fn a_new_footnote_goes_into_the_footnotes_part_and_the_body() {
    let source = sample();
    let mut doc = read(&source);
    let at = body_after(&doc, "Introduction");
    let selection = insert_footnote(&mut doc, &at).unwrap();
    insert_text(&mut doc, &selection.focus, "A new note.", None);
    let (saved, back) = check(&source, &doc, &["word/document.xml", "word/footnotes.xml"]);
    let xml = part(&saved, "word/footnotes.xml");
    assert!(xml.contains("<w:footnote w:id=\"3\">"), "{xml}");
    assert!(xml.contains("<w:footnoteRef/>"));
    assert!(xml.contains("A new note."));
    // The note that was there, and the separators, are as they were.
    let was = part(&source, "word/footnotes.xml");
    assert!(xml.starts_with(&was[..was.rfind("</w:footnotes>").unwrap()]));
    assert!(part(&saved, "word/document.xml").contains("<w:footnoteReference w:id=\"3\"/>"));
    assert_eq!(back.footnotes.notes.len(), 2);
}

#[test]
fn deleting_a_reference_drops_its_note_from_the_file() {
    let source = sample();
    let mut doc = read(&source);
    // The body paragraph that has the reference to note 2.
    let (path, p) = doc
        .paragraphs()
        .into_iter()
        .find(|(_, p)| p.runs.iter().any(|r| matches!(r.kind, Inline::Footnote(2))))
        .map(|(path, p)| (path, p.clone()))
        .unwrap();
    let mut start = 0;
    for run in &p.runs {
        if matches!(run.kind, Inline::Footnote(2)) {
            break;
        }
        start += run.text.len();
    }
    delete(
        &mut doc,
        &Pos::new(path.clone(), start),
        &Pos::new(path, start + aster_writer::OBJECT.len()),
    );
    assert!(doc.footnotes.notes.is_empty());
    let (saved, back) = check(&source, &doc, &["word/document.xml", "word/footnotes.xml"]);
    let xml = part(&saved, "word/footnotes.xml");
    assert!(!xml.contains("The footnote text"));
    assert!(xml.contains("continuationSeparator"));
    assert!(back.footnotes.notes.is_empty());
}

#[test]
fn a_blank_document_takes_a_footnote_and_gets_the_part_for_it() {
    let source = blank();
    let mut doc = read(&source);
    let at = doc.start().unwrap();
    insert_text(&mut doc, &at, "Text", None);
    let at = doc.end().unwrap();
    let selection = insert_footnote(&mut doc, &at).unwrap();
    insert_text(&mut doc, &selection.focus, "Source.", None);
    let (saved, back) = check(
        &source,
        &doc,
        &[
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/footnotes.xml",
        ],
    );
    let xml = part(&saved, "word/footnotes.xml");
    assert!(xml.contains("w:type=\"separator\" w:id=\"-1\""));
    assert!(xml.contains("w:type=\"continuationSeparator\" w:id=\"0\""));
    assert!(xml.contains("<w:footnote w:id=\"1\">"));
    assert!(part(&saved, "[Content_Types].xml").contains(
        "<Override PartName=\"/word/footnotes.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml\"/>"
    ));
    assert!(part(&saved, "word/_rels/document.xml.rels").contains("relationships/footnotes\""));
    assert_eq!(back.footnotes.notes.len(), 1);
    assert_eq!(
        note_texts(&back)[0].1,
        [format!("{} Source.", aster_writer::OBJECT)]
    );
}

// ---------------------------------------------------------------------------
// Headers and footers made here
// ---------------------------------------------------------------------------

#[test]
fn a_new_header_in_a_blank_document_is_a_part_a_relationship_and_a_reference() {
    let source = blank();
    let mut doc = read(&source);
    let story = ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    put_start(&mut doc, story, "Report");
    let (saved, back) = check(
        &source,
        &doc,
        &[
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/header1.xml",
        ],
    );
    assert!(part(&saved, "word/header1.xml").contains("<w:t xml:space=\"preserve\">Report</w:t>"));
    let main = part(&saved, "word/document.xml");
    // Just the reference is new in the final section properties.
    let was = part(&source, "word/document.xml");
    let sect = was.find("<w:sectPr>").unwrap();
    assert!(main.contains("<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId"));
    assert_eq!(&main[..sect], &was[..sect]);
    assert!(part(&saved, "[Content_Types].xml").contains(
        "<Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>"
    ));
    assert!(part(&saved, "word/_rels/document.xml.rels").contains("relationships/header\""));
    assert_eq!(back.sections()[0].1.headers.default, Some(0));
    assert_eq!(marginal_texts(&back), [vec!["Report".to_owned()]]);
    // Parts that did not need to change did not.
    for name in ["word/styles.xml", "word/settings.xml", "_rels/.rels"] {
        assert_eq!(parts(&saved)[name], parts(&source)[name], "{name}");
    }
}

#[test]
fn a_footer_with_a_page_number_and_a_header_in_one_save() {
    let source = blank();
    let mut doc = read(&source);
    let header = ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    put_start(&mut doc, header, "Top");
    let footer = ensure_marginal(&mut doc, 0, MarginalKind::Footer, MarginalSlot::Default).unwrap();
    let end = put_start(&mut doc, footer, "Page ");
    let end = insert_page_number(&mut doc, &end, PageField::Page);
    insert_text(&mut doc, &end, " of ", None);
    let end = doc.story_end(footer).unwrap();
    insert_page_number(&mut doc, &end, PageField::PageCount);
    let (saved, back) = check(
        &source,
        &doc,
        &[
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/footer1.xml",
            "word/header1.xml",
        ],
    );
    let xml = part(&saved, "word/footer1.xml");
    assert!(xml.contains("<w:fldSimple w:instr=\" PAGE \">"), "{xml}");
    assert!(xml.contains("<w:fldSimple w:instr=\" NUMPAGES \">"));
    let main = part(&saved, "word/document.xml");
    assert!(main.contains("<w:headerReference"));
    assert!(main.contains("<w:footerReference"));
    let footer = StoryId::Marginal(1);
    let kinds: Vec<Inline> = back
        .paragraph(&Path::in_story(footer, 0))
        .unwrap()
        .runs
        .iter()
        .map(|r| r.kind.clone())
        .filter(|k| matches!(k, Inline::PageNumber(_)))
        .collect();
    assert_eq!(
        kinds,
        [
            Inline::PageNumber(PageField::Page),
            Inline::PageNumber(PageField::PageCount)
        ]
    );
}

#[test]
fn a_page_number_in_a_header_that_was_read_and_in_the_body() {
    let source = sample();
    let mut doc = read(&source);
    let story = marginal_with(&doc, "First page header");
    let end = doc.story_end(story).unwrap();
    insert_page_number(&mut doc, &end, PageField::Page);
    let at = body_after(&doc, "Introduction");
    insert_page_number(&mut doc, &at, PageField::SectionPages);
    let (saved, _) = check(&source, &doc, &["word/document.xml", "word/header2.xml"]);
    assert!(part(&saved, "word/header2.xml").contains("w:instr=\" PAGE \""));
    assert!(part(&saved, "word/document.xml").contains("w:instr=\" SECTIONPAGES \""));
}

/// A document of two sections, the first with a header and a footer.
fn two_sections() -> Vec<u8> {
    let sect = |extra: &str| {
        format!(
            "<w:sectPr>{extra}<w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>"
        )
    };
    let body = format!(
        "<w:p><w:r><w:t>One</w:t></w:r></w:p><w:p><w:pPr>{}</w:pPr></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p>{}",
        sect("<w:headerReference w:type=\"default\" r:id=\"rIdH\"/>"),
        sect("")
    );
    docx_doc::doc(&body)
        .marginal(
            "rIdH",
            "header",
            "header1.xml",
            "<w:p><w:r><w:t>Chapter header</w:t></w:r></w:p>",
        )
        .bytes()
}

#[test]
fn a_footer_made_in_the_first_section_is_inherited_by_the_second() {
    let source = two_sections();
    let mut doc = read(&source);
    assert_eq!(doc.sections().len(), 2);
    assert_eq!(doc.sections()[1].1.headers.default, Some(0));
    let footer = ensure_marginal(&mut doc, 0, MarginalKind::Footer, MarginalSlot::Default).unwrap();
    put_start(&mut doc, footer, "Foot");
    // And editing the header the second section inherits edits the first's.
    let inherited = ensure_marginal(&mut doc, 1, MarginalKind::Header, MarginalSlot::Default);
    assert_eq!(inherited, Some(StoryId::Marginal(0)));
    let (saved, back) = check(
        &source,
        &doc,
        &[
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/footer1.xml",
        ],
    );
    let main = part(&saved, "word/document.xml");
    // One reference, in the section that introduces the footer; the second
    // section follows it, as in any file.
    assert_eq!(main.matches("<w:footerReference").count(), 1);
    assert!(main.find("<w:footerReference").unwrap() < main.find("Two").unwrap());
    assert_eq!(back.sections()[0].1.footers.default, Some(1));
    assert_eq!(back.sections()[1].1.footers.default, Some(1));
    assert_eq!(back.sections()[1].1.headers.default, Some(0));
}

#[test]
fn a_header_made_for_the_second_section_only_is_referenced_there() {
    let source = two_sections();
    let mut doc = read(&source);
    let first = ensure_marginal(&mut doc, 1, MarginalKind::Header, MarginalSlot::First).unwrap();
    put_start(&mut doc, first, "Only here");
    let (saved, back) = check(
        &source,
        &doc,
        &[
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/header2.xml",
        ],
    );
    let main = part(&saved, "word/document.xml");
    // In the final section properties, after the paragraph "Two".
    assert!(main.find("type=\"first\"").unwrap() > main.find("Two").unwrap());
    assert_eq!(back.sections()[0].1.headers.first, None);
    assert_eq!(back.sections()[1].1.headers.first, Some(1));
}

#[test]
fn entering_and_looking_at_headers_changes_nothing_in_the_file() {
    let source = two_sections();
    let doc = read(&source);
    for kind in [MarginalKind::Header, MarginalKind::Footer] {
        let _ = existing_marginal(&doc, 0, kind, MarginalSlot::Default);
    }
    assert_eq!(save(source.clone(), &doc).unwrap(), source);
}

#[test]
fn repeated_saves_each_diff_against_the_opened_bytes() {
    let source = blank();
    let mut doc = read(&source);
    let story = ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    put_start(&mut doc, story, "One");
    let first = save(source.clone(), &doc).unwrap();
    put_end(&mut doc, story, " two");
    let second = save(source.clone(), &doc).unwrap();
    assert!(part(&second, "word/header1.xml").contains("One two"));
    assert_eq!(parts(&second).len(), parts(&first).len());
    // Neither made two headers.
    assert_eq!(
        part(&second, "word/document.xml")
            .matches("headerReference")
            .count(),
        1
    );
    assert_eq!(
        part(&second, "word/_rels/document.xml.rels")
            .matches("relationships/header\"")
            .count(),
        1
    );
    assert_well_formed(&second);
    let back = read(&second);
    assert!(same_content(&doc, &back));
}

#[test]
fn the_corpus_style_edge_cases_do_not_panic() {
    // A header part with an empty root, a footnotes part with no notes, a
    // section that names a part that is not there.
    let body = "<w:p><w:r><w:t>Body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type=\"default\" r:id=\"rIdH\"/><w:footerReference w:type=\"default\" r:id=\"rIdMissing\"/></w:sectPr>";
    let source = docx_doc::doc(body)
        .marginal("rIdH", "header", "header1.xml", "")
        .footnotes("")
        .bytes();
    let mut doc = read(&source);
    assert_eq!(doc.marginals.len(), 1);
    let story = StoryId::Marginal(0);
    let at = doc.story_start(story);
    if let Some(at) = at {
        insert_text(&mut doc, &at, "x", None);
    }
    let end = doc.end().unwrap();
    let selection = insert_footnote(&mut doc, &end).unwrap();
    insert_text(&mut doc, &selection.focus, "n", None);
    let saved = save(source, &doc).expect("saves");
    assert_well_formed(&saved);
    let back = read(&saved);
    assert_eq!(back.footnotes.notes.len(), 1);
}
