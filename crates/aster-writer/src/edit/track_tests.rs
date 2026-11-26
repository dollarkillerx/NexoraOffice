//! Tracked changes and comments: reading them off the model, editing around
//! them, accepting and rejecting.

use std::sync::Arc;

use super::*;
use crate::track::{ChangeKind, CommentAnchor};
use crate::{Annotation, Change, Comment, Deleted, ParaChanges, Story};

fn look() -> RunLook {
    RunLook::default()
}

fn change(id: i64, author: &str) -> Arc<Change> {
    Arc::new(Change {
        id,
        author: Arc::from(author),
        date: Some(Arc::from("2024-05-01T09:30:00Z")),
        moved: false,
    })
}

fn plain(text: &str) -> Run {
    Run::text(text, look())
}

fn inserted(text: &str, id: i64, author: &str) -> Run {
    let mut run = plain(text);
    run.annotation = Some(Annotation::Inserted(change(id, author)));
    run
}

fn deleted(text: &str, id: i64, author: &str) -> Run {
    let mut run = Run::object(Inline::Marker, look());
    run.annotation = Some(Annotation::Deleted(Arc::new(Deleted {
        change: (*change(id, author)).clone(),
        runs: vec![plain(text)],
        base: look(),
    })));
    run
}

fn comment_mark(annotation: Annotation) -> Run {
    let mut run = Run::object(Inline::Marker, look());
    run.annotation = Some(annotation);
    run
}

fn para(runs: Vec<Run>) -> Paragraph {
    Paragraph {
        runs,
        ..Paragraph::default()
    }
}

fn doc(paragraphs: Vec<Paragraph>) -> Document {
    Document {
        body: Story {
            blocks: paragraphs.into_iter().map(Block::paragraph).collect(),
            part: None,
        },
        ..Document::default()
    }
}

fn at(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|(_, p)| p.text()).collect()
}

/// "one [ins: two] [del: gone]three".
fn sample() -> Document {
    doc(vec![para(vec![
        plain("one "),
        inserted("two ", 1, "Ann"),
        deleted("gone ", 2, "Bob"),
        plain("three"),
    ])])
}

#[test]
fn deleted_text_is_not_in_the_text() {
    let d = sample();
    assert_eq!(d.plain_text(), "one two three");
    assert_eq!(d.paragraph(&Path::body(0)).unwrap().len(), 13);
    assert!(d.has_changes());
    assert!(!doc(vec![para(vec![plain("x")])]).has_changes());
}

#[test]
fn changes_are_listed_in_reading_order() {
    let mut d = sample();
    d.body.blocks.push(Block::paragraph(Paragraph {
        runs: vec![plain("tail")],
        changes: ParaChanges {
            inserted_mark: Some(change(3, "Cy")),
            formatting: true,
            ..ParaChanges::default()
        },
        ..Paragraph::default()
    }));
    let all = d.changes();
    let kinds: Vec<_> = all.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            ChangeKind::Inserted,
            ChangeKind::Deleted,
            ChangeKind::Formatting,
            ChangeKind::MarkInserted
        ]
    );
    assert_eq!((all[0].start.offset, all[0].end.offset), (4, 8));
    assert_eq!((all[1].start.offset, all[1].end.offset), (8, 8));
    assert_eq!(all[1].author.as_deref(), Some("Bob"));
    let authors: Vec<String> = d.authors().iter().map(|a| a.to_string()).collect();
    assert_eq!(authors, ["Ann", "Bob", "Cy"]);
}

#[test]
fn typing_inside_an_insertion_stays_part_of_it() {
    let mut d = sample();
    // Same look: the run grows.
    insert_text(&mut d, &at(0, 6), "XX", None);
    let p = d.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "one twXXo three");
    let ins: Vec<_> = p
        .runs
        .iter()
        .filter(|r| matches!(r.annotation, Some(Annotation::Inserted(_))))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(ins, ["twXXo "]);
    // A look of its own: a new run, still inserted.
    let bold = RunLook {
        bold: true,
        ..look()
    };
    insert_text(&mut d, &at(0, 5), "B", Some(&bold));
    let p = d.paragraph(&Path::body(0)).unwrap();
    let ins: Vec<_> = p
        .runs
        .iter()
        .filter(|r| matches!(r.annotation, Some(Annotation::Inserted(_))))
        .map(|r| r.text.clone())
        .collect();
    assert_eq!(ins.concat(), "tBwXXo ");
    assert!(ins.len() >= 2);
}

#[test]
fn typing_elsewhere_is_not_tracked() {
    let mut d = sample();
    insert_text(&mut d, &at(0, 2), "Z", None);
    let p = d.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "onZe two three");
    assert!(
        p.runs
            .iter()
            .filter(|r| r.text.contains('Z'))
            .all(|r| r.annotation.is_none())
    );
}

#[test]
fn deleting_over_a_deletion_keeps_it() {
    let mut d = sample();
    let n = d.paragraph(&Path::body(0)).unwrap().runs.len();
    delete(&mut d, &at(0, 4), &at(0, 13));
    let p = d.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "one ");
    // The deletion marker is still there, and its text.
    assert!(p.runs.iter().any(crate::track::is_deletion));
    assert!(p.runs.len() <= n);
}

#[test]
fn accept_all_and_reject_all() {
    let mut accepted = sample();
    assert_eq!(accept_all(&mut accepted), 2);
    let p = accepted.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "one two three");
    assert!(p.runs.iter().all(|r| r.annotation.is_none()));
    assert!(!accepted.has_changes());
    // The deleted text is gone for good.
    assert!(!accepted.plain_text().contains("gone"));

    let mut rejected = sample();
    assert_eq!(reject_all(&mut rejected), 2);
    assert_eq!(rejected.plain_text(), "one gone three");
    assert!(!rejected.has_changes());
    assert_eq!(rejected.paragraph(&Path::body(0)).unwrap().runs.len(), 1);
}

#[test]
fn a_selection_resolves_only_what_it_touches() {
    // Selecting inside the insertion accepts it, not the deletion.
    let mut d = sample();
    assert_eq!(accept_in(&mut d, &at(0, 5), &at(0, 6)), 1);
    assert_eq!(d.changes().len(), 1);
    assert_eq!(d.changes()[0].kind, ChangeKind::Deleted);

    // A caret in the insertion rejects it.
    let mut d = sample();
    assert_eq!(reject_in(&mut d, &at(0, 5), &at(0, 5)), 1);
    assert_eq!(d.plain_text(), "one three");
    assert_eq!(d.changes().len(), 1);

    // A range that covers the deletion's place resolves it; one that only
    // ends or starts there does not.
    let mut d = sample();
    assert_eq!(reject_in(&mut d, &at(0, 7), &at(0, 9)), 2);
    assert_eq!(d.plain_text(), "one gone three");
    let mut d = sample();
    assert_eq!(accept_in(&mut d, &at(0, 4), &at(0, 8)), 1);
    assert_eq!(d.changes()[0].kind, ChangeKind::Deleted);
    // At the end of the paragraph a range that goes up to it has it.
    let mut d = doc(vec![para(vec![plain("ab"), deleted("x", 5, "Bob")])]);
    assert_eq!(accept_in(&mut d, &at(0, 0), &at(0, 2)), 1);
    // Nothing touched, nothing done.
    let mut d = sample();
    assert_eq!(accept_in(&mut d, &at(0, 0), &at(0, 2)), 0);
    assert_eq!(d, sample());
}

#[test]
fn untouched_paragraphs_stay_shared() {
    let mut d = doc(vec![
        para(vec![plain("a")]),
        para(vec![inserted("b", 1, "Ann")]),
        para(vec![plain("c")]),
    ]);
    let before = d.clone();
    accept_all(&mut d);
    assert!(d.body.blocks[0].is(&before.body.blocks[0]));
    assert!(!d.body.blocks[1].is(&before.body.blocks[1]));
    assert!(d.body.blocks[2].is(&before.body.blocks[2]));
}

#[test]
fn paragraph_marks_join_or_clear() {
    let marked = |text: &str, inserted: bool| Paragraph {
        runs: vec![plain(text)],
        changes: if inserted {
            ParaChanges {
                inserted_mark: Some(change(1, "Ann")),
                ..ParaChanges::default()
            }
        } else {
            ParaChanges {
                deleted_mark: Some(change(2, "Ann")),
                ..ParaChanges::default()
            }
        },
        ..Paragraph::default()
    };
    // An inserted mark: accepting keeps both paragraphs, rejecting joins.
    let make = || doc(vec![marked("one", true), para(vec![plain("two")])]);
    let mut d = make();
    assert_eq!(accept_all(&mut d), 1);
    assert_eq!(texts(&d), ["one", "two"]);
    assert!(!d.has_changes());
    let mut d = make();
    assert_eq!(reject_all(&mut d), 1);
    assert_eq!(texts(&d), ["onetwo"]);
    assert!(!d.has_changes());
    // A deleted mark: the other way round.
    let make = || doc(vec![marked("one", false), para(vec![plain("two")])]);
    let mut d = make();
    accept_all(&mut d);
    assert_eq!(texts(&d), ["onetwo"]);
    let mut d = make();
    reject_all(&mut d);
    assert_eq!(texts(&d), ["one", "two"]);
}

#[test]
fn a_joined_paragraph_keeps_the_second_marks_changes() {
    let mut d = doc(vec![
        Paragraph {
            runs: vec![plain("one")],
            changes: ParaChanges {
                inserted_mark: Some(change(1, "Ann")),
                ..ParaChanges::default()
            },
            ..Paragraph::default()
        },
        Paragraph {
            runs: vec![plain("two")],
            changes: ParaChanges {
                deleted_mark: Some(change(2, "Bob")),
                ..ParaChanges::default()
            },
            ..Paragraph::default()
        },
    ]);
    delete(&mut d, &at(0, 3), &at(1, 0));
    assert_eq!(texts(&d), ["onetwo"]);
    let p = d.paragraph(&Path::body(0)).unwrap();
    assert!(p.changes.inserted_mark.is_none());
    assert!(p.changes.deleted_mark.is_some());
}

#[test]
fn formatting_changes_are_accepted_but_not_rejected() {
    let mut d = doc(vec![Paragraph {
        runs: vec![plain("x")],
        changes: ParaChanges {
            formatting: true,
            ..ParaChanges::default()
        },
        ..Paragraph::default()
    }]);
    assert_eq!(reject_all(&mut d), 0);
    assert!(d.has_changes());
    assert_eq!(accept_all(&mut d), 1);
    assert!(!d.has_changes());
}

fn commented() -> Document {
    // "ab [start]cd[end][ref] ef"
    let mut d = doc(vec![para(vec![
        plain("ab "),
        comment_mark(Annotation::CommentStart(7)),
        plain("cd"),
        comment_mark(Annotation::CommentEnd(7)),
        comment_mark(Annotation::CommentReference(7)),
        plain(" ef"),
    ])]);
    d.comments = vec![Arc::new(Comment {
        id: 7,
        author: "Dee".into(),
        initials: "D".into(),
        date: None,
        paragraphs: vec!["first".into(), "second".into()],
    })];
    d
}

fn anchor(d: &Document) -> CommentAnchor {
    d.comment_anchors().into_iter().next().unwrap()
}

#[test]
fn a_comment_is_anchored_to_its_range() {
    let d = commented();
    assert_eq!(
        anchor(&d),
        CommentAnchor {
            id: 7,
            start: at(0, 3),
            end: at(0, 5)
        }
    );
    assert_eq!(d.comment(7).unwrap().text(), "first\nsecond");
    assert_eq!(d.authors()[0].as_ref(), "Dee");
}

#[test]
fn a_comment_anchor_moves_with_its_text() {
    let mut d = commented();
    // Typing before the range moves it.
    insert_text(&mut d, &at(0, 0), "XY", None);
    let a = anchor(&d);
    assert_eq!((a.start.offset, a.end.offset), (5, 7));
    // Typing inside the range grows it.
    insert_text(&mut d, &at(0, 6), "!!!", None);
    let a = anchor(&d);
    assert_eq!((a.start.offset, a.end.offset), (5, 10));
    // Deleting a part of it shrinks it; deleting after it leaves it alone.
    delete(&mut d, &at(0, 5), &at(0, 7));
    let a = anchor(&d);
    assert_eq!((a.start.offset, a.end.offset), (5, 8));
    delete(&mut d, &at(0, 9), &at(0, 12));
    let a = anchor(&d);
    assert_eq!((a.start.offset, a.end.offset), (5, 8));
}

#[test]
fn deleting_all_the_text_of_a_comment_keeps_it_in_place() {
    let mut d = commented();
    delete(&mut d, &at(0, 2), &at(0, 6));
    assert_eq!(d.plain_text(), "abef");
    let a = anchor(&d);
    assert_eq!(a.start, a.end);
    assert_eq!(a.start.offset, 2);
    assert_eq!(d.comments.len(), 1);
}

#[test]
fn a_comment_across_paragraphs_survives_joining_them() {
    let mut d = doc(vec![
        para(vec![
            plain("one "),
            comment_mark(Annotation::CommentStart(1)),
            plain("two"),
        ]),
        para(vec![
            plain("three"),
            comment_mark(Annotation::CommentEnd(1)),
            plain(" four"),
        ]),
    ]);
    let a = anchor(&d);
    assert_eq!(
        (a.start.path.clone(), a.end.path.clone()),
        (Path::body(0), Path::body(1))
    );
    delete(&mut d, &at(0, 7), &at(1, 0));
    assert_eq!(texts(&d), ["one twothree four"]);
    let a = anchor(&d);
    assert_eq!((a.start.offset, a.end.offset), (4, 12));
    assert_eq!(a.start.path, a.end.path);
}

#[test]
fn pasting_does_not_bring_tracking_along() {
    let d = sample();
    let blocks = fragment(&d, &at(0, 0), &at(0, 13));
    let mut other = doc(vec![para(vec![plain("x")])]);
    insert_fragment(&mut other, &at(0, 1), &blocks);
    assert_eq!(other.plain_text(), "xone two three");
    assert!(!other.has_changes());
}

#[test]
fn splitting_an_inserted_paragraph_keeps_the_insertion_on_both_halves() {
    let mut d = doc(vec![para(vec![inserted("abcdef", 1, "Ann")])]);
    split_paragraph(&mut d, &at(0, 3));
    assert_eq!(texts(&d), ["abc", "def"]);
    assert_eq!(d.changes().len(), 2);
}

#[test]
fn undoing_an_accept_brings_the_changes_back() {
    let mut history = History::default();
    let before = sample();
    let mut after = before.clone();
    accept_all(&mut after);
    history.record(&before, &Selection::default(), &Selection::default(), None);
    let (restored, _) = history.undo(&after).unwrap();
    assert!(restored.has_changes());
}
