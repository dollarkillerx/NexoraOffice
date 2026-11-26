//! Saving edited Word documents: what changes in the package and what stays
//! byte for byte, and that the file reads back as edited.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;
#[path = "common/docx_sample.rs"]
mod docx_sample;

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path as FsPath;
use std::sync::Arc;

use aster_ooxml::docx_save::{blank, save};
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{
    Align, Block, Break, Document, Inline, LineSpacing, Link, Path, Pos, Run, RunLook, RunPatch,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        let mut depth = 0i32;
        let mut roots = 0;
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

fn sample() -> Vec<u8> {
    docx_sample::sample()
}

/// The unpacked corpus folder as a package.
fn corpus() -> Option<Vec<u8>> {
    let dir = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/g4-ooxml/corpus/docx");
    if !dir.is_dir() {
        return None;
    }
    fn walk(root: &FsPath, dir: &FsPath, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((name, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut found = Vec::new();
    walk(&dir, &dir, &mut found);
    found.sort_by_key(|(name, _)| name != "[Content_Types].xml");
    Some(docx_sample::zip(&found))
}

fn find(doc: &Document, needle: &str) -> Path {
    doc.paragraphs()
        .into_iter()
        .find(|(_, p)| p.text().contains(needle))
        .unwrap_or_else(|| panic!("no paragraph with {needle:?}"))
        .0
}

/// The place just after `needle` in the paragraph that has it.
fn after(doc: &Document, needle: &str) -> Pos {
    let path = find(doc, needle);
    let text = doc.paragraph(&path).unwrap().text();
    Pos::new(path, text.find(needle).unwrap() + needle.len())
}

fn before(doc: &Document, needle: &str) -> Pos {
    let path = find(doc, needle);
    let text = doc.paragraph(&path).unwrap().text();
    Pos::new(path, text.find(needle).unwrap())
}

fn end_of(doc: &Document, needle: &str) -> Pos {
    let path = find(doc, needle);
    let len = doc.paragraph(&path).unwrap().len();
    Pos::new(path, len)
}

fn start_of(doc: &Document, needle: &str) -> Pos {
    Pos::new(find(doc, needle), 0)
}

/// The look text typed at `at` has, in bold.
fn bold_at(doc: &Document, at: &Pos) -> RunLook {
    let p = doc.paragraph(&at.path).unwrap();
    RunLook {
        bold: true,
        ..p.look_at(at.offset).clone()
    }
}

fn make_bold() -> RunPatch {
    RunPatch {
        bold: Some(true),
        ..RunPatch::default()
    }
}

/// Save `edited` over `source`, and check what must hold of every save: the
/// parts are well-formed, the file reads back with the same content, no part
/// but the ones named changed, every paragraph that was not edited has
/// exactly its original bytes, and saving the re-read document again changes
/// nothing. Returns the saved file.
fn check(source: &[u8], edited: &Document, may_change: &[&str]) -> Vec<u8> {
    check_except(source, edited, may_change, &[])
}

/// [`check`], but the paragraphs whose text starts with one of `rewritten`
/// are not expected to keep their bytes (an edit the model cannot tell from
/// "as read", such as the size of a picture).
fn check_except(
    source: &[u8],
    edited: &Document,
    may_change: &[&str],
    rewritten: &[&str],
) -> Vec<u8> {
    let saved = save(source.to_vec(), edited).expect("the document saves");
    assert_well_formed(&saved);
    let back = read(&saved);
    if !same_content(edited, &back) {
        let a: Vec<String> = edited.paragraphs().iter().map(|(_, p)| p.text()).collect();
        let b: Vec<String> = back.paragraphs().iter().map(|(_, p)| p.text()).collect();
        let pairs: Vec<_> = edited
            .paragraphs()
            .into_iter()
            .zip(back.paragraphs())
            .collect();
        for ((path, x), (_, y)) in pairs {
            if !same_blocks(
                &[Block::Paragraph(x.clone())],
                &[Block::Paragraph(y.clone())],
            ) {
                eprintln!("paragraph {path:?} differs");
                eprintln!("props equal: {}", x.props == y.props);
                if x.props != y.props {
                    eprintln!("edited {:?}\nsaved  {:?}", x.props, y.props);
                }
                eprintln!("base equal: {}", x.base == y.base);
                eprintln!("mark equal: {}", x.mark == y.mark);
                eprintln!("label equal: {}", x.label == y.label);
                eprintln!("runs: edited {} saved {}", x.runs.len(), y.runs.len());
                for (i, (r, t)) in x.runs.iter().zip(&y.runs).enumerate() {
                    if r.text != t.text
                        || r.kind != t.kind
                        || r.look != t.look
                        || r.style != t.style
                        || r.link != t.link
                    {
                        eprintln!(
                            "run {i}: edited {:?} {:.50?} style {:?} link {:?}",
                            r.text, r.kind, r.style, r.link
                        );
                        eprintln!(
                            "       saved  {:?} {:.50?} style {:?} link {:?}",
                            t.text, t.kind, t.style, t.link
                        );
                        eprintln!(
                            "       looks equal {}  edited {:?}\n       saved {:?}",
                            r.look == t.look,
                            r.look,
                            t.look
                        );
                        break;
                    }
                }
                break;
            }
        }
        eprintln!("{}", part(&saved, "word/document.xml"));
        panic!("different content after saving\n{a:?}\n{b:?}");
    }
    for name in changed(source, &saved) {
        assert!(
            name == "word/document.xml" || may_change.contains(&name.as_str()),
            "{name} changed"
        );
    }
    // Paragraphs not edited keep their bytes.
    let original = part(source, "word/document.xml");
    let saved_main = part(&saved, "word/document.xml");
    let original_doc = read(source);
    let untouched = |p: &aster_writer::Paragraph| {
        p.is_as_read() && !rewritten.iter().any(|text| p.text().starts_with(text))
    };
    for (_, p) in edited.paragraphs() {
        if untouched(p) {
            let range = &p.source.as_ref().unwrap().range;
            assert!(
                saved_main.contains(&original[range.clone()]),
                "paragraph {:?} lost its bytes",
                p.text()
            );
        }
    }
    let _ = original_doc;
    // Saving what was just read changes nothing.
    let again = save(saved.clone(), &back).expect("saves again");
    assert_eq!(again, saved, "a second save without edits changes bytes");
    saved
}

// ---------------------------------------------------------------------------
// No change
// ---------------------------------------------------------------------------

#[test]
fn a_document_saved_without_changes_is_the_file_it_was() {
    for bytes in [Some(sample()), corpus(), Some(blank())]
        .into_iter()
        .flatten()
    {
        let document = read(&bytes);
        let saved = save(bytes.clone(), &document).unwrap();
        assert_eq!(saved, bytes, "the whole file");
        assert_eq!(parts(&saved), parts(&bytes));
    }
}

#[test]
fn a_document_edited_and_edited_back_is_byte_identical_in_every_part() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let at = after(&doc, "Introduction");
    let end = insert_text(&mut doc, &at, "!", None);
    delete_backward(&mut doc, &end);
    let saved = save(bytes.clone(), &doc).unwrap();
    assert_eq!(parts(&saved), parts(&bytes));
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

type Edit = fn(&mut Document);

fn edits() -> Vec<(&'static str, Edit)> {
    vec![
        ("type into the title", |d| {
            let at = after(d, "样本");
            insert_text(d, &at, "新しい文字 and emoji 😀", None);
        }),
        ("type bold in the middle of a run", |d| {
            let at = after(d, "Introduction");
            insert_text(d, &at, " bold", Some(&bold_at(d, &at)));
        }),
        ("type text with tab and newline", |d| {
            let at = after(d, "quick brown");
            insert_text(d, &at, "A\tB\nC\nD", None);
        }),
        ("enter at the end of a heading", |d| {
            let at = end_of(d, "Introduction");
            let next = split_paragraph(d, &at);
            insert_text(d, &next, "Body after heading", None);
        }),
        ("enter in the middle of a run", |d| {
            let at = after(d, "第二级");
            split_paragraph(d, &at);
        }),
        ("line, page and column breaks", |d| {
            let at = after(d, "Before");
            let at = insert_break(d, &at, Break::Line);
            let at = insert_break(d, &at, Break::Page);
            insert_break(d, &at, Break::Column);
        }),
        ("delete inside a run", |d| {
            let from = after(d, "The quick ");
            let to = after(d, "quick brown");
            delete(d, &from, &to);
        }),
        ("delete across paragraphs", |d| {
            let from = after(d, "Heading two");
            let to = after(d, "見出し三 ");
            delete(d, &from, &to);
        }),
        ("backspace joins", |d| {
            let at = start_of(d, "見出し三");
            delete_backward(d, &at);
        }),
        ("delete forward joins", |d| {
            let at = end_of(d, "Heading two");
            delete_forward(d, &at);
        }),
        ("bold over several paragraphs", |d| {
            let from = after(d, "Heading two");
            let to = after(d, "中文段落，");
            apply_run_patch(d, &from, &to, &make_bold());
        }),
        ("formatting paragraph runs get a size and colour", |d| {
            let from = before(d, "italic");
            let to = after(d, "wavy");
            apply_run_patch(
                d,
                &from,
                &to,
                &RunPatch {
                    size: Some(14.0),
                    color: Some(Some([0, 128, 0, 255])),
                    italic: Some(false),
                    ..RunPatch::default()
                },
            );
        }),
        ("align, indent, spacing", |d| {
            let from = start_of(d, "中文段落");
            let to = end_of(d, "Bold");
            set_align(d, &from, &to, Align::Center);
            change_indent(d, &from, &to, 1);
            set_line_spacing(d, &from, &to, LineSpacing::Multiple(1.5));
            set_space_before(d, &from, &to, 200);
            set_space_after(d, &from, &to, 100);
        }),
        ("set a style", |d| {
            let at = start_of(d, "样本");
            set_style(d, &at, &at, "Heading2");
        }),
        ("heading back to normal", |d| {
            let at = start_of(d, "Introduction");
            set_style(d, &at, &at, "Normal");
        }),
        ("make a bullet list", |d| {
            let from = start_of(d, "Chapter");
            let to = start_of(d, "Ruby:");
            set_list(d, &from, &to, Some(ListKind::Bullet));
        }),
        ("make a numbered list of new kind and nest it", |d| {
            let from = start_of(d, "Before the break");
            let to = start_of(d, "Before the break");
            set_list(d, &from, &to, Some(ListKind::Numbered));
            change_level(d, &from, &to, 1);
        }),
        ("take items out of a list", |d| {
            let from = start_of(d, "Nested item");
            let to = start_of(d, "Nested again");
            set_list(d, &from, &to, None);
        }),
        ("list levels", |d| {
            let at = start_of(d, "Second item");
            change_level(d, &at, &at, 1);
        }),
        ("copy and paste", |d| {
            let from = after(d, "The quick");
            let to = after(d, "brown fox");
            let part = fragment(d, &from, &to);
            let at = end_of(d, "Heading two");
            insert_fragment(d, &at, &part);
        }),
        ("copy and paste several paragraphs", |d| {
            let from = start_of(d, "第二级");
            let to = after(d, "見出し三");
            let part = fragment(d, &from, &to);
            let at = after(d, "The second paragraph");
            insert_fragment(d, &at, &part);
        }),
        ("type in a table cell", |d| {
            let at = after(d, "Merged across");
            insert_text(d, &at, " more", None);
        }),
        ("type in a nested table", |d| {
            let at = after(d, "n2");
            insert_text(d, &at, "+", Some(&bold_at(d, &at)));
        }),
        ("split and join in a cell", |d| {
            let at = after(d, "Merged down");
            let next = split_paragraph(d, &at);
            insert_text(d, &next, "second", None);
            let at = after(d, "second");
            delete(d, &Pos::new(at.path.clone(), 0), &at);
        }),
        ("delete across cells", |d| {
            let from = after(d, "Score");
            let to = after(d, "Merged across");
            delete(d, &from, &to);
        }),
        ("delete from a paragraph into a table", |d| {
            let from = after(d, "Inline picture");
            let to = after(d, "Merged");
            delete(d, &from, &to);
        }),
        ("delete the paragraph that ends a section", |d| {
            let from = start_of(d, "After the break");
            let path = Path::body(d.paragraphs()[0].0.top());
            let _ = path;
            // The empty paragraph after it carries the section.
            let end = end_of(d, "After the break");
            let next = {
                let mut path = end.path.clone();
                *path.0.last_mut().unwrap() += 1;
                Pos::new(path, 0)
            };
            let _ = from;
            delete(d, &end, &next);
        }),
        ("join into the section paragraph", |d| {
            let end = end_of(d, "After the break");
            delete_forward(d, &end);
            let at = end_of(d, "After the break");
            insert_text(d, &at, "x", None);
        }),
        ("delete a whole range with a drawing", |d| {
            let from = start_of(d, "Two columns");
            let to = end_of(d, "The second paragraph");
            delete(d, &from, &to);
        }),
    ]
}

#[test]
fn every_edit_saves_and_reads_back() {
    let bytes = sample();
    for (name, edit) in edits() {
        let mut doc = read(&bytes);
        let before = doc.clone();
        edit(&mut doc);
        assert!(!same_content(&before, &doc), "{name} changed nothing");
        let numbering = doc.numbering.lists.len() != before.numbering.lists.len();
        let may: &[&str] = if numbering {
            &["word/numbering.xml"]
        } else {
            &[]
        };
        eprintln!("edit: {name}");
        check(&bytes, &doc, may);
    }
}

#[test]
fn every_edit_saves_into_the_corpus_document_too() {
    let Some(bytes) = corpus() else {
        return;
    };
    let mut doc = read(&bytes);
    let at = after(&doc, "English words");
    insert_text(&mut doc, &at, "【追加】", None);
    let at = start_of(&doc, "带批注");
    split_paragraph(&mut doc, &at);
    let from = start_of(&doc, "往返");
    let to = end_of(&doc, "往返");
    apply_run_patch(&mut doc, &from, &to, &make_bold());
    check(&bytes, &doc, &[]);
}

// ---------------------------------------------------------------------------
// Wrappers and markers
// ---------------------------------------------------------------------------

/// A one-paragraph document: `inner` as the content of a paragraph.
fn one(inner: &str) -> Vec<u8> {
    docx_doc::doc(&format!("<w:p>{inner}</w:p>")).bytes()
}

fn paragraph_xml(saved: &[u8]) -> String {
    let xml = part(saved, "word/document.xml");
    let start = xml.find("<w:body>").unwrap() + 8;
    let end = xml.find("</w:body>").unwrap();
    xml[start..end].to_owned()
}

fn run_edit(inner: &str, edit: impl FnOnce(&mut Document)) -> (Document, Vec<u8>) {
    let bytes = one(inner);
    let mut doc = read(&bytes);
    edit(&mut doc);
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    (doc, saved)
}

const LINK: &str = r#"<w:r><w:t xml:space="preserve">before </w:t></w:r><w:hyperlink r:id="rId9" w:history="1"><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>the link</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> after</w:t></w:r>"#;

fn with_link(inner: &str) -> Vec<u8> {
    docx_doc::doc(&format!("<w:p>{inner}</w:p>"))
        .rel("rId9", "hyperlink", "https://example.org/")
        .bytes()
}

fn linked(edit: impl FnOnce(&mut Document)) -> (Document, Vec<u8>) {
    let bytes = with_link(LINK);
    let mut doc = read(&bytes);
    edit(&mut doc);
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    (doc, saved)
}

#[test]
fn typing_inside_a_hyperlink_extends_it() {
    let (doc, saved) = linked(|d| {
        let at = after(d, "the ");
        insert_text(d, &at, "big ", None);
    });
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "before the big link after");
    let xml = paragraph_xml(&saved);
    assert_eq!(xml.matches("<w:hyperlink").count(), 1);
    assert!(xml.contains("the big link"));
    let link = read(&saved).paragraph(&Path::body(0)).unwrap().runs[1]
        .link
        .clone()
        .unwrap();
    assert_eq!(link.target, "https://example.org/");
}

#[test]
fn typing_inside_a_hyperlink_in_another_look_splits_it() {
    let (_, saved) = linked(|d| {
        let at = after(d, "the ");
        insert_text(d, &at, "BOLD", Some(&bold_at(d, &at)));
    });
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let linked: Vec<(&str, bool)> = p
        .runs
        .iter()
        .map(|r| (r.text.as_str(), r.link.is_some()))
        .collect();
    assert_eq!(
        linked,
        [
            ("before ", false),
            ("the ", true),
            ("BOLD", false),
            ("link", true),
            (" after", false)
        ]
    );
    // Both halves of the link point where it did, through one relationship.
    assert_eq!(
        part(&saved, "word/_rels/document.xml.rels")
            .matches("example.org")
            .count(),
        1
    );
}

#[test]
fn typing_right_after_a_hyperlink_is_outside_it() {
    let (_, saved) = linked(|d| {
        let at = after(d, "the link");
        insert_text(d, &at, "!", Some(&bold_at(d, &at)));
    });
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let marked: Vec<(&str, bool)> = p
        .runs
        .iter()
        .map(|r| (r.text.as_str(), r.link.is_some()))
        .collect();
    assert_eq!(
        marked,
        [
            ("before ", false),
            ("the link", true),
            ("!", false),
            (" after", false)
        ]
    );
    // The text after the link did not move into it.
    let xml = paragraph_xml(&saved);
    assert!(xml.contains("</w:hyperlink><w:r>"));
}

#[test]
fn text_with_a_new_link_gets_a_relationship_or_an_anchor() {
    let bytes = one("<w:r><w:t>plain</w:t></w:r>");
    let mut doc = read(&bytes);
    let at = end_of(&doc, "plain");
    let look = RunLook::default();
    let bold = bold_at(&doc, &at);
    insert_text(&mut doc, &at, " tail", Some(&bold));
    // Give the new run links by hand: one external, then one to a bookmark.
    {
        let p = doc.paragraph_mut(&Path::body(0)).unwrap();
        let mut external = Run::text(" site", look.clone());
        external.link = Some(Arc::new(Link {
            target: "https://example.com/a?b=1&c=2".to_owned(),
            tooltip: Some("tip".to_owned()),
        }));
        let mut internal = Run::text(" jump", look);
        internal.link = Some(Arc::new(Link {
            target: "#start".to_owned(),
            tooltip: None,
        }));
        p.runs.push(external);
        p.runs.push(internal);
    }
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let rels = part(&saved, "word/_rels/document.xml.rels");
    assert!(rels.contains("https://example.com/a?b=1&amp;c=2"));
    assert!(rels.contains("TargetMode=\"External\""));
    let xml = paragraph_xml(&saved);
    assert!(xml.contains("w:anchor=\"start\""));
    assert!(xml.contains("w:tooltip=\"tip\""));
}

#[test]
fn typing_inside_a_tracked_insertion_and_after_it() {
    let inner = r#"<w:r><w:t xml:space="preserve">a </w:t></w:r><w:ins w:id="1" w:author="R" w:date="2026-01-01T00:00:00Z"><w:r><w:t>inserted</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> z</w:t></w:r>"#;
    let (_, saved) = run_edit(inner, |d| {
        let at = after(d, "inser");
        insert_text(d, &at, "-in-", None);
    });
    let xml = paragraph_xml(&saved);
    assert!(xml.contains("<w:ins w:id=\"1\" w:author=\"R\""));
    assert!(xml.contains("inser-in-ted"));
    // A different look after the last run inside the insertion lands in it
    // (it has no link to differ by); the text after it stays outside.
    let (_, saved) = run_edit(inner, |d| {
        let at = after(d, "inserted");
        insert_text(d, &at, "B", Some(&bold_at(d, &at)));
    });
    let xml = paragraph_xml(&saved);
    assert_eq!(xml.matches("<w:ins ").count(), 1);
    assert_eq!(xml.matches("</w:ins>").count(), 1);
    let (_, saved) = run_edit(inner, |d| {
        let at = before(d, " z");
        insert_text(d, &at, "Q", Some(&bold_at(d, &at)));
    });
    assert_eq!(read(&saved).plain_text(), "a insertedQ z");
}

#[test]
fn bookmarks_comment_ranges_and_proofing_marks_survive_edits() {
    let inner = r#"<w:bookmarkStart w:id="0" w:name="b"/><w:r><w:t>one</w:t></w:r><w:commentRangeStart w:id="3"/><w:r><w:t>two</w:t></w:r><w:commentRangeEnd w:id="3"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="3"/></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>three</w:t></w:r><w:proofErr w:type="spellEnd"/><w:bookmarkEnd w:id="0"/>"#;
    let edits: [(&str, Edit); 6] = [
        ("type", |d| {
            let at = after(d, "tw");
            insert_text(d, &at, "XY", None);
        }),
        ("type with look", |d| {
            let at = after(d, "one");
            insert_text(d, &at, "!", Some(&bold_at(d, &at)));
        }),
        ("split", |d| {
            let at = after(d, "onetw");
            split_paragraph(d, &at);
        }),
        ("delete", |d| {
            let from = after(d, "on");
            let to = after(d, "onetwot");
            delete(d, &from, &to);
        }),
        ("delete all", |d| {
            let from = start_of(d, "one");
            let to = end_of(d, "one");
            delete(d, &from, &to);
        }),
        ("bold", |d| {
            let from = after(d, "o");
            let to = after(d, "onetwot");
            apply_run_patch(d, &from, &to, &make_bold());
        }),
    ];
    for (name, edit) in edits {
        eprintln!("{name}");
        let (doc, saved) = run_edit(inner, edit);
        let xml = paragraph_xml(&saved);
        for marker in [
            "bookmarkStart",
            "bookmarkEnd",
            "commentRangeStart",
            "commentRangeEnd",
            "commentReference",
            "proofErr w:type=\"spellStart\"",
        ] {
            assert!(xml.contains(marker), "{name}: {marker} lost\n{xml}");
        }
        assert!(doc.paragraphs().len() <= 2);
    }
}

#[test]
fn a_complex_field_is_kept_whole_when_typing_next_to_it() {
    let field = r#"<w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> of the end</w:t></w:r>"#;
    for (name, edit) in [
        (
            "before the field",
            (|d: &mut Document| {
                let at = after(d, "Page ");
                insert_text(d, &at, "no. ", Some(&bold_at(d, &at)));
            }) as fn(&mut Document),
        ),
        ("after the field", |d| {
            let at = before(d, " of the");
            insert_text(d, &at, "!", Some(&bold_at(d, &at)));
        }),
        ("split after the field", |d| {
            let at = before(d, " of the");
            split_paragraph(d, &at);
        }),
        ("delete the page number", |d| {
            let from = after(d, "Page ");
            let to = after(d, "Page 7");
            delete(d, &from, &to);
        }),
    ] {
        eprintln!("{name}");
        let (_, saved) = run_edit(field, edit);
        let xml = paragraph_xml(&saved);
        for part in [
            "fldCharType=\"begin\"",
            "instrText",
            "fldCharType=\"separate\"",
            "fldCharType=\"end\"",
        ] {
            assert!(xml.contains(part), "{name}: {part} lost");
        }
    }
}

#[test]
fn typing_inside_a_content_control_stays_inside() {
    let body = r#"<w:p><w:r><w:t>out</w:t></w:r></w:p><w:sdt><w:sdtPr><w:alias w:val="Box"/><w:id w:val="5"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>inside</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>tail</w:t></w:r></w:p>"#;
    let bytes = docx_doc::doc(body).bytes();
    let make = |edit: &dyn Fn(&mut Document)| {
        let mut doc = read(&bytes);
        edit(&mut doc);
        let saved = check(&bytes, &doc, &[]);
        part(&saved, "word/document.xml")
    };
    let xml = make(&|d| {
        let at = after(d, "insid");
        insert_text(d, &at, "XX", None);
    });
    assert!(xml.contains("<w:sdtContent><w:p><w:r><w:t xml:space=\"preserve\">insidXXe</w:t>"));
    // Enter inside it: the new paragraph is in the control.
    let xml = make(&|d| {
        let at = after(d, "insid");
        split_paragraph(d, &at);
    });
    assert_eq!(xml.matches("<w:sdt>").count(), 1);
    assert_eq!(xml.matches("</w:sdtContent>").count(), 1);
    let sdt = &xml[xml.find("<w:sdtContent>").unwrap()..xml.find("</w:sdtContent>").unwrap()];
    assert_eq!(
        sdt.matches("<w:p>").count() + sdt.matches("<w:p ").count(),
        3
    );
    // Removing everything in the control keeps the control.
    let xml = make(&|d| {
        let from = start_of(d, "inside");
        let to = end_of(d, "second");
        delete(d, &from, &to);
    });
    assert_eq!(xml.matches("<w:sdt>").count(), 1);
    // A new paragraph before the control stays out of it.
    let xml = make(&|d| {
        let at = end_of(d, "out");
        let next = split_paragraph(d, &at);
        insert_text(d, &next, "new", None);
    });
    assert!(xml.find("new").unwrap() < xml.find("<w:sdt>").unwrap());
}

#[test]
fn a_run_level_content_control_and_smart_tag_keep_their_wrappers() {
    let inner = r#"<w:r><w:t xml:space="preserve">a </w:t></w:r><w:sdt><w:sdtPr><w:alias w:val="x"/></w:sdtPr><w:sdtContent><w:r><w:t>boxed</w:t></w:r></w:sdtContent></w:sdt><w:smartTag w:uri="u" w:element="place"><w:r><w:t>Paris</w:t></w:r></w:smartTag>"#;
    let (_, saved) = run_edit(inner, |d| {
        let at = after(d, "box");
        insert_text(d, &at, "--", None);
        let at = after(d, "Par");
        insert_text(d, &at, "++", Some(&bold_at(d, &at)));
    });
    let xml = paragraph_xml(&saved);
    assert!(xml.contains("<w:sdtPr>") && xml.contains("<w:smartTag "));
}

#[test]
fn tables_nested_tables_and_cells_keep_their_bytes() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let at = after(&doc, "n1");
    insert_text(&mut doc, &at, "A", None);
    let at = after(&doc, "Merged down");
    insert_text(&mut doc, &at, "B", None);
    let saved = check(&bytes, &doc, &[]);
    let (before, after_) = (
        part(&bytes, "word/document.xml"),
        part(&saved, "word/document.xml"),
    );
    // The rest of the table, its grid and its properties are as they were.
    for keep in [
        "<w:tblGrid><w:gridCol w:w=\"2000\"/>",
        "<w:vMerge w:val=\"restart\"/>",
        "<w:trHeight w:val=\"700\" w:hRule=\"exact\"/>",
        "<w:tcBorders><w:left w:val=\"nil\"/></w:tcBorders>",
    ] {
        assert!(before.contains(keep) && after_.contains(keep), "{keep}");
    }
    assert_eq!(
        before.matches("<w:tc>").count(),
        after_.matches("<w:tc>").count()
    );
}

#[test]
fn splitting_and_joining_paragraphs_with_wrappers_and_markers() {
    let bytes = sample();
    let mut doc = read(&bytes);
    // The paragraph with a link, a jump, a footnote, a comment range and a
    // tracked insertion: split in the middle of each in turn, then join.
    for needle in ["a link", "jump", "Note", "commented", "inserted"] {
        let at = after(&doc, needle);
        let next = split_paragraph(&mut doc, &at);
        check(&bytes, &doc, &[]);
        delete_backward(&mut doc, &next);
        check(&bytes, &doc, &[]);
    }
}

// ---------------------------------------------------------------------------
// Lists and the blank document
// ---------------------------------------------------------------------------

#[test]
fn new_lists_are_written_into_the_numbering_part() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let from = start_of(&doc, "Before the break");
    set_list(&mut doc, &from, &from, Some(ListKind::Numbered));
    // The sample has numbered list 1 already: reused, nothing new.
    let saved = check(&bytes, &doc, &[]);
    assert_eq!(
        parts(&saved)["word/numbering.xml"],
        parts(&bytes)["word/numbering.xml"]
    );
    // A list of a new shape gets added.
    let mut doc = read(&bytes);
    let at = start_of(&doc, "Before the break");
    set_list(&mut doc, &at, &at, Some(ListKind::Bullet));
    let at2 = start_of(&doc, "Inside a content control");
    set_list(&mut doc, &at2, &at2, Some(ListKind::Numbered));
    check(&bytes, &doc, &[]);
}

#[test]
fn the_blank_document_reads_and_a_typed_document_saves() {
    let bytes = blank();
    assert_well_formed(&bytes);
    let doc = read(&bytes);
    assert_eq!(doc.last_section.page, (11_906, 16_838));
    for id in [
        "Normal",
        "Title",
        "Heading1",
        "Heading6",
        "ListParagraph",
        "Hyperlink",
        "TableGrid",
    ] {
        assert!(doc.styles.get(id).is_some(), "{id}");
    }
    assert!(doc.settings.compress_punctuation);
    assert_eq!(doc.settings.language.as_deref(), Some("en-US"));
    assert_eq!(doc.settings.east_asian_language.as_deref(), Some("zh-CN"));
    assert_eq!(doc.body.blocks.len(), 1);
    assert!(doc.paragraphs().iter().all(|(_, p)| p.is_as_read()));
    // Type a document into it: a title, a heading, text in two scripts, a
    // list and a table-free paragraph.
    let mut doc = doc;
    let mut at = Pos::new(Path::body(0), 0);
    set_style(&mut doc, &at, &at, "Title");
    at = insert_text(&mut doc, &at, "Notes 笔记", None);
    at = split_paragraph(&mut doc, &at);
    set_style(&mut doc, &at, &at, "Heading1");
    at = insert_text(&mut doc, &at, "First 第一", None);
    at = split_paragraph(&mut doc, &at);
    at = insert_text(&mut doc, &at, "Some text with 日本語 and ", None);
    let bold = bold_at(&doc, &at);
    at = insert_text(&mut doc, &at, "bold", Some(&bold));
    at = split_paragraph(&mut doc, &at);
    at = insert_text(&mut doc, &at, "one\ntwo\nthree", None);
    let first = Pos::new(Path::body(at.path.top() - 2), 0);
    set_list(&mut doc, &first, &at, Some(ListKind::Bullet));
    let saved = check(
        &bytes,
        &doc,
        &[
            "word/numbering.xml",
            "word/_rels/document.xml.rels",
            "[Content_Types].xml",
        ],
    );
    let back = read(&saved);
    assert_eq!(back.plain_text(), doc.plain_text());
    assert!(parts(&saved).contains_key("word/numbering.xml"));
    assert!(part(&saved, "[Content_Types].xml").contains("numbering+xml"));
    assert!(part(&saved, "word/_rels/document.xml.rels").contains("numbering.xml"));
}

// ---------------------------------------------------------------------------
// Documents that do not fit their sources
// ---------------------------------------------------------------------------

#[test]
fn a_document_that_does_not_match_the_file_saves_without_panicking() {
    let bytes = sample();
    let other =
        docx_doc::doc("<w:p><w:r><w:t>one</w:t></w:r></w:p><w:p><w:r><w:t>two</w:t></w:r></w:p>")
            .bytes();
    // A document read from another file, saved over this one: its ranges mean
    // nothing here. It must not panic; the result reads back as that
    // document.
    let stranger = read(&other);
    let saved = save(bytes.clone(), &stranger).expect("saves");
    assert_well_formed(&saved);
    assert_eq!(read(&saved).plain_text(), stranger.plain_text());
    // And the other way round, with ranges out of bounds.
    let big = read(&bytes);
    let saved = save(other.clone(), &big).expect("saves");
    assert_well_formed(&saved);
    // Drawings and rubies cannot be written without the bytes they were read
    // from; everything else is there.
    let back = read(&saved);
    assert_eq!(back.paragraphs().len(), big.paragraphs().len());
    assert!(back.plain_text().contains("The quick brown fox"));
    // A document made from nothing.
    let mut empty = Document::default();
    empty
        .body
        .blocks
        .push(Block::paragraph(aster_writer::Paragraph {
            runs: vec![Run::text("made here", RunLook::default())],
            ..Default::default()
        }));
    let saved = save(bytes, &empty).expect("saves");
    assert_well_formed(&saved);
    assert_eq!(read(&saved).plain_text(), "made here");
    let _ = Inline::Tab;
}

#[test]
fn a_package_that_is_not_a_document_is_refused() {
    let doc = Document::default();
    assert!(save(b"not a zip".to_vec(), &doc).is_err());
}

#[test]
fn sources_that_point_into_the_wrong_bytes_never_panic() {
    let bytes = sample();
    for shift in [1usize, 7, 5_000, 1_000_000] {
        let mut doc = read(&bytes);
        let paths: Vec<Path> = doc.paragraphs().into_iter().map(|(p, _)| p).collect();
        for path in &paths {
            let p = doc.paragraph_mut(path).unwrap();
            let source = p.source.clone().unwrap();
            let moved = |r: &std::ops::Range<usize>| r.start + shift..r.end + shift;
            let runs: Vec<_> = source
                .runs
                .iter()
                .map(|r| {
                    Arc::new(aster_writer::RunSource {
                        range: moved(&r.range),
                        parent: r.parent.as_ref().map(moved),
                        ..(**r).clone()
                    })
                })
                .collect();
            for (run, src) in p.runs.iter_mut().zip(&runs) {
                run.source = Some(src.clone());
            }
            p.source = Some(Arc::new(aster_writer::ParaSource {
                range: moved(&source.range),
                runs,
                ..(*source).clone()
            }));
        }
        let at = after(&doc, "Introduction");
        insert_text(&mut doc, &at, "!", None);
        let at = after(&doc, "quick brown");
        split_paragraph(&mut doc, &at);
        let saved = save(bytes.clone(), &doc).expect("saves");
        assert_well_formed(&saved);
        let back = read(&saved);
        assert_eq!(back.paragraphs().len(), doc.paragraphs().len());
        assert!(back.plain_text().contains("Introduction!"));
    }
}

#[test]
fn a_byte_order_mark_and_empty_elements_are_handled() {
    let mut d = docx_doc::doc(
        "<w:p/><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr></w:p><w:p><w:r><w:t>text</w:t></w:r></w:p>",
    );
    d.bom = true;
    let bytes = d.bytes();
    let mut doc = read(&bytes);
    insert_text(
        &mut doc,
        &Pos::new(Path::body(0), 0),
        "into an empty one",
        None,
    );
    insert_text(&mut doc, &Pos::new(Path::body(1), 0), "centred", None);
    let at = Pos::new(Path::body(2), 2);
    split_paragraph(&mut doc, &at);
    let saved = check(&bytes, &doc, &[]);
    let xml = parts(&saved)["word/document.xml"].clone();
    assert!(xml.starts_with(b"\xEF\xBB\xBF"));
    assert_eq!(
        read(&saved).plain_text(),
        "into an empty one\ncentred\nte\nxt"
    );
}

#[test]
fn a_section_break_goes_with_its_paragraph_and_stays_when_it_is_split() {
    let bytes = sample();
    let count = |saved: &[u8]| {
        part(saved, "word/document.xml")
            .matches("<w:sectPr>")
            .count()
    };
    let before = count(&bytes);
    assert_eq!(before, 3);
    // The empty paragraph that ends section one is taken into the paragraph
    // before it: the break is carried by the joined paragraph.
    let mut doc = read(&bytes);
    let end = end_of(&doc, "After the break");
    delete_forward(&mut doc, &end);
    let saved = check(&bytes, &doc, &[]);
    assert_eq!(count(&saved), 3);
    // The paragraph that ends section one is taken into the one after it:
    // that one has no break, so the break is gone.
    let mut doc = read(&bytes);
    let end = end_of(&doc, "After the break");
    let next = {
        let mut path = end.path.clone();
        *path.0.last_mut().unwrap() += 1;
        Pos::new(path, 0)
    };
    let after_it = {
        let mut path = next.path.clone();
        *path.0.last_mut().unwrap() += 1;
        Pos::new(path, 0)
    };
    delete(&mut doc, &next, &after_it);
    assert_eq!(doc.sections().len(), 2);
    // (The line grid of the section that is gone changes what spacing in
    // lines comes to for the paragraphs in it; the model does not follow, so
    // the content is not compared here.)
    let saved = save(bytes.clone(), &doc).expect("saves");
    assert_well_formed(&saved);
    assert_eq!(count(&saved), 2);
    assert_eq!(read(&saved).sections().len(), 2);
    assert_eq!(read(&saved).plain_text(), doc.plain_text());
    // Enter at the end of the paragraph with the break: the break moves to
    // the new paragraph.
    let mut doc = read(&bytes);
    let at = Pos::new(
        {
            let mut path = end_of(&doc, "After the break").path;
            *path.0.last_mut().unwrap() += 1;
            path
        },
        0,
    );
    split_paragraph(&mut doc, &at);
    let saved = check(&bytes, &doc, &[]);
    assert_eq!(count(&saved), 3);
    assert_eq!(read(&saved).sections().len(), 3);
}

// ---------------------------------------------------------------------------
// Random edits
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn random_pos(doc: &Document, rng: &mut Rng) -> Pos {
    let paragraphs = doc.paragraphs();
    let (path, p) = &paragraphs[rng.below(paragraphs.len())];
    let text = p.text();
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    Pos::new(path.clone(), boundaries[rng.below(boundaries.len())])
}

fn random_edit(doc: &mut Document, rng: &mut Rng, log: &mut Vec<String>) {
    const SNIPPETS: [&str; 7] = ["x", "日本語", "😀", "a\tb", "line\nline", " ", "é"];
    let a = random_pos(doc, rng);
    let kind = rng.below(18);
    log.push(format!("op {kind} at {a:?}"));
    match kind {
        0..=2 => {
            let text = SNIPPETS[rng.below(SNIPPETS.len())];
            let look = (rng.below(3) == 0).then(|| bold_at(doc, &a));
            insert_text(doc, &a, text, look.as_ref());
        }
        3 => {
            split_paragraph(doc, &a);
        }
        4 => {
            delete_backward(doc, &a);
        }
        5 => {
            delete_forward(doc, &a);
        }
        6 | 7 => {
            let b = random_pos(doc, rng);
            // Keep ranges short most of the time.
            let b = if rng.below(4) == 0 || a.path == b.path {
                b
            } else {
                a.clone()
            };
            delete(doc, &a, &b);
        }
        8 | 9 => {
            let b = random_pos(doc, rng);
            let patch = match rng.below(4) {
                0 => make_bold(),
                1 => RunPatch {
                    italic: Some(rng.below(2) == 0),
                    ..RunPatch::default()
                },
                2 => RunPatch {
                    color: Some(Some([200, 0, 0, 255])),
                    size: Some(13.0),
                    ..RunPatch::default()
                },
                _ => RunPatch {
                    bold: Some(false),
                    underline: Some(None),
                    ..RunPatch::default()
                },
            };
            apply_run_patch(doc, &a, &b, &patch);
        }
        10 => {
            let b = random_pos(doc, rng);
            let align = [Align::Left, Align::Center, Align::Right, Align::Justify][rng.below(4)];
            set_align(doc, &a, &b, align);
            change_indent(doc, &a, &b, rng.below(3) as i32 - 1);
        }
        11 => {
            // In a table cell the model cannot tell what the table style and
            // what the old paragraph style gave the base look, so the base
            // after a new style is only approximately what the file says;
            // styles are set on body paragraphs here.
            let id = ["Normal", "Heading1", "Title", "Heading3"][rng.below(4)];
            if a.path.0.len() == 1 {
                set_style(doc, &a, &a, id);
            }
        }
        12 => {
            let b = random_pos(doc, rng);
            let kind = match rng.below(3) {
                0 => None,
                1 => Some(ListKind::Bullet),
                _ => Some(ListKind::Numbered),
            };
            set_list(doc, &a, &b, kind);
        }
        13 => {
            change_level(doc, &a, &a, rng.below(3) as i32 - 1);
        }
        14 => {
            let b = random_pos(doc, rng);
            let part = fragment(doc, &a, &b);
            let c = random_pos(doc, rng);
            // The text in a text box reads with the table style of the cell
            // it ends up in, which the model does not follow: no text boxes
            // into cells here.
            let drawing = part
                .iter()
                .filter_map(Block::as_paragraph)
                .any(|p| p.runs.iter().any(|r| matches!(r.kind, Inline::Drawing(_))));
            if !(drawing && c.path.0.len() > 1) {
                insert_fragment(doc, &c, &part);
            }
        }
        15 => {
            insert_break(doc, &a, [Break::Line, Break::Page][rng.below(2)]);
        }
        16 => {
            set_line_spacing(
                doc,
                &a,
                &a,
                LineSpacing::Multiple(1.0 + rng.below(3) as f32 * 0.25),
            );
            set_space_after(doc, &a, &a, rng.below(300) as i32);
        }
        _ => {
            let b = random_pos(doc, rng);
            let (start, end) = if a <= b { (a, b) } else { (b, a) };
            let (start, end) = if start.path.0.len() == 1 && end.path.0.len() == 1 {
                (start, end)
            } else {
                (start.clone(), start)
            };
            delete(doc, &start, &end);
        }
    }
}

#[test]
fn random_edits_save_and_read_back() {
    let bytes = sample();
    let sections = read(&bytes).sections().len();
    let mut skipped = 0;
    for seed in 1..=500u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut doc = read(&bytes);
        let steps = 2 + rng.below(8);
        let mut log: Vec<String> = Vec::new();
        for _ in 0..steps {
            random_edit(&mut doc, &mut rng, &mut log);
        }
        // A section break that goes takes its line grid with it, and what a
        // paragraph's spacing in lines comes to depends on the grid: the file
        // then reads differently from the model, as it should. Those
        // sequences are left out here (the section tests above cover them).
        if doc.sections().len() != sections {
            skipped += 1;
            continue;
        }
        let numbering = doc.numbering.lists.len() > read(&bytes).numbering.lists.len();
        let may: &[&str] = if numbering {
            &["word/numbering.xml"]
        } else {
            &[]
        };
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&bytes, &doc, may)));
        if let Err(err) = result {
            eprintln!("seed {seed} failed after {steps} edits: {log:#?}");
            std::panic::resume_unwind(err);
        }
    }
    assert!(skipped < 250, "{skipped} sequences were left out");
}

// ---------------------------------------------------------------------------
// Tables, links and pictures made by the editing operations
// ---------------------------------------------------------------------------

const GRID_STYLES: &str = r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblCellMar><w:left w:w="108" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>"#;

fn tc(width: u32, props: &str, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{props}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

/// A paragraph, a table of four rows over three columns (a cell spanning
/// two columns in row 1, a merge down in the last column of rows 2 and 3),
/// and a paragraph.
fn grid_body() -> String {
    let mut t = String::from(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="6000" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>"#,
    );
    t += r#"<w:tr w:rsidR="00A10001">"#;
    t += &(tc(2000, "", "r0c0") + &tc(2000, "", "r0c1") + &tc(2000, "", "r0c2"));
    t += "</w:tr>";
    t += r#"<w:tr w:rsidR="00A10002"><w:trPr><w:cantSplit/></w:trPr>"#;
    t += &(tc(4000, r#"<w:gridSpan w:val="2"/>"#, "r1 wide") + &tc(2000, "", "r1c2"));
    t += "</w:tr>";
    t += r#"<w:tr w:rsidR="00A10003">"#;
    t += &(tc(2000, "", "r2c0")
        + &tc(2000, "", "r2c1")
        + &tc(2000, r#"<w:vMerge w:val="restart"/>"#, "r2c2"));
    t += "</w:tr>";
    t += r#"<w:tr w:rsidR="00A10004">"#;
    t += &(tc(2000, "", "r3c0") + &tc(2000, "", "r3c1") + &tc(2000, r#"<w:vMerge/>"#, ""));
    t += "</w:tr></w:tbl>";
    format!(
        "<w:p><w:r><w:t>before the table</w:t></w:r></w:p>{t}<w:p><w:r><w:t>after the table</w:t></w:r></w:p>"
    )
}

fn grid_bytes() -> Vec<u8> {
    docx_doc::doc(&grid_body()).styles(GRID_STYLES).bytes()
}

/// The table's block index in the body.
fn table_index(doc: &Document) -> usize {
    doc.body
        .blocks
        .iter()
        .position(|b| b.as_table().is_some())
        .expect("a table")
}

/// A position in a cell of the first table.
fn cell_pos(doc: &Document, row: usize, cell: usize) -> Pos {
    let t = table_index(doc);
    Pos::new(Path::body(t).cell_block(row, cell, 0), 0)
}

fn assert_tables_valid(doc: &Document) {
    for block in &doc.body.blocks {
        if let Block::Table(t) = block {
            check_table(t).unwrap();
        }
    }
}

/// The original bytes of row `row` of the table in `bytes`.
fn row_bytes(bytes: &[u8], row: usize) -> String {
    let doc = read(bytes);
    let t = doc.body.blocks[table_index(&doc)].as_table().unwrap();
    let xml = part(bytes, "word/document.xml");
    xml[t.rows[row].source.clone().unwrap()].to_owned()
}

fn table_xml(saved: &[u8]) -> String {
    let xml = part(saved, "word/document.xml");
    let start = xml.find("<w:tbl>").unwrap();
    let end = xml.rfind("</w:tbl>").unwrap() + 8;
    xml[start..end].to_owned()
}

fn grid_edit(edit: impl FnOnce(&mut Document)) -> (Vec<u8>, Document, Vec<u8>) {
    let bytes = grid_bytes();
    let mut doc = read(&bytes);
    edit(&mut doc);
    assert_tables_valid(&doc);
    let saved = check(&bytes, &doc, &[]);
    (bytes, doc, saved)
}

#[test]
fn rows_added_keep_the_rows_and_the_table_start_as_they_were() {
    let (bytes, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 1, 0);
        insert_rows(d, &at, &at, false);
        let at = cell_pos(d, 0, 0);
        insert_rows(d, &at, &at, true);
    });
    let (before, after_) = (table_xml(&bytes), table_xml(&saved));
    // The start of the table, up to the first row, is byte for byte.
    let head = before.find("<w:tr ").unwrap();
    assert!(after_.starts_with(&before[..head]));
    for row in 0..4 {
        assert!(after_.contains(&row_bytes(&bytes, row)), "row {row}");
    }
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    assert_eq!(t.rows.len(), 6);
    assert_eq!(t.columns, vec![2000, 2000, 2000]);
    check_table(t).unwrap();
}

#[test]
fn rows_removed_keep_the_other_rows_byte_for_byte() {
    let (bytes, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 0, 0);
        delete_rows(d, &at, &at);
    });
    let after_ = table_xml(&saved);
    assert!(!after_.contains("r0c0"));
    for row in 1..4 {
        assert!(after_.contains(&row_bytes(&bytes, row)), "row {row}");
    }
    let before = table_xml(&bytes);
    let head = before.find("<w:tr ").unwrap();
    assert!(after_.starts_with(&before[..head]));
}

#[test]
fn removing_the_start_of_a_merge_rewrites_only_the_row_that_becomes_it() {
    let (bytes, doc, saved) = grid_edit(|d| {
        let at = cell_pos(d, 2, 0);
        delete_rows(d, &at, &at);
    });
    let t = doc.body.blocks[table_index(&doc)].as_table().unwrap();
    // The merge had two cells; one is left, an ordinary cell.
    assert_eq!(t.rows[2].cells[2].merge, aster_writer::VMerge::None);
    let after_ = table_xml(&saved);
    assert!(after_.contains(&row_bytes(&bytes, 0)));
    assert!(after_.contains(&row_bytes(&bytes, 1)));
    assert!(!after_.contains("w:vMerge"));
}

#[test]
fn rows_added_inside_a_merge_stay_in_it() {
    let (_, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 2, 2);
        insert_rows(d, &at, &at, false);
    });
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    assert_eq!(t.rows.len(), 5);
    let merges: Vec<_> = t
        .rows
        .iter()
        .map(|r| r.cells.last().unwrap().merge)
        .collect();
    use aster_writer::VMerge::{Continue, None as No, Start};
    assert_eq!(merges, [No, No, Start, Continue, Continue]);
    check_table(t).unwrap();
}

#[test]
fn columns_added_keep_the_table_properties_and_the_text_of_the_cells() {
    let (bytes, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 0, 1);
        insert_columns(d, &at, &at, false);
    });
    let after_ = table_xml(&saved);
    let before = table_xml(&bytes);
    // `tblPr` as it was, but for the width that follows the grid.
    let pr = &before[before.find("<w:tblPr>").unwrap()..before.find("</w:tblPr>").unwrap()];
    assert!(after_.contains(&pr.replace("w:w=\"6000\"", "w:w=\"8000\"")));
    assert_eq!(after_.matches("<w:gridCol").count(), 4);
    // The rows keep their own start tags and properties.
    assert!(after_.contains(r#"<w:tr w:rsidR="00A10002"><w:trPr><w:cantSplit/></w:trPr>"#));
    // The paragraphs in the cells are the original bytes.
    for text in ["r0c0", "r1c2", "r3c1"] {
        let original = format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>");
        assert!(after_.contains(&original), "{text}");
    }
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    assert_eq!(t.columns, vec![2000, 2000, 2000, 2000]);
    check_table(t).unwrap();
    // The new column is right of column 1: the wide cell is left of it.
    assert_eq!(t.rows[1].cells[0].span, 2);
    assert_eq!(t.rows[1].cells.len(), 3);
}

#[test]
fn a_column_through_a_spanning_cell_widens_it_in_the_file() {
    let (_, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 0, 1);
        insert_columns(d, &at, &at, true);
    });
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    assert_eq!(t.rows[1].cells[0].span, 3);
    check_table(t).unwrap();
}

#[test]
fn columns_removed_narrow_spans_and_drop_cells() {
    let (_, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 0, 1);
        delete_columns(d, &at, &at);
    });
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    assert_eq!(t.columns, vec![2000, 2000]);
    assert_eq!(t.rows[1].cells[0].span, 1);
    assert_eq!(t.rows[0].cells.len(), 2);
    check_table(t).unwrap();
    assert!(table_xml(&saved).contains("<w:tblW w:w=\"4000\" w:type=\"dxa\"/>"));
}

#[test]
fn a_deleted_table_leaves_the_rest_untouched() {
    let (bytes, doc, saved) = grid_edit(|d| {
        let at = cell_pos(d, 1, 0);
        delete_table(d, &at);
    });
    assert!(doc.body.blocks.iter().all(|b| b.as_table().is_none()));
    let (before, after_) = (
        part(&bytes, "word/document.xml"),
        part(&saved, "word/document.xml"),
    );
    assert!(!after_.contains("<w:tbl>"));
    let cut = before.find("<w:tbl>").unwrap();
    assert!(after_.starts_with(&before[..cut]));
    assert!(after_.contains("after the table"));
}

#[test]
fn a_new_table_is_written_with_grid_borders_and_cells() {
    let (bytes, _, saved) = grid_edit(|d| {
        let at = after(d, "before");
        insert_table(d, &at, 2, 3, 9_000);
    });
    let back = read(&saved);
    let first = back.body.blocks.iter().find_map(Block::as_table).unwrap();
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.columns, vec![3000, 3000, 3000]);
    assert_eq!(first.props.style.as_deref(), Some("TableGrid"));
    assert!(first.rows[0].cells[0].borders.iter().all(Option::is_some));
    let xml = part(&saved, "word/document.xml");
    for needle in [
        "<w:tblStyle w:val=\"TableGrid\"/>",
        "<w:tblW w:w=\"9000\" w:type=\"dxa\"/>",
        "<w:tblBorders>",
        "<w:tcBorders>",
        "<w:tblGrid><w:gridCol w:w=\"3000\"/>",
    ] {
        assert!(xml.contains(needle), "{needle}");
    }
    // The old table is as it was.
    assert!(xml.contains(&table_xml(&bytes)));
}

#[test]
fn a_table_in_a_cell_is_nested_and_a_table_in_a_nested_table_too() {
    let (_, _, saved) = grid_edit(|d| {
        let at = cell_pos(d, 0, 0);
        let caret = insert_table(d, &at, 1, 2, 1_800);
        insert_table(d, &caret, 1, 1, 800);
    });
    let back = read(&saved);
    let outer = back.body.blocks[table_index(&back)].as_table().unwrap();
    let cell = &outer.rows[0].cells[0];
    let nested = cell.blocks.iter().find_map(Block::as_table).unwrap();
    assert_eq!(nested.columns, vec![900, 900]);
    assert!(
        nested.rows[0].cells[0]
            .blocks
            .iter()
            .any(|b| b.as_table().is_some())
    );
}

#[test]
fn a_table_edit_that_is_undone_saves_the_file_it_was() {
    let bytes = grid_bytes();
    let mut doc = read(&bytes);
    let before = doc.clone();
    let at = cell_pos(&doc, 1, 0);
    insert_rows(&mut doc, &at, &at, false);
    let mut history = History::default();
    let sel = Selection::caret(at.clone());
    history.record(&before, &sel, &sel, None);
    let (restored, _) = history.undo(&doc).unwrap();
    assert_eq!(save(bytes.clone(), &restored).unwrap(), bytes);
}

#[test]
fn tables_with_merges_and_nesting_survive_row_and_column_edits() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let at = after(&doc, "Merged across");
    insert_rows(&mut doc, &at, &at, false);
    let at = after(&doc, "Score");
    let _ = insert_columns(&mut doc, &at, &at, true);
    assert_tables_valid(&doc);
    let saved = save(bytes.clone(), &doc).unwrap();
    assert_well_formed(&saved);
    let back = read(&saved);
    assert_tables_valid(&back);
    let (a, b) = (
        doc.body.blocks.iter().find_map(Block::as_table).unwrap(),
        back.body.blocks.iter().find_map(Block::as_table).unwrap(),
    );
    assert_eq!(
        (a.columns.len(), a.rows.len()),
        (b.columns.len(), b.rows.len())
    );
    for (r, s) in a.rows.iter().zip(&b.rows) {
        assert_eq!(
            r.cells
                .iter()
                .map(|c| (c.span, c.merge))
                .collect::<Vec<_>>(),
            s.cells
                .iter()
                .map(|c| (c.span, c.merge))
                .collect::<Vec<_>>()
        );
    }
    // The nested table was in a cell that was written again: its bytes stay.
    assert!(part(&saved, "word/document.xml").contains("<w:t>n1</w:t>"));
}

// -- links -------------------------------------------------------------------

fn plain_link_doc() -> Vec<u8> {
    docx_doc::doc(
        r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>this thing</w:t></w:r><w:r><w:t xml:space="preserve"> now</w:t></w:r></w:p>"#,
    )
    .styles(GRID_STYLES)
    .bytes()
}

fn target() -> Link {
    Link {
        target: "https://example.org/page".to_owned(),
        tooltip: None,
    }
}

#[test]
fn a_link_added_to_text_that_was_read() {
    let bytes = plain_link_doc();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "this"), after(&doc, "this thing"));
    set_link(&mut doc, &from, &to, Some(target()));
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let xml = paragraph_xml(&saved);
    assert_eq!(xml.matches("<w:hyperlink").count(), 1);
    assert!(xml.contains("<w:rStyle w:val=\"Hyperlink\"/>"));
    // The run keeps its bold and its text.
    assert!(xml.contains("<w:b/>") && xml.contains("this thing"));
    let rels = part(&saved, "word/_rels/document.xml.rels");
    assert!(rels.contains("https://example.org/page") && rels.contains("External"));
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let linked: Vec<_> = p.runs.iter().filter(|r| r.link.is_some()).collect();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].text, "this thing");
    assert!(linked[0].look.bold);
    assert_eq!(linked[0].link.as_deref(), Some(&target()));
    // The first run is the bytes it was.
    assert!(xml.contains(r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:hyperlink"#));
}

#[test]
fn a_link_added_to_part_of_a_run_splits_it_inside_the_hyperlink() {
    let bytes = plain_link_doc();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "thing"), after(&doc, "thing"));
    set_link(&mut doc, &from, &to, Some(target()));
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let marked: Vec<_> = p
        .runs
        .iter()
        .map(|r| (r.text.as_str(), r.link.is_some()))
        .collect();
    assert_eq!(
        marked,
        [
            ("see ", false),
            ("this ", false),
            ("thing", true),
            (" now", false)
        ]
    );
}

fn linked_bytes() -> Vec<u8> {
    docx_doc::doc(
        r#"<w:p><w:r><w:t xml:space="preserve">go </w:t></w:r><w:hyperlink r:id="rId9" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/><w:b/></w:rPr><w:t>to the site</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> please</w:t></w:r></w:p>"#,
    )
    .styles(GRID_STYLES)
    .rel("rId9", "hyperlink", "https://example.org/")
    .bytes()
}

#[test]
fn a_link_removed_from_text_that_was_read_leaves_the_text_and_its_formatting() {
    let bytes = linked_bytes();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "to the"), after(&doc, "site"));
    set_link(&mut doc, &from, &to, None);
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let xml = paragraph_xml(&saved);
    assert!(!xml.contains("hyperlink"), "{xml}");
    assert!(xml.contains("to the site"));
    assert!(xml.contains("<w:b/>"));
    assert!(!xml.contains("Hyperlink"));
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    assert!(p.runs.iter().all(|r| r.link.is_none() && r.style.is_none()));
    assert!(
        p.runs
            .iter()
            .any(|r| r.look.bold && r.text == "to the site")
    );
    assert_eq!(p.text(), "go to the site please");
}

#[test]
fn removing_a_link_from_part_of_it_leaves_the_rest_linked() {
    let bytes = linked_bytes();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "to the"), after(&doc, "to the"));
    set_link(&mut doc, &from, &to, None);
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let marked: Vec<_> = p
        .runs
        .iter()
        .map(|r| (r.text.as_str(), r.link.is_some()))
        .collect();
    assert_eq!(
        marked,
        [
            ("go ", false),
            ("to the", false),
            (" site", true),
            (" please", false)
        ]
    );
    assert_eq!(paragraph_xml(&saved).matches("<w:hyperlink").count(), 1);
}

#[test]
fn a_link_with_another_target_replaces_the_old_one() {
    let bytes = linked_bytes();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "to the"), after(&doc, "site"));
    set_link(
        &mut doc,
        &from,
        &to,
        Some(Link {
            target: "https://example.net/new".to_owned(),
            tooltip: Some("new".to_owned()),
        }),
    );
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    let link = p.runs.iter().find_map(|r| r.link.clone()).unwrap();
    assert_eq!(link.target, "https://example.net/new");
    assert_eq!(link.tooltip.as_deref(), Some("new"));
    assert_eq!(paragraph_xml(&saved).matches("<w:hyperlink").count(), 1);
    assert!(paragraph_xml(&saved).contains("w:tooltip=\"new\""));
}

#[test]
fn a_link_to_a_bookmark_is_an_anchor_with_no_relationship() {
    let bytes = plain_link_doc();
    let mut doc = read(&bytes);
    let (from, to) = (before(&doc, "this"), after(&doc, "this"));
    set_link(
        &mut doc,
        &from,
        &to,
        Some(Link {
            target: "#chapter_1".to_owned(),
            tooltip: None,
        }),
    );
    let saved = check(&bytes, &doc, &[]);
    let xml = paragraph_xml(&saved);
    assert!(xml.contains("<w:hyperlink w:anchor=\"chapter_1\""));
    assert!(!xml.contains("r:id"));
    assert_eq!(
        part(&saved, "word/_rels/document.xml.rels"),
        part(&bytes, "word/_rels/document.xml.rels")
    );
    let back = read(&saved);
    let p = back.paragraph(&Path::body(0)).unwrap();
    assert!(
        p.runs
            .iter()
            .any(|r| r.link.as_ref().is_some_and(|l| l.target == "#chapter_1"))
    );
}

#[test]
fn a_linked_text_inserted_is_one_hyperlink() {
    let bytes = plain_link_doc();
    let mut doc = read(&bytes);
    let at = after(&doc, "see ");
    insert_link(&mut doc, &at, "HERE", target());
    let saved = check(&bytes, &doc, &["word/_rels/document.xml.rels"]);
    assert_eq!(paragraph_xml(&saved).matches("<w:hyperlink").count(), 1);
    assert!(paragraph_xml(&saved).contains("HERE"));
}

// -- pictures ----------------------------------------------------------------

fn png_bytes(width: u32, height: u32) -> Arc<[u8]> {
    Arc::from(docx_sample::png(width, height))
}

fn jpeg_like() -> Arc<[u8]> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend(b"not really a picture");
    Arc::from(bytes)
}

const MEDIA_CHANGES: &[&str] = &[
    "word/_rels/document.xml.rels",
    "[Content_Types].xml",
    "word/media/image1.png",
    "word/media/image2.png",
    "word/media/image1.jpeg",
    "word/media/image3.png",
    "word/media/image2.jpeg",
];

fn drawing_sizes(doc: &Document) -> Vec<(i64, i64)> {
    doc.paragraphs()
        .iter()
        .flat_map(|(_, p)| p.runs.iter())
        .filter_map(|r| match &r.kind {
            Inline::Drawing(d) => Some(d.size),
            _ => None,
        })
        .collect()
}

#[test]
fn a_picture_is_stored_once_per_image_and_related() {
    let bytes = grid_bytes();
    let mut doc = read(&bytes);
    let png = png_bytes(4, 3);
    let at = after(&doc, "before");
    let end = insert_picture(
        &mut doc,
        &at,
        png.clone(),
        (914_400, 685_800),
        Some("a & b".into()),
    )
    .unwrap();
    insert_picture(&mut doc, &end, png.clone(), (100_000, 100_000), None).unwrap();
    let at = after(&doc, "after the");
    insert_picture(&mut doc, &at, jpeg_like(), (200_000, 300_000), None).unwrap();
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    let all = parts(&saved);
    let media: Vec<&String> = all
        .keys()
        .filter(|n| n.starts_with("word/media/"))
        .collect();
    assert_eq!(media.len(), 2, "{media:?}");
    assert_eq!(all["word/media/image1.png"], png.to_vec());
    assert_eq!(all["word/media/image2.jpeg"], jpeg_like().to_vec());
    let rels = part(&saved, "word/_rels/document.xml.rels");
    assert_eq!(rels.matches("relationships/image").count(), 2);
    assert!(rels.contains("Target=\"media/image1.png\""));
    let types = part(&saved, "[Content_Types].xml");
    assert!(types.contains("Extension=\"png\"") && types.contains("image/png"));
    assert!(types.contains("Extension=\"jpeg\"") && types.contains("image/jpeg"));
    // Read back: the same pictures at the same sizes.
    let back = read(&saved);
    assert_eq!(
        drawing_sizes(&back),
        [(914_400, 685_800), (100_000, 100_000), (200_000, 300_000)]
    );
    let alt: Vec<Option<String>> = back
        .paragraphs()
        .iter()
        .flat_map(|(_, p)| p.runs.iter())
        .filter_map(|r| match &r.kind {
            Inline::Drawing(d) => Some(d.alt.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(alt[0].as_deref(), Some("a & b"));
}

#[test]
fn drawing_namespaces_are_declared_where_the_file_has_none() {
    // Remove the drawing namespaces from the root: they are added.
    let bytes = grid_bytes();
    let mut all = parts(&bytes);
    let xml = String::from_utf8(all["word/document.xml"].clone()).unwrap();
    let mut stripped = xml.clone();
    for ns in [
        " xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"",
        " xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"",
        " xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"",
        " xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
    ] {
        stripped = stripped.replace(ns, "");
    }
    assert_ne!(stripped, xml);
    all.insert("word/document.xml".to_owned(), stripped.into_bytes());
    let list: Vec<(String, Vec<u8>)> = all.into_iter().collect();
    let bytes = docx_sample::zip(&list);
    let mut doc = read(&bytes);
    let at = after(&doc, "before");
    insert_picture(&mut doc, &at, png_bytes(2, 2), (10_000, 10_000), None).unwrap();
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    let xml = part(&saved, "word/document.xml");
    let root = &xml[..xml.find("<w:body>").unwrap()];
    for ns in ["xmlns:wp=", "xmlns:a=", "xmlns:pic=", "xmlns:r="] {
        assert!(root.contains(ns), "{ns}");
    }
    assert_eq!(drawing_sizes(&read(&saved)), [(10_000, 10_000)]);
}

#[test]
fn a_new_drawing_id_does_not_repeat_one_in_the_file() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let at = after(&doc, "Introduction");
    insert_picture(&mut doc, &at, png_bytes(3, 3), (10_000, 20_000), None).unwrap();
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    let xml = part(&saved, "word/document.xml");
    let mut ids: Vec<String> = xml
        .match_indices("<wp:docPr id=\"")
        .map(|(i, _)| xml[i + 14..].split('"').next().unwrap().to_owned())
        .collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "{ids:?}");
    // The file's own picture part is not renamed.
    assert!(parts(&saved).contains_key("word/media/image1.png"));
    assert!(parts(&saved).contains_key("word/media/image2.png"));
    let rels = part(&saved, "word/_rels/document.xml.rels");
    assert!(rels.contains("media/image1.png") && rels.contains("media/image2.png"));
    // The content type for png was there already: not repeated.
    let types = part(&saved, "[Content_Types].xml");
    assert_eq!(types.matches("Extension=\"png\"").count(), 1);
}

#[test]
fn a_picture_in_a_table_cell_and_a_cropped_one() {
    let bytes = grid_bytes();
    let mut doc = read(&bytes);
    let at = cell_pos(&doc, 0, 1);
    let end = insert_picture(&mut doc, &at, png_bytes(5, 5), (50_000, 50_000), None).unwrap();
    // A crop is set on the model by hand: the operation makes uncropped ones.
    {
        let p = doc.paragraph_mut(&end.path).unwrap();
        if let Inline::Drawing(d) = &mut p.runs[0].kind
            && let aster_writer::DrawingContent::Picture(pic) = &mut Arc::make_mut(d).content
        {
            pic.crop = [0.1, 0.0, 0.25, 0.5];
        }
    }
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    let xml = part(&saved, "word/document.xml");
    assert!(xml.contains("<a:srcRect l=\"10000\" r=\"25000\" b=\"50000\"/>"));
    let back = read(&saved);
    let t = back.body.blocks[table_index(&back)].as_table().unwrap();
    let p = t.rows[0].cells[1].blocks[0].as_paragraph().unwrap();
    let Inline::Drawing(d) = &p.runs[0].kind else {
        panic!("a drawing");
    };
    let aster_writer::DrawingContent::Picture(pic) = &d.content else {
        panic!("a picture");
    };
    assert_eq!(pic.crop, [0.1, 0.0, 0.25, 0.5]);
}

#[test]
fn a_picture_copied_inside_the_document_is_written_again() {
    let bytes = grid_bytes();
    let mut doc = read(&bytes);
    let at = after(&doc, "before");
    let end = insert_picture(&mut doc, &at, png_bytes(4, 4), (30_000, 30_000), None).unwrap();
    let blocks = fragment(&doc, &at, &end);
    let target = after(&doc, "after the");
    insert_fragment(&mut doc, &target, &blocks);
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    assert_eq!(drawing_sizes(&read(&saved)).len(), 2);
    assert_eq!(
        parts(&saved)
            .keys()
            .filter(|n| n.starts_with("word/media/"))
            .count(),
        1
    );
}

#[test]
fn a_picture_read_from_the_file_that_is_resized_keeps_its_bytes() {
    let bytes = sample();
    let mut doc = read(&bytes);
    let (path, offset) = doc
        .paragraphs()
        .iter()
        .find_map(|(path, p)| {
            let mut at = 0;
            for run in &p.runs {
                if let Inline::Drawing(d) = &run.kind
                    && d.float.is_none()
                    && matches!(d.content, aster_writer::DrawingContent::Picture(_))
                {
                    return Some((path.clone(), at));
                }
                at += run.text.len();
            }
            None
        })
        .expect("an inline picture");
    resize_drawing(&mut doc, &Pos::new(path, offset), (2_286_000, 1_524_000));
    let saved = check_except(&bytes, &doc, &[], &["Inline picture"]);
    let (before_xml, after_xml) = (
        part(&bytes, "word/document.xml"),
        part(&saved, "word/document.xml"),
    );
    assert_eq!(
        before_xml.matches("cx=\"1143000\" cy=\"762000\"").count(),
        2
    );
    assert_eq!(
        after_xml.matches("cx=\"2286000\" cy=\"1524000\"").count(),
        2
    );
    // Nothing else changed: with the sizes put back it is the original.
    assert_eq!(
        after_xml.replace(
            "cx=\"2286000\" cy=\"1524000\"",
            "cx=\"1143000\" cy=\"762000\""
        ),
        before_xml
    );
    assert_eq!(
        parts(&saved)["word/media/image1.png"],
        parts(&bytes)["word/media/image1.png"]
    );
}

#[test]
fn a_picture_where_the_package_has_other_media_names() {
    let bytes = sample();
    let mut all = parts(&bytes);
    let image = all["word/media/image1.png"].clone();
    all.insert("word/media/image2.jpg".to_owned(), image);
    let list: Vec<(String, Vec<u8>)> = all.into_iter().collect();
    let bytes = docx_sample::zip(&list);
    let mut doc = read(&bytes);
    let at = after(&doc, "Introduction");
    insert_picture(&mut doc, &at, png_bytes(3, 3), (10_000, 20_000), None).unwrap();
    let saved = check(&bytes, &doc, MEDIA_CHANGES);
    // image2 is taken (by .jpg): the new one is image3.
    assert!(parts(&saved).contains_key("word/media/image3.png"));
}

#[test]
fn random_table_link_and_picture_edits_save_and_read_back() {
    let bytes = grid_bytes();
    for seed in 0..120u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut doc = read(&bytes);
        let mut log: Vec<String> = Vec::new();
        for _ in 0..(2 + rng.below(7)) {
            let a = random_pos(&doc, &mut rng);
            let b = if rng.below(2) == 0 {
                a.clone()
            } else {
                random_pos(&doc, &mut rng)
            };
            match rng.below(12) {
                0 | 1 => {
                    let above = rng.below(2) == 0;
                    log.push(format!("insert_rows {a:?} {b:?} {above}"));
                    insert_rows(&mut doc, &a, &b, above);
                }
                2 | 3 => {
                    let left = rng.below(2) == 0;
                    log.push(format!("insert_columns {a:?} {b:?} {left}"));
                    insert_columns(&mut doc, &a, &b, left);
                }
                4 => {
                    log.push(format!("delete_rows {a:?} {b:?}"));
                    delete_rows(&mut doc, &a, &b);
                }
                5 => {
                    log.push(format!("delete_columns {a:?} {b:?}"));
                    delete_columns(&mut doc, &a, &b);
                }
                6 => {
                    log.push(format!("insert_table {a:?}"));
                    insert_table(&mut doc, &a, 1 + rng.below(3), 1 + rng.below(3), 3_000);
                }
                7 => {
                    if rng.below(3) == 0 {
                        log.push(format!("delete_table {a:?}"));
                        delete_table(&mut doc, &a);
                    }
                }
                8 | 9 => {
                    let link = (rng.below(3) > 0).then(|| Link {
                        target: ["https://a.example/", "#mark", "https://b.example/?x=1&y=2"]
                            [rng.below(3)]
                        .to_owned(),
                        tooltip: None,
                    });
                    log.push(format!("set_link {a:?} {b:?} {link:?}"));
                    set_link(&mut doc, &a, &b, link);
                }
                10 => {
                    log.push(format!("insert_picture {a:?}"));
                    let image = if rng.below(2) == 0 {
                        png_bytes(2, 2)
                    } else {
                        jpeg_like()
                    };
                    let _ = insert_picture(&mut doc, &a, image, (50_000, 40_000), None);
                }
                _ => {
                    log.push(format!("insert_text {a:?}"));
                    insert_text(&mut doc, &a, "x y", None);
                }
            }
            assert_tables_valid_deep(&doc);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check(&bytes, &doc, MEDIA_AND_RELS)
        }));
        if let Err(err) = result {
            eprintln!("seed {seed} failed: {log:#?}");
            std::panic::resume_unwind(err);
        }
    }
}

const MEDIA_AND_RELS: &[&str] = &[
    "word/_rels/document.xml.rels",
    "[Content_Types].xml",
    "word/media/image1.png",
    "word/media/image2.png",
    "word/media/image1.jpeg",
    "word/media/image2.jpeg",
];

fn assert_tables_valid_deep(doc: &Document) {
    fn walk(blocks: &[Block]) {
        for block in blocks {
            if let Block::Table(t) = block {
                check_table(t).unwrap();
                for row in &t.rows {
                    for cell in &row.cells {
                        walk(&cell.blocks);
                    }
                }
            }
        }
    }
    walk(&doc.body.blocks);
}

// ---------------------------------------------------------------------------
// Styles in table cells
// ---------------------------------------------------------------------------

#[test]
fn the_style_a_paragraph_in_a_cell_has_gives_it_back_what_the_reader_made() {
    // The reader builds a cell paragraph's base from the document defaults,
    // the table style (with its conditional formats) and the paragraph style;
    // applying the paragraph's own style again must find the same.
    let mut checked = 0;
    for bytes in [sample(), grid_bytes()] {
        let doc = read(&bytes);
        let default_style = doc.styles.default_style.clone();
        for (path, p) in doc.paragraphs() {
            if path.in_cell().is_none() {
                continue;
            }
            let Some(style) = p.props.style.clone().or_else(|| default_style.clone()) else {
                continue;
            };
            let mut again = doc.clone();
            let at = Pos::new(path.clone(), 0);
            set_style(&mut again, &at, &at, &style);
            let q = again.paragraph(&path).unwrap();
            let mut expected = p.props.clone();
            expected.style = Some(style);
            // A list a paragraph is on comes from its own numbering, not
            // from its style.
            let mut got = q.props.clone();
            got.list = expected.list;
            got.indent_left = expected.indent_left;
            got.first_line = expected.first_line;
            assert_eq!(got, expected, "{:?}", p.text());
            assert_eq!(q.base, p.base, "{:?}", p.text());
            checked += 1;
        }
    }
    assert!(checked > 10, "{checked}");
}
