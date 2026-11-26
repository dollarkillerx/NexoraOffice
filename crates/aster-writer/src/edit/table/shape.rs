//! Merging and splitting cells, and the sizes of columns and rows.
//!
//! **Merging.** [`merge_cells`] takes the rectangle of cells the selection
//! covers (the corners are the cells the two ends are in). As in Word the
//! rectangle is widened until it cuts through no merged cell: a cell that
//! sticks out sideways or a vertical merge that goes on above or below is
//! taken in whole. The result is one cell as wide as the rectangle (through
//! [`Cell::span`]) and, when the rectangle has several rows, as high (the
//! first row's cell is a [`VMerge::Start`], the cells below it
//! [`VMerge::Continue`]). The contents of the cells go into the top-left
//! cell in reading order, one after the other; a cell with no text and no
//! object adds nothing (its markers are kept). The merged cell's look is the
//! top-left cell's, except that its right border is the top-right cell's and
//! its bottom border the bottom-left cell's.
//!
//! **Splitting.** [`unmerge_cell`] takes a merged cell back to its grid
//! cells: the content stays in the first. [`split_cell`] cuts a plain cell
//! (one grid column wide, not merged) into `columns` by `rows` cells, as the
//! Split Cells dialog does: the grid column becomes `columns` columns that
//! share its width, the cells of the other rows over that column grow by the
//! same number of grid columns; the other cells of the row become merged
//! downwards over the `rows` rows it now takes. A merged cell is not cut
//! into a different number of cells than it was made of: use
//! [`unmerge_cell`] for it.
//!
//! **Sizes.** The grid decides every cell's width, so widths are set on the
//! grid. [`move_column_border`] moves the line between two columns and the
//! neighbours trade width; the table's outer borders change the table's
//! width (the left one also its indent). No column gets narrower than
//! [`MIN_COLUMN`] (a column that already was narrower is not made narrower
//! still). Every row of the grid moves with the border, also those where a
//! merged cell hides it. [`set_row_height`] sets how high rows are.

use super::*;
use crate::{Inline, RowHeight};

/// The narrowest a column is made by resizing, in twips (a sixth of an
/// inch). Columns the file already has narrower than this stay as they are
/// but are not narrowed further.
pub const MIN_COLUMN: Twips = 240;
/// The widest a table is made by resizing: Word's own limit of 22 inches.
pub const MAX_TABLE_WIDTH: Twips = 31_680;
/// The highest a row is made, in twips.
pub const MAX_ROW_HEIGHT: Twips = 31_680;
/// The lowest an exact row height is: a row cannot be made to vanish by
/// resizing.
pub const MIN_EXACT_ROW: Twips = 20;
/// Each piece a column is cut into by a split gets at least this much.
const MIN_SPLIT_PIECE: Twips = 120;

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

/// A block of the table the selection covers: rows `rows.0..=rows.1`, grid
/// columns `columns.0..columns.1`, tiled by whole cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    rows: (usize, usize),
    columns: (usize, usize),
}

/// The cell of `row` that starts at grid column `column`.
fn cell_starting(row: &Row, column: usize) -> Option<(usize, &Cell)> {
    let mut at = row.grid_before as usize;
    for (i, cell) in row.cells.iter().enumerate() {
        if at == column {
            return Some((i, cell));
        }
        if at > column {
            return None;
        }
        at += cell.span.max(1) as usize;
    }
    None
}

/// The first and last row of the vertical merge the cell starting at grid
/// column `column` of row `r` is in (`r` twice for a cell that is not
/// merged).
fn merge_rows(table: &Table, r: usize, column: usize) -> (usize, usize) {
    let merge_at = |r: usize| {
        table
            .rows
            .get(r)
            .and_then(|row| cell_starting(row, column))
            .map(|(_, c)| c.merge)
    };
    let mut top = r;
    while top > 0 && merge_at(top) == Some(VMerge::Continue) {
        top -= 1;
    }
    let mut bottom = r;
    while merge_at(bottom + 1) == Some(VMerge::Continue) {
        bottom += 1;
    }
    (top, bottom)
}

/// The cells of `row` that overlap grid columns `c0..c1`, with their index.
fn overlapping(row: &Row, c0: usize, c1: usize) -> impl Iterator<Item = (usize, &Cell)> {
    let mut at = row.grid_before as usize;
    row.cells.iter().enumerate().filter(move |(_, cell)| {
        let (start, end) = (at, at + cell.span.max(1) as usize);
        at = end;
        end > c0 && start < c1
    })
}

/// The rectangle that `rows` and `columns` come to once it has been widened
/// to cut through no merged cell, or `None` where the table has holes in it
/// (rows that skip grid columns inside the rectangle).
fn grow_rect(table: &Table, rows: (usize, usize), columns: (usize, usize)) -> Option<Rect> {
    let last = table.rows.len().checked_sub(1)?;
    let (mut r0, mut r1) = (rows.0.min(last), rows.1.min(last));
    let (mut c0, mut c1) = columns;
    if r0 > r1 || c0 >= c1 {
        return None;
    }
    // Every pass that changes the rectangle only grows it, and it cannot
    // outgrow the table.
    for _ in 0..(table.rows.len() + table.columns.len() + 2) * 2 {
        let mut changed = false;
        let (first, last) = (r0, r1);
        for (r, row) in table.rows.iter().enumerate().take(last + 1).skip(first) {
            let mut at = row.grid_before as usize;
            for cell in &row.cells {
                let (start, end) = (at, at + cell.span.max(1) as usize);
                at = end;
                if end <= c0 || start >= c1 {
                    continue;
                }
                if start < c0 {
                    c0 = start;
                    changed = true;
                }
                if end > c1 {
                    c1 = end;
                    changed = true;
                }
                if cell.merge != VMerge::None {
                    let (top, bottom) = merge_rows(table, r, start);
                    if top < r0 {
                        r0 = top;
                        changed = true;
                    }
                    if bottom > r1 {
                        r1 = bottom;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            for row in &table.rows[r0..=r1] {
                let tiled: usize = overlapping(row, c0, c1)
                    .map(|(_, c)| c.span.max(1) as usize)
                    .sum();
                if tiled != c1 - c0 {
                    return None;
                }
            }
            return Some(Rect {
                rows: (r0, r1),
                columns: (c0, c1),
            });
        }
    }
    None
}

/// What a merge of the selection would do: the table, the rectangle and how
/// many cells it holds (a vertical merge counts once).
fn merge_plan(doc: &Document, from: &Pos, to: &Pos) -> Option<(Path, Rect, usize)> {
    let sel = covered(doc, from, to)?;
    let table = table_of(doc, &sel.table)?;
    let rect = grow_rect(table, sel.rows, sel.columns)?;
    let cells = table.rows[rect.rows.0..=rect.rows.1]
        .iter()
        .map(|row| {
            overlapping(row, rect.columns.0, rect.columns.1)
                .filter(|(_, c)| c.merge != VMerge::Continue)
                .count()
        })
        .sum();
    Some((sel.table, rect, cells))
}

/// Whether [`merge_cells`] would do anything for the selection from `from`
/// to `to`: both ends are in the same table, the cells they cover (widened
/// to whole merged cells) are more than one, and the table has no holes in
/// them. For enabling a menu item.
pub fn can_merge_cells(doc: &Document, from: &Pos, to: &Pos) -> bool {
    merge_plan(doc, from, to).is_some_and(|(_, _, cells)| cells > 1)
}

/// Whether the cell has no text and no object: it adds nothing to a merge.
fn is_blank(blocks: &[Block]) -> bool {
    blocks.iter().all(|block| match block {
        Block::Paragraph(p) => p
            .runs
            .iter()
            .all(|r| matches!(r.kind, Inline::Marker) || (r.is_text() && r.text.is_empty())),
        Block::Table(_) => false,
    })
}

/// Merge the cells the selection from `from` to `to` covers into one (see
/// the module documentation); where the caret goes: the start of the merged
/// cell. Does nothing, and gives `from` back, where [`can_merge_cells`] is
/// false.
pub fn merge_cells(doc: &mut Document, from: &Pos, to: &Pos) -> Pos {
    let Some((path, rect, cells)) = merge_plan(doc, from, to) else {
        return from.clone();
    };
    if cells <= 1 {
        return from.clone();
    }
    let Some(table) = table_of(doc, &path) else {
        return from.clone();
    };
    let (r0, r1) = rect.rows;
    let (c0, c1) = rect.columns;
    let Some((_, top_left)) = overlapping(&table.rows[r0], c0, c1).next() else {
        return from.clone();
    };
    let top_left = top_left.clone();
    let top_right = overlapping(&table.rows[r0], c0, c1)
        .last()
        .map_or(top_left.borders[2], |(_, c)| c.borders[2]);
    let bottom_left = overlapping(&table.rows[r1], c0, c1)
        .next()
        .map_or(top_left.borders[3], |(_, c)| c.borders[3]);
    let blank = empty_like(doc, &top_left);

    let Some(table) = table_mut(doc, &path) else {
        return from.clone();
    };
    let span = (c1 - c0) as u32;
    let mut content: Vec<Block> = Vec::new();
    let mut markers: Vec<Run> = Vec::new();
    let mut first_index = 0;
    for (r, row) in table.rows.iter_mut().enumerate().take(r1 + 1).skip(r0) {
        let indexes: Vec<usize> = overlapping(row, c0, c1).map(|(i, _)| i).collect();
        let (Some(&i0), Some(&i1)) = (indexes.first(), indexes.last()) else {
            continue;
        };
        let drained: Vec<Cell> = row.cells.drain(i0..=i1).collect();
        let old_source = drained[0].source.clone();
        for cell in drained {
            if is_blank(&cell.blocks) {
                for block in &cell.blocks {
                    collect_markers(block, &mut markers);
                }
            } else {
                content.extend(cell.blocks);
            }
        }
        let merged = if r == r0 {
            first_index = i0;
            Cell {
                blocks: Vec::new(),
                span,
                merge: if r1 > r0 { VMerge::Start } else { VMerge::None },
                borders: [
                    top_left.borders[0],
                    top_left.borders[1],
                    top_right,
                    bottom_left,
                ],
                ..top_left.clone()
            }
        } else {
            // What continues the merged cell keeps its place in the file
            // and looks like the cell above it.
            Cell {
                blocks: vec![Block::paragraph(blank.clone())],
                span,
                merge: VMerge::Continue,
                borders: [
                    top_left.borders[0],
                    None,
                    top_right,
                    if r == r1 { bottom_left } else { None },
                ],
                source: old_source,
                ..top_left.clone()
            }
        };
        row.cells.insert(i0, merged);
    }
    if content.is_empty() {
        content.push(Block::paragraph(blank.clone()));
    }
    if content.last().is_some_and(|b| b.as_table().is_some()) {
        content.push(Block::paragraph(blank));
    }
    if !markers.is_empty()
        && let Some(Block::Paragraph(p)) = content.last_mut()
    {
        Arc::make_mut(p).runs.extend(markers);
    }
    table.rows[r0].cells[first_index].blocks = content;
    repair_merges(table);
    let caret = cell_start(&path, table, r0, first_index).unwrap_or_else(|| from.clone());
    doc.renumber();
    caret
}

// ---------------------------------------------------------------------------
// Splitting
// ---------------------------------------------------------------------------

/// A cell like `template` with the paragraph `p` and nothing else.
fn blank_like(template: &Cell, p: Paragraph, span: u32, merge: VMerge) -> Cell {
    Cell {
        blocks: vec![Block::paragraph(p)],
        span,
        merge,
        borders: template.borders,
        shading: template.shading,
        margins: template.margins,
        align: template.align,
        layer: template.layer.clone(),
        source: None,
    }
}

/// The cell at `at`, when it can be split into `columns` by `rows`.
fn split_plan(doc: &Document, at: &Pos, columns: usize, rows: usize) -> Option<CellPos> {
    if columns == 0 || rows == 0 || columns * rows <= 1 {
        return None;
    }
    let cp = cell_at(doc, at)?;
    let table = table_of(doc, &cp.table)?;
    let cell = table.rows.get(cp.row)?.cells.get(cp.cell)?;
    if cell.span != 1 || cell.merge != VMerge::None {
        return None;
    }
    if table.columns.len() + columns - 1 > MAX_COLUMNS || table.rows.len() + rows - 1 > MAX_ROWS {
        return None;
    }
    let width = *table.columns.get(cp.column)?;
    if columns > 1 && width < columns as Twips * MIN_SPLIT_PIECE {
        return None;
    }
    Some(cp)
}

/// Whether [`split_cell`] can cut the cell `at` is in into `columns` by
/// `rows`: it is a plain cell (one grid column, not merged), the table
/// stays within the limits and a column is not cut into pieces under a
/// twelfth of an inch. For enabling a menu item.
pub fn can_split_cell(doc: &Document, at: &Pos, columns: usize, rows: usize) -> bool {
    split_plan(doc, at, columns, rows).is_some()
}

/// Cut the cell `at` is in into `columns` cells side by side and `rows`
/// cells one above the other (see the module documentation); the content
/// stays in the first (top left) one and the others are empty with the look
/// of the first paragraph. Where the caret goes: the start of the first
/// cell. Does nothing, and gives `at` back, where [`can_split_cell`] is
/// false.
///
/// Only a plain cell is cut. The widths of the new columns add up to the
/// old column's, the extra twips going to the first ones, so the table does
/// not change in width; the other cells over the column keep their own
/// content and look and only grow in span.
pub fn split_cell(doc: &mut Document, at: &Pos, columns: usize, rows: usize) -> Pos {
    let Some(cp) = split_plan(doc, at, columns, rows) else {
        return at.clone();
    };
    let Some(table) = table_of(doc, &cp.table) else {
        return at.clone();
    };
    let cell = table.rows[cp.row].cells[cp.cell].clone();
    // The paragraphs the new cells start with, for each cell of the row.
    let paragraphs: Vec<Paragraph> = table.rows[cp.row]
        .cells
        .iter()
        .map(|c| empty_like(doc, c))
        .collect();
    let fresh = paragraphs[cp.cell].clone();
    let Some(table) = table_mut(doc, &cp.table) else {
        return at.clone();
    };

    // The columns.
    let extra = columns - 1;
    if columns > 1 {
        let width = table.columns[cp.column];
        let (each, rest) = (width / columns as Twips, width % columns as Twips);
        let pieces: Vec<Twips> = (0..columns as Twips)
            .map(|i| each + Twips::from(i < rest))
            .collect();
        table.columns.splice(cp.column..=cp.column, pieces);
        for (r, row) in table.rows.iter_mut().enumerate() {
            if r == cp.row {
                let new: Vec<Cell> = (0..extra)
                    .map(|_| blank_like(&cell, fresh.clone(), 1, VMerge::None))
                    .collect();
                row.cells.splice(cp.cell + 1..cp.cell + 1, new);
                continue;
            }
            if cp.column < row.grid_before as usize {
                row.grid_before += extra as u32;
                continue;
            }
            let mut at = row.grid_before as usize;
            let mut widened = false;
            for other in &mut row.cells {
                let span = other.span.max(1) as usize;
                if cp.column >= at && cp.column < at + span {
                    other.span += extra as u32;
                    widened = true;
                    break;
                }
                at += span;
            }
            if !widened {
                row.grid_after += extra as u32;
            }
        }
    }

    // The rows: the cells that were cut go on in every new row; the others
    // of the row are merged over all of them.
    if rows > 1 {
        let cut = cp.cell..cp.cell + columns;
        let template = table.rows[cp.row].clone();
        let new_cells: Vec<Cell> = template
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if cut.contains(&i) {
                    blank_like(c, fresh.clone(), 1, VMerge::None)
                } else {
                    // The paragraphs were made for the cells as they were
                    // before the columns were cut.
                    let before = if i < cp.cell { i } else { i - extra };
                    let p = paragraphs.get(before).unwrap_or(&fresh).clone();
                    blank_like(c, p, c.span, VMerge::Continue)
                }
            })
            .collect();
        for (i, c) in table.rows[cp.row].cells.iter_mut().enumerate() {
            if !cut.contains(&i) && c.merge == VMerge::None {
                c.merge = VMerge::Start;
            }
        }
        let row = Row {
            cells: new_cells,
            header: false,
            source: None,
            ..template
        };
        for k in 0..rows - 1 {
            table.rows.insert(cp.row + 1 + k, row.clone());
        }
    }
    repair_merges(table);
    let caret = cell_start(&cp.table, table, cp.row, cp.cell).unwrap_or_else(|| at.clone());
    doc.renumber();
    caret
}

/// Whether the cell `at` is in is merged (wider than a grid column, or
/// joined with cells above or below), so that [`unmerge_cell`] has
/// something to take apart.
pub fn can_unmerge_cell(doc: &Document, at: &Pos) -> bool {
    cell_at(doc, at)
        .and_then(|cp| {
            let cell = table_of(doc, &cp.table)?
                .rows
                .get(cp.row)?
                .cells
                .get(cp.cell)?;
            Some(cell.span > 1 || cell.merge != VMerge::None)
        })
        .unwrap_or(false)
}

/// Take the merged cell `at` is in back to the grid cells it covers: every
/// row of a vertical merge gets its own cells and every cell wider than a
/// grid column is cut into one cell per column. The content stays in the
/// first (top left) cell; the others are empty with the look of the merged
/// cell. Where the caret goes: the start of the first cell. Does nothing, and
/// gives `at` back, where the cell is not merged.
pub fn unmerge_cell(doc: &mut Document, at: &Pos) -> Pos {
    if !can_unmerge_cell(doc, at) {
        return at.clone();
    }
    let Some(cp) = cell_at(doc, at) else {
        return at.clone();
    };
    let Some(table) = table_of(doc, &cp.table) else {
        return at.clone();
    };
    let (top, bottom) = merge_rows(table, cp.row, cp.column);
    let span = cp.span as u32;
    // A paragraph to start each new cell with, per row of the merge.
    let blanks: Vec<Paragraph> = (top..=bottom)
        .filter_map(|r| cell_starting(&table.rows[r], cp.column))
        .map(|(_, c)| empty_like(doc, c))
        .collect();
    let Some(table) = table_mut(doc, &cp.table) else {
        return at.clone();
    };
    for (k, r) in (top..=bottom).enumerate() {
        let Some((i, _)) = cell_starting(&table.rows[r], cp.column) else {
            continue;
        };
        let row = &mut table.rows[r];
        let old = row.cells[i].clone();
        row.cells[i].span = 1;
        row.cells[i].merge = VMerge::None;
        if r != top {
            // Not the first of the merge: its content is nothing.
            row.cells[i].blocks = vec![Block::paragraph(blanks[k].clone())];
        }
        let extra: Vec<Cell> = (1..span)
            .map(|_| blank_like(&old, blanks[k].clone(), 1, VMerge::None))
            .collect();
        row.cells.splice(i + 1..i + 1, extra);
    }
    repair_merges(table);
    let caret = cell_start(&cp.table, table, top, cp.cell).unwrap_or_else(|| at.clone());
    doc.renumber();
    caret
}

// ---------------------------------------------------------------------------
// Sizes
// ---------------------------------------------------------------------------

/// How far `width` can go down to `floor`: a column narrower than the
/// minimum may stay as it is but not shrink.
fn floor_of(width: Twips) -> Twips {
    MIN_COLUMN.min(width)
}

/// Make column `column` (a grid column of the table at `table`) `width`
/// twips wide; the table changes in width by the difference and the other
/// columns stay. `width` is held between [`MIN_COLUMN`] and what keeps the
/// table within [`MAX_TABLE_WIDTH`]. Returns the width the column has now,
/// or `None` for no such column.
pub fn set_column_width(
    doc: &mut Document,
    table: &Path,
    column: usize,
    width: Twips,
) -> Option<Twips> {
    let t = table_of(doc, table)?;
    let old = *t.columns.get(column)?;
    let others: Twips = t.columns.iter().sum::<Twips>() - old;
    let high = (MAX_TABLE_WIDTH - others).max(old);
    let width = width.clamp(floor_of(old), high);
    let t = table_mut(doc, table)?;
    t.columns[column] = width;
    Some(width)
}

/// Move the border `border` of the table at `table` by `delta` twips to the
/// right (to the left when negative). Border `i` is the line between column
/// `i - 1` and column `i`: the two trade width and the table stays as wide.
/// Border 0 is the table's left edge: the first column loses what the
/// indent gains. The last border is the right edge: the last column and
/// with it the table change width. Neither column goes under
/// [`MIN_COLUMN`] (or, where it already was, under what it was) and the
/// table stays within [`MAX_TABLE_WIDTH`]; `delta` is cut to what fits.
/// Returns how far the border moved, 0 for no such border or no room.
pub fn move_column_border(doc: &mut Document, table: &Path, border: usize, delta: Twips) -> Twips {
    let Some(t) = table_of(doc, table) else {
        return 0;
    };
    let n = t.columns.len();
    if border > n || n == 0 {
        return 0;
    }
    let total: Twips = t.columns.iter().sum();
    let moved = if border == 0 {
        // Positive: the edge moves right and the first column shrinks.
        let first = t.columns[0];
        delta.clamp(-(MAX_TABLE_WIDTH - total).max(0), first - floor_of(first))
    } else if border == n {
        let last = t.columns[n - 1];
        delta.clamp(floor_of(last) - last, (MAX_TABLE_WIDTH - total).max(0))
    } else {
        let (left, right) = (t.columns[border - 1], t.columns[border]);
        delta.clamp(floor_of(left) - left, right - floor_of(right))
    };
    if moved == 0 {
        return 0;
    }
    let Some(t) = table_mut(doc, table) else {
        return 0;
    };
    if border == 0 {
        t.columns[0] -= moved;
        t.props.indent += moved;
    } else if border == n {
        t.columns[n - 1] += moved;
    } else {
        t.columns[border - 1] += moved;
        t.columns[border] -= moved;
    }
    moved
}

/// Set the height of the rows the selection from `from` to `to` covers (one
/// for a caret). `AtLeast` and `Exactly` are held to 0 or [`MIN_EXACT_ROW`]
/// up to [`MAX_ROW_HEIGHT`] twips. Returns whether anything changed.
pub fn set_row_height(doc: &mut Document, from: &Pos, to: &Pos, height: RowHeight) -> bool {
    let Some(sel) = covered(doc, from, to) else {
        return false;
    };
    let height = match height {
        RowHeight::Auto => RowHeight::Auto,
        RowHeight::AtLeast(h) => RowHeight::AtLeast(h.clamp(0, MAX_ROW_HEIGHT)),
        RowHeight::Exactly(h) => RowHeight::Exactly(h.clamp(MIN_EXACT_ROW, MAX_ROW_HEIGHT)),
    };
    let Some(table) = table_mut(doc, &sel.table) else {
        return false;
    };
    let mut changed = false;
    for row in table
        .rows
        .iter_mut()
        .skip(sel.rows.0)
        .take(sel.rows.1 - sel.rows.0 + 1)
    {
        if row.height != height {
            row.height = height;
            changed = true;
        }
    }
    changed
}

/// The block of cells a selection that runs over several cells of one table
/// selects: whole rows `rows.0..=rows.1` and grid columns
/// `columns.0..columns.1`, widened over merged cells as a merge would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellRange {
    pub table: Path,
    pub rows: (usize, usize),
    pub columns: (usize, usize),
}

/// The cells the selection from `from` to `to` selects when its ends are in
/// two different cells of one table (the innermost one that holds both): a
/// selection inside one cell, or that is not in a table, is text, so `None`.
/// Where the table has holes in it the cells are not widened.
pub fn cell_range(doc: &Document, from: &Pos, to: &Pos) -> Option<CellRange> {
    let a = cells_around(doc, from);
    let b = cells_around(doc, to);
    for cell in a.iter().rev() {
        let Some(other) = b.iter().find(|o| o.table == cell.table) else {
            continue;
        };
        if (cell.row, cell.cell) == (other.row, other.cell) {
            // Both in one cell: the text of it, even in a table within it.
            return None;
        }
        let rows = (cell.row.min(other.row), cell.row.max(other.row));
        let columns = (
            cell.column.min(other.column),
            (cell.column + cell.span).max(other.column + other.span),
        );
        let table = table_of(doc, &cell.table)?;
        let grown = grow_rect(table, rows, columns);
        return Some(CellRange {
            table: cell.table.clone(),
            rows: grown.map_or(rows, |r| r.rows),
            columns: grown.map_or(columns, |r| r.columns),
        });
    }
    None
}

/// The start of the first paragraph of the first cell of row `row` of the
/// table at `table`: a place in the row, for the operations that take the
/// ends of a selection ([`set_row_height`]).
pub fn row_start(doc: &Document, table: &Path, row: usize) -> Option<Pos> {
    cell_start(table, table_of(doc, table)?, row, 0)
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
