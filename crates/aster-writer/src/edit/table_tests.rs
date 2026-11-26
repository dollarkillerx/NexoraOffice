//! Tests of tables, links and pictures.

use super::*;
use crate::{
    Cell, CellLayer, Drawing, DrawingContent, Link, ListRef, ParaProps, Row, Section, Story, Style,
    StyleKind, StyleSheet, Table, VMerge, paragraphs_of,
};

fn look() -> RunLook {
    RunLook::default()
}

fn text_para(text: &str) -> Paragraph {
    Paragraph {
        runs: if text.is_empty() {
            Vec::new()
        } else {
            vec![Run::text(text, look())]
        },
        ..Paragraph::default()
    }
}

fn doc_of(texts: &[&str]) -> Document {
    Document {
        body: Story {
            blocks: texts
                .iter()
                .map(|t| Block::paragraph(text_para(t)))
                .collect(),
            part: None,
        },
        ..Document::default()
    }
}

fn pos(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn cpos(table: usize, row: usize, cell: usize, offset: usize) -> Pos {
    Pos::new(Path::body(table).cell_block(row, cell, 0), offset)
}

fn table_at(doc: &Document, block: usize) -> &Table {
    doc.body.blocks[block].as_table().expect("a table")
}

/// Every table in the document, nested ones too, satisfies the rules.
fn assert_valid(doc: &Document) {
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

/// A table whose cells hold their own coordinates as text: `r0c1`.
fn labelled(rows: usize, columns: usize) -> Table {
    Table {
        columns: vec![1000; columns],
        rows: (0..rows)
            .map(|r| Row {
                cells: (0..columns)
                    .map(|c| Cell {
                        blocks: vec![Block::paragraph(text_para(&format!("r{r}c{c}")))],
                        ..Cell::default()
                    })
                    .collect(),
                ..Row::default()
            })
            .collect(),
        ..Table::default()
    }
}

fn with_table(table: Table) -> Document {
    let mut doc = doc_of(&["before"]);
    doc.body.blocks.push(Block::table(table));
    doc.body.blocks.push(Block::paragraph(text_para("after")));
    doc
}

fn cell_texts(table: &Table) -> Vec<Vec<String>> {
    table
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|c| {
                    paragraphs_of(&c.blocks)
                        .iter()
                        .map(|(_, p)| p.text())
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .collect()
        })
        .collect()
}

fn strs(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

fn table_style_sheet() -> Arc<StyleSheet> {
    let normal = Style {
        id: Arc::from("Normal"),
        name: "Normal".to_owned(),
        kind: StyleKind::Paragraph,
        based_on: None,
        next: None,
        para: ParaProps {
            style: Some(Arc::from("Normal")),
            space_after: 160,
            ..ParaProps::default()
        },
        run: look(),
        hidden: false,
        quick: true,
        priority: 0,
    };
    let heading = Style {
        id: Arc::from("Heading1"),
        name: "heading 1".to_owned(),
        next: Some(Arc::from("Normal")),
        para: ParaProps {
            style: Some(Arc::from("Heading1")),
            space_before: 240,
            space_after: 200,
            ..ParaProps::default()
        },
        run: RunLook {
            size: 16.0,
            bold: true,
            ..look()
        },
        ..normal.clone()
    };
    let grid = Style {
        id: Arc::from("TableGrid"),
        name: "Table Grid".to_owned(),
        kind: StyleKind::Table,
        para: ParaProps {
            space_after: 0,
            ..ParaProps::default()
        },
        run: look(),
        next: None,
        ..normal.clone()
    };
    Arc::new(StyleSheet {
        styles: vec![normal, heading, grid],
        default_para: ParaProps {
            space_after: 160,
            ..ParaProps::default()
        },
        default_run: look(),
        default_style: Some(Arc::from("Normal")),
    })
}

fn styled_doc(texts: &[&str]) -> Document {
    let mut doc = doc_of(texts);
    doc.styles = table_style_sheet();
    for block in &mut doc.body.blocks {
        if let Block::Paragraph(p) = block {
            let p = Arc::make_mut(p);
            p.props.space_after = 160;
        }
    }
    doc
}

// ---------------------------------------------------------------------------
// insert_table
// ---------------------------------------------------------------------------

#[test]
fn a_table_in_the_middle_of_a_paragraph_splits_it() {
    let mut doc = doc_of(&["hello", "tail"]);
    let tail = doc.body.blocks[1].clone();
    let caret = insert_table(&mut doc, &pos(0, 2), 2, 3, 9_000);
    assert_eq!(doc.body.blocks.len(), 4);
    assert_eq!(p_text(&doc, 0), "he");
    assert_eq!(p_text(&doc, 2), "llo");
    let table = table_at(&doc, 1);
    assert_eq!(table.columns, vec![3000, 3000, 3000]);
    assert_eq!(table.rows.len(), 2);
    assert_eq!(caret, cpos(1, 0, 0, 0));
    assert!(doc.paragraph(&caret.path).is_some());
    assert!(doc.body.blocks[3].is(&tail));
    assert_valid(&doc);
}

fn p_text(doc: &Document, block: usize) -> String {
    doc.paragraph(&Path::body(block)).unwrap().text()
}

#[test]
fn a_table_at_the_start_of_a_paragraph_goes_before_it() {
    let mut doc = doc_of(&["hello"]);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    assert!(doc.body.blocks[0].as_table().is_some());
    assert_eq!(p_text(&doc, 1), "hello");
    assert_eq!(doc.body.blocks.len(), 2);
}

#[test]
fn a_table_at_the_end_of_a_paragraph_is_followed_by_an_empty_one() {
    let mut doc = doc_of(&["hello"]);
    insert_table(&mut doc, &pos(0, 5), 1, 2, 1000);
    assert_eq!(p_text(&doc, 0), "hello");
    assert!(doc.body.blocks[1].as_table().is_some());
    assert_eq!(p_text(&doc, 2), "");
}

#[test]
fn a_table_in_an_empty_paragraph_goes_before_it() {
    let mut doc = doc_of(&["a", "", "b"]);
    insert_table(&mut doc, &pos(1, 0), 1, 1, 1000);
    assert_eq!(doc.body.blocks.len(), 4);
    assert!(doc.body.blocks[1].as_table().is_some());
    assert_eq!(p_text(&doc, 2), "");
}

#[test]
fn widths_add_up_to_the_width_asked() {
    let mut doc = doc_of(&["x"]);
    insert_table(&mut doc, &pos(0, 0), 1, 3, 10_000);
    let table = table_at(&doc, 0);
    assert_eq!(table.columns.iter().sum::<i32>(), 10_000);
    assert_eq!(table.columns, vec![3334, 3333, 3333]);
}

#[test]
fn every_new_cell_has_thin_borders_and_one_empty_paragraph() {
    let mut doc = doc_of(&["x"]);
    insert_table(&mut doc, &pos(0, 0), 2, 2, 4000);
    let table = table_at(&doc, 0);
    for row in &table.rows {
        for cell in &row.cells {
            assert!(cell.borders.iter().all(|b| {
                b.is_some_and(|b| b.width == 0.5 && b.style == crate::BorderStyle::Single)
            }));
            assert_eq!(cell.blocks.len(), 1);
            assert!(cell.blocks[0].as_paragraph().unwrap().is_empty());
            assert!(cell.source.is_none());
        }
    }
}

#[test]
fn the_table_style_is_table_grid_only_when_the_document_has_it() {
    let mut plain = doc_of(&["x"]);
    insert_table(&mut plain, &pos(0, 0), 1, 1, 1000);
    assert_eq!(table_at(&plain, 0).props.style, None);

    let mut doc = styled_doc(&["x"]);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    let table = table_at(&doc, 0);
    assert_eq!(table.props.style.as_deref(), Some("TableGrid"));
    // The table style sets no space after: the paragraphs have none.
    let cell = &table.rows[0].cells[0];
    assert!(cell.layer.is_some());
    assert_eq!(cell.blocks[0].as_paragraph().unwrap().props.space_after, 0);
}

#[test]
fn cell_paragraphs_follow_what_enter_would_make() {
    let mut doc = styled_doc(&["Title"]);
    let mut p = doc.paragraph(&Path::body(0)).unwrap().clone();
    p.props.style = Some(Arc::from("Heading1"));
    p.props.align = crate::Align::Center;
    doc.body.blocks[0] = Block::paragraph(p);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    // Heading 1's next style is Normal.
    let cell = &table_at(&doc, 0).rows[0].cells[0];
    let made = cell.blocks[0].as_paragraph().unwrap();
    assert_eq!(made.props.style.as_deref(), Some("Normal"));
    assert_eq!(made.base.size, look().size);

    // Without a next style, direct formatting stays.
    let mut doc = styled_doc(&["Text"]);
    let mut p = doc.paragraph(&Path::body(0)).unwrap().clone();
    p.props.align = crate::Align::Center;
    p.props.list = Some(ListRef { list: 1, level: 0 });
    doc.body.blocks[0] = Block::paragraph(p);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    let cell = &table_at(&doc, 0).rows[0].cells[0];
    let made = cell.blocks[0].as_paragraph().unwrap();
    assert_eq!(made.props.align, crate::Align::Center);
    assert_eq!(made.props.list, None);
}

#[test]
fn a_table_in_a_cell_is_nested() {
    let mut doc = with_table(labelled(1, 2));
    let caret = insert_table(&mut doc, &cpos(1, 0, 1, 1), 2, 2, 1000);
    let outer = table_at(&doc, 1);
    let cell = &outer.rows[0].cells[1];
    // "r0c1" cut after one character, the table, the rest.
    assert_eq!(cell.blocks.len(), 3);
    assert!(cell.blocks[1].as_table().is_some());
    assert_eq!(caret.path.steps, vec![1, 0, 1, 1, 0, 0, 0]);
    assert!(doc.paragraph(&caret.path).is_some());
    assert_valid(&doc);
}

#[test]
fn impossible_tables_change_nothing() {
    let mut doc = doc_of(&["x"]);
    let before = doc.clone();
    assert_eq!(insert_table(&mut doc, &pos(0, 0), 0, 2, 1000), pos(0, 0));
    assert_eq!(insert_table(&mut doc, &pos(0, 0), 2, 0, 1000), pos(0, 0));
    assert_eq!(insert_table(&mut doc, &pos(0, 0), 1, 64, 1000), pos(0, 0));
    assert_eq!(insert_table(&mut doc, &pos(7, 0), 1, 1, 1000), pos(7, 0));
    assert_eq!(doc, before);
}

#[test]
fn inserting_a_table_leaves_other_blocks_shared() {
    let mut doc = doc_of(&["a", "b", "c"]);
    let (a, c) = (doc.body.blocks[0].clone(), doc.body.blocks[2].clone());
    insert_table(&mut doc, &pos(1, 0), 1, 1, 1000);
    assert!(doc.body.blocks[0].is(&a));
    assert!(doc.body.blocks[3].is(&c));
}

// ---------------------------------------------------------------------------
// cell_at
// ---------------------------------------------------------------------------

#[test]
fn cell_at_names_the_innermost_cell() {
    let mut table = labelled(2, 3);
    table.rows[1].cells[0].span = 2;
    table.rows[1].cells.remove(1);
    let doc = with_table(table);
    assert_eq!(cell_at(&doc, &pos(0, 0)), None);
    let found = cell_at(&doc, &cpos(1, 1, 1, 0)).unwrap();
    assert_eq!(found.table, Path::body(1));
    assert_eq!((found.row, found.cell), (1, 1));
    assert_eq!((found.column, found.span), (2, 1));
    let wide = cell_at(&doc, &cpos(1, 1, 0, 0)).unwrap();
    assert_eq!((wide.column, wide.span), (0, 2));
    assert_eq!(cell_at(&doc, &pos(9, 0)), None);
}

#[test]
fn cell_at_in_a_nested_table_gives_the_inner_cell() {
    let mut doc = with_table(labelled(1, 1));
    insert_table(&mut doc, &cpos(1, 0, 0, 0), 1, 2, 2000);
    let inner = Path::body(1).cell_block(0, 0, 0);
    let found = cell_at(&doc, &Pos::new(inner.cell_block(0, 1, 0), 0)).unwrap();
    assert_eq!(found.table, inner);
    assert_eq!((found.row, found.cell, found.column), (0, 1, 1));
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[test]
fn rows_are_added_above_and_below() {
    let mut doc = with_table(labelled(2, 2));
    let caret = insert_rows(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 1, 0, 0), true);
    assert_eq!(caret, cpos(1, 1, 0, 0));
    assert_eq!(
        cell_texts(table_at(&doc, 1)),
        strs(&[&["r0c0", "r0c1"], &["", ""], &["r1c0", "r1c1"]])
    );
    let caret = insert_rows(&mut doc, &cpos(1, 2, 1, 0), &cpos(1, 2, 1, 0), false);
    assert_eq!(caret, cpos(1, 3, 0, 0));
    assert_eq!(table_at(&doc, 1).rows.len(), 4);
    assert_valid(&doc);
}

#[test]
fn as_many_rows_as_the_selection_covers_are_added() {
    let mut doc = with_table(labelled(4, 2));
    insert_rows(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 2, 1, 2), false);
    assert_eq!(table_at(&doc, 1).rows.len(), 6);
    // After the last covered row (row 2).
    let texts = cell_texts(table_at(&doc, 1));
    assert_eq!(texts[2][0], "r2c0");
    assert_eq!(texts[3][0], "");
    assert_eq!(texts[4][0], "");
    assert_eq!(texts[5][0], "r3c0");
    assert_valid(&doc);
}

#[test]
fn a_selection_backwards_covers_the_same_rows() {
    let mut a = with_table(labelled(4, 2));
    let mut b = a.clone();
    insert_rows(&mut a, &cpos(1, 1, 0, 0), &cpos(1, 2, 0, 0), true);
    insert_rows(&mut b, &cpos(1, 2, 0, 0), &cpos(1, 1, 0, 0), true);
    assert_eq!(a, b);
}

#[test]
fn a_new_row_copies_the_look_of_its_neighbour() {
    let mut table = labelled(2, 2);
    table.rows[0].cells[1].shading = Some([1, 2, 3, 255]);
    table.rows[0].cells[0].span = 1;
    table.rows[0].height = crate::RowHeight::AtLeast(500);
    table.rows[0].header = true;
    table.rows[0].cells[1].margins = [10, 20, 30, 40];
    let mut doc = with_table(table);
    let mut p = doc
        .paragraph(&Path::body(1).cell_block(0, 1, 0))
        .unwrap()
        .clone();
    p.mark.bold = true;
    p.props.align = crate::Align::Right;
    if let Some(Block::Table(t)) = doc.body.blocks.get_mut(1) {
        Arc::make_mut(t).rows[0].cells[1].blocks[0] = Block::paragraph(p);
    }
    insert_rows(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0), false);
    let table = table_at(&doc, 1);
    let new = &table.rows[1];
    assert!(!new.header);
    assert_eq!(new.height, crate::RowHeight::AtLeast(500));
    assert_eq!(new.cells[1].shading, Some([1, 2, 3, 255]));
    assert_eq!(new.cells[1].margins, [10, 20, 30, 40]);
    let made = new.cells[1].blocks[0].as_paragraph().unwrap();
    assert!(made.mark.bold);
    assert_eq!(made.props.align, crate::Align::Right);
    assert!(made.is_empty());
    assert!(made.source.is_none());
}

/// Three columns, four rows; column 0 is merged over rows 1 and 2.
fn merged() -> Document {
    let mut table = labelled(4, 3);
    table.rows[1].cells[0].merge = VMerge::Start;
    table.rows[2].cells[0].merge = VMerge::Continue;
    with_table(table)
}

fn merges(doc: &Document, column: usize) -> Vec<VMerge> {
    table_at(doc, 1)
        .rows
        .iter()
        .map(|r| r.cells[column].merge)
        .collect()
}

#[test]
fn rows_inside_a_vertical_merge_continue_it() {
    use VMerge::{Continue, None as No, Start};
    // Below the first row of the merge: inside it.
    let mut doc = merged();
    insert_rows(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0), false);
    assert_eq!(merges(&doc, 0), vec![No, Start, Continue, Continue, No]);
    assert_valid(&doc);
    // Above its continuation: inside it.
    let mut doc = merged();
    insert_rows(&mut doc, &cpos(1, 2, 1, 0), &cpos(1, 2, 1, 0), true);
    assert_eq!(merges(&doc, 0), vec![No, Start, Continue, Continue, No]);
    // Above its start: outside.
    let mut doc = merged();
    insert_rows(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0), true);
    assert_eq!(merges(&doc, 0), vec![No, No, Start, Continue, No]);
    // Below its end: outside.
    let mut doc = merged();
    insert_rows(&mut doc, &cpos(1, 2, 1, 0), &cpos(1, 2, 1, 0), false);
    assert_eq!(merges(&doc, 0), vec![No, Start, Continue, No, No]);
    assert_valid(&doc);
}

#[test]
fn rows_are_removed_with_the_caret_on_the_next() {
    let mut doc = with_table(labelled(4, 2));
    let caret = delete_rows(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0));
    assert_eq!(
        cell_texts(table_at(&doc, 1)),
        strs(&[&["r0c0", "r0c1"], &["r2c0", "r2c1"], &["r3c0", "r3c1"]])
    );
    assert_eq!(caret, cpos(1, 1, 1, 0));
    // The last rows: the caret goes to the new last row.
    let caret = delete_rows(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 2, 0, 0));
    assert_eq!(table_at(&doc, 1).rows.len(), 1);
    assert_eq!(caret, cpos(1, 0, 0, 0));
    assert_valid(&doc);
}

#[test]
fn deleting_every_row_deletes_the_table() {
    let mut doc = with_table(labelled(2, 2));
    let caret = delete_rows(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 1, 0, 0));
    assert_eq!(doc.body.blocks.len(), 2);
    assert_eq!(caret, pos(1, 0));
    assert_eq!(p_text(&doc, 1), "after");
}

#[test]
fn removing_the_start_of_a_merge_makes_the_next_cell_the_start() {
    use VMerge::{Continue, None as No, Start};
    let mut doc = merged();
    delete_rows(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0));
    // Row 2 (a Continue) is now row 1 and a lone cell: ordinary.
    assert_eq!(merges(&doc, 0), vec![No, No, No]);
    assert_valid(&doc);

    // A merge of three: removing the start leaves a merge of two.
    let mut table = labelled(4, 2);
    for (r, m) in [(0, Start), (1, Continue), (2, Continue)] {
        table.rows[r].cells[0].merge = m;
    }
    let mut doc = with_table(table);
    delete_rows(&mut doc, &cpos(1, 0, 1, 0), &cpos(1, 0, 1, 0));
    assert_eq!(merges(&doc, 0), vec![Start, Continue, No]);
    assert_valid(&doc);

    // Removing the middle of a merge keeps it.
    let mut table = labelled(4, 2);
    for (r, m) in [(0, Start), (1, Continue), (2, Continue)] {
        table.rows[r].cells[0].merge = m;
    }
    let mut doc = with_table(table);
    delete_rows(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0));
    assert_eq!(merges(&doc, 0), vec![Start, Continue, No]);
    assert_valid(&doc);
}

#[test]
fn markers_in_removed_rows_move_to_the_caret() {
    let mut table = labelled(3, 1);
    let mut p = text_para("gone");
    p.runs.push(Run::object(Inline::Marker, look()));
    table.rows[1].cells[0].blocks[0] = Block::paragraph(p);
    let mut doc = with_table(table);
    let caret = delete_rows(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 1, 0, 0));
    let p = doc.paragraph(&caret.path).unwrap();
    assert_eq!(p.text(), "r2c0");
    assert!(matches!(p.runs[0].kind, Inline::Marker));
}

// ---------------------------------------------------------------------------
// Columns
// ---------------------------------------------------------------------------

#[test]
fn columns_are_added_left_and_right() {
    let mut doc = with_table(labelled(2, 2));
    if let Some(Block::Table(t)) = doc.body.blocks.get_mut(1) {
        Arc::make_mut(t).columns = vec![1000, 2000];
    }
    let caret = insert_columns(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0), true);
    let table = table_at(&doc, 1);
    assert_eq!(table.columns, vec![1000, 2000, 2000]);
    assert_eq!(
        cell_texts(table),
        strs(&[&["r0c0", "", "r0c1"], &["r1c0", "", "r1c1"]])
    );
    assert_eq!(caret, cpos(1, 1, 1, 0));
    let caret = insert_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0), false);
    let table = table_at(&doc, 1);
    assert_eq!(table.columns, vec![1000, 1000, 2000, 2000]);
    assert_eq!(caret, cpos(1, 0, 1, 0));
    assert_eq!(table.rows[0].cells.len(), 4);
    assert_valid(&doc);
}

#[test]
fn as_many_columns_as_the_selection_covers_are_added() {
    let mut doc = with_table(labelled(2, 4));
    insert_columns(&mut doc, &cpos(1, 0, 1, 0), &cpos(1, 1, 2, 0), false);
    let table = table_at(&doc, 1);
    assert_eq!(table.columns.len(), 6);
    assert_eq!(
        cell_texts(table)[0],
        strs(&[&["r0c0", "r0c1", "r0c2", "", "", "r0c3"]])[0]
    );
    assert_valid(&doc);
}

/// Four columns; row 0 has a cell over columns 1 and 2, row 1 has four
/// cells, row 2 has a cell over columns 0 to 2.
fn spanning() -> Document {
    let mut table = labelled(3, 4);
    table.rows[0].cells[1].span = 2;
    table.rows[0].cells.remove(2);
    table.rows[2].cells[0].span = 3;
    table.rows[2].cells.drain(1..3);
    with_table(table)
}

#[test]
fn a_column_through_a_spanning_cell_widens_it() {
    let mut doc = spanning();
    // Between columns 1 and 2: the left of column 2.
    insert_columns(&mut doc, &cpos(1, 1, 2, 0), &cpos(1, 1, 2, 0), true);
    let table = table_at(&doc, 1);
    assert_eq!(table.columns.len(), 5);
    assert_eq!(table.rows[0].cells[1].span, 3);
    assert_eq!(table.rows[0].cells.len(), 3);
    assert_eq!(table.rows[1].cells.len(), 5);
    assert_eq!(table.rows[2].cells[0].span, 4);
    assert_valid(&doc);
}

#[test]
fn a_column_at_an_edge_of_a_spanning_cell_makes_a_cell() {
    let mut doc = spanning();
    // To the left of column 1: before the cell over 1 and 2, inside the
    // cell over 0 to 2.
    insert_columns(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0), true);
    let table = table_at(&doc, 1);
    assert_eq!(table.rows[0].cells.len(), 4);
    assert_eq!(table.rows[0].cells[1].span, 1);
    assert_eq!(table.rows[0].cells[2].span, 2);
    assert_eq!(table.rows[2].cells[0].span, 4);
    assert_valid(&doc);
}

#[test]
fn a_column_to_the_right_of_everything_goes_last() {
    let mut doc = spanning();
    insert_columns(&mut doc, &cpos(1, 1, 3, 0), &cpos(1, 1, 3, 0), false);
    let table = table_at(&doc, 1);
    assert_eq!(table.columns.len(), 5);
    assert_eq!(table.rows[1].cells.len(), 5);
    assert_eq!(table.rows[0].cells.last().unwrap().span, 1);
    assert_eq!(table.rows[0].cells.len(), 4);
    // Row 2's cell covers 0..3 and the column is after it: a new cell.
    assert_eq!(table.rows[2].cells.len(), 3);
    assert_valid(&doc);
}

#[test]
fn a_column_through_a_vertical_merge_widens_every_row_of_it() {
    let mut table = labelled(3, 3);
    table.rows[0].cells[0].span = 2;
    table.rows[0].cells.remove(1);
    table.rows[1].cells[0].span = 2;
    table.rows[1].cells.remove(1);
    table.rows[0].cells[0].merge = VMerge::Start;
    table.rows[1].cells[0].merge = VMerge::Continue;
    let mut doc = with_table(table);
    assert_valid(&doc);
    insert_columns(&mut doc, &cpos(1, 2, 1, 0), &cpos(1, 2, 1, 0), true);
    let table = table_at(&doc, 1);
    assert_eq!(table.rows[0].cells[0].span, 3);
    assert_eq!(table.rows[1].cells[0].span, 3);
    assert_valid(&doc);
}

#[test]
fn new_columns_in_a_row_with_empty_grid_columns() {
    let mut table = labelled(2, 3);
    table.rows[1].grid_before = 1;
    table.rows[1].cells.remove(0);
    let mut doc = with_table(table);
    assert_valid(&doc);
    // Left of column 0: in row 1 the empty stretch grows.
    insert_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0), true);
    let table = table_at(&doc, 1);
    assert_eq!(table.rows[1].grid_before, 2);
    assert_eq!(table.rows[0].cells.len(), 4);
    assert_valid(&doc);
}

#[test]
fn columns_are_removed_and_spans_narrowed() {
    let mut doc = spanning();
    let caret = delete_columns(&mut doc, &cpos(1, 1, 2, 0), &cpos(1, 1, 2, 0));
    let table = table_at(&doc, 1);
    assert_eq!(table.columns.len(), 3);
    assert_eq!(table.rows[0].cells[1].span, 1);
    assert_eq!(table.rows[1].cells.len(), 3);
    assert_eq!(table.rows[2].cells[0].span, 2);
    assert_eq!(caret, cpos(1, 1, 2, 0));
    assert_valid(&doc);
}

#[test]
fn a_spanning_cell_removed_whole_goes() {
    let mut doc = spanning();
    // Columns 1 and 2: the cell over them in row 0 goes.
    delete_columns(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 2, 0));
    let table = table_at(&doc, 1);
    assert_eq!(table.columns.len(), 2);
    assert_eq!(
        cell_texts(table)[0],
        vec!["r0c0".to_owned(), "r0c3".to_owned()]
    );
    assert_eq!(table.rows[2].cells[0].span, 1);
    assert_valid(&doc);
}

#[test]
fn removing_the_first_and_last_columns() {
    let mut doc = with_table(labelled(2, 3));
    delete_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0));
    assert_eq!(
        cell_texts(table_at(&doc, 1)),
        strs(&[&["r0c1", "r0c2"], &["r1c1", "r1c2"]])
    );
    let caret = delete_columns(&mut doc, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0));
    assert_eq!(table_at(&doc, 1).columns.len(), 1);
    assert_eq!(caret, cpos(1, 1, 0, 0));
    assert_valid(&doc);
}

#[test]
fn deleting_every_column_deletes_the_table() {
    let mut doc = with_table(labelled(2, 2));
    let caret = delete_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0));
    assert_eq!(doc.body.blocks.len(), 2);
    assert_eq!(caret, pos(1, 0));
}

#[test]
fn columns_under_empty_grid_stretches_shrink_them() {
    let mut table = labelled(2, 3);
    table.rows[1].grid_before = 1;
    table.rows[1].cells.remove(0);
    let mut doc = with_table(table);
    delete_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0));
    let table = table_at(&doc, 1);
    assert_eq!(table.rows[1].grid_before, 0);
    assert_eq!(table.rows[1].cells.len(), 2);
    assert_valid(&doc);
}

#[test]
fn the_start_of_a_vertical_merge_removed_with_its_column_takes_it_all() {
    let mut doc = merged();
    delete_columns(&mut doc, &cpos(1, 1, 0, 0), &cpos(1, 1, 0, 0));
    assert_valid(&doc);
    assert_eq!(table_at(&doc, 1).columns.len(), 2);
}

#[test]
fn a_selection_in_a_nested_table_acts_on_the_table_that_holds_both_ends() {
    let mut doc = with_table(labelled(2, 2));
    insert_table(&mut doc, &cpos(1, 0, 0, 0), 2, 2, 2000);
    let inner = Path::body(1).cell_block(0, 0, 0);
    let in_inner = Pos::new(inner.cell_block(1, 1, 0), 0);
    let in_outer = cpos(1, 1, 0, 0);
    // From inside the nested table to a cell of the outer table: the outer.
    // (it covers row 0, which holds the nested table, and row 1.)
    insert_rows(&mut doc, &in_inner, &in_outer, false);
    assert_eq!(table_at(&doc, 1).rows.len(), 4);
    // Within the nested table: the nested one.
    let in_inner2 = Pos::new(inner.cell_block(0, 0, 0), 0);
    insert_rows(&mut doc, &in_inner2, &in_inner, false);
    let nested = table_at(&doc, 1).rows[0].cells[0].blocks[0]
        .as_table()
        .unwrap();
    assert_eq!(nested.rows.len(), 4);
    assert_valid(&doc);
}

#[test]
fn outside_a_table_row_and_column_operations_do_nothing() {
    let mut doc = with_table(labelled(2, 2));
    let before = doc.clone();
    assert_eq!(
        insert_rows(&mut doc, &pos(0, 0), &pos(0, 0), true),
        pos(0, 0)
    );
    assert_eq!(
        insert_columns(&mut doc, &pos(0, 0), &pos(0, 0), true),
        pos(0, 0)
    );
    assert_eq!(delete_rows(&mut doc, &pos(0, 0), &pos(0, 0)), pos(0, 0));
    assert_eq!(delete_columns(&mut doc, &pos(0, 0), &pos(0, 0)), pos(0, 0));
    assert_eq!(delete_table(&mut doc, &pos(0, 0)), pos(0, 0));
    assert_eq!(doc, before);
}

#[test]
fn other_blocks_and_rows_stay_shared() {
    let mut doc = with_table(labelled(3, 2));
    let (before, after) = (doc.body.blocks[0].clone(), doc.body.blocks[2].clone());
    let kept = table_at(&doc, 1).rows[2].cells[1].blocks[0].clone();
    insert_rows(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0), false);
    assert!(doc.body.blocks[0].is(&before));
    assert!(doc.body.blocks[2].is(&after));
    assert!(table_at(&doc, 1).rows[3].cells[1].blocks[0].is(&kept));
    delete_columns(&mut doc, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0));
    assert!(doc.body.blocks[0].is(&before));
}

// ---------------------------------------------------------------------------
// delete_table
// ---------------------------------------------------------------------------

#[test]
fn a_table_is_deleted_with_the_caret_after_it() {
    let mut doc = with_table(labelled(2, 2));
    let caret = delete_table(&mut doc, &cpos(1, 1, 1, 2));
    assert_eq!(doc.body.blocks.len(), 2);
    assert_eq!(caret, pos(1, 0));
}

#[test]
fn a_nested_table_is_deleted_and_the_cell_keeps_a_paragraph() {
    let mut doc = with_table(labelled(1, 1));
    insert_table(&mut doc, &cpos(1, 0, 0, 0), 1, 1, 1000);
    // The cell is now: table, "r0c0".
    let inner = Pos::new(Path::body(1).cell_block(0, 0, 0).cell_block(0, 0, 0), 0);
    let caret = delete_table(&mut doc, &inner);
    let cell = &table_at(&doc, 1).rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1);
    assert_eq!(caret, cpos(1, 0, 0, 0));
    assert_valid(&doc);
}

#[test]
fn a_table_at_the_end_of_a_cell_leaves_a_paragraph() {
    let mut doc = with_table(labelled(1, 1));
    // Make the nested table the only block of the cell.
    insert_table(&mut doc, &cpos(1, 0, 0, 0), 1, 1, 1000);
    if let Some(Block::Table(t)) = doc.body.blocks.get_mut(1) {
        Arc::make_mut(t).rows[0].cells[0].blocks.truncate(1);
    }
    let inner = Pos::new(Path::body(1).cell_block(0, 0, 0).cell_block(0, 0, 0), 0);
    delete_table(&mut doc, &inner);
    let cell = &table_at(&doc, 1).rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1);
    assert!(cell.blocks[0].as_paragraph().is_some());
}

#[test]
fn a_last_table_goes_and_the_caret_is_at_the_end_before_it() {
    let mut doc = doc_of(&["a"]);
    doc.body.blocks.push(Block::table(labelled(1, 1)));
    doc.body.blocks.push(Block::paragraph(text_para("")));
    let caret = delete_table(&mut doc, &cpos(1, 0, 0, 0));
    assert_eq!(caret, pos(1, 0));
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

#[test]
fn undo_restores_the_exact_document() {
    let mut doc = with_table(labelled(3, 3));
    let mut history = History::default();
    type Step = Box<dyn Fn(&mut Document)>;
    let steps: Vec<Step> = vec![
        Box::new(|d| {
            insert_rows(d, &cpos(1, 1, 1, 0), &cpos(1, 1, 1, 0), true);
        }),
        Box::new(|d| {
            insert_columns(d, &cpos(1, 0, 0, 0), &cpos(1, 0, 1, 0), false);
        }),
        Box::new(|d| {
            delete_rows(d, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0));
        }),
        Box::new(|d| {
            delete_columns(d, &cpos(1, 0, 0, 0), &cpos(1, 0, 0, 0));
        }),
        Box::new(|d| {
            delete_table(d, &cpos(1, 0, 0, 0));
        }),
        Box::new(|d| {
            insert_table(d, &pos(0, 2), 2, 2, 3000);
        }),
    ];
    let mut states = vec![doc.clone()];
    for step in &steps {
        let before = doc.clone();
        step(&mut doc);
        history.record(
            &before,
            &Selection::caret(pos(0, 0)),
            &Selection::caret(pos(0, 0)),
            None,
        );
        states.push(doc.clone());
    }
    for expected in states.iter().rev().skip(1) {
        let (restored, _) = history.undo(&doc).unwrap();
        assert_eq!(&restored, expected);
        doc = restored;
    }
    assert_valid(&doc);
}

// ---------------------------------------------------------------------------
// set_style in cells
// ---------------------------------------------------------------------------

#[test]
fn a_style_in_a_cell_keeps_what_the_table_style_gives() {
    let mut doc = styled_doc(&["x"]);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    let at = cpos(0, 0, 0, 0);
    // Normal sets space after 160 as the document default, the table style
    // sets 0: in the cell the table style shows through.
    set_style(&mut doc, &at, &at, "Normal");
    let made = doc.paragraph(&at.path).unwrap();
    assert_eq!(made.props.space_after, 0);
    // Heading 1 sets its own.
    set_style(&mut doc, &at, &at, "Heading1");
    let made = doc.paragraph(&at.path).unwrap();
    assert_eq!(made.props.space_after, 200);
    assert_eq!(made.props.space_before, 240);
    assert_eq!(made.base.size, 16.0);
    // And back.
    set_style(&mut doc, &at, &at, "Normal");
    let made = doc.paragraph(&at.path).unwrap();
    assert_eq!(made.props.space_after, 0);
    assert_eq!(made.props.space_before, 0);
    // Outside a table the plain style applies.
    set_style(&mut doc, &pos(1, 0), &pos(1, 0), "Normal");
    assert_eq!(
        doc.paragraph(&Path::body(1)).unwrap().props.space_after,
        160
    );
}

#[test]
fn the_table_layer_gives_run_looks_too() {
    let mut doc = styled_doc(&["x"]);
    insert_table(&mut doc, &pos(0, 0), 1, 1, 1000);
    let at = cpos(0, 0, 0, 0);
    let layer = CellLayer {
        run: RunLook {
            bold: true,
            ..look()
        },
        para: ParaProps::default(),
    };
    if let Some(Block::Table(t)) = doc.body.blocks.get_mut(0) {
        Arc::make_mut(t).rows[0].cells[0].layer = Some(Arc::new(layer));
    }
    insert_text(&mut doc, &at, "hello", None);
    set_style(&mut doc, &at, &at, "Normal");
    let made = doc.paragraph(&at.path).unwrap();
    assert!(made.base.bold);
    // Heading 1 is bold by itself and 16 points.
    set_style(&mut doc, &at, &at, "Heading1");
    let made = doc.paragraph(&at.path).unwrap();
    assert!(made.base.bold);
    assert_eq!(made.base.size, 16.0);
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

fn link(target: &str) -> Link {
    Link {
        target: target.to_owned(),
        tooltip: None,
    }
}

fn blue() -> Option<crate::Rgba> {
    Some([0x05, 0x63, 0xC1, 0xFF])
}

fn hyperlink_styles() -> Arc<StyleSheet> {
    let mut sheet = (*table_style_sheet()).clone();
    sheet.styles.push(Style {
        id: Arc::from("Hyperlink"),
        name: "Hyperlink".to_owned(),
        kind: StyleKind::Character,
        based_on: None,
        next: None,
        para: ParaProps::default(),
        run: RunLook {
            color: blue(),
            underline: Some(crate::Underline {
                style: crate::UnderlineStyle::Single,
                color: None,
            }),
            ..look()
        },
        hidden: false,
        quick: false,
        priority: 0,
    });
    Arc::new(sheet)
}

#[test]
fn a_link_splits_runs_and_gives_the_direct_look() {
    let mut doc = doc_of(&["one two three"]);
    set_link(
        &mut doc,
        &pos(0, 4),
        &pos(0, 7),
        Some(link("https://a.example/")),
    );
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.runs.len(), 3);
    assert_eq!(p.runs[1].text, "two");
    assert_eq!(p.runs[1].link.as_deref(), Some(&link("https://a.example/")));
    assert_eq!(p.runs[1].look.color, blue());
    assert!(p.runs[1].look.underline.is_some());
    assert_eq!(p.runs[1].style, None);
    assert_eq!(p.runs[0].link, None);
    assert_eq!(p.runs[0].look.color, None);
}

#[test]
fn a_link_uses_the_hyperlink_style_when_there_is_one() {
    let mut doc = doc_of(&["one two"]);
    doc.styles = hyperlink_styles();
    set_link(&mut doc, &pos(0, 0), &pos(0, 3), Some(link("#top")));
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.runs[0].style.as_deref(), Some("Hyperlink"));
    assert_eq!(p.runs[0].look.color, blue());
}

#[test]
fn taking_a_link_off_undoes_exactly_its_look() {
    for styled in [false, true] {
        let mut doc = doc_of(&["one two three"]);
        if styled {
            doc.styles = hyperlink_styles();
        }
        // The second word is bold before and after.
        apply_run_patch(
            &mut doc,
            &pos(0, 4),
            &pos(0, 7),
            &RunPatch {
                bold: Some(true),
                ..RunPatch::default()
            },
        );
        let before = doc.clone();
        set_link(
            &mut doc,
            &pos(0, 0),
            &pos(0, 13),
            Some(link("https://a.example/")),
        );
        assert_ne!(doc, before);
        set_link(&mut doc, &pos(0, 0), &pos(0, 13), None);
        assert!(same_content(&doc, &before), "styled: {styled}");
        let p = doc.paragraph(&Path::body(0)).unwrap();
        assert!(p.runs.iter().all(|r| r.link.is_none() && r.style.is_none()));
        assert!(p.runs[1].look.bold);
    }
}

#[test]
fn a_look_the_run_changed_after_linking_survives_removal() {
    let mut doc = doc_of(&["word"]);
    set_link(
        &mut doc,
        &pos(0, 0),
        &pos(0, 4),
        Some(link("https://a.example/")),
    );
    apply_run_patch(
        &mut doc,
        &pos(0, 0),
        &pos(0, 4),
        &RunPatch {
            color: Some(Some([255, 0, 0, 255])),
            ..RunPatch::default()
        },
    );
    set_link(&mut doc, &pos(0, 0), &pos(0, 4), None);
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.runs[0].look.color, Some([255, 0, 0, 255]));
    assert_eq!(p.runs[0].look.underline, None);
}

#[test]
fn linking_the_same_link_changes_nothing_and_shares_the_paragraph() {
    let mut doc = doc_of(&["word", "other"]);
    set_link(&mut doc, &pos(0, 0), &pos(0, 4), Some(link("x")));
    let (a, b) = (doc.body.blocks[0].clone(), doc.body.blocks[1].clone());
    set_link(&mut doc, &pos(0, 0), &pos(0, 4), Some(link("x")));
    assert!(doc.body.blocks[0].is(&a));
    set_link(&mut doc, &pos(1, 0), &pos(1, 5), None);
    assert!(doc.body.blocks[1].is(&b));
}

#[test]
fn changing_the_target_keeps_the_look() {
    let mut doc = doc_of(&["word"]);
    set_link(&mut doc, &pos(0, 0), &pos(0, 4), Some(link("x")));
    let look_before = doc.paragraph(&Path::body(0)).unwrap().runs[0].look.clone();
    set_link(&mut doc, &pos(0, 0), &pos(0, 4), Some(link("y")));
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.runs.len(), 1);
    assert_eq!(p.runs[0].link.as_deref(), Some(&link("y")));
    assert_eq!(p.runs[0].look, look_before);
}

#[test]
fn a_link_over_several_paragraphs_links_each() {
    let mut doc = doc_of(&["abc", "def", "ghi"]);
    set_link(&mut doc, &pos(0, 1), &pos(2, 2), Some(link("x")));
    let linked = |b: usize| {
        doc.paragraph(&Path::body(b))
            .unwrap()
            .runs
            .iter()
            .filter(|r| r.link.is_some())
            .map(|r| r.text.clone())
            .collect::<String>()
    };
    assert_eq!(linked(0), "bc");
    assert_eq!(linked(1), "def");
    assert_eq!(linked(2), "gh");
}

#[test]
fn link_range_finds_the_whole_stretch() {
    let mut doc = doc_of(&["one two three four"]);
    set_link(&mut doc, &pos(0, 4), &pos(0, 13), Some(link("x")));
    // Make a second run inside with another look.
    apply_run_patch(
        &mut doc,
        &pos(0, 8),
        &pos(0, 9),
        &RunPatch {
            bold: Some(true),
            ..RunPatch::default()
        },
    );
    for offset in [4, 6, 8, 12, 13] {
        let (start, end, found) = link_range(&doc, &pos(0, offset)).unwrap();
        assert_eq!((start.offset, end.offset), (4, 13), "at {offset}");
        assert_eq!(found.target, "x");
    }
    assert!(link_range(&doc, &pos(0, 3)).is_none());
    assert!(link_range(&doc, &pos(0, 14)).is_none());
    assert!(link_range(&doc, &pos(5, 0)).is_none());
}

#[test]
fn two_links_next_to_each_other_are_two_stretches() {
    let mut doc = doc_of(&["aabb"]);
    set_link(&mut doc, &pos(0, 0), &pos(0, 2), Some(link("x")));
    set_link(&mut doc, &pos(0, 2), &pos(0, 4), Some(link("y")));
    let (s, e, l) = link_range(&doc, &pos(0, 2)).unwrap();
    assert_eq!((s.offset, e.offset, l.target.as_str()), (2, 4, "y"));
    let (s, e, l) = link_range(&doc, &pos(0, 1)).unwrap();
    assert_eq!((s.offset, e.offset, l.target.as_str()), (0, 2, "x"));
}

#[test]
fn inserting_a_link_types_linked_text() {
    let mut doc = doc_of(&["ab"]);
    let end = insert_link(&mut doc, &pos(0, 1), "LINK", link("https://x.example/"));
    assert_eq!(end, pos(0, 5));
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), "aLINKb");
    let linked: Vec<&Run> = p.runs.iter().filter(|r| r.link.is_some()).collect();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].text, "LINK");
    assert_eq!(insert_link(&mut doc, &pos(0, 0), "", link("x")), pos(0, 0));
}

// ---------------------------------------------------------------------------
// Pictures
// ---------------------------------------------------------------------------

fn png() -> Arc<[u8]> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend([0; 20]);
    Arc::from(bytes)
}

#[test]
fn a_picture_is_an_inline_drawing_in_the_text() {
    let mut doc = doc_of(&["abcd"]);
    let end = insert_picture(
        &mut doc,
        &pos(0, 2),
        png(),
        (914_400, 457_200),
        Some("alt".into()),
    )
    .unwrap();
    assert_eq!(end, pos(0, 2 + crate::OBJECT.len()));
    let p = doc.paragraph(&Path::body(0)).unwrap();
    assert_eq!(p.text(), format!("ab{}cd", crate::OBJECT));
    let Inline::Drawing(drawing) = &p.runs[1].kind else {
        panic!("a drawing");
    };
    assert_eq!(drawing.size, (914_400, 457_200));
    assert_eq!(drawing.float, None);
    assert_eq!(drawing.alt.as_deref(), Some("alt"));
    assert!(matches!(&drawing.content, DrawingContent::Picture(pic) if pic.crop == [0.0; 4]));
}

#[test]
fn other_formats_and_bad_sizes_are_refused() {
    let mut doc = doc_of(&["abcd"]);
    let before = doc.clone();
    let text: Arc<[u8]> = Arc::from(&b"<svg/>"[..]);
    assert_eq!(
        insert_picture(&mut doc, &pos(0, 0), text, (1, 1), None),
        Err(PictureError::UnknownFormat)
    );
    assert_eq!(
        insert_picture(&mut doc, &pos(0, 0), png(), (0, 5), None),
        Err(PictureError::BadSize)
    );
    assert_eq!(
        insert_picture(&mut doc, &pos(3, 0), png(), (5, 5), None),
        Err(PictureError::BadPosition)
    );
    assert_eq!(doc, before);
}

#[test]
fn image_formats_are_known_by_their_first_bytes() {
    assert_eq!(image_format(&png()), Some(ImageFormat::Png));
    assert_eq!(
        image_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some(ImageFormat::Jpeg)
    );
    assert_eq!(image_format(b"GIF89a.."), Some(ImageFormat::Gif));
    assert_eq!(image_format(b"II*\0...."), Some(ImageFormat::Tiff));
    let mut bmp = b"BM".to_vec();
    bmp.extend([0; 30]);
    assert_eq!(image_format(&bmp), Some(ImageFormat::Bmp));
    assert_eq!(image_format(b"RIFFxxxxWEBP"), None);
    assert_eq!(image_format(b""), None);
}

#[test]
fn resizing_a_drawing_changes_only_its_size() {
    let mut doc = doc_of(&["ab", "other"]);
    insert_picture(&mut doc, &pos(0, 1), png(), (100, 200), None).unwrap();
    let other = doc.body.blocks[1].clone();
    resize_drawing(&mut doc, &pos(0, 1), (300, 400));
    let p = doc.paragraph(&Path::body(0)).unwrap();
    let Inline::Drawing(drawing) = &p.runs[1].kind else {
        panic!("a drawing");
    };
    assert_eq!(drawing.size, (300, 400));
    assert!(doc.body.blocks[1].is(&other));
    // Not at a drawing, or a bad size: nothing.
    let before = doc.clone();
    resize_drawing(&mut doc, &pos(0, 0), (1, 1));
    resize_drawing(&mut doc, &pos(0, 1), (0, 1));
    resize_drawing(&mut doc, &pos(9, 0), (1, 1));
    assert_eq!(doc, before);
}

#[test]
fn a_picture_in_a_table_cell() {
    let mut doc = with_table(labelled(1, 1));
    insert_picture(&mut doc, &cpos(1, 0, 0, 0), png(), (10, 10), None).unwrap();
    let p = doc.paragraph(&cpos(1, 0, 0, 0).path).unwrap();
    assert!(matches!(p.runs[0].kind, Inline::Drawing(_)));
}

#[allow(dead_code)]
fn unused(_: Section, _: Drawing) {}

// ---------------------------------------------------------------------------
// Widths
// ---------------------------------------------------------------------------

/// A4 with margins of 1,000 and 800 twips: 11,906 - 1,800 = 10,106 wide.
fn page_doc(blocks: Vec<Block>) -> Document {
    let mut doc = Document::default();
    doc.body.blocks = blocks;
    let mut section = Section::default();
    section.margins.left = 1_000;
    section.margins.right = 800;
    doc.last_section = Arc::new(section);
    doc
}

#[test]
fn the_space_for_a_block_is_the_text_width_or_the_cell() {
    let mut table = labelled(1, 2);
    table.columns = vec![3_000, 2_000];
    table.rows[0].cells[0].margins = [100, 0, 150, 0];
    let doc = page_doc(vec![Block::paragraph(text_para("x")), Block::table(table)]);
    assert_eq!(available_width(&doc, &pos(0, 0)), 10_106);
    // In a cell: its column less its margins.
    assert_eq!(space_for_block(&doc, &cpos(1, 0, 0, 0).path), 3_000 - 250);
    // A cell's own margins default to 115 on each side.
    assert_eq!(available_width(&doc, &cpos(1, 0, 1, 0)), 2_000 - 230);

    // Columns in the section: one column's width.
    let mut doc = page_doc(vec![Block::paragraph(text_para("x"))]);
    let mut section = (*doc.last_section).clone();
    section.columns.count = 2;
    section.columns.space = 706;
    doc.last_section = Arc::new(section);
    assert_eq!(available_width(&doc, &pos(0, 0)), (10_106 - 706) / 2);

    // The section the block is in: an earlier one ends at its paragraph.
    let mut first = text_para("a");
    let mut narrow = Section::default();
    narrow.margins.left = 2_000;
    narrow.margins.right = 2_000;
    first.section = Some(Arc::new(narrow));
    let mut doc = page_doc(vec![
        Block::paragraph(first),
        Block::paragraph(text_para("b")),
    ]);
    doc.last_section = Arc::new(Section::default());
    assert_eq!(available_width(&doc, &pos(0, 0)), 11_906 - 4_000);
    assert_eq!(available_width(&doc, &pos(1, 0)), 11_906 - 2_880);
}

#[test]
fn a_table_wider_than_its_space_is_scaled_down_in_proportion() {
    let mut table = labelled(2, 3);
    table.columns = vec![6_000, 3_000, 3_000];
    let mut doc = page_doc(vec![Block::table(table)]);
    assert!(fit_table(&mut doc, &Path::body(0)));
    let columns = table_at(&doc, 0).columns.clone();
    assert_eq!(columns.iter().sum::<i32>(), 10_106, "{columns:?}");
    assert!((columns[0] - 2 * columns[1]).abs() <= 2, "{columns:?}");
    assert_valid(&doc);
    // It fits now, and one that fits is left alone.
    assert!(!fit_table(&mut doc, &Path::body(0)));
    let mut table = labelled(1, 2);
    table.columns = vec![1_000, 1_000];
    let mut doc = page_doc(vec![Block::table(table)]);
    assert!(!fit_table(&mut doc, &Path::body(0)));
    assert_eq!(table_at(&doc, 0).columns, vec![1_000, 1_000]);
    // The indent counts: the table starts that far in.
    let mut table = labelled(1, 2);
    table.columns = vec![5_000, 5_000];
    table.props.indent = 1_000;
    let mut doc = page_doc(vec![Block::table(table)]);
    assert!(fit_table(&mut doc, &Path::body(0)));
    assert_eq!(
        table_at(&doc, 0).columns.iter().sum::<i32>(),
        10_106 - 1_000
    );
    // Nothing to do for a path that is no table.
    let mut doc = page_doc(vec![Block::paragraph(text_para("x"))]);
    assert!(!fit_table(&mut doc, &Path::body(0)));
}

#[test]
fn a_nested_table_is_fitted_to_the_cell_it_is_in() {
    let mut outer = labelled(1, 1);
    outer.columns = vec![4_000];
    let mut doc = page_doc(vec![Block::table(outer)]);
    let at = cpos(0, 0, 0, 0);
    let width = available_width(&doc, &at);
    assert_eq!(width, 4_000 - 230);
    insert_table(&mut doc, &at, 2, 3, width);
    let inner_path = Path::body(0).cell_block(0, 0, 0);
    let inner = doc.block(&inner_path).and_then(|b| b.as_table()).unwrap();
    assert_eq!(inner.columns.iter().sum::<i32>(), 4_000 - 230);
    // A fourth column wider than the cell: scaled to the cell's width.
    let caret = Pos::new(inner_path.cell_block(0, 0, 0), 0);
    let caret = insert_columns(&mut doc, &caret, &caret, false);
    let table = cell_at(&doc, &caret).unwrap().table;
    assert!(fit_table(&mut doc, &table));
    let inner = doc.block(&inner_path).and_then(|b| b.as_table()).unwrap();
    assert_eq!(inner.columns.len(), 4);
    assert_eq!(inner.columns.iter().sum::<i32>(), 4_000 - 230);
    assert_valid(&doc);
}
