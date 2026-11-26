//! Tracked changes and comments through the view.

use std::sync::Arc;

use aster_writer::{
    Annotation, Block, Change, Comment, Deleted, Document, Margins, Paragraph, Path, Pos, Run,
    RunLook, Section, Story,
};

use super::*;

fn look() -> RunLook {
    RunLook {
        size: 10.0,
        ..RunLook::default()
    }
}

fn text(s: &str) -> Run {
    Run::text(s, look())
}

fn change(id: i64, author: &str) -> Change {
    Change {
        id,
        author: Arc::from(author),
        date: None,
        moved: false,
    }
}

fn inserted(s: &str, author: &str) -> Run {
    let mut run = text(s);
    run.annotation = Some(Annotation::Inserted(Arc::new(change(1, author))));
    run
}

fn deleted(s: &str, author: &str) -> Run {
    let mut run = Run::object(aster_writer::Inline::Marker, look());
    run.annotation = Some(Annotation::Deleted(Arc::new(Deleted {
        change: change(2, author),
        runs: vec![text(s)],
        base: look(),
    })));
    run
}

fn mark(annotation: Annotation) -> Run {
    let mut run = Run::object(aster_writer::Inline::Marker, look());
    run.annotation = Some(annotation);
    run
}

fn para(runs: Vec<Run>) -> Block {
    Block::paragraph(Paragraph {
        runs,
        base: look(),
        mark: look(),
        ..Paragraph::default()
    })
}

fn pos(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

/// "one [two]{gone}three", a paragraph of "ab cd ef" with a comment on "cd",
/// and plenty of plain paragraphs after them.
fn reviewed() -> Document {
    let mut blocks = vec![
        para(vec![
            text("one "),
            inserted("two ", "Ann"),
            deleted("gone ", "Bob"),
            text("three"),
        ]),
        para(vec![
            text("ab "),
            mark(Annotation::CommentStart(7)),
            text("cd"),
            mark(Annotation::CommentEnd(7)),
            mark(Annotation::CommentReference(7)),
            text(" ef"),
        ]),
    ];
    blocks.extend((0..60).map(|i| para(vec![text(&format!("filler line {i}"))])));
    blocks.push(para(vec![
        text("late "),
        inserted("change", "Cy"),
        text(" here"),
    ]));
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(Section {
            page: (6_000, 4_000),
            margins: Margins {
                top: 400,
                right: 400,
                bottom: 400,
                left: 400,
                header: 200,
                footer: 200,
                gutter: 0,
            },
            ..Section::default()
        }),
        comments: vec![Arc::new(Comment {
            id: 7,
            author: "Dee".into(),
            initials: "D".into(),
            date: Some("2024-06-01T08:00:00Z".into()),
            paragraphs: vec!["first".into(), "second".into()],
        })],
        ..Document::default()
    }
}

fn view() -> DocView {
    let mut view = DocView::new(reviewed());
    view.resize(600, 500, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    view
}

fn first_text(view: &DocView) -> String {
    view.layout().pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            aster_layout::Item::Text(t) => Some(t.paragraph.text().to_owned()),
            _ => None,
        })
        .unwrap_or_default()
}

fn texts(view: &DocView) -> Vec<String> {
    view.document()
        .paragraphs()
        .iter()
        .take(2)
        .map(|(_, p)| p.text())
        .collect()
}

#[test]
fn markup_is_shown_until_switched_off() {
    let mut view = view();
    assert!(view.show_markup());
    assert_eq!(first_text(&view), "one two gone three");
    assert!(view.set_show_markup(false));
    assert!(!view.show_markup());
    assert_eq!(first_text(&view), "one two three");
    assert!(!view.set_show_markup(false));
    assert!(view.set_show_markup(true));
    assert_eq!(first_text(&view), "one two gone three");
    // The setting is not an edit.
    assert!(!view.is_modified());
    assert!(!view.undo());
}

#[test]
fn the_view_knows_whether_there_are_changes() {
    let mut view = view();
    assert!(view.has_tracked_changes());
    assert!(view.accept_all_changes());
    assert!(!view.has_tracked_changes());
    assert!(view.undo());
    assert!(view.has_tracked_changes());
    assert!(!DocView::new(Document::default()).has_tracked_changes());
}

#[test]
fn comments_are_listed_with_their_place() {
    let view = view();
    let comments = view.comments();
    assert_eq!(comments.len(), 1);
    let c = &comments[0];
    assert_eq!(
        (c.id, c.author.as_str(), c.initials.as_str()),
        (7, "Dee", "D")
    );
    assert_eq!(c.date.as_deref(), Some("2024-06-01T08:00:00Z"));
    assert_eq!(c.text, "first\nsecond");
    assert_eq!(c.range, Some((pos(1, 3), pos(1, 5))));
    assert_eq!(c.page, Some(0));
    let rect = c.anchor.unwrap();
    assert!(rect.width() > 0.0 && rect.height() > 0.0);
    assert!(rect.x0 >= 20.0 && rect.x1 <= 280.0);
    assert_eq!(c.color, view.layout().author_color("Dee"));
}

#[test]
fn a_comment_is_found_under_its_text_and_its_mark() {
    let view = view();
    let anchor = view.comments()[0].anchor.unwrap();
    // Window pixels of the page's point.
    let at = |x: f32, y: f32| {
        let page = view.page_rect(0);
        let ppp = view.pixels_per_point();
        (page.x0 + f64::from(x) * ppp, page.y0 + f64::from(y) * ppp)
    };
    let (x, y) = at((anchor.x0 + anchor.x1) / 2.0, (anchor.y0 + anchor.y1) / 2.0);
    assert_eq!(view.comment_at(x, y), Some(7));
    // The mark beside the text, right of the margin.
    let (x, y) = at(290.0, (anchor.y0 + anchor.y1) / 2.0);
    assert_eq!(view.comment_at(x, y), Some(7));
    // Other text, and the first line.
    let (x, y) = at(anchor.x0 - 10.0, (anchor.y0 + anchor.y1) / 2.0);
    assert_eq!(view.comment_at(x, y), None);
    let (x, y) = at(40.0, 25.0);
    assert_eq!(view.comment_at(x, y), None);
}

#[test]
fn going_to_a_comment_selects_its_text() {
    let mut view = view();
    assert!(view.go_to_comment(7));
    assert_eq!(view.selected_text(), "cd");
    assert!(!view.go_to_comment(99));
}

#[test]
fn next_and_previous_step_through_the_changes_and_wrap() {
    let mut view = view();
    let all = view.changes();
    assert_eq!(all.len(), 3);
    // From the start: the insertion, then the deletion, then the late one.
    view.set_selection(pos(0, 0), pos(0, 0));
    assert!(view.next_change());
    assert_eq!(view.selected_text(), "two ");
    assert!(view.next_change());
    assert_eq!(view.selected_text(), "");
    assert_eq!(view.caret(), Some(&pos(0, 8)));
    assert!(view.next_change());
    assert_eq!(view.selected_text(), "change");
    // The last one has the page of it in view.
    assert!(view.current_page() > 0 || view.layout().pages.len() == 1);
    // After the last: the first.
    assert!(view.next_change());
    assert_eq!(view.selected_text(), "two ");
    // Backwards: before the first is the last.
    assert!(view.previous_change());
    assert_eq!(view.selected_text(), "change");
    assert!(view.previous_change());
    assert_eq!(view.caret(), Some(&pos(0, 8)));
    assert!(!DocView::new(Document::default()).next_change());
}

#[test]
fn accepting_and_rejecting_by_selection_are_one_step_of_undo_each() {
    let mut view = view();
    view.set_selection(pos(0, 4), pos(0, 8));
    assert!(view.accept_changes());
    assert_eq!(view.changes().len(), 2);
    assert!(view.is_modified());
    // Rejecting the deletion at the caret brings its text back.
    view.set_selection(pos(0, 8), pos(0, 8));
    assert!(view.reject_changes());
    assert_eq!(texts(&view)[0], "one two gone three");
    assert_eq!(view.changes().len(), 1);
    // Nothing there to resolve: not a step.
    view.set_selection(pos(1, 0), pos(1, 2));
    assert!(!view.accept_changes());
    assert!(!view.reject_changes());
    // One undo for each.
    assert!(view.undo());
    assert_eq!(view.changes().len(), 2);
    assert_eq!(texts(&view)[0], "one two three");
    assert!(view.undo());
    assert_eq!(view.changes().len(), 3);
    assert!(view.redo());
    assert_eq!(view.changes().len(), 2);
}

#[test]
fn all_changes_are_accepted_or_rejected_in_one_step() {
    let mut view = view();
    assert!(view.reject_all_changes());
    assert!(!view.has_tracked_changes());
    assert_eq!(texts(&view)[0], "one gone three");
    assert!(view.undo());
    assert!(view.has_tracked_changes());
    assert_eq!(texts(&view)[0], "one two three");
    assert!(view.accept_all_changes());
    assert_eq!(texts(&view)[0], "one two three");
    assert!(!view.has_tracked_changes());
    assert!(!view.accept_all_changes());
}

#[test]
fn typing_inside_an_insertion_stays_part_of_it() {
    let mut view = view();
    view.set_selection(pos(0, 6), pos(0, 6));
    assert!(view.type_text("XY"));
    let change = &view.changes()[0];
    assert_eq!((change.start.offset, change.end.offset), (4, 10));
    // Typing in plain text is not tracked.
    view.set_selection(pos(1, 1), pos(1, 1));
    view.type_text("Z");
    assert_eq!(view.changes().len(), 3);
}

#[test]
fn a_comment_follows_its_text_through_edits() {
    let mut view = view();
    view.set_selection(pos(1, 0), pos(1, 0));
    view.type_text("123");
    let c = &view.comments()[0];
    assert_eq!(c.range, Some((pos(1, 6), pos(1, 8))));
    // All of its text deleted: the comment stays where it was.
    view.set_selection(pos(1, 6), pos(1, 8));
    assert!(view.backspace(Unit::Cluster));
    let c = &view.comments()[0];
    assert_eq!(c.range, Some((pos(1, 6), pos(1, 6))));
    assert!(c.anchor.is_some());
}

#[test]
fn the_pdf_follows_the_switch() {
    let document = reviewed();
    let info = crate::doc_pdf::DocPdfInfo::default();
    let on = crate::doc_pdf::export_document_with(&document, &info, true).unwrap();
    let off = crate::doc_pdf::export_document_with(&document, &info, false).unwrap();
    let default = crate::doc_pdf::export_document(&document, &info).unwrap();
    assert_ne!(on, off);
    // Markup is the default.
    assert_eq!(on.len(), default.len());
}
