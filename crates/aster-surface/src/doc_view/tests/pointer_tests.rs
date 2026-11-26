//! Tests of what the pointer and the keys do to tables and pictures: cells
//! selected by dragging, merged and split, the lines of a table dragged,
//! pictures moved, resized from any corner and nudged, in the body, in
//! headers and in notes; and that all of it waits for an input method.

use super::*;
use crate::doc_view::objects::NUDGE;
use crate::text_surface::ImeEvent;
use aster_writer::edit::same_content;
use aster_writer::{
    Drawing, DrawingContent, Float, FloatAt, FloatAxis, FloatFrom, Inline, Note, Notes, Picture,
    RowHeight, StoryId, VMerge, Wrap,
};

/// A 1 by 1 pixel PNG file.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
const PT: i64 = 12_700;

fn image() -> Arc<[u8]> {
    Arc::from(PNG.to_vec())
}

fn put(view: &mut DocView, at: Pos) {
    view.set_selection(at.clone(), at);
}

/// A table of columns 75 points wide.
fn grid_of(rows: &[&[&str]]) -> Block {
    let columns = vec![1_500; rows.iter().map(|r| r.len()).max().unwrap_or(1)];
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

fn table_at(view: &DocView, block: usize) -> &Table {
    view.document()
        .block(&Path::body(block))
        .and_then(|b| b.as_table())
        .expect("a table")
}

fn picture(width: i64, height: i64, float: Option<Float>) -> Arc<Drawing> {
    Arc::new(Drawing {
        size: (width * PT, height * PT),
        content: DrawingContent::Picture(Picture {
            image: image(),
            crop: [0.0; 4],
        }),
        float,
        alt: None,
    })
}

fn float_at(x: i64, y: i64, from: FloatFrom) -> Float {
    Float {
        x: FloatAxis {
            from,
            at: FloatAt::Offset(x * PT),
        },
        y: FloatAxis {
            from,
            at: FloatAt::Offset(y * PT),
        },
        wrap: Wrap::None,
        behind: false,
        distance: [0; 4],
    }
}

/// A paragraph with a picture after "ab".
fn with_drawing(drawing: Arc<Drawing>) -> Block {
    let mut p = paragraph("ab");
    p.runs.push(Run::object(Inline::Drawing(drawing), look()));
    p.runs.push(Run::text("cd", look()));
    Block::paragraph(p)
}

/// The first picture on a page, in the window's pixels: left, top, right,
/// bottom.
fn picture_box(view: &DocView, page: usize) -> (f64, f64, f64, f64) {
    let rect = view.layout().pages[page]
        .items
        .iter()
        .find_map(|item| match item {
            aster_layout::Item::Picture(p) => Some(p.rect),
            _ => None,
        })
        .expect("a picture on the page");
    let origin = view.page_rect(page);
    let ppp = view.pixels_per_point();
    (
        origin.x0 + f64::from(rect.x0) * ppp,
        origin.y0 + f64::from(rect.y0) * ppp,
        origin.x0 + f64::from(rect.x1) * ppp,
        origin.y0 + f64::from(rect.y1) * ppp,
    )
}

fn middle(b: (f64, f64, f64, f64)) -> (f64, f64) {
    ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0)
}

fn click_at(view: &mut DocView, x: f64, y: f64) {
    view.press(x, y, 1, false);
    view.release();
}

fn drawing_of(view: &DocView, path: &Path) -> Drawing {
    view.document()
        .paragraph(path)
        .unwrap()
        .runs
        .iter()
        .find_map(|r| match &r.kind {
            Inline::Drawing(d) => Some((**d).clone()),
            _ => None,
        })
        .expect("a drawing")
}

fn offsets(float: &Float) -> (i64, i64) {
    let at = |axis: &FloatAxis| match axis.at {
        FloatAt::Offset(v) => v,
        other => panic!("{other:?}"),
    };
    (at(&float.x), at(&float.y))
}

fn sel_ends(view: &DocView) -> Option<(Pos, Pos)> {
    view.selection().map(|(a, b)| (a.clone(), b.clone()))
}

/// Where a place in a table cell is in the window, a little to the right.
fn in_cell(view: &DocView, row: usize, column: usize) -> (f64, f64) {
    let (x, y) = at(view, &cell_pos(0, row, column, 0));
    (x + 1.0, y)
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

#[test]
fn dragging_across_cells_selects_the_block_drawn_as_whole_cells_and_merges_it() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b", "c"], &["d", "e", "f"]])]));
    let start = (in_cell(&view, 0, 0), in_cell(&view, 1, 1));
    view.press(start.0.0, start.0.1, 1, false);
    // Still in one cell: text.
    assert!(!view.can_merge_cells());
    view.drag(start.1.0, start.1.1);
    view.release();
    assert!(view.can_merge_cells());

    // Drawn as the whole block: one rectangle as wide as two columns.
    let rects = view.selection_rects().to_vec();
    assert_eq!(rects.len(), 1, "{rects:?}");
    let grid = view.layout().table_grids(0).next().unwrap().clone();
    let (page, rect) = rects[0];
    assert_eq!(page, 0);
    assert_eq!((rect.x0, rect.x1), (grid.columns[0], grid.columns[2]));
    assert_eq!((rect.y0, rect.y1), (grid.rows[0].top, grid.rows[1].bottom));

    // The merge takes exactly that block, as one step.
    assert!(view.merge_cells());
    let t = table_at(&view, 0);
    assert_eq!(t.rows[0].cells.len(), 2);
    assert_eq!(t.rows[0].cells[0].span, 2);
    assert_eq!(t.rows[0].cells[0].merge, VMerge::Start);
    assert_eq!(t.rows[1].cells[0].merge, VMerge::Continue);
    assert_eq!(caret(&view), cell_pos(0, 0, 0, 0));
    assert!(!view.can_merge_cells());
    assert!(view.undo());
    assert_eq!(table_at(&view, 0).rows[0].cells.len(), 3);
    assert!(!view.can_undo());
}

#[test]
fn dragging_from_the_far_corner_selects_the_same_block() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b", "c"], &["d", "e", "f"]])]));
    let (from, to) = (in_cell(&view, 1, 2), in_cell(&view, 0, 1));
    view.press(from.0, from.1, 1, false);
    view.drag(to.0, to.1);
    view.release();
    let rects = view.selection_rects().to_vec();
    let grid = view.layout().table_grids(0).next().unwrap().clone();
    assert_eq!(rects.len(), 1);
    assert_eq!(
        (rects[0].1.x0, rects[0].1.x1),
        (grid.columns[1], grid.columns[3])
    );
    assert!(view.merge_cells());
    assert_eq!(table_at(&view, 0).rows[0].cells[1].span, 2);
}

#[test]
fn a_selection_inside_one_cell_is_text() {
    let mut view = view_of(doc(vec![grid_of(&[&["abc", "b"]])]));
    view.set_selection(cell_pos(0, 0, 0, 0), cell_pos(0, 0, 0, 2));
    let rects = view.selection_rects().to_vec();
    assert!(!rects.is_empty());
    let grid = view.layout().table_grids(0).next().unwrap().clone();
    // Only the text, a line of it: narrower than the cell.
    assert!(rects[0].1.x1 < grid.columns[1] - 5.0, "{rects:?}");
    assert!(!view.can_merge_cells());
}

#[test]
fn cells_are_split_and_unmerged_one_step_each() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"], &["c", "d"]]), pb("after")]));
    put(&mut view, cell_pos(0, 0, 0, 0));
    assert!(view.can_split_cell(2, 1));
    assert!(!view.can_split_cell(0, 1));
    assert!(!view.can_unmerge_cell());
    assert!(view.command(DocCommand::SplitCell {
        columns: 2,
        rows: 1,
    }));
    assert_eq!(table_at(&view, 0).rows[0].cells.len(), 3);
    assert_eq!(table_at(&view, 0).columns.len(), 3);
    assert!(view.undo());
    assert_eq!(table_at(&view, 0).rows[0].cells.len(), 2);

    // A merged cell can be taken apart again.
    view.set_selection(cell_pos(0, 0, 0, 0), cell_pos(0, 1, 1, 0));
    assert!(view.command(DocCommand::MergeCells));
    assert!(view.can_unmerge_cell());
    assert!(!view.can_split_cell(2, 1), "a merged cell is not split");
    let merged = view.document().clone();
    assert!(view.command(DocCommand::UnmergeCell));
    assert_eq!(table_at(&view, 0).rows[0].cells.len(), 2);
    assert_eq!(table_at(&view, 0).rows[1].cells.len(), 2);
    assert!(view.undo());
    assert!(same_content(view.document(), &merged));
    // Outside tables there is nothing to do.
    put(&mut view, pos(1, 0));
    assert!(!view.can_unmerge_cell() && !view.can_merge_cells());
}

#[test]
fn cells_merge_in_a_header_too() {
    let mut document = doc(vec![pb("body")]);
    document.marginals = vec![Arc::new(Story {
        blocks: vec![grid_of(&[&["h1", "h2"]]), pb("")],
        part: None,
    })];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    let mut view = view_of(document);
    assert!(view.enter_header());
    let cell = |c: usize| {
        Pos::new(
            Path::in_story(StoryId::Marginal(0), 0).cell_block(0, c, 0),
            0,
        )
    };
    view.set_selection(cell(0), cell(1));
    assert!(view.can_merge_cells());
    assert!(view.merge_cells());
    let merged = view
        .document()
        .block(&Path::in_story(StoryId::Marginal(0), 0))
        .and_then(|b| b.as_table())
        .unwrap();
    assert_eq!(merged.rows[0].cells.len(), 1);
    assert_eq!(merged.rows[0].cells[0].span, 2);
    assert!(view.undo());
}

#[test]
fn cell_commands_wait_for_an_input_method() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"], &["c", "d"]])]));
    view.set_selection(cell_pos(0, 0, 0, 0), cell_pos(0, 0, 1, 0));
    assert!(view.can_merge_cells());
    view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    });
    assert!(view.is_composing());
    let before = view.document().clone();
    assert!(!view.can_merge_cells() && !view.can_split_cell(2, 1) && !view.can_unmerge_cell());
    assert!(!view.merge_cells() && !view.split_cell(2, 1) && !view.unmerge_cell());
    for command in [
        DocCommand::MergeCells,
        DocCommand::SplitCell {
            columns: 2,
            rows: 1,
        },
        DocCommand::UnmergeCell,
        DocCommand::PictureLayout(PictureLayout::Square),
    ] {
        assert!(!view.command(command.clone()), "{command:?}");
    }
    assert_eq!(*view.document(), before);
    assert!(view.ime(ImeEvent::Commit("x".into())));
    assert!(view.can_merge_cells() || view.document() != &before);
}

// ---------------------------------------------------------------------------
// Lines of tables
// ---------------------------------------------------------------------------

/// A point of the window on the line between columns `border - 1` and
/// `border` of the first table, in the middle of its first row.
fn column_line(view: &DocView, border: usize) -> (f64, f64) {
    let grid = view.layout().table_grids(0).next().unwrap();
    let rect = view.page_rect(0);
    let ppp = view.pixels_per_point();
    (
        rect.x0 + f64::from(grid.columns[border]) * ppp,
        rect.y0 + f64::from((grid.rows[0].top + grid.rows[0].bottom) / 2.0) * ppp,
    )
}

fn row_line(view: &DocView, row: usize) -> (f64, f64) {
    let grid = view.layout().table_grids(0).next().unwrap();
    let rect = view.page_rect(0);
    let ppp = view.pixels_per_point();
    (
        rect.x0 + f64::from((grid.columns[0] + grid.columns[1]) / 2.0) * ppp,
        rect.y0 + f64::from(grid.rows[row].bottom) * ppp,
    )
}

#[test]
fn a_column_line_is_dragged_with_a_guide_and_one_step_on_release() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"], &["c", "d"]]), pb("after")]));
    put(&mut view, cell_pos(0, 0, 0, 0));
    let before = view.document().clone();
    let selection = sel_ends(&view);
    let scroll = view.scroll_range();
    let (x, y) = column_line(&view, 1);
    assert_eq!(view.handle_at(x + 1.0, y), Some(Handle::ColumnBorder));
    assert_eq!(view.handle_at(x + 12.0, y), None);

    assert!(!view.press(x, y, 1, false));
    assert!(view.is_dragging());
    assert!(
        view.handle_at(x + 30.0, y + 400.0).is_some(),
        "the drag keeps its cursor"
    );
    assert!(view.drag(x + 30.0, y));
    // Only a guide while dragging.
    assert_eq!(*view.document(), before);
    view.render();
    let guide = pixel(&view, (x + 30.0) as usize, y as usize);
    assert_eq!(guide, DocStyle::default().object);
    assert!(!view.type_text("x"), "typing waits for the button");
    assert!(view.release());
    assert!(!view.is_dragging());

    // At 100 % a pixel is a point: 30 pixels are 600 twips.
    assert_eq!(table_at(&view, 0).columns, vec![2_100, 900]);
    assert_eq!(sel_ends(&view), selection);
    assert_eq!(view.scroll_range(), scroll);
    // One step.
    assert!(view.undo());
    assert!(same_content(view.document(), &before));
    assert!(!view.can_undo());
    assert!(view.redo());
    assert_eq!(table_at(&view, 0).columns, vec![2_100, 900]);
}

#[test]
fn a_line_is_not_moved_further_than_the_columns_allow_and_a_click_is_no_step() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"]])]));
    let (x, y) = column_line(&view, 1);
    // Pressed and let go in the same place.
    view.press(x, y, 1, false);
    assert!(!view.release());
    assert!(!view.can_undo());
    // Dragged far past the next column: held at its minimum.
    view.press(x, y, 1, false);
    view.drag(x + 500.0, y);
    assert!(view.release());
    let columns = table_at(&view, 0).columns.clone();
    assert_eq!(columns.iter().sum::<i32>(), 3_000);
    assert_eq!(columns[1], aster_writer::edit::MIN_COLUMN);
}

#[test]
fn the_bottom_of_a_row_is_dragged_to_an_at_least_height() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"], &["c", "d"]])]));
    let (x, y) = row_line(&view, 0);
    assert_eq!(view.handle_at(x, y - 1.0), Some(Handle::RowBorder));
    let height = view.layout().table_grids(0).next().unwrap().rows[0].height;
    let before = view.document().clone();
    view.press(x, y, 1, false);
    assert!(view.drag(x, y + 30.0));
    assert_eq!(*view.document(), before);
    assert!(view.release());
    let now = (f64::from(height) * 20.0).round() as i32 + 600;
    assert_eq!(table_at(&view, 0).rows[0].height, RowHeight::AtLeast(now));
    assert_eq!(table_at(&view, 0).rows[1].height, RowHeight::Auto);
    let grown = view.layout().table_grids(0).next().unwrap().rows[0].height;
    assert!((grown - height - 30.0).abs() < 0.2, "{height} {grown}");
    assert!(view.undo());
    assert!(same_content(view.document(), &before));
}

#[test]
fn an_exact_row_stays_exact_when_dragged() {
    let mut document = doc(vec![grid_of(&[&["a"], &["b"]])]);
    if let Some(Block::Table(t)) = document.body.blocks.get_mut(0) {
        Arc::make_mut(t).rows[0].height = RowHeight::Exactly(400);
    }
    let mut view = view_of(document);
    let (x, y) = row_line(&view, 0);
    view.press(x, y, 1, false);
    view.drag(x, y + 10.0);
    assert!(view.release());
    assert_eq!(table_at(&view, 0).rows[0].height, RowHeight::Exactly(600));
}

#[test]
fn lines_hidden_by_merged_cells_are_not_handles() {
    let mut view = view_of(doc(vec![grid_of(&[&["a", "b"], &["c", "d"]])]));
    view.set_selection(cell_pos(0, 0, 0, 0), cell_pos(0, 0, 1, 0));
    assert!(view.merge_cells());
    // In the first row, the middle line is under the merged cell.
    let (x, y) = column_line(&view, 1);
    assert_eq!(view.handle_at(x, y), None);
    let grid = view.layout().table_grids(0).next().unwrap().clone();
    let rect = view.page_rect(0);
    let second = rect.y0
        + f64::from((grid.rows[1].top + grid.rows[1].bottom) / 2.0) * view.pixels_per_point();
    assert_eq!(view.handle_at(x, second), Some(Handle::ColumnBorder));
}

#[test]
fn the_lines_of_a_header_table_are_dragged_from_inside_the_header_only() {
    let mut document = doc(vec![grid_of(&[&["a", "b"]])]);
    document.marginals = vec![Arc::new(Story {
        blocks: vec![grid_of(&[&["h1", "h2"]]), pb("")],
        part: None,
    })];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    let mut view = view_of(document);
    let header_table = Path::in_story(StoryId::Marginal(0), 0);
    let line = |view: &DocView, table: &Path| {
        let grid = view
            .layout()
            .table_grids(0)
            .find(|g| g.table == *table)
            .unwrap();
        let rect = view.page_rect(0);
        let ppp = view.pixels_per_point();
        (
            rect.x0 + f64::from(grid.columns[1]) * ppp,
            rect.y0 + f64::from((grid.rows[0].top + grid.rows[0].bottom) / 2.0) * ppp,
        )
    };
    let (hx, hy) = line(&view, &header_table);
    let (bx, by) = line(&view, &Path::body(0));
    // From the body the header's line is not there, the body's is.
    assert_eq!(view.handle_at(hx, hy), None);
    assert_eq!(view.handle_at(bx, by), Some(Handle::ColumnBorder));
    assert!(view.enter_header());
    assert_eq!(view.handle_at(hx, hy), Some(Handle::ColumnBorder));
    assert_eq!(view.handle_at(bx, by), None);
    view.press(hx, hy, 1, false);
    view.drag(hx + 20.0, hy);
    assert!(view.release());
    let t = view
        .document()
        .block(&header_table)
        .and_then(|b| b.as_table())
        .unwrap();
    assert_eq!(t.columns, vec![1_900, 1_100]);
    assert!(view.in_marginal());
}

// ---------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------

fn floating_view() -> DocView {
    view_of(doc(vec![
        with_drawing(picture(40, 30, Some(float_at(150, 100, FloatFrom::Page)))),
        pb("next"),
    ]))
}

#[test]
fn a_floating_picture_is_dragged_to_a_new_place_as_one_step() {
    let mut view = floating_view();
    let path = Path::body(0);
    let start = picture_box(&view, 0);
    // One gesture: press selects it, the drag moves it.
    let (x, y) = middle(start);
    view.press(x, y, 1, false);
    assert!(view.selected_object().is_some());
    assert!(view.drag(x + 40.0, y + 20.0));
    assert!(view.is_dragging());
    assert_eq!(view.handle_at(x + 40.0, y + 20.0), Some(Handle::Move));
    // Nothing is changed while it is only a ghost.
    assert_eq!(
        offsets(&drawing_of(&view, &path).float.unwrap()),
        (150 * PT, 100 * PT)
    );
    view.render();
    assert!(!view.type_text("x"));
    assert!(view.release());
    assert_eq!(
        offsets(&drawing_of(&view, &path).float.unwrap()),
        (190 * PT, 120 * PT)
    );
    let end = picture_box(&view, 0);
    assert!((end.0 - start.0 - 40.0).abs() < 1.0 && (end.1 - start.1 - 20.0).abs() < 1.0);
    // Still selected; one undo takes it back.
    assert!(view.selected_object().is_some());
    assert!(view.undo());
    assert_eq!(
        offsets(&drawing_of(&view, &path).float.unwrap()),
        (150 * PT, 100 * PT)
    );
    assert!(view.selected_object().is_some());
    assert!(!view.can_undo());
}

#[test]
fn a_small_movement_on_a_picture_is_a_click() {
    let mut view = floating_view();
    let (x, y) = middle(picture_box(&view, 0));
    let before = view.document().clone();
    view.press(x, y, 1, false);
    assert!(!view.drag(x + 2.0, y + 1.0));
    assert!(!view.release());
    assert_eq!(*view.document(), before);
    assert!(!view.can_undo());
    assert!(view.selected_object().is_some());
}

#[test]
fn a_picture_in_the_line_is_moved_to_where_the_caret_shows() {
    let mut view = view_of(doc(vec![
        with_drawing(picture(40, 20, None)),
        pb("second"),
        pb("third"),
    ]));
    let (x, y) = middle(picture_box(&view, 0));
    view.press(x, y, 1, false);
    let (tx, ty) = at(&view, &pos(1, 3));
    assert!(view.drag(tx, ty));
    let target = view.moving.as_ref().and_then(|m| m.target.clone());
    assert_eq!(target, Some(pos(1, 3)));
    // A caret is drawn there.
    view.render();
    assert!(view.release());
    let texts: Vec<String> = view
        .document()
        .paragraphs()
        .iter()
        .map(|(_, p)| p.text().replace('\u{FFFC}', "#"))
        .collect();
    assert_eq!(texts, ["abcd", "sec#ond", "third"]);
    let object = view.selected_object().expect("still selected");
    assert_eq!(object.pos, pos(1, 3));
    assert!(view.undo());
    assert_eq!(
        view.document().paragraph(&Path::body(0)).unwrap().text(),
        "ab\u{FFFC}cd"
    );
    assert!(!view.can_undo());
}

#[test]
fn dropping_a_picture_in_the_line_where_it_is_changes_nothing() {
    let mut view = view_of(doc(vec![with_drawing(picture(40, 20, None)), pb("second")]));
    let (x, y) = middle(picture_box(&view, 0));
    let before = view.document().clone();
    let revision = view.revision();
    view.press(x, y, 1, false);
    // Next to itself, on the same line.
    let (ax, ay) = at(&view, &pos(0, 2 + '\u{FFFC}'.len_utf8()));
    assert!(view.drag(ax, ay));
    assert!(!view.release());
    assert_eq!(*view.document(), before);
    assert_eq!(view.revision(), revision);
}

#[test]
fn a_floating_picture_resizes_from_any_corner_keeping_the_opposite_one() {
    for corner in [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ] {
        let mut view = floating_view();
        let start = picture_box(&view, 0);
        let (x, y) = middle(start);
        click_at(&mut view, x, y);
        let (hx, hy) = match corner {
            Corner::TopLeft => (start.0, start.1),
            Corner::TopRight => (start.2, start.1),
            Corner::BottomRight => (start.2, start.3),
            Corner::BottomLeft => (start.0, start.3),
        };
        assert_eq!(view.handle_at(hx, hy), Some(Handle::Corner(corner)));
        // Pull the corner in towards the middle.
        view.press(hx, hy, 1, false);
        assert!(view.is_resizing());
        view.drag(hx + (x - hx) / 2.0, hy + (y - hy) / 2.0);
        assert!(view.release());
        let end = picture_box(&view, 0);
        assert!(end.2 - end.0 < start.2 - start.0 - 5.0, "{corner:?}");
        let fixed = |b: (f64, f64, f64, f64)| match corner {
            Corner::TopLeft => (b.2, b.3),
            Corner::TopRight => (b.0, b.3),
            Corner::BottomRight => (b.0, b.1),
            Corner::BottomLeft => (b.2, b.1),
        };
        let (a, b) = (fixed(start), fixed(end));
        assert!(
            (a.0 - b.0).abs() < 1.5 && (a.1 - b.1).abs() < 1.5,
            "{corner:?}: {a:?} {b:?}"
        );
        // One undo gives the old size and place back.
        assert!(view.undo());
        assert_eq!(picture_box(&view, 0), start);
    }
}

#[test]
fn the_arrow_keys_nudge_a_selected_floating_picture() {
    let mut view = floating_view();
    let path = Path::body(0);
    let (x, y) = middle(picture_box(&view, 0));
    click_at(&mut view, x, y);
    let caret_before = view.selection().map(|(a, _)| a.clone());
    assert!(view.move_caret(Move::Right, false));
    assert!(view.move_caret(Move::Down, false));
    assert!(view.move_caret(Move::Down, false));
    assert!(view.move_caret(Move::Left, false));
    assert!(view.move_caret(Move::Left, false));
    assert!(view.move_caret(Move::Up, false));
    let step = (NUDGE * 12_700.0) as i64;
    assert_eq!(
        offsets(&drawing_of(&view, &path).float.unwrap()),
        (150 * PT - step, 100 * PT + step)
    );
    // The picture is still what is selected.
    assert_eq!(view.selection().map(|(a, _)| a.clone()), caret_before);
    assert!(view.selected_object().is_some());
    // One step each.
    assert!(view.undo());
    assert_eq!(
        offsets(&drawing_of(&view, &path).float.unwrap()),
        (150 * PT - step, 100 * PT + 2 * step)
    );
    // With Shift an arrow is a selection as ever: not a nudge.
    let before = view.document().clone();
    view.move_caret(Move::Right, true);
    assert_eq!(*view.document(), before);
    // A picture in the line is not nudged: the arrow moves the caret.
    let mut inline = view_of(doc(vec![with_drawing(picture(40, 20, None))]));
    let (x, y) = middle(picture_box(&inline, 0));
    click_at(&mut inline, x, y);
    let before = inline.document().clone();
    assert!(inline.move_caret(Move::Right, false));
    assert_eq!(*inline.document(), before);
    assert_eq!(inline.selected_object(), None);
    assert!(!inline.nudge_object(1, 0));
}

#[test]
fn a_selected_picture_has_a_frame_and_handles_but_no_tint() {
    for float in [None, Some(float_at(150, 100, FloatFrom::Page))] {
        let mut view = view_of(doc(vec![with_drawing(picture(40, 30, float)), pb("next")]));
        // Text selected is tinted: it has rectangles.
        view.set_selection(pos(1, 0), pos(1, 3));
        assert!(!view.selection_rects().is_empty());
        let (x, y) = middle(picture_box(&view, 0));
        click_at(&mut view, x, y);
        assert!(view.selected_object().is_some());
        assert!(view.selection_rects().is_empty(), "no tint for an object");
        // The frame is there.
        let b = picture_box(&view, 0);
        view.render();
        assert_eq!(
            pixel(&view, (b.0 - 1.0) as usize, y as usize),
            DocStyle::default().object
        );
        // Nothing beside the picture is tinted either: just left of the
        // frame, and just under it, the page is white.
        assert_eq!(pixel(&view, (b.0 - 6.0) as usize, y as usize), [255; 4]);
        assert_eq!(pixel(&view, x as usize, (b.3 + 6.0) as usize), [255; 4]);
    }
}

#[test]
fn how_a_picture_sits_with_the_text_is_asked_and_set() {
    let mut view = view_of(doc(vec![with_drawing(picture(40, 20, None)), pb("next")]));
    assert_eq!(view.picture_layout(), None);
    assert!(!view.set_picture_layout(PictureLayout::Square));
    let (x, y) = middle(picture_box(&view, 0));
    click_at(&mut view, x, y);
    assert_eq!(view.picture_layout(), Some(PictureLayout::Inline));
    assert_eq!(
        view.selected_object().map(|o| o.layout),
        Some(PictureLayout::Inline)
    );
    assert!(!view.set_picture_layout(PictureLayout::Inline));
    assert!(view.command(DocCommand::PictureLayout(PictureLayout::Square)));
    assert_eq!(view.picture_layout(), Some(PictureLayout::Square));
    assert!(view.set_picture_layout(PictureLayout::BehindText));
    assert_eq!(view.picture_layout(), Some(PictureLayout::BehindText));
    // Still selected, with handles, and one step each.
    let object = view.selected_object().expect("selected");
    assert!(!object.inline);
    assert!(view.undo());
    assert_eq!(view.picture_layout(), Some(PictureLayout::Square));
    assert!(view.undo());
    assert_eq!(view.picture_layout(), Some(PictureLayout::Inline));
}

// ---------------------------------------------------------------------------
// Other stories
// ---------------------------------------------------------------------------

fn header_document(drawing: Arc<Drawing>) -> Document {
    let mut p = paragraph("head");
    p.runs.push(Run::object(Inline::Drawing(drawing), look()));
    let mut document = doc((0..30).map(|i| pb(&format!("line {i}"))).collect());
    document.marginals = vec![Arc::new(Story {
        blocks: vec![Block::paragraph(p)],
        part: None,
    })];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    document
}

fn header_drawing(view: &DocView) -> Drawing {
    drawing_of(view, &Path::in_story(StoryId::Marginal(0), 0))
}

#[test]
fn a_picture_in_a_header_is_selected_resized_and_deleted() {
    let mut view = view_of(header_document(picture(30, 15, None)));
    let b = picture_box(&view, 0);
    // Outside the header a click there is a click in the body.
    click_at(&mut view, middle(b).0, middle(b).1);
    assert_eq!(view.selected_object(), None);
    assert!(view.enter_header());
    click_at(&mut view, middle(b).0, middle(b).1);
    let object = view.selected_object().expect("selected in the header");
    assert_eq!(object.pos.path.story, StoryId::Marginal(0));
    assert!(view.in_marginal());
    // No tint, and the frame is on the page.
    assert!(view.selection_rects().is_empty());

    // Resize from the bottom right corner.
    let start = header_drawing(&view).size;
    view.press(b.2, b.3, 1, false);
    assert!(view.is_resizing());
    view.drag(b.2 + 15.0, b.3 + 7.5);
    assert!(view.release());
    let now = header_drawing(&view).size;
    assert!(now.0 > start.0 + 10 * PT, "{now:?}");
    assert_eq!(
        view.selected_object().map(|o| o.pos),
        Some(object.pos.clone())
    );
    assert!(view.undo());
    assert_eq!(header_drawing(&view).size, start);

    // Delete takes it out of the header, in one step.
    assert!(view.selected_object().is_some());
    assert!(view.delete_forward(Unit::Cluster));
    let text = view
        .document()
        .paragraph(&Path::in_story(StoryId::Marginal(0), 0))
        .unwrap()
        .text();
    assert!(!text.contains('\u{FFFC}'), "{text:?}");
    assert!(view.undo());
    assert!(view.selected_object().is_some());
}

#[test]
fn a_floating_picture_in_a_header_is_moved_and_nudged() {
    let float = float_at(100, 8, FloatFrom::Page);
    let mut view = view_of(header_document(picture(20, 10, Some(float))));
    let b = picture_box(&view, 0);
    assert!(view.enter_header());
    let (x, y) = middle(b);
    view.press(x, y, 1, false);
    assert!(view.drag(x + 30.0, y + 5.0));
    assert!(view.release());
    let float = header_drawing(&view).float.unwrap();
    assert_eq!(offsets(&float), (130 * PT, 13 * PT));
    assert!(view.move_caret(Move::Right, false));
    assert_eq!(
        offsets(&header_drawing(&view).float.unwrap()).0,
        130 * PT + (NUDGE * 12_700.0) as i64
    );
    assert!(view.in_marginal());
    assert!(view.selected_object().is_some());
}

#[test]
fn a_picture_in_a_footnote_is_selected_moved_and_resized_like_the_body_s() {
    let mut first = paragraph("note ref");
    first.runs.push(Run::object(Inline::Footnote(1), look()));
    let mut note = paragraph("");
    note.runs = vec![
        Run::object(Inline::Footnote(1), look()),
        Run::text(" see ", look()),
        Run::object(Inline::Drawing(picture(30, 15, None)), look()),
        Run::text(" and more words after it", look()),
    ];
    let mut document = doc(vec![Block::paragraph(first), pb("second")]);
    document.footnotes = Notes {
        notes: vec![Note {
            id: 1,
            blocks: vec![Block::paragraph(note)],
        }],
        part: None,
    };
    let mut view = view_of(document);
    let b = picture_box(&view, 0);
    let (x, y) = middle(b);
    click_at(&mut view, x, y);
    let object = view.selected_object().expect("selected in the note");
    assert_eq!(object.pos.path.story, StoryId::Footnote(1));
    assert!(view.selection_rects().is_empty());
    // Resize it.
    let size = object.size;
    view.press(b.2, b.3, 1, false);
    view.drag(b.2 - 10.0, b.3 - 5.0);
    assert!(view.release());
    let after = view.selected_object().unwrap().size;
    assert!(after.0 < size.0, "{after:?} {size:?}");
    // Move it to the start of the note's text.
    let b = picture_box(&view, 0);
    let (x, y) = middle(b);
    view.press(x, y, 1, false);
    let note_start = Pos::new(Path::in_story(StoryId::Footnote(1), 0), 0);
    let (tx, ty) = {
        let caret = view.layout().caret(&note_start).unwrap();
        let r = view.page_rect(caret.page);
        let ppp = view.pixels_per_point();
        (
            r.x0 + f64::from(caret.x) * ppp + 1.0,
            r.y0 + f64::from(caret.top + caret.height / 2.0) * ppp,
        )
    };
    assert!(view.drag(tx, ty));
    assert!(view.release());
    let marks = |view: &DocView| {
        view.document()
            .paragraph(&Path::in_story(StoryId::Footnote(1), 0))
            .unwrap()
            .text()
            .matches('\u{FFFC}')
            .count()
    };
    // The picture was after " see ", the reference mark and a space away
    // from the start; it is before them now.
    let moved = view.selected_object().expect("still selected");
    assert!(moved.pos.offset < 8, "{:?}", moved.pos);
    assert_eq!(marks(&view), 2);
    // And delete it: the note's own number stays.
    assert!(view.delete_forward(Unit::Cluster));
    assert_eq!(marks(&view), 1);
}

#[test]
fn picture_drags_nudges_and_layouts_wait_for_an_input_method() {
    let mut view = floating_view();
    let b = picture_box(&view, 0);
    let (x, y) = middle(b);
    click_at(&mut view, x, y);
    put(&mut view, pos(1, 2));
    view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    });
    assert!(view.is_composing());
    let before = view.document().clone();
    assert!(!view.press(x, y, 1, false));
    assert!(!view.drag(x + 40.0, y));
    assert!(!view.release());
    assert!(!view.is_dragging());
    assert!(!view.nudge_object(1, 0));
    assert!(!view.set_picture_layout(PictureLayout::Square));
    assert_eq!(*view.document(), before);

    // A drag that was going on when the composition began changes nothing
    // when the button goes up.
    let mut view = floating_view();
    let (x, y) = middle(picture_box(&view, 0));
    view.press(x, y, 1, false);
    view.drag(x + 40.0, y + 20.0);
    let before = view.document().clone();
    view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    });
    assert!(!view.release());
    assert_eq!(*view.document(), before);
    assert!(!view.is_dragging());
}
