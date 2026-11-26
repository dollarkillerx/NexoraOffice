//! Tests of tables, links and pictures through the view: the commands, undo,
//! the pointer on pictures, and what waits for an input method.

use super::*;
use crate::text_surface::ImeEvent;
use aster_writer::edit::same_content;
use aster_writer::{Drawing, DrawingContent, Float, FloatAt, FloatAxis, FloatFrom, Picture, Wrap};

/// A 1 by 1 pixel PNG file.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The text width of the test pages, twips: 6,000 less two 400 margins.
const TEXT: i32 = 5_200;
/// EMU in a pixel at 96 to the inch, and in a point.
const PX: i64 = 9_525;
const PT: i64 = 12_700;

fn png() -> Arc<[u8]> {
    Arc::from(PNG.to_vec())
}

fn put(view: &mut DocView, at: Pos) {
    view.set_selection(at.clone(), at);
}

fn tbl(view: &DocView, block: usize) -> &Table {
    view.document()
        .block(&Path::body(block))
        .and_then(|b| b.as_table())
        .expect("a table")
}

fn is_table(view: &DocView, block: usize) -> bool {
    view.document()
        .block(&Path::body(block))
        .is_some_and(|b| b.as_table().is_some())
}

fn texts(view: &DocView) -> Vec<String> {
    view.document()
        .paragraphs()
        .iter()
        .map(|(_, p)| p.text())
        .collect()
}

fn grid() -> Document {
    doc(vec![
        pb("top"),
        table(&[&["a1", "a2"], &["b1", "b2"]]),
        pb("end"),
    ])
}

/// The first picture on page 0, in the window's pixels: left, top, right,
/// bottom.
fn picture_box(view: &DocView) -> (f64, f64, f64, f64) {
    let page = &view.layout().pages[0];
    let rect = page
        .items
        .iter()
        .find_map(|item| match item {
            aster_layout::Item::Picture(p) => Some(p.rect),
            _ => None,
        })
        .expect("a picture on the page");
    let origin = view.page_rect(0);
    let ppp = view.pixels_per_point();
    (
        origin.x0 + f64::from(rect.x0) * ppp,
        origin.y0 + f64::from(rect.y0) * ppp,
        origin.x0 + f64::from(rect.x1) * ppp,
        origin.y0 + f64::from(rect.y1) * ppp,
    )
}

fn drawing_size(view: &DocView, block: usize) -> (i64, i64) {
    let p = view.document().paragraph(&Path::body(block)).unwrap();
    p.runs
        .iter()
        .find_map(|r| match &r.kind {
            aster_writer::Inline::Drawing(d) => Some(d.size),
            _ => None,
        })
        .expect("a drawing")
}

/// A view with "a", a 40 by 20 pixel picture and "b" in the first paragraph
/// and the caret away from it.
fn with_picture() -> DocView {
    let mut view = view_of(doc(vec![pb("ab"), pb("cd")]));
    put(&mut view, pos(0, 1));
    assert_eq!(view.insert_picture(png(), (40, 20), None), Ok(true));
    put(&mut view, pos(1, 1));
    view
}

fn click_at(view: &mut DocView, x: f64, y: f64) {
    view.press(x, y, 1, false);
    view.release();
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn a_table_is_made_at_the_caret_as_wide_as_the_text() {
    let mut view = view_of(doc(vec![pb("hello"), pb("tail")]));
    let original = view.document().clone();
    put(&mut view, pos(0, 5));
    assert!(!view.in_table());
    assert!(view.command(DocCommand::InsertTable {
        rows: 3,
        columns: 2
    }));
    assert!(is_table(&view, 1));
    let t = tbl(&view, 1);
    assert_eq!((t.rows.len(), t.columns.len()), (3, 2));
    assert_eq!(t.columns.iter().sum::<i32>(), TEXT);
    assert_eq!(caret(&view), cell_pos(1, 0, 0, 0));
    assert!(view.in_table());
    assert!(view.is_modified());
    // One step.
    assert!(view.undo());
    assert!(same_content(view.document(), &original));
    assert_eq!(caret(&view), pos(0, 5));
    assert!(view.redo());
    assert!(is_table(&view, 1));
    // Nothing for no rows, or no columns.
    assert!(!view.command(DocCommand::InsertTable {
        rows: 0,
        columns: 2
    }));
    assert!(!view.command(DocCommand::InsertTable {
        rows: 2,
        columns: 0
    }));
}

#[test]
fn a_table_in_a_cell_is_as_wide_as_the_cell() {
    let mut view = view_of(grid());
    put(&mut view, cell_pos(1, 0, 1, 1));
    assert!(view.insert_table(2, 2));
    let outer = tbl(&view, 1);
    let inner = outer.rows[0].cells[1]
        .blocks
        .iter()
        .find_map(|b| b.as_table())
        .expect("a nested table");
    let width: i32 = inner.columns.iter().sum();
    assert!(width <= 2_000 && width > 1_000, "{width}");
    assert!(view.in_table());
}

#[test]
fn rows_and_columns_are_added_and_removed_one_step_each() {
    let mut view = view_of(grid());
    let original = view.document().clone();
    put(&mut view, cell_pos(1, 0, 0, 1));
    assert!(view.in_table());

    assert!(view.command(DocCommand::InsertRows { above: false }));
    assert_eq!(tbl(&view, 1).rows.len(), 3);
    assert_eq!(caret(&view), cell_pos(1, 1, 0, 0));
    assert!(view.command(DocCommand::InsertRows { above: true }));
    assert_eq!(tbl(&view, 1).rows.len(), 4);
    assert_eq!(caret(&view), cell_pos(1, 1, 0, 0));
    assert!(view.undo());
    assert_eq!(tbl(&view, 1).rows.len(), 3);
    assert!(view.undo());
    assert_eq!(tbl(&view, 1).rows.len(), 2);
    assert!(same_content(view.document(), &original));
    assert!(view.redo());
    assert!(view.redo());
    assert_eq!(tbl(&view, 1).rows.len(), 4);

    // Delete rows again.
    assert!(view.command(DocCommand::DeleteRows));
    assert_eq!(tbl(&view, 1).rows.len(), 3);
    assert!(view.command(DocCommand::DeleteRows));
    assert_eq!(tbl(&view, 1).rows.len(), 2);
    assert!(view.in_table());

    // A column on the right makes the table wider than the text: the
    // columns are scaled to fit.
    put(&mut view, cell_pos(1, 0, 0, 0));
    assert!(view.command(DocCommand::InsertColumns { left: false }));
    let widths = tbl(&view, 1).columns.clone();
    assert_eq!(widths.len(), 3);
    assert_eq!(widths.iter().sum::<i32>(), TEXT, "{widths:?}");
    assert!(
        widths.iter().all(|w| (w - widths[0]).abs() <= 2),
        "{widths:?}"
    );
    assert_eq!(caret(&view), cell_pos(1, 0, 1, 0));
    assert!(view.command(DocCommand::InsertColumns { left: true }));
    assert_eq!(tbl(&view, 1).columns.len(), 4);
    assert_eq!(tbl(&view, 1).columns.iter().sum::<i32>(), TEXT);
    assert!(view.undo());
    assert_eq!(tbl(&view, 1).columns.len(), 3);
    assert!(view.undo());
    assert_eq!(
        tbl(&view, 1).columns,
        vec![2_000, 2_000],
        "undo gives the widths back"
    );
    assert!(view.redo());
    assert!(view.command(DocCommand::DeleteColumns));
    assert_eq!(tbl(&view, 1).columns.len(), 2);

    // A table that fits is not scaled.
    let mut view = view_of(doc(vec![table(&[&["a"], &["b"]]), pb("x")]));
    put(&mut view, cell_pos(0, 0, 0, 0));
    assert!(view.insert_columns(false));
    assert_eq!(tbl(&view, 0).columns, vec![2_000, 2_000]);
}

#[test]
fn deleting_the_table_and_commands_outside_tables() {
    let mut view = view_of(grid());
    put(&mut view, cell_pos(1, 1, 1, 0));
    assert!(view.command(DocCommand::DeleteTable));
    assert!(!is_table(&view, 1));
    assert!(texts(&view).contains(&"end".to_owned()));
    assert!(!view.in_table());
    assert!(view.undo());
    assert!(is_table(&view, 1));

    // In a paragraph none of them does anything.
    put(&mut view, pos(0, 1));
    let before = view.document().clone();
    for command in [
        DocCommand::InsertRows { above: true },
        DocCommand::InsertRows { above: false },
        DocCommand::InsertColumns { left: true },
        DocCommand::InsertColumns { left: false },
        DocCommand::DeleteRows,
        DocCommand::DeleteColumns,
        DocCommand::DeleteTable,
    ] {
        assert!(!view.command(command.clone()), "{command:?}");
        assert_eq!(*view.document(), before);
    }
    // Selecting a row's cells deletes that row.
    view.set_selection(cell_pos(1, 0, 0, 0), cell_pos(1, 0, 1, 1));
    assert!(view.delete_rows());
    assert_eq!(tbl(&view, 1).rows.len(), 1);
}

#[test]
fn tab_in_the_last_cell_adds_a_row_and_goes_into_it() {
    let mut view = view_of(grid());
    let original = view.document().clone();
    put(&mut view, cell_pos(1, 1, 1, 1));
    assert!(view.tab(false));
    assert_eq!(tbl(&view, 1).rows.len(), 3);
    assert_eq!(caret(&view), cell_pos(1, 2, 0, 0));
    // Typing goes into the new cell; Tab goes on through the new row.
    view.type_text("c1");
    assert!(view.tab(false));
    assert_eq!(caret(&view), cell_pos(1, 2, 1, 0));
    assert!(view.tab(false));
    assert_eq!(tbl(&view, 1).rows.len(), 4);
    assert_eq!(caret(&view), cell_pos(1, 3, 0, 0));
    // Each added row is one step of undo.
    assert!(view.undo());
    assert_eq!(tbl(&view, 1).rows.len(), 3);
    assert_eq!(caret(&view), cell_pos(1, 2, 1, 0));
    // Shift+Tab in the first cell does not add a row.
    put(&mut view, cell_pos(1, 0, 0, 0));
    let rows = tbl(&view, 1).rows.len();
    assert!(!view.tab(true));
    assert_eq!(tbl(&view, 1).rows.len(), rows);
    // Tab from a middle cell only moves.
    assert!(view.tab(false));
    assert_eq!(caret(&view), cell_pos(1, 0, 1, 0));
    assert_eq!(tbl(&view, 1).rows.len(), rows);
    while view.undo() {}
    assert!(same_content(view.document(), &original));
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

fn target_of(view: &DocView, block: usize, offset: usize) -> Option<String> {
    let p = view.document().paragraph(&Path::body(block))?;
    let mut start = 0;
    for run in &p.runs {
        let end = start + run.text.len();
        if offset > start && offset <= end {
            return run.link.as_ref().map(|l| l.target.clone());
        }
        start = end;
    }
    None
}

#[test]
fn a_link_is_set_replaced_and_removed_on_the_selection_or_around_the_caret() {
    let mut view = view_of(doc(vec![pb("see the site now"), pb("plain")]));
    let original = view.document().clone();
    assert_eq!(view.link_info(), LinkInfo::default());
    view.set_selection(pos(0, 4), pos(0, 12));
    assert_eq!(view.link_info().text, "the site");
    assert_eq!(view.link_info().target, None);
    assert!(view.command(DocCommand::SetLink(Some("https://example.com/".into()))));
    assert_eq!(
        target_of(&view, 0, 6).as_deref(),
        Some("https://example.com/")
    );
    assert_eq!(target_of(&view, 0, 2), None);
    assert_eq!(
        view.link_info().target.as_deref(),
        Some("https://example.com/")
    );
    // The caret inside it finds the whole link.
    put(&mut view, pos(0, 7));
    let info = view.link_info();
    assert_eq!(info.text, "the site");
    assert_eq!(info.target.as_deref(), Some("https://example.com/"));
    assert_eq!(
        view.link_at_caret().as_deref(),
        Some("https://example.com/")
    );
    // Replace it, from the caret, then take it off.
    assert!(view.command(DocCommand::SetLink(Some(" https://other.org/ ".into()))));
    assert_eq!(
        target_of(&view, 0, 6).as_deref(),
        Some("https://other.org/")
    );
    assert_eq!(
        target_of(&view, 0, 11).as_deref(),
        Some("https://other.org/")
    );
    assert!(view.command(DocCommand::SetLink(None)));
    assert_eq!(target_of(&view, 0, 6), None);
    assert_eq!(view.link_at_caret(), None);
    assert!(
        !view.command(DocCommand::SetLink(None)),
        "nothing to remove"
    );
    // Each was one step.
    assert!(view.undo() && view.undo() && view.undo());
    assert!(same_content(view.document(), &original));
}

#[test]
fn the_link_dialog_edits_text_and_address_or_types_a_new_link() {
    let mut view = view_of(doc(vec![pb("see the site now"), pb("plain")]));
    view.set_selection(pos(0, 4), pos(0, 12));
    // The same text and a new address: only the link is set.
    assert!(view.command(DocCommand::EditLink {
        text: "the site".into(),
        target: "https://a.org/".into(),
    }));
    assert_eq!(texts(&view)[0], "see the site now");
    assert_eq!(target_of(&view, 0, 6).as_deref(), Some("https://a.org/"));
    // Changed text, caret inside the link: the link's text is replaced.
    put(&mut view, pos(0, 5));
    assert!(view.command(DocCommand::EditLink {
        text: "the page".into(),
        target: "https://b.org/".into(),
    }));
    assert_eq!(texts(&view)[0], "see the page now");
    assert_eq!(target_of(&view, 0, 6).as_deref(), Some("https://b.org/"));
    assert_eq!(target_of(&view, 0, 14), None);
    assert_eq!(caret(&view), pos(0, 12));
    // Empty text keeps what is there; an empty address removes the link.
    put(&mut view, pos(0, 5));
    assert!(view.command(DocCommand::EditLink {
        text: String::new(),
        target: "https://c.org/".into(),
    }));
    assert_eq!(texts(&view)[0], "see the page now");
    assert!(view.command(DocCommand::EditLink {
        text: "the page".into(),
        target: "  ".into(),
    }));
    assert_eq!(target_of(&view, 0, 6), None);
    assert_eq!(texts(&view)[0], "see the page now");
    // At a caret in plain text the text is typed as a link; without text
    // the address is.
    put(&mut view, pos(1, 5));
    assert!(view.command(DocCommand::EditLink {
        text: " docs".into(),
        target: "https://d.org/".into(),
    }));
    assert_eq!(texts(&view)[1], "plain docs");
    assert_eq!(target_of(&view, 1, 8).as_deref(), Some("https://d.org/"));
    assert_eq!(target_of(&view, 1, 3), None);
    assert_eq!(caret(&view), pos(1, 10));
    put(&mut view, pos(1, 0));
    assert!(view.command(DocCommand::EditLink {
        text: String::new(),
        target: "https://e.org/".into(),
    }));
    assert_eq!(texts(&view)[1], "https://e.org/plain docs");
    // Nothing to do without an address at a caret.
    put(&mut view, pos(0, 0));
    assert!(!view.command(DocCommand::EditLink {
        text: "x".into(),
        target: String::new(),
    }));
    // InsertLink replaces the selection with linked text.
    view.set_selection(pos(0, 0), pos(0, 3));
    assert!(view.command(DocCommand::InsertLink {
        text: "look at".into(),
        target: "https://f.org/".into(),
    }));
    assert_eq!(texts(&view)[0], "look at the page now");
    assert_eq!(target_of(&view, 0, 4).as_deref(), Some("https://f.org/"));
    // The pointer finds it too.
    let (x, y) = at(&view, &pos(0, 3));
    assert_eq!(view.link_at(x, y).as_deref(), Some("https://f.org/"));
    // A selection over text with and without a link is not one link.
    view.set_selection(pos(0, 0), pos(1, 5));
    assert_eq!(view.link_info().target, None);
    assert_eq!(view.link_info().text, "");
}

// ---------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------

#[test]
fn a_picture_is_put_in_at_the_caret_at_its_pixel_size_or_the_text_width() {
    let mut view = view_of(doc(vec![pb("ab"), pb("cd")]));
    let original = view.document().clone();
    put(&mut view, pos(0, 1));
    assert_eq!(
        view.insert_picture(png(), (40, 20), Some("a dot".into())),
        Ok(true)
    );
    assert_eq!(texts(&view)[0], "a\u{FFFC}b");
    assert_eq!(drawing_size(&view, 0), (40 * PX, 20 * PX));
    assert_eq!(caret(&view), pos(0, 1 + aster_writer::OBJECT.len()));
    assert!(view.is_modified());
    assert!(view.undo());
    assert!(same_content(view.document(), &original));
    assert_eq!(caret(&view), pos(0, 1));

    // Too wide: scaled to the text width, keeping its proportions.
    put(&mut view, pos(1, 0));
    assert_eq!(view.insert_picture(png(), (1000, 500), None), Ok(true));
    let (w, h) = drawing_size(&view, 1);
    assert_eq!(w, i64::from(TEXT) * 635);
    assert!((h * 2 - w).abs() <= 2, "{w} by {h}");

    // A selection is replaced.
    view.set_selection(pos(0, 0), pos(0, 2));
    assert_eq!(view.insert_picture(png(), (10, 10), None), Ok(true));
    assert_eq!(texts(&view)[0], "\u{FFFC}");

    // Not a picture, no size: nothing changes.
    let before = view.document().clone();
    assert_eq!(
        view.insert_picture(Arc::from(b"not a picture".to_vec()), (10, 10), None),
        Err(aster_writer::edit::PictureError::UnknownFormat)
    );
    assert_eq!(
        view.insert_picture(png(), (0, 10), None),
        Err(aster_writer::edit::PictureError::BadSize)
    );
    assert_eq!(*view.document(), before);
}

#[test]
fn a_click_selects_a_picture_and_delete_backspace_and_typing_act_on_it() {
    let mut view = with_picture();
    assert_eq!(view.selected_object(), None);
    let (x0, y0, x1, y1) = picture_box(&view);
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let object = view.selected_object().expect("the picture is selected");
    assert_eq!(object.pos, pos(0, 1));
    assert!(object.inline);
    assert_eq!(object.size, (40 * PX, 20 * PX));
    assert_eq!(
        view.selection().map(|(a, b)| (a.clone(), b.clone())),
        Some((pos(0, 1), pos(0, 1 + aster_writer::OBJECT.len())))
    );
    // Delete, Backspace and typing take it out.
    assert!(view.delete_forward(Unit::Cluster));
    assert_eq!(texts(&view)[0], "ab");
    assert_eq!(view.selected_object(), None);
    assert!(view.undo());
    assert_eq!(view.selected_object().map(|o| o.pos), Some(pos(0, 1)));
    assert!(view.backspace(Unit::Cluster));
    assert_eq!(texts(&view)[0], "ab");
    assert!(view.undo());
    assert!(view.type_text("X"));
    assert_eq!(texts(&view)[0], "aXb");
    assert!(view.undo());
    // An arrow key collapses to one side of it.
    assert!(view.move_caret(Move::Right, false));
    assert_eq!(view.selected_object(), None);
    assert_eq!(caret(&view), pos(0, 1 + aster_writer::OBJECT.len()));
    // A click beside it is a click in the text.
    click_at(&mut view, x0 - 8.0, (y0 + y1) / 2.0);
    assert_eq!(view.selected_object(), None);
    // The picture shows its frame once selected.
    view.render();
    let plain: Vec<[u8; 4]> = (y0 as usize..y1 as usize)
        .map(|y| pixel(&view, x0 as usize, y))
        .collect();
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    view.render();
    let framed: Vec<[u8; 4]> = (y0 as usize..y1 as usize)
        .map(|y| pixel(&view, x0 as usize, y))
        .collect();
    assert_ne!(plain, framed, "the frame is drawn");
}

#[test]
fn dragging_a_corner_resizes_the_picture_in_proportion_as_one_step() {
    let mut view = with_picture();
    let (x0, y0, x1, y1) = picture_box(&view);
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    assert_eq!(
        view.handle_at(x1, y1),
        Some(Handle::Corner(Corner::BottomRight))
    );
    assert_eq!(
        view.handle_at(x0, y0),
        Some(Handle::Corner(Corner::TopLeft))
    );
    // Over the picture itself it is moved by dragging.
    assert_eq!(
        view.handle_at((x0 + x1) / 2.0, (y0 + y1) / 2.0),
        Some(Handle::Move)
    );
    assert_eq!(view.handle_at(x1 + 40.0, y1 + 40.0), None);
    let start = (40 * PX, 20 * PX);

    // Drag the bottom right corner out by half the size.
    assert!(!view.press(x1, y1, 1, false));
    assert!(view.is_resizing());
    assert!(view.drag(x1 + (x1 - x0) / 2.0, y1 + (y1 - y0) / 2.0));
    let during = drawing_size(&view, 0);
    assert!(during.0 > start.0 && during.1 > start.1);
    // Typing and undo wait for the button.
    assert!(!view.type_text("x"));
    assert!(!view.undo());
    assert!(view.release());
    assert!(!view.is_resizing());
    let (w, h) = drawing_size(&view, 0);
    assert!((w - start.0 * 3 / 2).abs() <= 2 * PT, "{w}");
    assert!(
        (w * start.1 - h * start.0).abs() <= start.0 + start.1,
        "{w} by {h}"
    );
    // The top left corner of the picture did not move.
    let (nx0, ny0, _, _) = picture_box(&view);
    assert!((nx0 - x0).abs() <= 1.0 && (ny0 - y0).abs() <= 1.0);
    // One step: one undo gives the old size, and redo the new.
    assert!(view.undo());
    assert_eq!(drawing_size(&view, 0), start);
    assert!(view.redo());
    assert_eq!(drawing_size(&view, 0), (w, h));

    // The top left corner dragged in makes it smaller by the same measure;
    // the line puts a picture where it was, so its top left stays.
    let (x0, y0, x1, y1) = picture_box(&view);
    view.press(x0, y0, 1, false);
    assert!(view.drag(x0 + (x1 - x0) / 2.0, y0 + (y1 - y0) / 2.0));
    view.release();
    let (sw, sh) = drawing_size(&view, 0);
    assert!(sw < w && sh < h);
    assert!((sw * 2 - w).abs() <= 2 * PT, "{sw} of {w}");
    let (nx0, ny0, _, _) = picture_box(&view);
    assert!((nx0 - x0).abs() <= 1.0 && (ny0 - y0).abs() <= 1.0);

    // Never smaller than a few points, even dragged past the other corner.
    let (x0, y0, x1, y1) = picture_box(&view);
    view.press(x1, y1, 1, false);
    view.drag(x0 - 50.0, y0 - 50.0);
    view.release();
    let (w, h) = drawing_size(&view, 0);
    assert!(w >= 8 * PT - 1 && h >= 8 * PT - 1, "{w} by {h}");
}

#[test]
fn a_press_and_release_on_a_handle_without_moving_is_no_step() {
    let mut view = with_picture();
    let (x0, y0, x1, y1) = picture_box(&view);
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    view.press(x1, y1, 1, false);
    view.release();
    assert_eq!(drawing_size(&view, 0), (40 * PX, 20 * PX));
    // The first undo takes out the picture itself.
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "ab");
}

#[test]
fn a_floating_picture_is_selected_and_deleted_and_has_handles_too() {
    let drawing = Drawing {
        size: (40 * PT, 30 * PT),
        content: DrawingContent::Picture(Picture {
            image: png(),
            crop: [0.0; 4],
        }),
        float: Some(Float {
            x: FloatAxis {
                from: FloatFrom::Page,
                at: FloatAt::Offset(150 * PT),
            },
            y: FloatAxis {
                from: FloatFrom::Page,
                at: FloatAt::Offset(100 * PT),
            },
            wrap: Wrap::None,
            behind: false,
            distance: [0; 4],
        }),
        alt: None,
    };
    let mut p = paragraph("ab");
    p.runs.insert(
        1,
        aster_writer::Run::object(aster_writer::Inline::Drawing(Arc::new(drawing)), look()),
    );
    let mut view = view_of(doc(vec![Block::paragraph(p), pb("cd")]));
    let (x0, y0, x1, y1) = picture_box(&view);
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let object = view.selected_object().expect("selected");
    assert!(!object.inline);
    assert_eq!(
        view.handle_at(x1, y1),
        Some(Handle::Corner(Corner::BottomRight))
    );
    // Pressing on a corner starts a resize, and letting go without moving
    // changes nothing.
    view.press(x1, y1, 1, false);
    assert!(view.is_resizing());
    assert!(view.release());
    assert_eq!(object.size, view.selected_object().unwrap().size);
    click_at(&mut view, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    assert!(view.delete_forward(Unit::Cluster));
    assert!(!texts(&view)[0].contains('\u{FFFC}'));
}

#[test]
fn tables_links_and_pictures_wait_for_an_input_method() {
    let mut document = grid();
    {
        let mut view = view_of(document.clone());
        put(&mut view, pos(0, 1));
        view.insert_picture(png(), (20, 20), None).unwrap();
        document = view.document().clone();
    }
    let mut view = view_of(document);
    let (x0, y0, x1, y1) = picture_box(&view);
    let picture = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    view.set_selection(cell_pos(1, 1, 1, 0), cell_pos(1, 1, 1, 2));
    view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    });
    assert!(view.is_composing());
    let before = view.document().clone();
    let selection = view.selection().map(|(a, b)| (a.clone(), b.clone()));
    let commands = [
        DocCommand::InsertTable {
            rows: 2,
            columns: 2,
        },
        DocCommand::InsertRows { above: true },
        DocCommand::InsertRows { above: false },
        DocCommand::InsertColumns { left: true },
        DocCommand::InsertColumns { left: false },
        DocCommand::DeleteRows,
        DocCommand::DeleteColumns,
        DocCommand::DeleteTable,
        DocCommand::SetLink(Some("https://a.org/".into())),
        DocCommand::SetLink(None),
        DocCommand::EditLink {
            text: "x".into(),
            target: "https://a.org/".into(),
        },
        DocCommand::InsertLink {
            text: "x".into(),
            target: "https://a.org/".into(),
        },
    ];
    for command in commands {
        assert!(!view.command(command.clone()), "{command:?}");
    }
    assert_eq!(view.insert_picture(png(), (5, 5), None), Ok(false));
    assert!(!view.tab(false));
    assert!(!view.press(picture.0, picture.1, 1, false));
    assert!(!view.drag(picture.0 + 20.0, picture.1 + 20.0));
    assert!(!view.release());
    assert!(!view.is_resizing());
    assert_eq!(view.selected_object(), None);
    assert!(view.is_composing());
    assert_eq!(*view.document(), before);
    assert_eq!(
        view.selection().map(|(a, b)| (a.clone(), b.clone())),
        selection
    );
    // After the commit they work again.
    assert!(view.ime(ImeEvent::Commit("你".into())));
    assert!(view.command(DocCommand::InsertRows { above: false }));
}

// ---------------------------------------------------------------------------
// The modified marker
// ---------------------------------------------------------------------------

#[test]
fn the_view_is_modified_only_while_it_differs_from_what_was_saved() {
    let mut view = view_of(lines(3));
    view.type_text("a");
    view.seal_history();
    assert!(view.is_modified());
    // A save: what was saved is this document; the history stays.
    view.mark_saved_as(view.document().clone(), view.revision());
    assert!(!view.is_modified());
    assert!(view.can_undo());
    view.type_text("b");
    assert!(view.is_modified());
    // Undoing back to the saved document clears the mark; further undo and
    // redo set it and clear it.
    assert!(view.undo());
    assert!(!view.is_modified());
    assert!(view.undo());
    assert!(view.is_modified());
    assert!(view.redo());
    assert!(!view.is_modified());
    assert!(view.redo());
    assert!(view.is_modified());

    // Saved while another edit was made: still modified, and clean again
    // when that edit is undone.
    let mut view = view_of(lines(3));
    view.type_text("a");
    let (saved, revision) = (view.document().clone(), view.revision());
    view.seal_history();
    view.type_text("b");
    view.mark_saved_as(saved, revision);
    assert!(view.is_modified());
    assert!(view.undo());
    assert!(!view.is_modified());

    // Typing and then deleting what was typed is the saved document too.
    let mut view = view_of(lines(3));
    view.mark_saved();
    view.type_text("z");
    assert!(view.is_modified());
    view.backspace(Unit::Cluster);
    assert!(!view.is_modified());
    // Not known: the document came back from a recovery record.
    view.mark_unsaved();
    assert!(view.is_modified());
    view.mark_saved();
    assert!(!view.is_modified());
}
