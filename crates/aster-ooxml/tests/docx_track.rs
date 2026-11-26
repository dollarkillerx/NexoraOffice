//! Tracked changes and comments: read from a file, kept through editing and
//! saving, accepted and rejected.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;

use std::io::{Cursor, Read};

use aster_ooxml::docx_save::save;
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{Annotation, Document, Inline, Path, Pos};

const BODY: &str = concat!(
    "<w:p><w:r><w:t xml:space=\"preserve\">one </w:t></w:r>",
    "<w:ins w:id=\"1\" w:author=\"Ann\" w:date=\"2024-05-01T09:30:00Z\"><w:r><w:t xml:space=\"preserve\">two </w:t></w:r></w:ins>",
    "<w:del w:id=\"2\" w:author=\"Bob\" w:date=\"2024-05-02T10:00:00Z\"><w:r><w:rPr><w:b/></w:rPr><w:delText xml:space=\"preserve\">gone </w:delText></w:r></w:del>",
    "<w:r><w:t>three</w:t></w:r></w:p>",
    "<w:p><w:r><w:t xml:space=\"preserve\">ab </w:t></w:r><w:commentRangeStart w:id=\"7\"/>",
    "<w:r><w:t>cd</w:t></w:r><w:commentRangeEnd w:id=\"7\"/>",
    "<w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/></w:rPr><w:commentReference w:id=\"7\"/></w:r>",
    "<w:r><w:t xml:space=\"preserve\"> ef</w:t></w:r></w:p>",
    "<w:p><w:pPr><w:rPr><w:ins w:id=\"3\" w:author=\"Cy\" w:date=\"2024-05-03T00:00:00Z\"/></w:rPr></w:pPr>",
    "<w:r><w:t>marked</w:t></w:r></w:p>",
    "<w:p><w:r><w:t>last</w:t></w:r></w:p>",
);

const COMMENTS: &str = concat!(
    "<w:comment w:id=\"7\" w:author=\"Dee\" w:date=\"2024-06-01T08:00:00Z\" w:initials=\"D\">",
    "<w:p><w:r><w:t>first</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:comment>"
);

fn file() -> Vec<u8> {
    docx_doc::doc(BODY).comments(COMMENTS).bytes()
}

fn read(bytes: &[u8]) -> Document {
    Docx.read(bytes.to_vec()).unwrap()
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn main_xml(bytes: &[u8]) -> String {
    part(bytes, "word/document.xml")
}

fn at(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

/// Save the edited document over the file, read it back, and check that
/// what comes back is what was saved.
fn round_trip(bytes: &[u8], doc: &Document) -> (Vec<u8>, Document) {
    let saved = save(bytes.to_vec(), doc).unwrap();
    let back = read(&saved);
    assert!(
        same_content(doc, &back),
        "the saved file reads back differently:\n{}",
        main_xml(&saved)
    );
    (saved, back)
}

#[test]
fn the_file_is_read_with_its_changes_and_comments() {
    let doc = read(&file());
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "one two three");
    let ins = p
        .runs
        .iter()
        .find(|r| matches!(r.annotation, Some(Annotation::Inserted(_))))
        .unwrap();
    assert_eq!(ins.text, "two ");
    let Some(Annotation::Inserted(change)) = &ins.annotation else {
        unreachable!()
    };
    assert_eq!(
        (change.id, &*change.author, change.date.as_deref()),
        (1, "Ann", Some("2024-05-01T09:30:00Z"))
    );
    let del = p
        .runs
        .iter()
        .find_map(|r| match &r.annotation {
            Some(Annotation::Deleted(d)) => Some(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(&*del.change.author, "Bob");
    assert_eq!(del.runs.len(), 1);
    assert_eq!(del.runs[0].text, "gone ");
    assert!(del.runs[0].look.bold);
    // The deleted run is a marker: no text.
    assert!(
        p.runs
            .iter()
            .any(|r| r.kind == Inline::Marker && r.text.is_empty())
    );

    // Comments and where they are.
    assert_eq!(doc.comments.len(), 1);
    let comment = doc.comment(7).unwrap();
    assert_eq!(
        (comment.author.as_str(), comment.initials.as_str()),
        ("Dee", "D")
    );
    assert_eq!(comment.date.as_deref(), Some("2024-06-01T08:00:00Z"));
    assert_eq!(comment.paragraphs, ["first", "second"]);
    let anchors = doc.comment_anchors();
    assert_eq!(anchors.len(), 1);
    assert_eq!(
        (anchors[0].start.clone(), anchors[0].end.clone()),
        (at(1, 3), at(1, 5))
    );
    assert_eq!(doc.plain_text(), "one two three\nab cd ef\nmarked\nlast");

    // A paragraph whose mark was inserted.
    let marked = doc.paragraph(&Path::body(2)).unwrap();
    assert_eq!(marked.changes.inserted_mark.as_ref().unwrap().id, 3);
    assert!(doc.has_changes());
}

#[test]
fn an_unedited_document_is_saved_as_it_was() {
    let bytes = file();
    let doc = read(&bytes);
    assert_eq!(save(bytes.clone(), &doc).unwrap(), bytes);
}

#[test]
fn an_edit_next_to_a_change_leaves_it_in_the_file() {
    let bytes = file();
    let mut doc = read(&bytes);
    insert_text(&mut doc, &at(0, 2), "X", None);
    insert_text(&mut doc, &at(1, 1), "Y", None);
    insert_text(&mut doc, &at(2, 6), "Z", None);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(xml.contains("<w:ins w:id=\"1\" w:author=\"Ann\" w:date=\"2024-05-01T09:30:00Z\"><w:r><w:t xml:space=\"preserve\">two </w:t></w:r></w:ins>"), "{xml}");
    assert!(xml.contains("<w:del w:id=\"2\" w:author=\"Bob\" w:date=\"2024-05-02T10:00:00Z\"><w:r><w:rPr><w:b/></w:rPr><w:delText xml:space=\"preserve\">gone </w:delText></w:r></w:del>"));
    assert!(xml.contains("<w:commentRangeStart w:id=\"7\"/>"));
    assert!(xml.contains("<w:commentRangeEnd w:id=\"7\"/>"));
    assert!(xml.contains("<w:commentReference w:id=\"7\"/>"));
    assert!(xml.contains("<w:ins w:id=\"3\" w:author=\"Cy\" w:date=\"2024-05-03T00:00:00Z\"/>"));
    assert_eq!(
        back.plain_text(),
        "onXe two three\naYb cd ef\nmarkedZ\nlast"
    );
    // The comments part is the one the file had.
    assert_eq!(
        part(&saved, "word/comments.xml"),
        part(&bytes, "word/comments.xml")
    );
    assert_eq!(back.changes().len(), 3);
    assert_eq!(back.comment_anchors().len(), 1);
}

#[test]
fn text_typed_into_an_insertion_is_written_inside_it() {
    let bytes = file();
    let mut doc = read(&bytes);
    // "one tw|o three": the run grows.
    insert_text(&mut doc, &at(0, 6), "XX", None);
    // A look of its own: a new run, inside the insertion too.
    let bold = aster_writer::RunLook {
        bold: true,
        ..doc.paragraph(&Path::body(0)).unwrap().runs[0].look.clone()
    };
    insert_text(&mut doc, &at(0, 5), "B", Some(&bold));
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    let start = xml.find("<w:ins w:id=\"1\"").unwrap();
    let end = xml[start..].find("</w:ins>").unwrap() + start;
    let inside = &xml[start..end];
    assert!(inside.contains(">B<"), "{inside}");
    assert!(inside.contains("XX"), "{inside}");
    // And not elsewhere.
    assert!(!xml[..start].contains('B'));
    let inserted: String = back
        .paragraph(&Path::body(0))
        .unwrap()
        .runs
        .iter()
        .filter(|r| matches!(r.annotation, Some(Annotation::Inserted(_))))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(inserted, "tBwoXX ".replace("woXX", "wXXo"));
}

#[test]
fn a_split_inserted_paragraph_is_inserted_in_both_halves() {
    let bytes = docx_doc::doc(
        "<w:p><w:ins w:id=\"1\" w:author=\"Ann\"><w:r><w:t>abcdef</w:t></w:r></w:ins></w:p>",
    )
    .bytes();
    let mut doc = read(&bytes);
    split_paragraph(&mut doc, &at(0, 3));
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert_eq!(xml.matches("<w:ins ").count(), 2, "{xml}");
    assert_eq!(back.changes().len(), 2);
}

#[test]
fn accepting_and_rejecting_are_written() {
    let bytes = file();
    // Accept everything.
    let mut doc = read(&bytes);
    assert!(accept_all(&mut doc) >= 3);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(!xml.contains("<w:ins"), "{xml}");
    assert!(!xml.contains("<w:del"), "{xml}");
    assert!(!xml.contains("gone"), "{xml}");
    assert!(xml.contains("two "));
    assert!(!back.has_changes());
    assert_eq!(back.plain_text(), "one two three\nab cd ef\nmarked\nlast");
    // The comment is not a change: it stays.
    assert!(xml.contains("<w:commentRangeStart w:id=\"7\"/>"));

    // Reject everything.
    let mut doc = read(&bytes);
    reject_all(&mut doc);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(!xml.contains("<w:ins"), "{xml}");
    assert!(!xml.contains("<w:del"), "{xml}");
    assert!(!xml.contains("two "), "{xml}");
    assert!(xml.contains("gone "));
    assert!(!back.has_changes());
    // The inserted paragraph mark went: "marked" joined "last".
    assert_eq!(back.plain_text(), "one gone three\nab cd ef\nmarkedlast");
}

#[test]
fn a_selection_accepts_only_what_it_touches() {
    let bytes = file();
    let mut doc = read(&bytes);
    assert_eq!(accept_in(&mut doc, &at(0, 4), &at(0, 7)), 1);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(!xml.contains("<w:ins w:id=\"1\""), "{xml}");
    assert!(xml.contains("<w:del w:id=\"2\""));
    assert!(xml.contains("<w:ins w:id=\"3\""));
    assert_eq!(back.changes().len(), 2);
}

#[test]
fn accepting_part_of_a_wrapper_keeps_the_rest_inside_it() {
    let bytes = docx_doc::doc(concat!(
        "<w:p><w:ins w:id=\"1\" w:author=\"Ann\"><w:r><w:t xml:space=\"preserve\">aa </w:t></w:r>",
        "<w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">bb </w:t></w:r>",
        "<w:r><w:t>cc</w:t></w:r></w:ins></w:p>"
    ))
    .bytes();
    let mut doc = read(&bytes);
    // Only the bold middle run.
    assert_eq!(accept_in(&mut doc, &at(0, 3), &at(0, 4)), 1);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert_eq!(xml.matches("<w:ins ").count(), 2, "{xml}");
    assert_eq!(back.plain_text(), "aa bb cc");
    let inserted: Vec<_> = back
        .paragraph(&Path::body(0))
        .unwrap()
        .runs
        .iter()
        .filter(|r| matches!(r.annotation, Some(Annotation::Inserted(_))))
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(inserted, ["aa ", "cc"]);
}

#[test]
fn deleting_text_never_deletes_a_change() {
    let bytes = file();
    let mut doc = read(&bytes);
    // From inside "one" to inside "three": covers the insertion and the
    // deletion.
    delete(&mut doc, &at(0, 2), &at(0, 10));
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    // The inserted text is ordinary text and goes; the deletion stays.
    assert!(xml.contains("<w:del w:id=\"2\""), "{xml}");
    assert!(xml.contains("gone"));
    assert!(!xml.contains("two"));
    assert_eq!(back.paragraph(&Path::body(0)).unwrap().text(), "onree");
}

#[test]
fn comments_follow_their_text_and_stay_when_it_is_deleted() {
    let bytes = file();
    let mut doc = read(&bytes);
    insert_text(&mut doc, &at(1, 0), "ZZ", None);
    delete(&mut doc, &at(1, 5), &at(1, 7));
    let (saved, back) = round_trip(&bytes, &doc);
    let anchors = back.comment_anchors();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].start, anchors[0].end);
    assert_eq!(anchors[0].start, at(1, 5));
    let xml = main_xml(&saved);
    assert!(xml.contains("<w:commentRangeStart w:id=\"7\"/>"));
    assert!(xml.contains("<w:commentReference w:id=\"7\"/>"));
}

#[test]
fn an_inserted_paragraph_mark_follows_the_end_of_a_split_paragraph() {
    let bytes = file();
    let mut doc = read(&bytes);
    split_paragraph(&mut doc, &at(2, 3));
    let (saved, back) = round_trip(&bytes, &doc);
    // The mark at the end of the text is the second half's.
    assert!(back.paragraph(&Path::body(2)).unwrap().changes.is_none());
    assert!(
        back.paragraph(&Path::body(3))
            .unwrap()
            .changes
            .inserted_mark
            .is_some()
    );
    assert_eq!(main_xml(&saved).matches("<w:ins w:id=\"3\"").count(), 1);
}

#[test]
fn formatting_records_are_accepted_away() {
    let bytes = docx_doc::doc(concat!(
        "<w:p><w:pPr><w:pPrChange w:id=\"1\" w:author=\"Ann\"><w:pPr/></w:pPrChange></w:pPr>",
        "<w:r><w:rPr><w:b/><w:rPrChange w:id=\"2\" w:author=\"Ann\"><w:rPr/></w:rPrChange></w:rPr><w:t>x</w:t></w:r></w:p>"
    ))
    .bytes();
    let mut doc = read(&bytes);
    assert!(doc.paragraph(&Path::body(0)).unwrap().changes.formatting);
    // Editing the paragraph keeps the records.
    insert_text(&mut doc, &at(0, 1), "y", None);
    let (saved, _) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(xml.contains("<w:pPrChange"), "{xml}");
    assert!(xml.contains("<w:rPrChange"), "{xml}");
    // Accepting takes them out.
    assert_eq!(accept_all(&mut doc), 1);
    let (saved, back) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(!xml.contains("Change"), "{xml}");
    assert!(!back.has_changes());
    assert!(back.paragraph(&Path::body(0)).unwrap().runs[0].look.bold);
}

#[test]
fn rejecting_a_formatting_change_is_left_undone() {
    let bytes = docx_doc::doc(
        "<w:p><w:pPr><w:pPrChange w:id=\"1\" w:author=\"Ann\"><w:pPr/></w:pPrChange></w:pPr><w:r><w:t>x</w:t></w:r></w:p>",
    )
    .bytes();
    let mut doc = read(&bytes);
    assert_eq!(reject_all(&mut doc), 0);
    assert_eq!(save(bytes.clone(), &doc).unwrap(), bytes);
}

#[test]
fn a_moved_text_is_a_deletion_and_an_insertion() {
    let bytes = docx_doc::doc(concat!(
        "<w:p><w:moveFrom w:id=\"1\" w:author=\"Ann\"><w:r><w:t>moved</w:t></w:r></w:moveFrom><w:r><w:t>a</w:t></w:r></w:p>",
        "<w:p><w:moveTo w:id=\"2\" w:author=\"Ann\"><w:r><w:t>moved</w:t></w:r></w:moveTo></w:p>"
    ))
    .bytes();
    let mut doc = read(&bytes);
    let kinds: Vec<_> = doc
        .changes()
        .iter()
        .map(|c| (c.kind, c.start.path.top()))
        .collect();
    use aster_writer::track::ChangeKind::*;
    assert_eq!(kinds, [(Deleted, 0), (Inserted, 1)]);
    insert_text(&mut doc, &at(1, 2), "!", None);
    let (saved, _) = round_trip(&bytes, &doc);
    let xml = main_xml(&saved);
    assert!(xml.contains("<w:moveFrom"), "{xml}");
    assert!(xml.contains("<w:moveTo"), "{xml}");
}

#[test]
fn a_comment_across_paragraphs_is_one_range() {
    let bytes = docx_doc::doc(concat!(
        "<w:p><w:r><w:t xml:space=\"preserve\">a </w:t></w:r><w:commentRangeStart w:id=\"1\"/><w:r><w:t>b</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>c</w:t></w:r><w:commentRangeEnd w:id=\"1\"/>",
        "<w:r><w:commentReference w:id=\"1\"/></w:r></w:p>"
    ))
    .bytes();
    let doc = read(&bytes);
    let a = &doc.comment_anchors()[0];
    assert_eq!((a.start.clone(), a.end.clone()), (at(0, 2), at(1, 1)));
    // No `comments.xml`: the anchor is there, the comment is not.
    assert!(doc.comment(1).is_none());
}
