//! Tests of the document view, without a window. Documents are built in
//! code on small pages (300 by 200 points, 20-point margins) so that a few
//! paragraphs fill a page, and from the sample DOCX of `aster-ooxml`.
//! Assertions are about relations, not exact positions: fonts are the
//! system's.

use std::sync::Arc;

use aster_writer::{
    Block, Cell, Document, Inline, Link, Margins, Paragraph, Path, Pos, Row, Run, RunLook, Section,
    Story, Table,
};

use super::*;

#[path = "../../../aster-ooxml/tests/common/docx_sample.rs"]
mod docx_sample;

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

fn doc(blocks: Vec<Block>) -> Document {
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(small()),
        ..Document::default()
    }
}

fn cell(text: &str) -> Cell {
    Cell {
        blocks: vec![pb(text)],
        ..Cell::default()
    }
}

fn table(rows: &[&[&str]]) -> Block {
    let columns = vec![2_000; rows.iter().map(|r| r.len()).max().unwrap_or(1)];
    Block::table(Table {
        columns,
        rows: rows
            .iter()
            .map(|row| Row {
                cells: row.iter().map(|t| cell(t)).collect(),
                ..Row::default()
            })
            .collect(),
        ..Table::default()
    })
}

fn lines(n: usize) -> Document {
    doc((0..n).map(|i| pb(&format!("line number {i}"))).collect())
}

/// A window of 600 by 500 pixels at 100 %.
fn view_of(document: Document) -> DocView {
    let mut view = DocView::new(document);
    view.resize(600, 500, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    view
}

fn pos(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn cell_pos(table: usize, row: usize, cell: usize, offset: usize) -> Pos {
    Pos::new(Path::body(table).cell_block(row, cell, 0), offset)
}

/// Where a place is in the window (it may be outside the window).
fn at(view: &DocView, pos: &Pos) -> (f64, f64) {
    let caret = view.layout().caret(pos).expect("a place on a page");
    let rect = view.page_rect(caret.page);
    let ppp = view.pixels_per_point();
    (
        rect.x0 + f64::from(caret.x) * ppp,
        rect.y0 + f64::from(caret.top + caret.height / 2.0) * ppp,
    )
}

fn click(view: &mut DocView, pos: &Pos) {
    let (x, y) = at(view, pos);
    view.press(x, y, 1, false);
    view.release();
}

fn drag(view: &mut DocView, from: &Pos, to: &Pos) {
    let (x, y) = at(view, from);
    view.press(x, y, 1, false);
    let (x, y) = at(view, to);
    view.drag(x, y);
    view.release();
}

fn caret(view: &DocView) -> Pos {
    view.caret().cloned().expect("a caret")
}

fn page_of(view: &DocView, pos: &Pos) -> usize {
    view.layout().page_of(pos).expect("a page")
}

/// Every place the view reports is a `char` boundary of a paragraph.
fn assert_valid(view: &DocView, pos: &Pos) {
    let paragraph = view
        .document()
        .paragraph(&pos.path)
        .unwrap_or_else(|| panic!("no paragraph at {pos:?}"));
    assert!(pos.offset <= paragraph.len(), "{pos:?}");
    assert!(paragraph.text().is_char_boundary(pos.offset), "{pos:?}");
}

#[test]
fn a_view_can_be_built_on_one_thread_and_used_on_another() {
    fn is_send<T: Send>() {}
    is_send::<DocView>();
    let view = std::thread::spawn(|| DocView::new(lines(5)))
        .join()
        .expect("built");
    assert_eq!(view.page_count(), 1);
}

// ---------------------------------------------------------------------------
// Pages, scrolling and zoom
// ---------------------------------------------------------------------------

#[test]
fn pages_are_stacked_with_a_scroll_range_that_covers_them() {
    let mut view = view_of(lines(60));
    let pages = view.page_count();
    assert!(pages >= 4, "{pages} pages");
    let (offset, max) = view.scroll_range();
    assert_eq!(offset, 0.0);
    // At 100 % a page is 200 pixels; the range is at least all pages less
    // the window.
    assert!(max >= 200.0 * pages as f64 - 500.0, "{max}");
    assert!(view.scroll_by(100.0));
    assert_eq!(view.scroll_range().0, 100.0);
    assert!(view.scroll_by(1.0e9));
    assert_eq!(view.scroll_range().0, max);
    assert!(!view.scroll_by(1.0));
    assert!(view.scroll_by(-1.0e9));
    assert_eq!(view.scroll_range().0, 0.0);
    // The first page is centred in the window.
    let rect = view.page_rect(0);
    assert!((rect.x0 - (600.0 - 300.0) / 2.0).abs() <= 1.0, "{rect:?}");
}

#[test]
fn pages_of_different_sizes_are_each_centred() {
    let landscape = Section {
        page: (8_000, 4_000),
        ..small()
    };
    let mut first = paragraph("portrait");
    first.section = Some(Arc::new(small()));
    let mut document = doc(vec![Block::paragraph(first), pb("landscape")]);
    document.last_section = Arc::new(landscape);
    let view = view_of(document);
    assert_eq!(view.page_count(), 2);
    let (a, b) = (view.page_rect(0), view.page_rect(1));
    assert!(b.width() > a.width());
    let centre = |r: Rect| (r.x0 + r.x1) / 2.0;
    assert!((centre(a) - 300.0).abs() <= 1.0);
    assert!((centre(b) - 300.0).abs() <= 1.0);
}

#[test]
fn a_page_wider_than_the_window_scrolls_sideways() {
    let mut view = view_of(lines(3));
    view.resize(200, 300, 1.0);
    let before = view.page_rect(0).x0;
    assert!(view.wheel(40.0, 0.0));
    assert_eq!(view.page_rect(0).x0, before - 40.0);
    assert!(view.wheel(1.0e6, 0.0));
    // The right edge of the page, with the margin, is at the window's edge.
    assert!(view.page_rect(0).x1 <= 200.0 + 25.0 * 1.0 + 1.0);
    assert!(view.wheel(-1.0e6, 0.0));
    assert_eq!(view.scroll_x, 0.0);
}

#[test]
fn zoom_steps_and_modes_are_the_pdf_views() {
    let mut view = view_of(lines(3));
    assert_eq!(view.zoom_percent(), 100.0);
    assert!(view.zoom_in());
    assert_eq!(view.zoom_percent(), 125.0);
    assert!(view.zoom_out());
    assert!(view.zoom_out());
    assert_eq!(view.zoom_percent(), 90.0);
    assert!(view.set_zoom(Zoom::Percent(1.0e6)));
    assert_eq!(view.zoom_percent(), ZOOM_MAX);
    assert!(!view.zoom_in());
    assert!(view.set_zoom(Zoom::FitWidth));
    // 300-point pages in 600 pixels less two margins of 24.
    let expected = (600.0 - 48.0) / 300.0 * 100.0;
    assert!((f64::from(view.zoom_percent()) - expected).abs() <= 1.0);
    assert!(view.set_zoom(Zoom::FitPage));
    let page = f64::from(view.zoom_percent());
    assert!(page <= expected + 1.0);
    assert!(page <= (500.0 - 48.0) / 200.0 * 100.0 + 1.0);
}

#[test]
fn zoom_keeps_the_place_being_looked_at() {
    let mut view = view_of(lines(120));
    assert!(view.page_count() >= 8);
    let before = view.page_count();
    // Look at the middle of the fourth page.
    view.set_scroll(view.page_top(3) + 100.0);
    let page = view.page_at(view.scroll_y);
    let fraction = |view: &DocView| {
        let top = view.page_top(page);
        let height = f64::from(view.sizes[page].1) * view.pixels_per_point();
        (view.scroll_y - top) / height
    };
    let into = fraction(&view);
    for zoom in [Zoom::Percent(200.0), Zoom::Percent(50.0), Zoom::FitWidth] {
        view.set_zoom(zoom);
        assert_eq!(view.page_at(view.scroll_y), page, "{zoom:?}");
        assert!((fraction(&view) - into).abs() < 0.02, "{zoom:?}");
    }
    assert_eq!(view.page_count(), before, "layout does not depend on zoom");
    // Resizing keeps it too.
    view.resize(900, 700, 1.0);
    assert_eq!(view.page_at(view.scroll_y), page);
    assert!((fraction(&view) - into).abs() < 0.02);
}

#[test]
fn layout_does_not_change_with_the_zoom_or_the_window() {
    let mut view = view_of(lines(40));
    let heights: Vec<f32> = view.layout().pages.iter().map(|p| p.height).collect();
    let caret = view.layout().caret(&pos(30, 3)).unwrap();
    view.set_zoom(Zoom::Percent(300.0));
    view.resize(1000, 400, 2.0);
    let again = view.layout().caret(&pos(30, 3)).unwrap();
    assert_eq!(caret, again);
    let now: Vec<f32> = view.layout().pages.iter().map(|p| p.height).collect();
    assert_eq!(heights, now);
}

#[test]
fn the_current_page_follows_the_caret_or_the_top_of_the_view() {
    let mut view = view_of(lines(120));
    assert_eq!(view.current_page(), 0);
    // Scrolling away from the caret: the page is where the window is.
    view.set_scroll(view.page_top(4));
    assert!(view.current_page() >= 3);
    // Put the caret on a later page: the page is the caret's.
    let target = pos(100, 0);
    view.go_to(&target);
    assert_eq!(view.current_page(), page_of(&view, &target));
}

// ---------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------

#[test]
fn a_click_places_the_caret_and_a_drag_selects() {
    let mut view = view_of(doc(vec![pb("first paragraph"), pb("second paragraph")]));
    click(&mut view, &pos(1, 3));
    assert_eq!(caret(&view), pos(1, 3));
    assert!(!view.has_selection());
    assert_eq!(view.selected_text(), "");
    drag(&mut view, &pos(0, 6), &pos(1, 6));
    assert_eq!(view.selected_text(), "paragraph\nsecond");
    // Backwards.
    drag(&mut view, &pos(1, 6), &pos(0, 6));
    assert_eq!(view.selected_text(), "paragraph\nsecond");
    assert_eq!(caret(&view), pos(0, 6));
    // A click anywhere drops the selection.
    click(&mut view, &pos(0, 2));
    assert!(!view.has_selection());
}

#[test]
fn shift_click_extends_from_the_old_caret() {
    let mut view = view_of(doc(vec![pb("one two three four")]));
    click(&mut view, &pos(0, 4));
    let (x, y) = at(&view, &pos(0, 13));
    assert!(view.press(x, y, 1, true));
    assert_eq!(view.selected_text(), "two three");
    let (x, y) = at(&view, &pos(0, 0));
    view.press(x, y, 1, true);
    assert_eq!(view.selected_text(), "one ");
}

#[test]
fn a_drag_selects_across_wrapped_lines_and_pages() {
    let text = "word ".repeat(30);
    let mut blocks = vec![pb(&text)];
    blocks.extend((0..60).map(|i| pb(&format!("line number {i}"))));
    let mut view = view_of(doc(blocks));
    let (from, to) = (pos(0, 12), pos(55, 4));
    assert!(page_of(&view, &to) > page_of(&view, &from) + 2);
    drag(&mut view, &from, &to);
    let selected = view.selected_text();
    let expected = text_between(view.document(), &from, &to);
    assert_eq!(selected, expected);
    assert!(selected.starts_with("rd word"));
    assert!(selected.ends_with("line"));
    assert_eq!(selected.lines().count(), 56);
    // The selection is painted on every page between.
    let rects = view.selection_rects().to_vec();
    let pages: std::collections::BTreeSet<usize> = rects.iter().map(|(p, _)| *p).collect();
    assert!(pages.len() >= 3, "{pages:?}");
}

#[test]
fn a_drag_selects_across_table_cells() {
    let document = doc(vec![
        pb("before"),
        table(&[&["a1", "a2"], &["b1", "b2"]]),
        pb("after"),
    ]);
    let mut view = view_of(document);
    drag(&mut view, &cell_pos(1, 0, 1, 0), &cell_pos(1, 1, 1, 2));
    assert_eq!(view.selected_text(), "a2\nb1\nb2");
    drag(&mut view, &pos(0, 3), &cell_pos(1, 0, 0, 1));
    assert_eq!(view.selected_text(), "ore\na");
    drag(&mut view, &cell_pos(1, 1, 0, 0), &pos(2, 3));
    assert_eq!(view.selected_text(), "b1\nb2\naft");
}

#[test]
fn double_click_selects_a_word_and_triple_click_a_paragraph() {
    let latin = "The quick brown fox";
    let chinese = "我们喜欢办公软件，也喜欢写文档。";
    let japanese = "私は日本語の文書を書きます";
    let mut view = view_of(doc(vec![pb(latin), pb(chinese), pb(japanese)]));
    let word = |view: &mut DocView, target: Pos, clicks: u8| {
        let (x, y) = at(view, &target);
        view.press(x, y, clicks, false);
        view.release();
        view.selected_text()
    };
    assert_eq!(word(&mut view, pos(0, 6), 2), "quick");
    assert_eq!(word(&mut view, pos(0, 6), 3), latin);
    let picked = word(&mut view, pos(1, 3), 2);
    assert!(
        !picked.is_empty() && picked.chars().count() < chinese.chars().count(),
        "{picked:?}"
    );
    assert!(chinese.contains(&picked));
    assert_eq!(word(&mut view, pos(1, 3), 3), chinese);
    let picked = word(&mut view, pos(2, 6), 2);
    assert!(
        !picked.is_empty() && picked.chars().count() < japanese.chars().count(),
        "{picked:?}"
    );
    assert!(japanese.contains(&picked));
    assert_eq!(word(&mut view, pos(2, 6), 3), japanese);
}

#[test]
fn a_drag_after_a_double_click_extends_by_words() {
    let mut view = view_of(doc(vec![pb("alpha beta gamma delta")]));
    let (x, y) = at(&view, &pos(0, 8));
    view.press(x, y, 2, false);
    assert_eq!(view.selected_text(), "beta");
    let (x, y) = at(&view, &pos(0, 14));
    view.drag(x, y);
    assert_eq!(view.selected_text(), "beta gamma");
    let (x, y) = at(&view, &pos(0, 2));
    view.drag(x, y);
    assert_eq!(view.selected_text(), "alpha beta");
    view.release();
    assert!(!view.drag(x, y), "no press, no drag");
}

#[test]
fn dragging_past_the_edge_scrolls_and_extends() {
    let mut view = view_of(lines(120));
    click(&mut view, &pos(2, 0));
    let (x, y) = at(&view, &pos(2, 0));
    view.press(x, y, 1, false);
    let before = view.scroll_y;
    for _ in 0..10 {
        assert!(view.drag_scroll(x, 600.0));
    }
    assert!(view.scroll_y > before);
    let focus = view.selection().expect("a selection").1.clone();
    assert!(focus > pos(2, 0));
    assert_valid(&view, &focus);
    view.release();
}

#[test]
fn points_anywhere_give_valid_places() {
    let mut view = view_of(doc(vec![
        pb("ab\u{301}c 漢字 🙂 end"),
        table(&[&["x", "é"], &["", "z"]]),
        pb(""),
        pb("tail"),
    ]));
    for zoom in [100.0, 33.0, 400.0] {
        view.set_zoom(Zoom::Percent(zoom));
        for y in (-40..700).step_by(7) {
            for x in (-40_i32..700).step_by(9) {
                if let Some(found) = view.pos_at(f64::from(x), f64::from(y)) {
                    assert_valid(&view, &found);
                }
                // And a click there moves the caret to a valid place.
                view.press(f64::from(x), f64::from(y), 1 + x.rem_euclid(3) as u8, false);
                assert_valid(&view, &caret(&view));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

#[test]
fn arrows_move_by_grapheme_cluster_across_paragraphs() {
    let mut view = view_of(doc(vec![pb("ae\u{301}x"), pb("😀🇯🇵z"), pb("")]));
    view.move_caret(Move::Right, false);
    assert_eq!(caret(&view), pos(0, 1));
    view.move_caret(Move::Right, false);
    // Over "e" and its accent together.
    assert_eq!(caret(&view), pos(0, 4));
    view.move_caret(Move::Left, false);
    assert_eq!(caret(&view), pos(0, 1));
    view.move_caret(Move::Right, false);
    view.move_caret(Move::Right, false);
    assert_eq!(caret(&view), pos(0, 5));
    // The end of a paragraph to the start of the next.
    view.move_caret(Move::Right, false);
    assert_eq!(caret(&view), pos(1, 0));
    view.move_caret(Move::Right, false);
    assert_eq!(caret(&view), pos(1, 4));
    // A flag is one cluster.
    view.move_caret(Move::Right, false);
    assert_eq!(caret(&view), pos(1, 12));
    view.move_caret(Move::Left, false);
    assert_eq!(caret(&view), pos(1, 4));
    view.move_caret(Move::Left, false);
    view.move_caret(Move::Left, false);
    // And back to the end of the one before.
    assert_eq!(caret(&view), pos(0, 5));
    view.move_caret(Move::DocStart, false);
    assert!(!view.move_caret(Move::Left, false));
    view.move_caret(Move::DocEnd, false);
    assert_eq!(caret(&view), pos(2, 0));
    assert!(!view.move_caret(Move::Right, false));
}

#[test]
fn shift_arrows_select_and_plain_arrows_collapse() {
    let mut view = view_of(doc(vec![pb("hello world"), pb("next")]));
    for _ in 0..3 {
        view.move_caret(Move::Right, true);
    }
    assert_eq!(view.selected_text(), "hel");
    view.move_caret(Move::Left, true);
    assert_eq!(view.selected_text(), "he");
    // Right collapses to the end of the selection, Left to its start.
    view.move_caret(Move::Right, false);
    assert!(!view.has_selection());
    assert_eq!(caret(&view), pos(0, 2));
    view.move_caret(Move::Right, true);
    view.move_caret(Move::Right, true);
    view.move_caret(Move::Left, false);
    assert_eq!(caret(&view), pos(0, 2));
    // Across a paragraph boundary, with the line break in the text.
    view.move_caret(Move::LineEnd, true);
    view.move_caret(Move::Right, true);
    view.move_caret(Move::Right, true);
    assert_eq!(view.selected_text(), "llo world\nn");
    assert!(view.select_all());
    assert_eq!(view.selected_text(), "hello world\nnext");
}

#[test]
fn word_steps_and_line_and_document_ends() {
    let mut view = view_of(doc(vec![pb("alpha beta  gamma"), pb("delta")]));
    view.move_caret(Move::WordRight, false);
    assert_eq!(caret(&view), pos(0, 6));
    view.move_caret(Move::WordRight, false);
    assert_eq!(caret(&view), pos(0, 12));
    view.move_caret(Move::WordRight, false);
    assert_eq!(caret(&view), pos(0, 17));
    view.move_caret(Move::WordRight, false);
    assert_eq!(caret(&view), pos(1, 0));
    view.move_caret(Move::WordLeft, false);
    assert_eq!(caret(&view), pos(0, 17));
    view.move_caret(Move::WordLeft, false);
    assert_eq!(caret(&view), pos(0, 12));
    view.move_caret(Move::WordLeft, true);
    assert_eq!(view.selected_text(), "beta  ");
    view.move_caret(Move::LineStart, false);
    assert_eq!(caret(&view), pos(0, 0));
    view.move_caret(Move::LineEnd, false);
    assert_eq!(caret(&view), pos(0, 17));
    view.move_caret(Move::DocEnd, true);
    assert_eq!(view.selected_text(), "\ndelta");
    view.move_caret(Move::DocStart, false);
    assert_eq!(caret(&view), pos(0, 0));
    assert!(!view.has_selection());
    // Chinese words are cut by the dictionary.
    let mut view = view_of(doc(vec![pb("我们喜欢办公软件")]));
    view.move_caret(Move::WordRight, false);
    let first = caret(&view);
    assert!(first.offset > 0 && first.offset < 24);
    assert_valid(&view, &first);
    view.move_caret(Move::WordLeft, false);
    assert_eq!(caret(&view), pos(0, 0));
}

#[test]
fn line_start_and_end_follow_wrapped_lines() {
    let text = "word ".repeat(40);
    let mut view = view_of(doc(vec![pb(&text)]));
    click(&mut view, &pos(0, 110));
    view.move_caret(Move::LineStart, false);
    let start = caret(&view);
    assert!(start.offset > 0 && start.offset <= 110);
    view.move_caret(Move::LineEnd, false);
    let end = caret(&view);
    assert!(end.offset >= 110 && end.offset < text.len());
    assert_ne!(
        view.layout().caret(&start).unwrap().top,
        view.layout().caret(&pos(0, 0)).unwrap().top
    );
}

#[test]
fn up_and_down_keep_their_goal_column() {
    let text = "word ".repeat(60);
    let mut view = view_of(doc(vec![pb(&text), pb("short"), pb(&text)]));
    click(&mut view, &pos(0, 43));
    let goal = view.layout().caret(&caret(&view)).unwrap().x;
    let mut tops = vec![view.layout().caret(&caret(&view)).unwrap().top];
    for _ in 0..3 {
        view.move_caret(Move::Down, false);
        let c = view.layout().caret(&caret(&view)).unwrap();
        assert!(c.top > *tops.last().unwrap());
        tops.push(c.top);
        assert!((c.x - goal).abs() < 12.0, "{c:?} vs {goal}");
    }
    // Through a short paragraph and out again to the same column.
    click(&mut view, &pos(0, 280));
    let goal = view.layout().caret(&caret(&view)).unwrap().x;
    view.move_caret(Move::Down, false);
    let mut seen_short = false;
    for _ in 0..4 {
        seen_short |= caret(&view).path == Path::body(1);
        view.move_caret(Move::Down, false);
    }
    assert!(seen_short);
    let c = view.layout().caret(&caret(&view)).unwrap();
    assert!((c.x - goal).abs() < 12.0, "{c:?} vs {goal}");
    for _ in 0..3 {
        view.move_caret(Move::Up, false);
    }
    // Up from the first line goes to the start, Down from the last to the end.
    view.move_caret(Move::DocStart, false);
    view.move_caret(Move::Right, false);
    view.move_caret(Move::Up, false);
    assert_eq!(caret(&view), pos(0, 0));
    view.move_caret(Move::DocEnd, false);
    view.move_caret(Move::Left, false);
    view.move_caret(Move::Down, false);
    assert_eq!(Some(caret(&view)), view.document().end());
}

#[test]
fn up_and_down_go_across_pages() {
    let mut view = view_of(lines(80));
    let mut pages = vec![0];
    let mut last = pos(0, 0);
    for _ in 0..79 {
        view.move_caret(Move::Down, false);
        let now = caret(&view);
        assert!(now > last);
        last = now.clone();
        let page = page_of(&view, &now);
        assert!(page >= *pages.last().unwrap());
        pages.push(page);
    }
    assert_eq!(last, pos(79, 0));
    assert!(*pages.last().unwrap() >= 3);
    for _ in 0..79 {
        view.move_caret(Move::Up, false);
    }
    assert_eq!(caret(&view), pos(0, 0));
}

#[test]
fn up_and_down_go_into_and_out_of_tables() {
    let document = doc(vec![
        pb("before"),
        table(&[&["a1", "a2"], &["b1", "b2"]]),
        pb("after"),
    ]);
    let mut view = view_of(document);
    click(&mut view, &pos(0, 1));
    view.move_caret(Move::Down, false);
    assert_eq!(caret(&view).path, Path::body(1).cell_block(0, 0, 0));
    view.move_caret(Move::Down, false);
    assert_eq!(caret(&view).path, Path::body(1).cell_block(1, 0, 0));
    view.move_caret(Move::Down, false);
    assert_eq!(caret(&view).path, Path::body(2));
    // Back up into the table in the column the caret was in.
    view.move_caret(Move::Up, false);
    assert_eq!(caret(&view).path, Path::body(1).cell_block(1, 0, 0));
    // Left and Right walk the cells in reading order.
    view.move_caret(Move::DocStart, false);
    for _ in 0.."before".len() + 1 {
        view.move_caret(Move::Right, false);
    }
    assert_eq!(caret(&view), cell_pos(1, 0, 0, 0));
    view.move_caret(Move::Right, true);
    view.move_caret(Move::Right, true);
    view.move_caret(Move::Right, true);
    assert_eq!(view.selected_text(), "a1\n");
    for _ in 0..30 {
        view.move_caret(Move::Right, false);
        assert_valid(&view, &caret(&view));
    }
    assert_eq!(caret(&view), pos(2, 5));
}

#[test]
fn page_up_and_down_move_by_a_window() {
    let mut view = view_of(lines(120));
    let start = caret(&view);
    view.move_caret(Move::PageDown, false);
    let one = caret(&view);
    assert!(one > start);
    assert!(view.scroll_y > 0.0);
    let scrolled = view.scroll_y;
    view.move_caret(Move::PageDown, false);
    assert!(caret(&view) > one);
    assert!(view.scroll_y > scrolled);
    // The caret stays in the window.
    let (_, y, _, h) = view.caret_rect().unwrap();
    assert!(y >= 0.0 && y + h <= 500.0, "{y}");
    view.move_caret(Move::PageUp, false);
    view.move_caret(Move::PageUp, false);
    view.move_caret(Move::PageUp, false);
    assert_eq!(caret(&view), pos(0, 0));
    assert_eq!(view.scroll_y, 0.0);
    // To the end and back, selecting.
    for _ in 0..40 {
        view.move_caret(Move::PageDown, true);
    }
    assert_eq!(Some(caret(&view)), view.document().end());
    assert!(view.has_selection());
    assert_valid(&view, &caret(&view));
}

#[test]
fn the_view_scrolls_to_keep_the_caret_in_sight() {
    let mut view = view_of(lines(120));
    view.move_caret(Move::DocEnd, false);
    let (_, y, _, h) = view.caret_rect().unwrap();
    assert!(y >= 0.0 && y + h <= 500.0);
    assert!(view.scroll_y > 0.0);
    view.move_caret(Move::DocStart, false);
    assert_eq!(view.scroll_y, 0.0);
    // Zoomed in, a caret off to the side comes into view.
    view.set_zoom(Zoom::Percent(400.0));
    view.move_caret(Move::LineEnd, false);
    let (x, _, w, _) = view.caret_rect().unwrap();
    assert!(x >= 0.0 && x + w <= 600.0, "{x}");
}

// ---------------------------------------------------------------------------
// Reading the selection
// ---------------------------------------------------------------------------

#[test]
fn selected_text_has_a_line_per_paragraph_without_objects() {
    let mut with_objects = paragraph("a\tb");
    with_objects.runs = vec![
        Run::text("a", look()),
        Run::object(Inline::Footnote(1), look()),
        Run::text("\tb", look()),
        Run::object(Inline::Footnote(2), look()),
    ];
    let document = doc(vec![Block::paragraph(with_objects), pb("second")]);
    let text = text_between(&document, &pos(0, 0), &document.end().unwrap());
    assert_eq!(text, "a\tb\nsecond");
    // Either order, and nothing for an empty range.
    assert_eq!(
        text_between(&document, &document.end().unwrap(), &pos(0, 0)),
        text
    );
    assert_eq!(text_between(&document, &pos(1, 2), &pos(1, 2)), "");
    let mut view = view_of(document);
    view.select_all();
    assert_eq!(view.selected_text(), "a\tb\nsecond");
}

// ---------------------------------------------------------------------------
// Find
// ---------------------------------------------------------------------------

fn query(pattern: &str) -> DocQuery {
    DocQuery {
        pattern: pattern.into(),
        ..DocQuery::default()
    }
}

#[test]
fn find_counts_highlights_and_wraps_round() {
    let mut blocks: Vec<Block> = (0..60)
        .map(|i| pb(&format!("row {i} Needle here")))
        .collect();
    blocks.push(table(&[&["needle in a cell"]]));
    let mut view = view_of(doc(blocks));
    assert_eq!(view.set_find(query("NEEDLE")), 61);
    assert_eq!(view.hit_count(), 61);
    assert_eq!(view.current_hit(), Some(0));
    // Case-sensitive: only the paragraphs.
    let strict = DocQuery {
        pattern: "Needle".into(),
        case_sensitive: true,
        ..DocQuery::default()
    };
    assert_eq!(view.set_find(strict), 60);
    // From the caret at the start: the first one is current.
    assert_eq!(view.current_hit(), Some(0));
    assert!(view.find_step(true));
    assert_eq!(view.current_hit(), Some(1));
    assert_eq!(view.selected_text(), "Needle");
    assert!(view.find_step(false));
    assert!(view.find_step(false));
    assert_eq!(view.current_hit(), Some(59), "wrapped round to the last");
    assert!(view.find_step(true));
    assert_eq!(view.current_hit(), Some(0), "and to the first again");
    // The match is shown.
    let (_, y, _, h) = view.caret_rect().unwrap();
    assert!(y >= 0.0 && y + h <= 500.0);
    view.find_step(false);
    let (_, y, _, h) = view.caret_rect().unwrap();
    assert!(y >= 0.0 && y + h <= 500.0, "{y}");
    assert!(view.scroll_y > 0.0);
    // The marks are on the visible page only until they are asked for.
    let marks = view.hit_marks(page_of(&view, &pos(59, 0)));
    assert!(marks.iter().any(|(current, _)| *current));
    assert!(!marks.is_empty());
    assert_eq!(view.set_find(query("nothing like this")), 0);
    assert!(!view.find_step(true));
    view.clear_find();
    assert_eq!(view.hit_count(), 0);
    assert!(view.hit_marks(0).is_empty());
}

#[test]
fn find_starts_from_the_caret_and_whole_words_are_asked_for() {
    let mut view = view_of(doc(vec![pb("cat concat"), pb("cat. category"), pb("cat")]));
    click(&mut view, &pos(1, 2));
    assert_eq!(view.set_find(query("cat")), 5);
    // The first at or after the caret: "category", not the "cat." before it.
    assert_eq!(view.hit(view.current_hit().unwrap()).unwrap().0, &pos(1, 5));
    let whole = DocQuery {
        pattern: "cat".into(),
        whole_word: true,
        ..DocQuery::default()
    };
    assert_eq!(view.set_find(whole), 3);
    let chinese = DocQuery {
        pattern: "办公".into(),
        ..DocQuery::default()
    };
    let mut view = view_of(doc(vec![pb("我们喜欢办公软件，办公室")]));
    assert_eq!(view.set_find(chinese), 2);
}

// ---------------------------------------------------------------------------
// Outline, links, status
// ---------------------------------------------------------------------------

#[test]
fn the_outline_jumps_to_headings() {
    let mut blocks = Vec::new();
    for i in 0..30 {
        let mut heading = paragraph(&format!("Heading {i}"));
        heading.props.outline_level = Some(if i % 5 == 0 { 0 } else { 1 });
        blocks.push(Block::paragraph(heading));
        blocks.extend((0..8).map(|n| pb(&format!("body {i} {n}"))));
    }
    let mut view = view_of(doc(blocks));
    let outline: Vec<_> = view.outline().to_vec();
    assert_eq!(outline.len(), 30);
    assert_eq!(outline[0].level, 0);
    assert_eq!(outline[1].level, 1);
    assert_eq!(outline[3].text, "Heading 3");
    assert!(outline[29].page > outline[0].page);
    for entry in [&outline[29], &outline[12], &outline[0]] {
        assert!(view.go_to(&entry.pos));
        assert_eq!(caret(&view), entry.pos);
        assert_eq!(view.current_page(), entry.page);
        let (_, y, _, h) = view.caret_rect().unwrap();
        assert!(y >= 0.0 && y + h <= 500.0, "{y}");
    }
    // A place that is not in the document goes nowhere.
    assert!(!view.go_to(&pos(9999, 0)));
}

#[test]
fn a_link_is_found_under_the_pointer_only() {
    let mut linked = paragraph("see the site now");
    let link = Arc::new(Link {
        target: "https://example.com/".into(),
        tooltip: None,
    });
    let mut run = Run::text("the site", look());
    run.link = Some(link);
    linked.runs = vec![Run::text("see ", look()), run, Run::text(" now", look())];
    let view = view_of(doc(vec![Block::paragraph(linked), pb("plain text")]));
    let inside = at(&view, &pos(0, 7));
    let (x, y) = (inside.0 + 1.0, inside.1);
    assert_eq!(view.link_at(x, y).as_deref(), Some("https://example.com/"));
    let (x, y) = at(&view, &pos(0, 1));
    assert_eq!(view.link_at(x + 1.0, y), None);
    let (x, y) = at(&view, &pos(0, 14));
    assert_eq!(view.link_at(x + 1.0, y), None);
    let (x, y) = at(&view, &pos(1, 3));
    assert_eq!(view.link_at(x, y), None);
    // Off the pages.
    assert_eq!(view.link_at(2.0, 2.0), None);
}

#[test]
fn the_caret_area_is_where_the_caret_is() {
    let mut view = view_of(doc(vec![pb("hello")]));
    click(&mut view, &pos(0, 3));
    let (x, y, w, h) = view.ime_cursor_area();
    let (cx, cy) = at(&view, &pos(0, 3));
    assert!((x + w / 2.0 - cx).abs() <= 2.0);
    assert!(cy > y && cy < y + h);
    assert!(!view.is_composing());
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

fn pixel(view: &DocView, x: usize, y: usize) -> [u8; 4] {
    let data = view.pixmap.data_as_u8_slice();
    let at = (y * usize::from(view.width) + x) * 4;
    [data[at], data[at + 1], data[at + 2], data[at + 3]]
}

fn dark_pixels(view: &DocView, rect: Rect) -> usize {
    let mut count = 0;
    for y in rect.y0.max(0.0) as usize..(rect.y1 as usize).min(usize::from(view.height)) {
        for x in rect.x0.max(0.0) as usize..(rect.x1 as usize).min(usize::from(view.width)) {
            let p = pixel(view, x, y);
            if u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) < 300 {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn pages_are_painted_white_with_text_on_the_canvas_colour() {
    let mut view = view_of(lines(30));
    view.set_style(DocStyle {
        background: [10, 20, 30, 255],
        ..DocStyle::default()
    });
    view.render();
    let rect = view.page_rect(0);
    // The canvas is the style's, beside and under the page.
    assert_eq!(pixel(&view, 2, 2), [10, 20, 30, 255]);
    // The page is white where there is no text, with text on it.
    assert_eq!(
        pixel(&view, (rect.x1 - 3.0) as usize, (rect.y1 - 3.0) as usize),
        [255, 255, 255, 255]
    );
    assert!(dark_pixels(&view, rect) > 50);
}

#[test]
fn the_selection_and_the_caret_change_pixels_and_blink() {
    let mut view = view_of(doc(vec![pb("some text to select"), pb("more")]));
    view.set_caret_on(false);
    view.render();
    let page = view.page_rect(0);
    let plain = view.pixmap.data_as_u8_slice().to_vec();
    // Caret on, then off.
    view.set_caret_on(true);
    assert!(view.needs_render());
    view.render();
    let with_caret = view.pixmap.data_as_u8_slice().to_vec();
    view.set_caret_on(false);
    assert!(view.needs_render());
    view.render();
    assert_ne!(with_caret, view.pixmap.data_as_u8_slice());
    assert_eq!(plain, view.pixmap.data_as_u8_slice());
    view.set_caret_on(true);
    drag(&mut view, &pos(0, 5), &pos(0, 9));
    assert!(view.needs_render());
    view.render();
    let selected = view.pixmap.data_as_u8_slice().to_vec();
    assert_ne!(selected, plain);
    // Only the selection's band changed: nothing above the first line.
    let (x, y) = at(&view, &pos(0, 7));
    let at_pixel = (y as usize * 600 + x as usize) * 4;
    assert_ne!(
        selected[at_pixel..at_pixel + 4],
        plain[at_pixel..at_pixel + 4]
    );
    assert!(page.y0 >= 0.0);
    // Nothing to draw: no repaint.
    assert!(!view.needs_render());
    view.render();
    // Find marks change pixels too.
    view.clear_selection();
    view.render();
    let before = view.pixmap.data_as_u8_slice().to_vec();
    view.set_find(query("more"));
    view.render();
    assert_ne!(before, view.pixmap.data_as_u8_slice());
}

#[test]
fn scrolling_reuses_painted_pages() {
    let mut view = view_of(lines(120));
    view.render();
    let first = Arc::as_ptr(&view.shots[&0].pixmap);
    view.scroll_by(30.0);
    view.render();
    assert_eq!(first, Arc::as_ptr(&view.shots[&0].pixmap));
    // Far away, the first pages are let go.
    view.set_scroll(view.page_top(view.page_count() - 1));
    view.render();
    assert!(!view.shots.contains_key(&0));
    // And a budget is kept to.
    view.set_zoom(Zoom::Percent(200.0));
    view.set_pixel_budget(1);
    view.render();
    assert!(view.shots.len() <= 1);
}

#[test]
fn a_page_too_large_to_keep_is_painted_as_a_band() {
    let mut document = doc(vec![pb("Big text at a big zoom"), pb("second")]);
    document.last_section = Arc::new(Section::default());
    let mut view = view_of(document);
    view.set_zoom(Zoom::Percent(800.0));
    assert!(view.is_large(0));
    view.go_to(&pos(0, 0));
    view.render();
    assert!(view.shots.is_empty());
    assert!(view.band.is_some());
    assert!(view.pixel_bytes() < 64 << 20);
    let rect = view.page_rect(0);
    assert!(dark_pixels(&view, rect) > 100);
    // Scrolling a little reuses the band; a long way paints another.
    let band = Arc::as_ptr(&view.band.as_ref().unwrap().pixmap);
    view.scroll_by(40.0);
    view.render();
    assert_eq!(band, Arc::as_ptr(&view.band.as_ref().unwrap().pixmap));
    view.scroll_by(3000.0);
    view.render();
    assert_ne!(band, Arc::as_ptr(&view.band.as_ref().unwrap().pixmap));
}

#[test]
fn a_cropped_page_paints_what_the_whole_page_does_there() {
    let view = view_of(lines(10));
    let page = &view.layout().pages[0];
    let mut painter = DocPainter::new();
    let scale = 3.0;
    let whole = painter.render(page, scale);
    // From whole points, as bands are: 33 by 50 points.
    let (x0, y0) = (99.0_f64, 150.0_f64);
    let (w, h) = (300.0_f64, 200.0_f64);
    let area = [x0, y0, x0 + w, y0 + h].map(|v| v / scale);
    let part = painter.render(&crop_page(page, area), scale);
    assert_eq!(
        (usize::from(part.width()), usize::from(part.height())),
        (300, 200)
    );
    let (a, b) = (whole.data_as_u8_slice(), part.data_as_u8_slice());
    let mut differing = 0;
    for y in 0..200usize {
        for x in 0..300usize {
            let from = ((y + 150) * usize::from(whole.width()) + x + 99) * 4;
            let to = (y * 300 + x) * 4;
            if a[from..from + 4]
                .iter()
                .zip(&b[to..to + 4])
                .any(|(p, q)| p.abs_diff(*q) > 3)
            {
                differing += 1;
            }
        }
    }
    assert!(differing < 60 * 3, "{differing} pixels differ");
}

// ---------------------------------------------------------------------------
// Edge cases and real files
// ---------------------------------------------------------------------------

fn exercise(view: &mut DocView) {
    view.render();
    for movement in [
        Move::Left,
        Move::Right,
        Move::WordLeft,
        Move::WordRight,
        Move::Up,
        Move::Down,
        Move::LineStart,
        Move::LineEnd,
        Move::DocStart,
        Move::DocEnd,
        Move::PageUp,
        Move::PageDown,
    ] {
        view.move_caret(movement, false);
        view.move_caret(movement, true);
        if let Some(caret) = view.caret() {
            assert_valid(view, &caret.clone());
        }
    }
    view.select_all();
    let _ = view.selected_text();
    view.clear_selection();
    for (x, y) in [(10.0, 10.0), (300.0, 100.0), (590.0, 490.0), (-5.0, -5.0)] {
        view.press(x, y, 1, false);
        view.press(x, y, 2, false);
        view.press(x, y, 3, false);
        view.drag(x + 20.0, y + 20.0);
        view.drag_scroll(x, y + 600.0);
        view.release();
        let _ = view.link_at(x, y);
    }
    view.set_find(query("a"));
    view.find_step(true);
    view.find_step(false);
    view.clear_find();
    for zoom in [
        Zoom::FitWidth,
        Zoom::FitPage,
        Zoom::Percent(10.0),
        Zoom::Percent(800.0),
    ] {
        view.set_zoom(zoom);
        view.render();
    }
    view.ime_cursor_area();
    view.current_page();
    view.outline();
    view.text();
}

#[test]
fn an_empty_document_and_one_with_only_a_table_do_not_panic() {
    let mut view = view_of(Document::default());
    assert_eq!(view.page_count(), 1);
    assert!(view.caret().is_none());
    assert_eq!(view.selected_text(), "");
    assert!(!view.select_all());
    assert!(view.outline().is_empty());
    exercise(&mut view);
    let mut view = view_of(doc(Vec::new()));
    exercise(&mut view);
    let mut view = view_of(doc(vec![table(&[
        &["only", "a table"],
        &["with", "cells"],
    ])]));
    exercise(&mut view);
    assert_eq!(view.set_find(query("cells")), 1);
    view.press(
        at(&view, &cell_pos(0, 1, 1, 2)).0,
        at(&view, &cell_pos(0, 1, 1, 2)).1,
        1,
        false,
    );
    assert_eq!(caret(&view), cell_pos(0, 1, 1, 2));
    // A zero-sized window.
    let mut view = DocView::new(lines(3));
    view.render();
    view.resize(0, 0, 1.0);
    exercise(&mut view);
    view.resize(1, 1, 3.0);
    exercise(&mut view);
}

#[test]
fn replacing_the_document_lays_it_out_again() {
    let mut view = view_of(lines(5));
    click(&mut view, &pos(4, 3));
    view.set_find(query("line"));
    view.set_document(lines(60));
    assert!(view.page_count() > 2);
    assert_eq!(caret(&view), pos(4, 3));
    assert_eq!(view.hit_count(), 0);
    view.set_document(lines(2));
    // The caret was past the end: it moves to the start.
    assert_valid(&view, &caret(&view));
    view.set_document(Document::default());
    assert!(view.caret().is_none());
    view.render();
}

#[test]
fn the_sample_document_opens_reads_finds_and_paints() {
    let bytes = docx_sample::sample();
    let mut document = aster_ooxml::docx::read(bytes).expect("a DOCX file");
    document.renumber();
    let mut view = DocView::new(document);
    view.resize(900, 700, 2.0);
    assert!(view.page_count() >= 3);
    assert!(!view.outline().is_empty());
    // The headings are reachable.
    for entry in view.outline().to_vec() {
        assert!(view.go_to(&entry.pos), "{entry:?}");
        assert_valid(&view, &caret(&view));
    }
    view.select_all();
    let all = view.selected_text();
    assert!(all.len() > 200);
    assert!(!all.contains('\u{fffc}'));
    assert_eq!(all.lines().count(), view.document().paragraphs().len());
    assert!(view.set_find(query("the")) > 0);
    // Every page paints something and every point is a valid place.
    for page in 0..view.page_count() {
        view.go_to_page(page);
        view.render();
        let rect = view.page_rect(page);
        let visible = Rect::new(
            rect.x0.max(0.0),
            rect.y0.max(0.0),
            rect.x1.min(900.0 * 2.0),
            rect.y1.min(700.0 * 2.0),
        );
        assert!(dark_pixels(&view, visible) > 20, "page {page} is blank");
    }
    exercise(&mut view);
    for y in (0..700).step_by(23) {
        for x in (0..900).step_by(31) {
            if let Some(found) = view.pos_at(f64::from(x), f64::from(y)) {
                assert_valid(&view, &found);
            }
        }
    }
}

mod edit_tests;
mod object_tests;
