//! Tests of merging, splitting and resizing.

use super::*;
use crate::edit::table_tests::{
    assert_valid, cell_texts, cpos, labelled, strs, table_at, text_para, with_table,
};

fn doc3x3() -> Document {
    with_table(labelled(3, 3))
}

fn t(doc: &Document) -> &Table {
    table_at(doc, 1)
}

#[test]
fn a_merge_of_two_cells_in_a_row_joins_their_text() {
    let mut doc = doc3x3();
    let caret = merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0));
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells.len(), 2);
    assert_eq!(table.rows[0].cells[0].span, 2);
    assert_eq!(table.rows[0].cells[0].merge, VMerge::None);
    assert_eq!(cell_texts(table)[0], strs(&[&["r0c0|r0c1", "r0c2"]])[0]);
    assert_eq!(caret, cpos(1, 0, 0, 0));
    // Nothing else moved.
    assert_eq!(table.columns, vec![1000; 3]);
    assert_eq!(table.rows[1].cells.len(), 3);
}

#[test]
fn a_merge_down_makes_a_start_and_continuations() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 0, 1, 0), &cpos(1, 2, 1, 0));
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells[1].merge, VMerge::Start);
    assert_eq!(table.rows[1].cells[1].merge, VMerge::Continue);
    assert_eq!(table.rows[2].cells[1].merge, VMerge::Continue);
    assert_eq!(
        cell_texts(table)[0][1],
        "r0c1|r1c1|r2c1",
        "the content is in the first cell"
    );
    assert_eq!(cell_texts(table)[1][1], "");
}

#[test]
fn a_block_of_cells_merges_into_one() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 1, 1, 0));
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells.len(), 2);
    assert_eq!(table.rows[0].cells[0].span, 2);
    assert_eq!(table.rows[0].cells[0].merge, VMerge::Start);
    assert_eq!(table.rows[1].cells.len(), 2);
    assert_eq!(table.rows[1].cells[0].span, 2);
    assert_eq!(table.rows[1].cells[0].merge, VMerge::Continue);
    assert_eq!(cell_texts(table)[0][0], "r0c0|r0c1|r1c0|r1c1");
}

#[test]
fn the_rectangle_takes_in_the_merged_cells_it_cuts_through() {
    let mut doc = doc3x3();
    // A cell over the first two columns of row 0.
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0));
    // The second column of rows 0 and 1 touches it: the whole is taken.
    assert!(can_merge_cells(&doc, &cpos(1, 0, 1, 0), &cpos(1, 1, 1, 0)));
    let from = cpos(1, 1, 1, 0);
    let to = cpos(1, 1, 2, 0);
    merge_cells(&mut doc, &from, &to);
    assert_valid(&doc);
    let table = t(&doc);
    // Row 1 had three cells; columns 1 and 2 are merged, the first stays.
    assert_eq!(table.rows[1].cells.len(), 2);
    // Now take row 0's wide cell and the cell under it: row 0's merged
    // cell is widened over columns 0..2 and row 1's merged one over 1..3,
    // so the rectangle ends up over all three columns.
    let (a, b) = (cpos(1, 0, 0, 0), cpos(1, 1, 1, 0));
    assert!(can_merge_cells(&doc, &a, &b));
    merge_cells(&mut doc, &a, &b);
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells.len(), 1);
    assert_eq!(table.rows[0].cells[0].span, 3);
    assert_eq!(table.rows[0].cells[0].merge, VMerge::Start);
}

#[test]
fn a_vertical_merge_cut_by_the_selection_is_taken_whole() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 0, 2, 0), &cpos(1, 2, 2, 0));
    // A selection over the first two rows of column 1 and 2 touches the
    // merge in column 2: it is taken down to row 2.
    merge_cells(&mut doc, &cpos(1, 0, 1, 0), &cpos(1, 1, 2, 0));
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells[1].span, 2);
    assert_eq!(table.rows[2].cells[1].span, 2);
    assert_eq!(table.rows[2].cells[1].merge, VMerge::Continue);
}

#[test]
fn empty_cells_add_nothing_and_an_empty_first_cell_gives_way() {
    let mut table = labelled(1, 3);
    table.rows[0].cells[0].blocks = vec![Block::paragraph(text_para(""))];
    table.rows[0].cells[1].blocks = vec![Block::paragraph(text_para(""))];
    let mut doc = with_table(table);
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 2, 0));
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[0].cells.len(), 1);
    // One paragraph: the text of the last cell, no blank lines.
    assert_eq!(table.rows[0].cells[0].blocks.len(), 1);
    assert_eq!(cell_texts(table)[0][0], "r0c2");
}

#[test]
fn what_cannot_merge_is_refused() {
    let mut doc = doc3x3();
    let caret = cpos(1, 1, 1, 0);
    assert!(!can_merge_cells(&doc, &caret, &caret));
    assert_eq!(merge_cells(&mut doc, &caret, &caret), caret);
    // Outside the table.
    let outside = Pos::new(Path::body(0), 0);
    assert!(!can_merge_cells(&doc, &outside, &caret));
    let before = doc.clone();
    merge_cells(&mut doc, &outside, &outside);
    assert!(crate::edit::same_content(&doc, &before));
    // A selection that is one merged cell is nothing to merge.
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0));
    assert!(!can_merge_cells(&doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0)));
}

#[test]
fn a_merge_keeps_the_right_and_bottom_borders_of_the_far_cells() {
    let line = |w: f32| crate::Border {
        style: crate::BorderStyle::Single,
        width: w,
        color: None,
        space: 0.0,
    };
    let mut table = labelled(2, 2);
    table.rows[0].cells[1].borders[2] = Some(line(3.0));
    table.rows[1].cells[0].borders[3] = Some(line(2.0));
    let mut doc = with_table(table);
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 1, 1, 0));
    let cell = &t(&doc).rows[0].cells[0];
    assert_eq!(cell.borders[2], Some(line(3.0)));
    assert_eq!(cell.borders[3], Some(line(2.0)));
}

#[test]
fn unmerging_gives_back_the_grid_cells_with_the_content_in_the_first() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 1, 1, 0));
    assert!(can_unmerge_cell(&doc, &cpos(1, 0, 0, 0)));
    let caret = unmerge_cell(&mut doc, &cpos(1, 1, 0, 0));
    assert_valid(&doc);
    assert_eq!(caret, cpos(1, 0, 0, 0));
    let table = t(&doc);
    assert!(
        table
            .rows
            .iter()
            .all(|r| r.cells.len() == 3 && r.cells.iter().all(|c| c.span == 1))
    );
    assert!(
        table
            .rows
            .iter()
            .all(|r| r.cells.iter().all(|c| c.merge == VMerge::None))
    );
    assert_eq!(cell_texts(table)[0][0], "r0c0|r0c1|r1c0|r1c1");
    assert_eq!(cell_texts(table)[0][1], "");
    assert_eq!(cell_texts(table)[1][0], "");
    assert!(!can_unmerge_cell(&doc, &cpos(1, 0, 0, 0)));
    // Unmerging a plain cell does nothing.
    let before = doc.clone();
    unmerge_cell(&mut doc, &cpos(1, 0, 0, 0));
    assert!(crate::edit::same_content(&doc, &before));
}

#[test]
fn a_plain_cell_splits_into_columns() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 1, 1, 0));
    // Column 2 is 1000 wide: split row 0's cell there into 4.
    assert!(can_split_cell(&doc, &cpos(1, 0, 2, 0), 4, 1));
    let caret = split_cell(&mut doc, &cpos(1, 0, 2, 0), 4, 1);
    assert_valid(&doc);
    assert_eq!(caret, cpos(1, 0, 2, 0));
    let table = t(&doc);
    assert_eq!(table.columns, vec![1000, 1000, 250, 250, 250, 250]);
    assert_eq!(table.rows[0].cells.len(), 6);
    assert_eq!(cell_texts(table)[0][2], "r0c2");
    assert_eq!(cell_texts(table)[0][3], "");
    // The other rows' cells over the column grew; the merged cell over
    // columns 0 and 1 did not.
    assert_eq!(table.rows[1].cells[0].span, 2);
    assert_eq!(table.rows[1].cells[1].span, 4);
    assert_eq!(table.rows[2].cells[2].span, 4);
    assert_eq!(table.columns.iter().sum::<i32>(), 3000);
}

#[test]
fn the_width_of_a_split_goes_to_the_first_columns_first() {
    let mut table = labelled(1, 2);
    table.columns = vec![1003, 1000];
    let mut doc = with_table(table);
    split_cell(&mut doc, &cpos(1, 0, 0, 0), 3, 1);
    assert_eq!(t(&doc).columns, vec![335, 334, 334, 1000]);
}

#[test]
fn a_plain_cell_splits_into_rows() {
    let mut doc = doc3x3();
    let caret = split_cell(&mut doc, &cpos(1, 1, 1, 0), 1, 3);
    assert_valid(&doc);
    assert_eq!(caret, cpos(1, 1, 1, 0));
    let table = t(&doc);
    assert_eq!(table.rows.len(), 5);
    // The cell is three cells, one in each of three rows; the cells beside
    // it are merged over them.
    assert_eq!(table.rows[1].cells[0].merge, VMerge::Start);
    assert_eq!(table.rows[2].cells[0].merge, VMerge::Continue);
    assert_eq!(table.rows[3].cells[0].merge, VMerge::Continue);
    assert_eq!(table.rows[1].cells[1].merge, VMerge::None);
    assert_eq!(table.rows[2].cells[1].merge, VMerge::None);
    assert_eq!(cell_texts(table)[1][1], "r1c1");
    assert_eq!(cell_texts(table)[2][1], "");
    assert_eq!(cell_texts(table)[1][0], "r1c0");
    assert_eq!(cell_texts(table)[2][0], "");
    // The row after the rows is the old row 2.
    assert_eq!(cell_texts(table)[4][0], "r2c0");
}

#[test]
fn a_cell_splits_into_a_grid_of_cells() {
    let mut doc = doc3x3();
    split_cell(&mut doc, &cpos(1, 1, 1, 0), 2, 2);
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.columns, vec![1000, 500, 500, 1000]);
    assert_eq!(table.rows.len(), 4);
    assert_eq!(table.rows[1].cells.len(), 4);
    assert_eq!(table.rows[2].cells.len(), 4);
    // Both new cells of the second row are plain, not continuations.
    assert!(
        table.rows[2].cells[1..3]
            .iter()
            .all(|c| c.merge == VMerge::None)
    );
    assert_eq!(table.rows[2].cells[0].merge, VMerge::Continue);
    assert_eq!(table.rows[0].cells[1].span, 2);
}

#[test]
fn a_split_in_a_merged_cell_over_the_column_keeps_the_merge() {
    let mut doc = doc3x3();
    // Column 1 of rows 1 and 2 is a merge down.
    merge_cells(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 2, 1, 0));
    split_cell(&mut doc, &cpos(1, 0, 1, 0), 2, 1);
    assert_valid(&doc);
    let table = t(&doc);
    assert_eq!(table.rows[1].cells[1].span, 2);
    assert_eq!(table.rows[1].cells[1].merge, VMerge::Start);
    assert_eq!(table.rows[2].cells[1].span, 2);
    assert_eq!(table.rows[2].cells[1].merge, VMerge::Continue);
}

#[test]
fn what_does_not_split_cleanly_is_refused() {
    let mut doc = doc3x3();
    let at = cpos(1, 0, 0, 0);
    assert!(!can_split_cell(&doc, &at, 1, 1));
    assert!(!can_split_cell(&doc, &at, 0, 2));
    // A column of 1000 twips in 20 pieces.
    assert!(!can_split_cell(&doc, &at, 20, 1));
    // More columns than a table has.
    assert!(!can_split_cell(&doc, &at, 70, 1));
    merge_cells(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 1, 1, 0));
    assert!(!can_split_cell(&doc, &cpos(1, 1, 0, 0), 2, 1));
    let before = doc.clone();
    assert_eq!(
        split_cell(&mut doc, &cpos(1, 1, 0, 0), 2, 2),
        cpos(1, 1, 0, 0)
    );
    assert!(crate::edit::same_content(&doc, &before));
}

#[test]
fn borders_move_between_neighbours_and_the_table_keeps_its_width() {
    let mut doc = doc3x3();
    let table = Path::body(1);
    assert_eq!(move_column_border(&mut doc, &table, 1, 300), 300);
    assert_eq!(t(&doc).columns, vec![1300, 700, 1000]);
    assert_eq!(move_column_border(&mut doc, &table, 2, -200), -200);
    assert_eq!(t(&doc).columns, vec![1300, 500, 1200]);
    // The neighbour keeps a minimum.
    assert_eq!(
        move_column_border(&mut doc, &table, 1, 5000),
        500 - MIN_COLUMN
    );
    assert_eq!(t(&doc).columns, vec![1560, MIN_COLUMN, 1200]);
    assert_eq!(
        move_column_border(&mut doc, &table, 1, -5000),
        -(1560 - MIN_COLUMN)
    );
    assert_eq!(t(&doc).columns.iter().sum::<i32>(), 3000);
}

#[test]
fn the_outer_borders_change_the_width_and_the_indent() {
    let mut doc = doc3x3();
    let table = Path::body(1);
    assert_eq!(move_column_border(&mut doc, &table, 3, 500), 500);
    assert_eq!(t(&doc).columns, vec![1000, 1000, 1500]);
    assert_eq!(move_column_border(&mut doc, &table, 3, -400), -400);
    assert_eq!(move_column_border(&mut doc, &table, 0, 200), 200);
    assert_eq!(t(&doc).columns, vec![800, 1000, 1100]);
    assert_eq!(t(&doc).props.indent, 200);
    // Pulled left, the indent comes back and the first column grows.
    assert_eq!(move_column_border(&mut doc, &table, 0, -300), -300);
    assert_eq!(t(&doc).columns[0], 1100);
    assert_eq!(t(&doc).props.indent, -100);
    // Not beyond the biggest table.
    let grown = move_column_border(&mut doc, &table, 3, 100_000);
    assert_eq!(
        t(&doc).columns.iter().sum::<i32>(),
        MAX_TABLE_WIDTH,
        "grew by {grown}"
    );
    assert_eq!(move_column_border(&mut doc, &table, 3, 1), 0);
}

#[test]
fn a_border_that_is_not_there_does_nothing() {
    let mut doc = doc3x3();
    assert_eq!(move_column_border(&mut doc, &Path::body(1), 4, 10), 0);
    assert_eq!(move_column_border(&mut doc, &Path::body(0), 1, 10), 0);
}

#[test]
fn a_column_width_is_set_on_the_grid() {
    let mut doc = doc3x3();
    let table = Path::body(1);
    assert_eq!(set_column_width(&mut doc, &table, 1, 2500), Some(2500));
    assert_eq!(t(&doc).columns, vec![1000, 2500, 1000]);
    assert_eq!(set_column_width(&mut doc, &table, 1, 1), Some(MIN_COLUMN));
    assert_eq!(set_column_width(&mut doc, &table, 9, 100), None);
    assert_eq!(
        set_column_width(&mut doc, &table, 0, 1_000_000),
        Some(MAX_TABLE_WIDTH - MIN_COLUMN - 1000)
    );
}

#[test]
fn a_column_already_narrower_than_the_minimum_is_not_narrowed() {
    let mut table = labelled(1, 2);
    table.columns = vec![100, 1000];
    let mut doc = with_table(table);
    let path = Path::body(1);
    assert_eq!(set_column_width(&mut doc, &path, 0, 50), Some(100));
    assert_eq!(move_column_border(&mut doc, &path, 1, -50), 0);
    assert_eq!(move_column_border(&mut doc, &path, 1, 50), 50);
}

#[test]
fn row_heights_are_set_for_the_selected_rows() {
    let mut doc = doc3x3();
    assert!(set_row_height(
        &mut doc,
        &cpos(1, 0, 0, 0),
        &cpos(1, 1, 2, 0),
        RowHeight::AtLeast(600)
    ));
    let table = t(&doc);
    assert_eq!(table.rows[0].height, RowHeight::AtLeast(600));
    assert_eq!(table.rows[1].height, RowHeight::AtLeast(600));
    assert_eq!(table.rows[2].height, RowHeight::Auto);
    assert!(!set_row_height(
        &mut doc,
        &cpos(1, 0, 0, 0),
        &cpos(1, 0, 0, 0),
        RowHeight::AtLeast(600)
    ));
    set_row_height(
        &mut doc,
        &cpos(1, 2, 0, 0),
        &cpos(1, 2, 0, 0),
        RowHeight::Exactly(1),
    );
    assert_eq!(t(&doc).rows[2].height, RowHeight::Exactly(MIN_EXACT_ROW));
    set_row_height(
        &mut doc,
        &cpos(1, 2, 0, 0),
        &cpos(1, 2, 0, 0),
        RowHeight::Auto,
    );
    assert_eq!(t(&doc).rows[2].height, RowHeight::Auto);
    let outside = Pos::new(Path::body(0), 0);
    assert!(!set_row_height(
        &mut doc,
        &outside,
        &outside,
        RowHeight::Auto
    ));
}

#[test]
fn merges_and_splits_keep_the_markers_and_every_table_valid() {
    // A short random walk of merges, splits and unmerges over a table.
    let mut doc = with_table(labelled(4, 4));
    let mut seed = 12345u64;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n.max(1)
    };
    for _ in 0..200 {
        let table = t(&doc);
        let (rows, cells) = (table.rows.len(), table.rows[0].cells.len());
        let (r1, r2) = (next(rows), next(rows));
        let a = cpos(1, r1, next(table.rows[r1].cells.len()), 0);
        let b = cpos(1, r2, next(table.rows[r2].cells.len()), 0);
        match next(4) {
            0 => {
                merge_cells(&mut doc, &a, &b);
            }
            1 => {
                unmerge_cell(&mut doc, &a);
            }
            2 => {
                split_cell(&mut doc, &a, 1 + next(3), 1 + next(3));
            }
            _ => {
                let _ = cells;
                move_column_border(&mut doc, &Path::body(1), next(5), next(400) as i32 - 200);
            }
        }
        assert_valid(&doc);
    }
}

#[test]
fn a_selection_over_two_cells_selects_the_block_between() {
    let doc = doc3x3();
    let range = cell_range(&doc, &cpos(1, 0, 2, 0), &cpos(1, 1, 0, 0)).expect("two cells");
    assert_eq!(range.table, crate::Path::body(1));
    assert_eq!((range.rows, range.columns), ((0, 1), (0, 3)));
    // Inside one cell it is text; so it is outside tables.
    assert!(cell_range(&doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 2)).is_none());
    let outside = crate::Pos::new(crate::Path::body(0), 0);
    assert!(cell_range(&doc, &outside, &cpos(1, 0, 0, 0)).is_none());
}

#[test]
fn the_block_of_cells_takes_in_merged_cells_whole() {
    let mut doc = doc3x3();
    merge_cells(&mut doc, &cpos(1, 0, 1, 0), &cpos(1, 1, 1, 0));
    // The merged cell sticks out of the block asked for.
    let range = cell_range(&doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0)).unwrap();
    assert_eq!((range.rows, range.columns), ((0, 1), (0, 2)));
}

#[test]
fn a_row_has_a_place_to_start_from() {
    let mut doc = doc3x3();
    let at = row_start(&doc, &crate::Path::body(1), 2).unwrap();
    assert_eq!(at, cpos(1, 2, 0, 0));
    assert!(row_start(&doc, &crate::Path::body(1), 9).is_none());
    assert!(set_row_height(
        &mut doc,
        &at,
        &at,
        crate::RowHeight::AtLeast(600)
    ));
    assert_eq!(t(&doc).rows[2].height, crate::RowHeight::AtLeast(600));
    assert_eq!(t(&doc).rows[1].height, crate::RowHeight::Auto);
}
