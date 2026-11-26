//! Tables: making one, adding and removing rows and columns, merging and
//! splitting cells, and sizes (see `shape` for the last three).
//!
//! **Which table.** Row and column operations take two positions, the ends
//! of a selection. The table is the innermost one that holds the first
//! position and also the second (the second may be deeper inside it, in a
//! nested table: its cell in this table is the one that holds it); when the
//! second is outside every table the first holds, only the first counts.
//! Positions outside any table make the operation do nothing and give the
//! first position back.
//!
//! **Merged cells stay valid.** After every operation, in every row
//! `grid_before + sum(span) + grid_after` is the number of grid columns,
//! every cell has a paragraph, and every [`VMerge::Continue`] cell has a
//! [`VMerge::Start`] or `Continue` cell directly above it that covers the
//! same grid columns ([`check_table`] tests all three). The rules that keep
//! it so are in the operations' documentation; the repair after removing
//! rows or columns is this: a `Continue` cell with no merged cell above it
//! becomes the start of the merge if another `Continue` cell follows it
//! directly, and an ordinary cell otherwise.
//!
//! **Table styles.** A cell's table-style layer ([`Cell::layer`]) is copied
//! with the cell it is modelled on. Conditional formats of a table style
//! (first row bold, banded rows) depend on a cell's place in the table; they
//! are not worked out again after rows or columns come or go, as Word does
//! when the file is next opened. Looks already in the model stay.

use std::sync::Arc;

use super::{collect_markers, locate, snap, split_raw, style_base};
use crate::{
    Block, Border, BorderStyle, Cell, CellLayer, Document, Paragraph, Path, Pos, Row, Run,
    StyleKind, Table, TableProps, Twips, VMerge, paragraphs_of,
};

mod shape;
pub use shape::{
    MAX_ROW_HEIGHT, MAX_TABLE_WIDTH, MIN_COLUMN, MIN_EXACT_ROW, can_merge_cells, can_split_cell,
    can_unmerge_cell, merge_cells, move_column_border, set_column_width, set_row_height,
    split_cell, unmerge_cell,
};

/// Most columns a table made here has (Word's own limit).
const MAX_COLUMNS: usize = 63;
/// Most rows a table made here has.
const MAX_ROWS: usize = 10_000;

// ---------------------------------------------------------------------------
// Finding the table
// ---------------------------------------------------------------------------

/// A table cell found by a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellPos {
    /// The table: the path of its block.
    pub table: Path,
    pub row: usize,
    /// Index into the row's cells.
    pub cell: usize,
    /// The first grid column the cell covers.
    pub column: usize,
    /// How many grid columns it covers.
    pub span: usize,
}

impl CellPos {
    /// The path of block `block` of the cell.
    pub fn block(&self, block: usize) -> Path {
        self.table.cell_block(self.row, self.cell, block)
    }
}

/// The table cells a position is in, outermost first.
fn cells_around(doc: &Document, pos: &Pos) -> Vec<CellPos> {
    let steps = &pos.path.steps;
    let mut out = Vec::new();
    let mut at = 1;
    while at + 3 <= steps.len() {
        let table = pos.path.with_steps(steps[..at].to_vec());
        let (row, cell) = (steps[at] as usize, steps[at + 1] as usize);
        let Some(t) = doc.block(&table).and_then(|b| b.as_table()) else {
            break;
        };
        let Some(r) = t.rows.get(row) else { break };
        let Some(c) = r.cells.get(cell) else { break };
        let column = r.grid_before as usize
            + r.cells[..cell]
                .iter()
                .map(|c| c.span.max(1) as usize)
                .sum::<usize>();
        out.push(CellPos {
            table,
            row,
            cell,
            column,
            span: c.span.max(1) as usize,
        });
        at += 3;
    }
    out
}

/// The innermost table cell `pos` is in, or `None` outside tables.
pub fn cell_at(doc: &Document, pos: &Pos) -> Option<CellPos> {
    cells_around(doc, pos).pop()
}

/// What a selection covers of one table: rows `rows.0..=rows.1` and grid
/// columns `columns.0..columns.1`.
struct Covered {
    table: Path,
    rows: (usize, usize),
    columns: (usize, usize),
    /// The cell of the first position (its row and cell index).
    first: (usize, usize),
}

fn covered(doc: &Document, from: &Pos, to: &Pos) -> Option<Covered> {
    let a = cells_around(doc, from);
    let b = cells_around(doc, to);
    let (a, b) = if a.is_empty() {
        (b, Vec::new())
    } else {
        (a, b)
    };
    let first = a.last()?;
    // The innermost table of `from` that also holds `to`.
    for cell in a.iter().rev() {
        if let Some(other) = b.iter().find(|o| o.table == cell.table) {
            return Some(Covered {
                table: cell.table.clone(),
                rows: (cell.row.min(other.row), cell.row.max(other.row)),
                columns: (
                    cell.column.min(other.column),
                    (cell.column + cell.span).max(other.column + other.span),
                ),
                first: (cell.row, cell.cell),
            });
        }
    }
    Some(Covered {
        table: first.table.clone(),
        rows: (first.row, first.row),
        columns: (first.column, first.column + first.span),
        first: (first.row, first.cell),
    })
}

fn table_of<'a>(doc: &'a Document, path: &Path) -> Option<&'a Table> {
    doc.block(path)?.as_table()
}

fn table_mut<'a>(doc: &'a mut Document, path: &Path) -> Option<&'a mut Table> {
    match doc.block_mut(path)? {
        Block::Table(table) => Some(Arc::make_mut(table)),
        Block::Paragraph(_) => None,
    }
}

/// The start of the first paragraph in cell `cell` of row `row` of the
/// table at `table_path`.
fn cell_start(table_path: &Path, table: &Table, row: usize, cell: usize) -> Option<Pos> {
    let cell_ref = table.rows.get(row)?.cells.get(cell)?;
    let (inner, _) = paragraphs_of(&cell_ref.blocks).into_iter().next()?;
    let mut steps = table_path.steps.clone();
    steps.extend([row as u32, cell as u32]);
    steps.extend(inner.steps);
    Some(Pos::new(table_path.with_steps(steps), 0))
}

// ---------------------------------------------------------------------------
// Widths
// ---------------------------------------------------------------------------

/// The width in twips of the space a block at `path` (a paragraph or a
/// table) sits in: the cell it is in (the grid columns it covers less the
/// cell's left and right margins), or else the text width of its section (a
/// column's, in a section with columns: the first). At least 1.
pub fn space_for_block(doc: &Document, path: &Path) -> Twips {
    let steps = &path.steps;
    if steps.len() >= 4 {
        let n = steps.len();
        let table_path = path.with_steps(steps[..n - 3].to_vec());
        let (row, cell) = (steps[n - 3] as usize, steps[n - 2] as usize);
        if let Some(table) = table_of(doc, &table_path)
            && let Some(r) = table.rows.get(row)
            && let Some(c) = r.cells.get(cell)
        {
            let column = r.grid_before as usize
                + r.cells[..cell]
                    .iter()
                    .map(|c| c.span.max(1) as usize)
                    .sum::<usize>();
            let span = c.span.max(1) as usize;
            let width: Twips = table.columns.iter().skip(column).take(span).copied().sum();
            return (width - c.margins[0] - c.margins[2]).max(1);
        }
    }
    let top = path.top();
    let section = doc
        .sections()
        .into_iter()
        .find(|(last, _)| *last >= top)
        .map_or(&*doc.last_section, |(_, section)| &**section);
    let text =
        section.page.0 - section.margins.left - section.margins.right - section.margins.gutter;
    let columns = &section.columns;
    let width = if columns.count > 1 {
        match columns.widths.first() {
            Some((width, _)) => *width,
            None => (text - columns.space * (columns.count as Twips - 1)) / columns.count as Twips,
        }
    } else {
        text
    };
    width.max(1)
}

/// The width a table made at `at` has room for: [`space_for_block`] of the
/// paragraph the table goes before or after. Pass it to [`insert_table`].
pub fn available_width(doc: &Document, at: &Pos) -> Twips {
    space_for_block(doc, &at.path)
}

/// Scale the columns of the table at `table` down, in proportion, when the
/// table (its columns and its indent) is wider than the space it sits in
/// ([`space_for_block`]); each column keeps at least 1 twip and the widths
/// add up to the space exactly. Returns whether anything changed. A table
/// that fits is left alone.
pub fn fit_table(doc: &mut Document, table: &Path) -> bool {
    let space = space_for_block(doc, table);
    let Some(t) = table_of(doc, table) else {
        return false;
    };
    let room = (space - t.props.indent).max(t.columns.len() as Twips);
    let total: i64 = t.columns.iter().map(|w| i64::from(*w)).sum();
    if total <= i64::from(room) || t.columns.is_empty() {
        return false;
    }
    let mut widths: Vec<Twips> = t
        .columns
        .iter()
        .map(|w| ((i64::from(*w) * i64::from(room) + total / 2) / total).max(1) as Twips)
        .collect();
    // Rounding: the last column takes up the difference.
    let sum: Twips = widths.iter().sum();
    if let Some(last) = widths.last_mut() {
        *last = (*last + (room - sum)).max(1);
    }
    match table_mut(doc, table) {
        Some(t) => {
            t.columns = widths;
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Checking
// ---------------------------------------------------------------------------

/// Whether the table (and the tables inside its cells) satisfy the rules
/// the editing operations keep: row widths add up to the grid, every cell
/// has a paragraph, every vertically merged continuation has a merged cell
/// above it over the same columns. The first problem found, described.
pub fn check_table(table: &Table) -> Result<(), String> {
    let grid = table.columns.len();
    for (r, row) in table.rows.iter().enumerate() {
        let width: usize = row.cells.iter().map(|c| c.span as usize).sum::<usize>()
            + row.grid_before as usize
            + row.grid_after as usize;
        if width != grid {
            return Err(format!("row {r} covers {width} of {grid} grid columns"));
        }
        let mut column = row.grid_before as usize;
        for (c, cell) in row.cells.iter().enumerate() {
            if cell.span == 0 {
                return Err(format!("cell {c} of row {r} has span 0"));
            }
            if paragraphs_of(&cell.blocks).is_empty() {
                return Err(format!("cell {c} of row {r} has no paragraph"));
            }
            if cell.merge == VMerge::Continue {
                let above = r
                    .checked_sub(1)
                    .and_then(|r| table.rows.get(r))
                    .and_then(|row| find_cell(row, column, cell.span as usize));
                if !above.is_some_and(|a| a.merge != VMerge::None) {
                    return Err(format!(
                        "cell {c} of row {r} continues a merge that is not above it"
                    ));
                }
            }
            for block in &cell.blocks {
                if let Block::Table(inner) = block {
                    check_table(inner)?;
                }
            }
            column += cell.span as usize;
        }
    }
    Ok(())
}

/// The cell of `row` that covers exactly grid columns `column..column +
/// span`.
fn find_cell(row: &Row, column: usize, span: usize) -> Option<&Cell> {
    let mut at = row.grid_before as usize;
    for cell in &row.cells {
        if at == column && cell.span as usize == span {
            return Some(cell);
        }
        at += cell.span as usize;
    }
    None
}

/// Make vertical merges valid again (see the module documentation).
fn repair_merges(table: &mut Table) {
    for r in 0..table.rows.len() {
        let mut fixes = Vec::new();
        {
            let above = r.checked_sub(1).map(|a| &table.rows[a]);
            let below = table.rows.get(r + 1);
            let row = &table.rows[r];
            let mut column = row.grid_before as usize;
            for (c, cell) in row.cells.iter().enumerate() {
                let span = cell.span.max(1) as usize;
                if cell.merge == VMerge::Continue
                    && !above
                        .and_then(|a| find_cell(a, column, span))
                        .is_some_and(|a| a.merge != VMerge::None)
                {
                    let followed = below
                        .and_then(|b| find_cell(b, column, span))
                        .is_some_and(|b| b.merge == VMerge::Continue);
                    fixes.push((
                        c,
                        if followed {
                            VMerge::Start
                        } else {
                            VMerge::None
                        },
                    ));
                }
                column += span;
            }
        }
        for (c, merge) in fixes {
            table.rows[r].cells[c].merge = merge;
        }
    }
}

// ---------------------------------------------------------------------------
// Making paragraphs and cells
// ---------------------------------------------------------------------------

/// An empty paragraph in the document's default style.
fn default_paragraph(doc: &Document) -> Paragraph {
    let (props, base) = doc.styles.paragraph_look(None);
    Paragraph {
        props: props.clone(),
        base: base.clone(),
        mark: base.clone(),
        ..Paragraph::default()
    }
}

/// An empty paragraph with the look of the first paragraph of `cell`.
fn empty_like(doc: &Document, cell: &Cell) -> Paragraph {
    match paragraphs_of(&cell.blocks).first() {
        Some((_, p)) => Paragraph {
            props: p.props.clone(),
            base: p.base.clone(),
            mark: p.mark.clone(),
            ..Paragraph::default()
        },
        None => default_paragraph(doc),
    }
}

/// A cell with `template`'s structure (borders, shading, margins, vertical
/// alignment, table-style layer) and one empty paragraph that looks like the
/// template's first. One column wide and not merged downwards unless
/// `merge` says so.
fn cell_like(doc: &Document, template: &Cell, span: u32, merge: VMerge) -> Cell {
    Cell {
        blocks: vec![Block::paragraph(empty_like(doc, template))],
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

/// The paragraph a new one after `p` would be where Enter is pressed at the
/// end of it (the style's next style, direct formatting kept, no list, no
/// section), moved from the cell layer `old` to `new`.
fn paragraph_after(
    doc: &Document,
    p: &Paragraph,
    old: Option<&CellLayer>,
    new: Option<&CellLayer>,
) -> Paragraph {
    let styles = &doc.styles;
    let same_id = p.props.style.clone();
    let next = same_id
        .as_deref()
        .and_then(|id| styles.get(id))
        .and_then(|style| style.next.clone())
        .filter(|id| Some(&**id) != same_id.as_deref())
        .filter(|id| {
            styles
                .get(id)
                .is_some_and(|style| style.kind == StyleKind::Paragraph)
        });
    let (old_props, old_base) = style_base(styles, old, same_id.as_deref());
    let (props, base, mark) = match &next {
        Some(next) => {
            let (props, base) = style_base(styles, new, Some(&**next));
            (props, base.clone(), base)
        }
        None => {
            let (new_props, base) = style_base(styles, new, same_id.as_deref());
            let mut props = new_props.clone();
            macro_rules! direct {
                ($($f:ident),+) => {
                    $( if p.props.$f != old_props.$f { props.$f = p.props.$f.clone(); } )+
                };
            }
            direct!(
                align,
                indent_left,
                indent_right,
                first_line,
                space_before,
                space_after,
                line,
                contextual_spacing,
                keep_next,
                keep_lines,
                page_break_before,
                widow_control,
                outline_level,
                tabs,
                borders,
                border_between,
                shading,
                snap_to_grid
            );
            let mut mark = p.mark.clone();
            super::rebase_look(&mut mark, &old_base, &base);
            (props, base, mark)
        }
    };
    let mut props = props;
    props.list = None;
    Paragraph {
        props,
        base,
        mark,
        ..Paragraph::default()
    }
}

// ---------------------------------------------------------------------------
// Inserting a table
// ---------------------------------------------------------------------------

/// A new table of `rows` by `columns` cells, `width` twips wide in all with
/// equal columns, at `at`; where the caret goes: the start of the first
/// cell. More than 63 columns, more than 10,000 rows, none of either, or a
/// bad position does nothing and gives `at` back.
///
/// **Where it goes**, as in Word. At the start of a paragraph (empty or not)
/// the table goes before it. Anywhere else the paragraph is cut at the caret
/// and the table goes between the two halves, so that at the end of a
/// paragraph an empty paragraph follows the table; a table is always
/// followed by a paragraph. In a table cell the table is a nested table in
/// that cell.
///
/// **What it is.** Every cell has a single thin black border on all four
/// sides ([`Cell::borders`]), the default margins, and one empty paragraph:
/// the paragraph a new one after the paragraph at `at` would be (its style's
/// next style, its direct formatting; no list), with the look the table
/// style gives it. The table style is `TableGrid` when the document's style
/// sheet has that table style, none otherwise. Layout is automatic, the
/// alignment left, the indent 0. Widths are `width` divided as evenly as
/// whole twips allow (the first columns get the extra twips).
pub fn insert_table(
    doc: &mut Document,
    at: &Pos,
    rows: usize,
    columns: usize,
    width: Twips,
) -> Pos {
    if rows == 0 || columns == 0 || rows > MAX_ROWS || columns > MAX_COLUMNS {
        return at.clone();
    }
    let Some(at) = locate(doc, at) else {
        return at.clone();
    };
    let Some(p) = doc.paragraph(&at.path) else {
        return at;
    };
    let styles = doc.styles.clone();
    let old_layer = super::cell_layer_of(doc, &at.path);
    let table_style = styles
        .get("TableGrid")
        .filter(|style| style.kind == StyleKind::Table);
    let layer = table_style.map(|style| {
        let mut para = style.para.clone();
        para.style = None;
        Arc::new(CellLayer {
            run: style.run.clone(),
            para,
        })
    });
    let paragraph = paragraph_after(doc, p, old_layer.as_deref(), layer.as_deref());

    let width = width.max(columns as Twips);
    let (each, extra) = (width / columns as Twips, width % columns as Twips);
    let widths: Vec<Twips> = (0..columns as Twips)
        .map(|i| each + Twips::from(i < extra))
        .collect();
    let props = TableProps {
        style: table_style.map(|style| style.id.clone()),
        ..TableProps::default()
    };
    let line = Border {
        style: BorderStyle::Single,
        width: 0.5,
        color: None,
        space: 0.0,
    };
    let cell = Cell {
        blocks: vec![Block::paragraph(paragraph)],
        borders: [Some(line); 4],
        margins: props.cell_margins,
        layer,
        ..Cell::default()
    };
    let table = Table {
        columns: widths,
        rows: (0..rows)
            .map(|_| Row {
                cells: vec![cell.clone(); columns],
                ..Row::default()
            })
            .collect(),
        props,
        source: None,
    };

    let index = at.path.steps.last().copied().unwrap_or(0) as usize;
    let mut steps = at.path.steps.clone();
    let place = if at.offset > 0 {
        split_raw(doc, &at, false);
        index + 1
    } else {
        index
    };
    *steps.last_mut().expect("a path has steps") = place as u32;
    let Some(siblings) = doc.siblings_mut(&at.path) else {
        return at;
    };
    if place > siblings.len() {
        return at;
    }
    siblings.insert(place, Block::table(table));
    doc.renumber();
    Pos::new(at.path.with_steps(steps).cell_block(0, 0, 0), 0)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Whether the merge of the cell covering `column..column + span` in row
/// `b - 1` goes on in row `b`.
fn merge_continues(table: &Table, b: usize, column: usize, span: usize) -> bool {
    b > 0
        && match (table.rows.get(b - 1), table.rows.get(b)) {
            (Some(above), Some(below)) => {
                find_cell(below, column, span).is_some_and(|c| c.merge == VMerge::Continue)
                    && find_cell(above, column, span).is_some_and(|c| c.merge != VMerge::None)
            }
            _ => false,
        }
}

/// Add as many rows as the selection from `from` to `to` covers (one for a
/// caret), above the first row it touches or below the last; where the
/// caret goes: the first cell of the first new row.
///
/// Each new row copies the row next to it (the first covered row when above,
/// the last when below): its cells, their spans, borders, shading, margins
/// and vertical alignment, the columns it skips, its height and whether it
/// may split; not that it repeats as a header. Cells are empty except for
/// one paragraph with the look of the copied cell's first paragraph.
///
/// **Merged cells.** A new cell is a continuation of a vertical merge
/// exactly when the new row lands inside one, that is, when the merged cell
/// goes on from the row before the new rows into the row after them over
/// the same columns; otherwise it is an ordinary cell. A new row above the
/// start of a merge, or below its end, is outside it.
pub fn insert_rows(doc: &mut Document, from: &Pos, to: &Pos, above: bool) -> Pos {
    let Some(sel) = covered(doc, from, to) else {
        return from.clone();
    };
    let Some(table) = table_of(doc, &sel.table) else {
        return from.clone();
    };
    let count = sel.rows.1 - sel.rows.0 + 1;
    let at = if above { sel.rows.0 } else { sel.rows.1 + 1 };
    let template = if above { sel.rows.0 } else { sel.rows.1 };
    let Some(model) = table.rows.get(template) else {
        return from.clone();
    };
    let mut row = Row {
        cells: Vec::with_capacity(model.cells.len()),
        header: false,
        source: None,
        ..model.clone()
    };
    let mut column = model.grid_before as usize;
    for cell in &model.cells {
        let span = cell.span.max(1);
        let merge = if merge_continues(table, at, column, span as usize) {
            VMerge::Continue
        } else {
            VMerge::None
        };
        row.cells.push(cell_like(doc, cell, span, merge));
        column += span as usize;
    }
    let Some(table) = table_mut(doc, &sel.table) else {
        return from.clone();
    };
    for k in 0..count {
        table.rows.insert(at + k, row.clone());
    }
    repair_merges(table);
    let caret = cell_start(&sel.table, table, at, 0).unwrap_or_else(|| from.clone());
    doc.renumber();
    caret
}

/// Take out the rows the selection covers; where the caret goes: the same
/// cell of the row that took the place of the first removed one (the last
/// row when the removed ones were at the end). When the selection covers
/// every row, the whole table goes ([`delete_table`]).
///
/// **Merged cells.** A merged cell that goes on below the removed rows keeps
/// its place; when the removed rows held its start, the cell that was below
/// them becomes the start (and is empty: the content was in the removed
/// row). Markers (bookmarks and the like) in what is removed move to the
/// caret's paragraph.
pub fn delete_rows(doc: &mut Document, from: &Pos, to: &Pos) -> Pos {
    let Some(sel) = covered(doc, from, to) else {
        return from.clone();
    };
    let Some(table) = table_of(doc, &sel.table) else {
        return from.clone();
    };
    let last = sel.rows.1.min(table.rows.len().saturating_sub(1));
    if sel.rows.0 == 0 && last + 1 >= table.rows.len() {
        return remove_table(doc, &sel.table);
    }
    let Some(table) = table_mut(doc, &sel.table) else {
        return from.clone();
    };
    let mut moved: Vec<Run> = Vec::new();
    for row in table.rows.drain(sel.rows.0..=last) {
        for cell in &row.cells {
            for block in &cell.blocks {
                collect_markers(block, &mut moved);
            }
        }
    }
    repair_merges(table);
    let row = sel.rows.0.min(table.rows.len() - 1);
    let cell = sel
        .first
        .1
        .min(table.rows[row].cells.len().saturating_sub(1));
    let caret = cell_start(&sel.table, table, row, cell).unwrap_or_else(|| from.clone());
    rehome(doc, &caret, moved);
    doc.renumber();
    caret
}

/// Markers put at the start of the paragraph at `caret`.
fn rehome(doc: &mut Document, caret: &Pos, markers: Vec<Run>) {
    if markers.is_empty() {
        return;
    }
    if let Some(p) = doc.paragraph_mut(&caret.path) {
        p.runs.splice(0..0, markers);
    }
}

// ---------------------------------------------------------------------------
// Columns
// ---------------------------------------------------------------------------

/// Add as many columns as the selection covers (one for a caret) to the left
/// of the first column it touches or to the right of the last; where the
/// caret goes: the new cell in the first selected row.
///
/// **Width.** Each new column is as wide as the column next to it (the first
/// covered column when on the left, the last when on the right). The table
/// grows by the widths added; other columns keep theirs. A view that must
/// keep the table inside the margins scales the grid afterwards.
///
/// **What each row gets.** A cell that covers the place the new column goes
/// through (a spanning cell, merged cells in every row of the merge) is
/// widened by one grid column for each new column. Where the new column goes
/// between two cells, a new cell goes there: it copies the neighbouring cell
/// (the one on the side the selection is on) as [`insert_rows`] does, is one
/// column wide, and is never merged downwards. In a row whose empty
/// columns (`grid_before`, `grid_after`) the place is in, the empty stretch
/// grows instead of getting a cell.
pub fn insert_columns(doc: &mut Document, from: &Pos, to: &Pos, left: bool) -> Pos {
    let Some(sel) = covered(doc, from, to) else {
        return from.clone();
    };
    let Some(table) = table_of(doc, &sel.table) else {
        return from.clone();
    };
    let count = sel.columns.1.saturating_sub(sel.columns.0).max(1);
    let grid = table.columns.len();
    let at = if left { sel.columns.0 } else { sel.columns.1 }.min(grid);
    let neighbour = if left {
        sel.columns.0.min(grid.saturating_sub(1))
    } else {
        sel.columns.1.saturating_sub(1).min(grid.saturating_sub(1))
    };
    let width = table.columns.get(neighbour).copied().unwrap_or(1_440);

    // New cells first, from the table as it is.
    let mut additions: Vec<Option<(usize, Cell)>> = Vec::with_capacity(table.rows.len());
    for row in &table.rows {
        let mut column = row.grid_before as usize;
        let mut made = None;
        if at >= column {
            for (i, cell) in row.cells.iter().enumerate() {
                let span = cell.span.max(1) as usize;
                if at == column {
                    let model = if left || i == 0 {
                        cell
                    } else {
                        &row.cells[i - 1]
                    };
                    made = Some((i, cell_like(doc, model, 1, VMerge::None)));
                    break;
                }
                column += span;
            }
            if made.is_none()
                && at == column
                && let Some(last) = row.cells.last()
            {
                made = Some((row.cells.len(), cell_like(doc, last, 1, VMerge::None)));
            }
        }
        additions.push(made);
    }

    let Some(table) = table_mut(doc, &sel.table) else {
        return from.clone();
    };
    for _ in 0..count {
        table.columns.insert(at, width);
    }
    for (row, made) in table.rows.iter_mut().zip(additions) {
        let before = row.grid_before as usize;
        if at < before {
            row.grid_before += count as u32;
            continue;
        }
        if let Some((index, cell)) = made {
            for _ in 0..count {
                row.cells.insert(index, cell.clone());
            }
            continue;
        }
        let mut column = before;
        let mut widened = false;
        for cell in &mut row.cells {
            let span = cell.span.max(1) as usize;
            if at > column && at < column + span {
                cell.span += count as u32;
                widened = true;
                break;
            }
            column += span;
        }
        if !widened {
            row.grid_after += count as u32;
        }
    }
    repair_merges(table);
    let row = sel.rows.0.min(table.rows.len().saturating_sub(1));
    let caret = table
        .rows
        .get(row)
        .and_then(|r| cell_index_at(r, at))
        .and_then(|c| cell_start(&sel.table, table, row, c))
        .unwrap_or_else(|| from.clone());
    doc.renumber();
    caret
}

/// The index of the cell of `row` that covers grid column `column`.
fn cell_index_at(row: &Row, column: usize) -> Option<usize> {
    let mut at = row.grid_before as usize;
    for (i, cell) in row.cells.iter().enumerate() {
        let end = at + cell.span.max(1) as usize;
        if column >= at && column < end {
            return Some(i);
        }
        at = end;
    }
    None
}

/// Take out the grid columns the selection covers (every column of every
/// cell it touches); where the caret goes: the cell now at the first removed
/// column, in the row of the first position. When the selection covers every
/// column, the whole table goes ([`delete_table`]).
///
/// **Merged cells.** A cell that covers some of the removed columns and
/// some others is narrowed by the removed ones (merged cells in every row of
/// a merge alike); one that is covered whole is removed, a vertical merge
/// with it. A row left with no cell is removed. Markers in what is removed
/// move to the caret's paragraph.
pub fn delete_columns(doc: &mut Document, from: &Pos, to: &Pos) -> Pos {
    let Some(sel) = covered(doc, from, to) else {
        return from.clone();
    };
    let Some(table) = table_of(doc, &sel.table) else {
        return from.clone();
    };
    let grid = table.columns.len();
    let (c0, c1) = (sel.columns.0.min(grid), sel.columns.1.min(grid));
    if c0 == 0 && c1 >= grid {
        return remove_table(doc, &sel.table);
    }
    let Some(table) = table_mut(doc, &sel.table) else {
        return from.clone();
    };
    let mut moved: Vec<Run> = Vec::new();
    let overlap = |s: usize, e: usize| e.min(c1).saturating_sub(s.max(c0));
    table.columns.drain(c0..c1);
    for row in &mut table.rows {
        let before = row.grid_before as usize;
        let after_start = grid.saturating_sub(row.grid_after as usize);
        row.grid_before = row.grid_before.saturating_sub(overlap(0, before) as u32);
        row.grid_after = row
            .grid_after
            .saturating_sub(overlap(after_start, grid) as u32);
        let mut column = before;
        let mut kept = Vec::with_capacity(row.cells.len());
        for mut cell in std::mem::take(&mut row.cells) {
            let span = cell.span.max(1) as usize;
            let cut = overlap(column, column + span);
            column += span;
            if cut >= span {
                for block in &cell.blocks {
                    collect_markers(block, &mut moved);
                }
                continue;
            }
            cell.span = (span - cut) as u32;
            kept.push(cell);
        }
        row.cells = kept;
    }
    // A row with no cell goes.
    let mut r = 0;
    let first_row = sel.rows.0;
    let mut removed_before = 0;
    while r < table.rows.len() {
        if table.rows[r].cells.is_empty() {
            table.rows.remove(r);
            if r < first_row {
                removed_before += 1;
            }
        } else {
            r += 1;
        }
    }
    if table.rows.is_empty() {
        return remove_table(doc, &sel.table);
    }
    repair_merges(table);
    let row = (first_row - removed_before.min(first_row)).min(table.rows.len() - 1);
    let cell = cell_index_at(&table.rows[row], c0)
        .unwrap_or(table.rows[row].cells.len().saturating_sub(1));
    let caret = cell_start(&sel.table, table, row, cell).unwrap_or_else(|| from.clone());
    rehome(doc, &caret, moved);
    doc.renumber();
    caret
}

// ---------------------------------------------------------------------------
// Deleting a table
// ---------------------------------------------------------------------------

/// Take out the innermost table `at` is in; where the caret goes: the start
/// of the paragraph that follows it (the end of the last paragraph before it
/// when nothing follows). A position outside tables does nothing. A cell the
/// table was the only block of gets an empty paragraph; markers in the table
/// move to the caret's paragraph.
pub fn delete_table(doc: &mut Document, at: &Pos) -> Pos {
    match cell_at(doc, at) {
        Some(cell) => remove_table(doc, &cell.table),
        None => at.clone(),
    }
}

fn remove_table(doc: &mut Document, table: &Path) -> Pos {
    let Some(index) = table.steps.last().map(|&step| step as usize) else {
        return Pos::default();
    };
    let mut moved: Vec<Run> = Vec::new();
    let blank = default_paragraph(doc);
    let Some(siblings) = doc.siblings_mut(table) else {
        return Pos::default();
    };
    if index >= siblings.len() || siblings[index].as_table().is_none() {
        return Pos::default();
    }
    collect_markers(&siblings[index], &mut moved);
    siblings.remove(index);
    if siblings.last().is_none_or(|b| b.as_table().is_some()) {
        siblings.push(Block::paragraph(blank));
    }
    // The first paragraph at or after where the table was, else the last
    // one before it.
    let paragraphs = doc.paragraphs_in(table.story);
    let caret = paragraphs
        .iter()
        .find(|(path, _)| path >= table)
        .map(|(path, _)| Pos::new(path.clone(), 0))
        .or_else(|| {
            paragraphs
                .last()
                .map(|(path, p)| Pos::new(path.clone(), p.len()))
        })
        .unwrap_or_default();
    let caret = Pos::new(caret.path.clone(), snap_in(doc, &caret));
    rehome(doc, &caret, moved);
    doc.renumber();
    caret
}

fn snap_in(doc: &Document, pos: &Pos) -> usize {
    doc.paragraph(&pos.path)
        .map_or(0, |p| snap(p, pos.offset, false))
}
