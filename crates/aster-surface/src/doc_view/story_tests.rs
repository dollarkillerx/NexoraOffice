//! The view in headers, footers and notes.

use std::sync::Arc;

use aster_writer::edit;
use aster_writer::{
    Block, Document, Inline, Margins, Note, Notes, Paragraph, Path, Pos, Run, RunLook, Section,
    Story, StoryId,
};

use super::*;
use crate::text_surface::ImeEvent;

fn look() -> RunLook {
    RunLook {
        size: 10.0,
        ..RunLook::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph {
        runs: vec![Run::text(text, look())],
        base: look(),
        mark: look(),
        ..Paragraph::default()
    }
}

fn pb(text: &str) -> Block {
    Block::paragraph(paragraph(text))
}

fn small() -> Section {
    Section {
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
    }
}

/// Forty lines over several pages; the first line carries footnote 1; a
/// header "Head text" and a footer "Foot text" on every page.
fn sample(with_header: bool) -> Document {
    let mut blocks: Vec<Block> = (0..40).map(|i| pb(&format!("line number {i}"))).collect();
    let mut first = paragraph("line with note");
    first.runs.push(Run::object(Inline::Footnote(1), look()));
    blocks[0] = Block::paragraph(first);
    let mut note = paragraph("");
    note.runs = vec![
        Run::object(Inline::Footnote(1), look()),
        Run::text(" the note text", look()),
    ];
    let mut section = small();
    let mut marginals = Vec::new();
    if with_header {
        marginals = vec![
            Arc::new(Story {
                blocks: vec![pb("Head text")],
                part: None,
            }),
            Arc::new(Story {
                blocks: vec![pb("Foot text")],
                part: None,
            }),
        ];
        section.headers.default = Some(0);
        section.footers.default = Some(1);
    }
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(section),
        marginals,
        footnotes: Notes {
            notes: vec![Note {
                id: 1,
                blocks: vec![Block::paragraph(note)],
            }],
            part: None,
        },
        ..Document::default()
    }
}

fn view_of(document: Document) -> DocView {
    let mut view = DocView::new(document);
    view.resize(600, 500, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    view
}

fn body(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn header(block: usize, offset: usize) -> Pos {
    Pos::new(Path::in_story(StoryId::Marginal(0), block), offset)
}

fn note(offset: usize) -> Pos {
    Pos::new(Path::in_story(StoryId::Footnote(1), 0), offset)
}

/// Where a place is in the window.
fn at(view: &DocView, pos: &Pos, page: Option<usize>) -> (f64, f64) {
    let caret = view
        .layout()
        .caret_on(page, pos)
        .expect("a place on a page");
    let rect = view.page_rect(caret.page);
    let ppp = view.pixels_per_point();
    (
        rect.x0 + f64::from(caret.x) * ppp,
        rect.y0 + f64::from(caret.top + caret.height / 2.0) * ppp,
    )
}

fn click_at(view: &mut DocView, x: f64, y: f64, clicks: u8) -> bool {
    let changed = view.press(x, y, clicks, false);
    view.release();
    changed
}

/// A point of the window in the header strip of `page`.
fn in_header_strip(view: &DocView, page: usize) -> (f64, f64) {
    let rect = view.page_rect(page);
    let ppp = view.pixels_per_point();
    (rect.x0 + 40.0 * ppp, rect.y0 + 12.0 * ppp)
}

fn texts(document: &Document, story: StoryId) -> Vec<String> {
    document
        .paragraphs_in(story)
        .iter()
        .map(|(_, p)| p.text())
        .collect()
}

#[test]
fn a_click_in_a_footnote_puts_the_caret_there_and_a_click_in_the_body_goes_back() {
    let mut view = view_of(sample(false));
    assert_eq!(view.active_story(), StoryId::Body);
    let (x, y) = at(&view, &note(8), None);
    assert!(click_at(&mut view, x, y, 1));
    assert_eq!(view.active_story(), StoryId::Footnote(1));
    assert_eq!(view.caret().unwrap().path.story, StoryId::Footnote(1));
    // Typing goes into the note.
    assert!(view.type_text("X"));
    assert!(texts(view.document(), StoryId::Footnote(1))[0].contains('X'));
    assert_eq!(
        texts(view.document(), StoryId::Body)[0],
        "line with note\u{FFFC}"
    );
    // A click in the body is a body click again.
    let (x, y) = at(&view, &body(3, 2), None);
    click_at(&mut view, x, y, 1);
    assert_eq!(view.active_story(), StoryId::Body);
    assert_eq!(view.caret().unwrap(), &body(3, 2));
    // Double-click in the note picks a word of it.
    let (x, y) = at(&view, &note(6), None);
    click_at(&mut view, x, y, 2);
    assert_eq!(view.active_story(), StoryId::Footnote(1));
    assert!(view.has_selection());
    assert!(!view.selected_text().is_empty());
}

#[test]
fn a_double_click_in_the_header_strip_enters_it_and_a_double_click_in_the_body_leaves() {
    let mut view = view_of(sample(true));
    assert!(view.page_count() >= 2);
    let (bx, by) = at(&view, &body(5, 0), None);
    click_at(&mut view, bx, by, 1);
    // One click in the strip is a click in the body, as before.
    let (hx, hy) = in_header_strip(&view, 1);
    click_at(&mut view, hx, hy, 1);
    assert_eq!(view.active_story(), StoryId::Body);
    // The double-click on page 1 enters the header there.
    click_at(&mut view, hx, hy, 2);
    assert!(view.in_marginal());
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert_eq!(view.marginal_page(), Some(1));
    assert!(!view.has_selection(), "entering picks no word");
    assert_eq!(view.caret().unwrap().path.story, StoryId::Marginal(0));
    // The caret is drawn on page 1.
    assert_eq!(view.caret_geometry().unwrap().page, 1);
    // A single click in the body does nothing while inside.
    let before = caret(&view);
    let (bx, by) = at(&view, &body(5, 3), None);
    click_at(&mut view, bx, by, 1);
    assert_eq!(caret(&view), before);
    assert!(view.in_marginal());
    // A double-click in the body leaves and puts the caret there.
    click_at(&mut view, bx, by, 2);
    assert!(!view.in_marginal());
    assert_eq!(view.active_story(), StoryId::Body);
    assert!(caret(&view).path.story.is_body());
    // Esc leaves too, and goes back to where the caret was.
    view.set_selection(body(7, 4), body(7, 4));
    click_at(&mut view, hx, hy, 2);
    assert!(view.in_marginal());
    assert!(view.leave_marginal());
    assert_eq!(caret(&view), body(7, 4));
    assert!(!view.leave_marginal());
}

#[test]
fn enter_header_and_enter_footer_act_on_the_page_the_caret_is_on() {
    let mut view = view_of(sample(true));
    view.go_to(&body(30, 0));
    let page = view.current_page();
    assert!(page >= 1);
    assert!(view.enter_header());
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert_eq!(view.marginal_page(), Some(page));
    assert_eq!(caret(&view), header(0, 0));
    assert!(view.enter_footer());
    assert_eq!(view.active_story(), StoryId::Marginal(1));
    assert_eq!(view.current_page(), page);
    // Clicking the strips moves between the two.
    let (hx, hy) = in_header_strip(&view, page);
    click_at(&mut view, hx, hy, 1);
    assert_eq!(view.active_story(), StoryId::Marginal(0));
}

#[test]
fn typing_selecting_and_moving_in_a_header_work() {
    let mut view = view_of(sample(true));
    assert!(view.enter_header());
    view.move_caret(Move::LineEnd, false);
    assert_eq!(caret(&view), header(0, 9));
    assert!(view.type_text("!"));
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["Head text!"]);
    view.move_caret(Move::LineStart, true);
    assert_eq!(view.selected_text(), "Head text!");
    view.move_caret(Move::Right, false);
    assert!(!view.has_selection());
    assert_eq!(caret(&view), header(0, 10));
    // Up and Down go nowhere past the only line: to its start and end.
    view.move_caret(Move::Up, false);
    assert_eq!(caret(&view), header(0, 0));
    view.move_caret(Move::Down, false);
    assert_eq!(caret(&view), header(0, 10));
    view.move_caret(Move::WordLeft, false);
    // The "!" is a word of its own.
    assert_eq!(caret(&view), header(0, 9));
    assert!(view.select_all());
    assert_eq!(view.selected_text(), "Head text!");
    // Selection rectangles are on the header's page.
    let rects = view.selection_rects().to_vec();
    assert!(!rects.is_empty());
    assert!(
        rects
            .iter()
            .all(|(page, r)| *page == view.marginal_page().unwrap() && r.y1 < 40.0)
    );
    // Replacing the selection by typing.
    assert!(view.type_text("New"));
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["New"]);
    // The body is untouched.
    assert_eq!(texts(view.document(), StoryId::Body)[1], "line number 1");
    // Backspace and Delete.
    assert!(view.backspace(Unit::Cluster));
    assert!(view.delete_forward(Unit::Cluster) || caret(&view).offset == 2);
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["Ne"]);
}

#[test]
fn the_mouse_selects_in_a_header_and_a_footnote() {
    let mut view = view_of(sample(true));
    let page = 1;
    let (x0, y0) = at(&view, &header(0, 0), Some(page));
    let (x1, y1) = at(&view, &header(0, 4), Some(page));
    view.press(
        in_header_strip(&view, page).0,
        in_header_strip(&view, page).1,
        2,
        false,
    );
    view.release();
    assert!(view.in_marginal());
    view.press(x0 + 0.5, y0, 1, false);
    view.drag(x1, y1);
    view.release();
    assert_eq!(view.selected_text(), "Head");
    // A drag that wanders into the body keeps to the header.
    let (bx, by) = at(&view, &body(10, 3), None);
    view.press(x0 + 0.5, y0, 1, false);
    view.drag(bx, by);
    view.release();
    assert!(view.caret().unwrap().path.story == StoryId::Marginal(0));
    // In a note.
    let mut view = view_of(sample(false));
    let (a, b) = (at(&view, &note(3), None), at(&view, &note(9), None));
    view.press(a.0, a.1, 1, false);
    view.drag(b.0, b.1);
    view.release();
    assert_eq!(view.active_story(), StoryId::Footnote(1));
    assert_eq!(view.selected_text().len(), 6);
}

#[test]
fn clipboard_and_formatting_work_in_a_header_and_a_note() {
    for story in [StoryId::Marginal(0), StoryId::Footnote(1)] {
        let mut view = view_of(sample(true));
        let (start, end) = match story {
            StoryId::Marginal(_) => (header(0, 0), header(0, 4)),
            _ => (note(4), note(8)),
        };
        view.set_selection(start.clone(), end);
        let text = view.selected_text();
        assert_eq!(text.len(), 4);
        let fragment = view.selected_fragment().unwrap();
        assert!(view.command(DocCommand::Bold));
        assert_eq!(view.format_state().bold, Tri::On);
        // Cut it and paste it back at the end.
        let (cut, _) = view.cut().unwrap();
        assert_eq!(cut, text);
        let end = view.document().story_end(story).unwrap();
        view.set_selection(end.clone(), end);
        assert!(view.paste_fragment(&fragment));
        assert!(view.paste_text("-"));
        assert!(texts(view.document(), story)[0].ends_with(&format!("{text}-")));
        // Undo steps back through the story, restoring the caret's story.
        assert!(view.undo());
        assert_eq!(view.active_story(), story);
        assert!(view.undo());
        assert_eq!(view.active_story(), story);
        assert!(view.redo());
        assert_eq!(view.active_story(), story);
    }
}

#[test]
fn undo_and_redo_restore_the_story_and_the_text() {
    let mut view = view_of(sample(true));
    let before = view.document().clone();
    view.set_selection(header(0, 4), header(0, 4));
    view.type_text("abc");
    view.seal_history();
    // A double-click in the body leaves the header.
    let (x, y) = at(&view, &body(2, 1), None);
    click_at(&mut view, x, y, 2);
    view.type_text("Z");
    assert_eq!(view.active_story(), StoryId::Body);
    // Undoing the body edit: caret in the body.
    assert!(view.undo());
    assert_eq!(view.active_story(), StoryId::Body);
    // Undoing the header edit: caret in the header again.
    assert!(view.undo());
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert!(edit::same_content(view.document(), &before));
    assert!(!view.is_modified());
    assert!(view.redo());
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert_eq!(
        texts(view.document(), StoryId::Marginal(0)),
        ["Headabc text"]
    );
    assert!(view.redo());
    assert_eq!(view.active_story(), StoryId::Body);
}

#[test]
fn an_input_method_composes_in_a_header() {
    let mut view = view_of(sample(true));
    view.enter_header();
    view.move_caret(Move::LineEnd, false);
    assert!(view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    }));
    assert!(view.is_composing());
    // Shown, but not yet in the document.
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["Head text"]);
    assert!(view.caret_rect().is_some());
    assert!(view.ime(ImeEvent::Commit("你好".into())));
    assert!(!view.is_composing());
    assert_eq!(
        texts(view.document(), StoryId::Marginal(0)),
        ["Head text你好"]
    );
    assert_eq!(view.active_story(), StoryId::Marginal(0));
}

#[test]
fn entering_a_header_that_is_not_there_changes_nothing_and_the_first_edit_makes_it() {
    let mut view = view_of(sample(false));
    let before = view.document().clone();
    let (x, y) = in_header_strip(&view, 0);
    click_at(&mut view, x, y, 2);
    assert!(view.in_marginal());
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert!(view.caret_rect().is_some());
    assert!(view.caret_shown());
    // Nothing changed: not the document, not the layout, not the history.
    assert!(edit::same_content(view.document(), &before));
    assert!(view.document().marginals.is_empty());
    assert!(!view.is_modified());
    assert!(!view.can_undo());
    // Keys that would act on text do nothing.
    assert!(!view.backspace(Unit::Cluster));
    assert!(!view.enter());
    assert!(!view.move_caret(Move::Right, false));
    assert!(view.document().marginals.is_empty());
    // The first typing makes it, in one step.
    assert!(view.type_text("Title"));
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["Title"]);
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert_eq!(view.document().sections()[0].1.headers.default, Some(0));
    assert!(view.is_modified());
    // Undo takes the header and the text away, back to the body.
    assert!(view.undo());
    assert!(view.document().marginals.is_empty());
    assert_eq!(view.active_story(), StoryId::Body);
    assert!(!view.is_modified());
    assert!(edit::same_content(view.document(), &before));
    assert!(view.redo());
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["Title"]);
}

#[test]
fn leaving_a_header_that_is_not_there_makes_nothing_and_a_no_op_edit_makes_nothing() {
    let mut view = view_of(sample(false));
    let before = view.document().clone();
    assert!(view.enter_footer());
    assert!(view.in_marginal());
    // Bold with only a caret changes no text: nothing is made.
    view.command(DocCommand::Bold);
    assert!(view.document().marginals.is_empty());
    assert!(view.leave_marginal());
    assert!(!view.in_marginal());
    assert!(edit::same_content(view.document(), &before));
    assert!(!view.is_modified());
    assert!(!view.can_undo());
    // An input method that starts in it is shown but not made.
    assert!(view.enter_header());
    assert!(view.ime(ImeEvent::Preedit {
        text: "a".into(),
        cursor: None,
    }));
    assert!(view.document().marginals.is_empty());
    assert!(view.cancel_composition());
    assert!(view.document().marginals.is_empty());
    assert!(view.ime(ImeEvent::Preedit {
        text: "b".into(),
        cursor: None,
    }));
    assert!(view.ime(ImeEvent::Commit("b".into())));
    assert_eq!(texts(view.document(), StoryId::Marginal(0)), ["b"]);
}

#[test]
fn a_footnote_is_inserted_at_the_caret_and_the_caret_goes_into_it() {
    let mut view = view_of(sample(false));
    view.set_selection(body(2, 4), body(2, 4));
    let before = view.document().clone();
    assert!(view.insert_footnote());
    assert_eq!(view.document().footnotes.notes.len(), 2);
    assert_eq!(view.active_story(), StoryId::Footnote(2));
    let caret = caret(&view);
    assert_eq!(caret.path, Path::in_story(StoryId::Footnote(2), 0));
    assert!(view.caret_rect().is_some());
    // Typing goes into the note, shown at the bottom of a page.
    view.type_text("Source");
    assert!(texts(view.document(), StoryId::Footnote(2))[0].ends_with("Source"));
    // Not in a note.
    assert!(!view.insert_footnote());
    // Undo goes back to the body and the document without the note.
    assert!(view.undo());
    assert!(view.undo());
    assert!(edit::same_content(view.document(), &before));
    assert_eq!(view.active_story(), StoryId::Body);
    assert_eq!(caret_of(&view), body(2, 4));
}

fn caret_of(view: &DocView) -> Pos {
    view.caret().cloned().unwrap()
}

fn caret(view: &DocView) -> Pos {
    caret_of(view)
}

#[test]
fn a_page_number_goes_into_a_header_and_into_a_header_not_made_yet() {
    let mut view = view_of(sample(true));
    view.enter_header();
    view.move_caret(Move::LineEnd, false);
    assert!(view.insert_page_number());
    let p = view
        .document()
        .paragraph(&header(0, 0).path)
        .unwrap()
        .clone();
    assert!(
        p.runs
            .iter()
            .any(|r| matches!(r.kind, Inline::PageNumber(_)))
    );
    // The layout shows the page's number, not the stored text.
    let text = view
        .document()
        .paragraph(&header(0, 0).path)
        .unwrap()
        .text();
    assert!(text.starts_with("Head text"));

    let mut view = view_of(sample(false));
    view.enter_footer();
    assert!(view.document().marginals.is_empty());
    assert!(view.insert_page_number());
    assert_eq!(view.document().marginals.len(), 1);
    assert_eq!(view.active_story(), StoryId::Marginal(0));
    assert!(view.document().sections()[0].1.footers.default == Some(0));
}

#[test]
fn dashed_lines_mark_the_strips_only_while_inside() {
    let mut view = view_of(sample(true));
    view.render();
    let plain = view.pixmap.data_as_u8_slice().to_vec();
    view.enter_header();
    view.render();
    let inside = view.pixmap.data_as_u8_slice().to_vec();
    assert_ne!(plain, inside);
    view.leave_marginal();
    view.render();
    assert_eq!(plain, view.pixmap.data_as_u8_slice());
}

#[test]
fn find_stays_in_the_body() {
    let mut view = view_of(sample(true));
    let count = view.set_find(DocQuery {
        pattern: "text".into(),
        ..DocQuery::default()
    });
    // "Head text", "Foot text" and the note's text are not found.
    assert_eq!(count, 0);
}
