//! Where the lines of tables are, for dragging them.

use std::sync::Arc;

use aster_writer::*;

use crate::{Layout, Layouter, TableBorder};

fn look() -> RunLook {
    RunLook {
        size: 10.0,
        ..RunLook::default()
    }
}

fn pb(text: &str) -> Block {
    Block::paragraph(Paragraph {
        runs: vec![Run::text(text, look())],
        base: look(),
        mark: look(),
        ..Paragraph::default()
    })
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

fn grid(columns: usize, rows: usize) -> Table {
    Table {
        columns: vec![1_000; columns],
        rows: (0..rows)
            .map(|r| Row {
                cells: (0..columns).map(|c| cell(&format!("{r}{c}"))).collect(),
                ..Row::default()
            })
            .collect(),
        ..Table::default()
    }
}

fn lay(document: &Document) -> Layout {
    Layouter::new().layout(document)
}

#[test]
fn the_lines_of_a_table_are_found_near_a_point() {
    let layout = lay(&doc(vec![Block::table(grid(3, 2))]));
    let g = layout.table_grids(0).next().expect("a table");
    assert_eq!(g.table, Path::body(0));
    assert_eq!(g.columns.len(), 4);
    assert_eq!(g.rows.len(), 2);
    // 1000 twips are 50 points.
    assert!((g.columns[2] - g.columns[1] - 50.0).abs() < 0.01);
    let (x, y) = (g.columns[1], (g.rows[0].top + g.rows[0].bottom) / 2.0);
    match layout.table_border_at(0, x + 1.0, y, 2.0, false) {
        Some(TableBorder::Column { border, table, .. }) => {
            assert_eq!((border, table), (1, Path::body(0)));
        }
        other => panic!("{other:?}"),
    }
    assert!(layout.table_border_at(0, x + 5.0, y, 2.0, false).is_none());
    // The bottom of the first row, over the second column.
    let at = (g.columns[1] + g.columns[2]) / 2.0;
    match layout.table_border_at(0, at, g.rows[0].bottom - 1.0, 2.0, false) {
        Some(TableBorder::Row { row, height, .. }) => {
            assert_eq!(row, 0);
            assert!((height - g.rows[0].height).abs() < 0.01);
        }
        other => panic!("{other:?}"),
    }
    // Not for the tables of headers and footers.
    assert!(layout.table_border_at(0, x, y, 2.0, true).is_none());
}

#[test]
fn a_line_a_merged_cell_hides_is_not_a_border() {
    let mut table = grid(3, 2);
    // The first row's first cell covers two grid columns.
    table.rows[0].cells.remove(1);
    table.rows[0].cells[0].span = 2;
    let layout = lay(&doc(vec![Block::table(table)]));
    let g = layout.table_grids(0).next().unwrap();
    let in_first_row = (g.rows[0].top + g.rows[0].bottom) / 2.0;
    let in_second_row = (g.rows[1].top + g.rows[1].bottom) / 2.0;
    assert!(
        layout
            .table_border_at(0, g.columns[1], in_first_row, 2.0, false)
            .is_none()
    );
    assert!(
        layout
            .table_border_at(0, g.columns[1], in_second_row, 2.0, false)
            .is_some()
    );
}

#[test]
fn the_bottom_of_a_vertically_merged_cell_is_only_at_its_end() {
    let mut table = grid(2, 3);
    table.rows[0].cells[0].merge = VMerge::Start;
    table.rows[1].cells[0].merge = VMerge::Continue;
    let layout = lay(&doc(vec![Block::table(table)]));
    let g = layout.table_grids(0).next().unwrap();
    let left = (g.columns[0] + g.columns[1]) / 2.0;
    let right = (g.columns[1] + g.columns[2]) / 2.0;
    let hit = |x: f32, y: f32| layout.table_border_at(0, x, y, 2.0, false);
    // The first row ends over the right column but not over the merged one.
    assert!(matches!(
        hit(right, g.rows[0].bottom),
        Some(TableBorder::Row { row: 0, .. })
    ));
    assert!(hit(left, g.rows[0].bottom).is_none());
    assert!(matches!(
        hit(left, g.rows[1].bottom),
        Some(TableBorder::Row { row: 1, .. })
    ));
}

#[test]
fn a_nested_table_is_found_before_the_table_round_it() {
    let inner = grid(2, 1);
    let mut outer = grid(1, 1);
    outer.rows[0].cells[0].blocks = vec![Block::table(inner), pb("")];
    let layout = lay(&doc(vec![Block::table(outer)]));
    let grids: Vec<_> = layout.table_grids(0).collect();
    assert_eq!(grids.len(), 2);
    let inner = grids
        .iter()
        .find(|g| g.table.steps.len() > 1)
        .expect("the nested table");
    assert_eq!(inner.table, Path::body(0).cell_block(0, 0, 0));
    let y = (inner.rows[0].top + inner.rows[0].bottom) / 2.0;
    match layout.table_border_at(0, inner.columns[1], y, 2.0, false) {
        Some(TableBorder::Column { table, border, .. }) => {
            assert_eq!((table, border), (inner.table.clone(), 1));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_table_split_over_pages_has_its_lines_on_each() {
    let layout = lay(&doc(vec![Block::table(grid(2, 30))]));
    assert!(layout.pages.len() >= 2);
    let mut seen = Vec::new();
    for page in 0..layout.pages.len() {
        for g in layout.table_grids(page) {
            seen.extend(g.rows.iter().map(|r| r.row));
            assert!(g.rows.iter().all(|r| r.bottom <= layout.pages[page].height));
        }
    }
    // A row that is cut by a page edge is on both pages.
    seen.dedup();
    assert_eq!(seen, (0..30).collect::<Vec<_>>());
}

#[test]
fn the_cells_of_a_range_are_one_rectangle_per_page() {
    let layout = lay(&doc(vec![Block::table(grid(3, 3))]));
    let g = layout.table_grids(0).next().unwrap();
    let rects = layout.cell_rects(None, &Path::body(0), (0, 1), (1, 3));
    assert_eq!(rects.len(), 1);
    let (page, r) = rects[0];
    assert_eq!(page, 0);
    assert_eq!((r.x0, r.x1), (g.columns[1], g.columns[3]));
    assert_eq!((r.y0, r.y1), (g.rows[0].top, g.rows[1].bottom));
    assert!(
        layout
            .cell_rects(None, &Path::body(0), (5, 6), (0, 1))
            .is_empty()
    );
}

#[test]
fn a_table_in_a_header_is_found_on_each_page() {
    let mut document = doc((0..40).map(|i| pb(&format!("line {i}"))).collect());
    document.marginals = vec![Arc::new(Story {
        blocks: vec![Block::table(grid(2, 1)), pb("")],
        part: None,
    })];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    assert!(layout.pages.len() >= 2);
    let header = Path::in_story(StoryId::Marginal(0), 0);
    for page in 0..layout.pages.len() {
        let g = layout
            .table_grids(page)
            .find(|g| g.table == header)
            .expect("the header's table on every page");
        let y = (g.rows[0].top + g.rows[0].bottom) / 2.0;
        assert!(matches!(
            layout.table_border_at(page, g.columns[1], y, 2.0, true),
            Some(TableBorder::Column { border: 1, .. })
        ));
        assert!(
            layout
                .table_border_at(page, g.columns[1], y, 2.0, false)
                .is_none()
        );
    }
    // Asked for one page, the rectangle is that page's.
    let rects = layout.cell_rects(Some(1), &header, (0, 0), (0, 2));
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].0, 1);
}

#[test]
fn text_set_lower_on_the_page_moves_the_lines_with_it() {
    let mut document = doc(vec![Block::table(grid(2, 1))]);
    let before = lay(&document);
    let top = before.table_grids(0).next().unwrap().rows[0].top;
    let mut section = small();
    section.vertical_align = CellAlign::Bottom;
    document.last_section = Arc::new(section);
    let after = lay(&document);
    let moved = after.table_grids(0).next().unwrap().rows[0].top;
    assert!(moved > top + 20.0, "{top} {moved}");
    // And the lines agree with where the text is.
    let caret = after
        .caret(&Pos::new(Path::body(0).cell_block(0, 0, 0), 0))
        .unwrap();
    let row = &after.table_grids(0).next().unwrap().rows[0];
    assert!(caret.top >= row.top && caret.top <= row.bottom);
}
