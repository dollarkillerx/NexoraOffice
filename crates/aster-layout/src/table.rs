//! Tables: grid, rows, cells and what is drawn for them.
//!
//! A table is laid out as a whole into rows of known height; the flow then
//! decides which rows go on which page. A row is put on a page as one or
//! more slices (`Slice`): a row that is taller than a page is cut between
//! lines of its cells.

use aster_writer::{Align, Border, CellAlign, Path, RowHeight, Table, VMerge};

use crate::cx::{Cx, EmitCtx, Entry, Layer, Out, Paths, Stack, border_color, rule_style};
use crate::display::{Item, Rect, Rgba};
use crate::shape::pt;

/// Most grid columns taken from a table.
const MAX_COLUMNS: usize = 1_000;
/// A row is never taller than this, points.
const MAX_ROW: f32 = 50_000.0;

pub(crate) struct RowLayout {
    pub top: f32,
    pub height: f32,
    pub exact: bool,
    pub cant_split: bool,
    pub header: bool,
}

pub(crate) struct CellLayout {
    pub row0: usize,
    /// Rows it covers.
    pub rows: usize,
    pub col0: usize,
    pub cols: usize,
    pub stack: Stack,
    /// Left, top, right, bottom.
    pub margins: [f32; 4],
    /// Content shift down for the vertical alignment.
    pub voff: f32,
    pub shading: Option<Rgba>,
    pub borders: [Option<Border>; 4],
    align: CellAlign,
}

pub(crate) struct TableLayout {
    /// Left edge relative to the container's left.
    pub x: f32,
    /// Grid lines relative to the table's left edge.
    pub cols: Vec<f32>,
    pub rows: Vec<RowLayout>,
    pub cells: Vec<CellLayout>,
    /// The cell at each grid position of each row.
    pub owner: Vec<Vec<Option<usize>>>,
}

/// A part of a row put on a page: `c0..c1` of its height.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slice {
    pub row: usize,
    pub c0: f32,
    pub c1: f32,
    pub repeat: bool,
}

impl TableLayout {
    pub fn height(&self) -> f32 {
        self.rows.last().map_or(0.0, |row| row.top + row.height)
    }

    pub fn width(&self) -> f32 {
        self.cols.last().copied().unwrap_or(0.0)
    }

    /// Rows at the top marked as headers.
    pub fn header_rows(&self) -> usize {
        self.rows.iter().take_while(|row| row.header).count()
    }

    /// Where a row may be cut next after `c0`, no further than `target`
    /// (in the row's own height): not through a line of any cell.
    pub fn cut(&self, row: usize, c0: f32, target: f32) -> f32 {
        let layout = &self.rows[row];
        if target >= layout.height - 0.01 {
            return layout.height;
        }
        let pieces = self.pieces_of_row(row);
        let mut cut = target;
        loop {
            let mut moved = false;
            for &(top, bottom) in &pieces {
                if top < cut && bottom > cut + 0.01 && top > c0 + 0.01 {
                    cut = top;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        if cut <= c0 + 0.01 {
            // Nothing fits: take the first line of whatever comes next.
            cut = pieces
                .iter()
                .filter(|(top, bottom)| *top >= c0 - 0.01 && *bottom > c0 + 0.01)
                .map(|(_, bottom)| *bottom)
                .fold(f32::INFINITY, f32::min);
            if !cut.is_finite() {
                cut = layout.height;
            }
        }
        cut.min(layout.height)
    }

    /// The lines of a row's cells as `(top, bottom)` in the row's height.
    fn pieces_of_row(&self, row: usize) -> Vec<(f32, f32)> {
        let layout = &self.rows[row];
        let mut out = Vec::new();
        for ci in self.owner[row].iter().flatten() {
            let cell = &self.cells[*ci];
            if cell.col0 > 0 && self.owner[row][cell.col0 - 1] == Some(*ci) {
                continue;
            }
            let group = layout.top - self.rows[cell.row0].top;
            let base = cell.margins[1] + cell.voff - group;
            for (top, bottom) in cell.stack.pieces() {
                out.push((base + top, base + bottom));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.dedup();
        out
    }
}

impl Stack {
    /// The units content can be cut between: lines and rows of nested
    /// tables, as `(top, bottom)`.
    pub fn pieces(&self) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        for entry in &self.entries {
            match entry {
                Entry::Para(p) => {
                    for line in p.shaped.paragraph.lines() {
                        out.push((p.top + line.top, p.top + line.top + line.height));
                    }
                }
                Entry::Table(table, _, top) => {
                    for row in &table.rows {
                        out.push((top + row.top, top + row.top + row.height));
                    }
                }
            }
        }
        out
    }
}

fn resolve(a: Option<&Border>, b: Option<&Border>) -> Option<Border> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.width > a.width { *b } else { *a }),
        (Some(a), None) => Some(*a),
        (None, Some(b)) => Some(*b),
        (None, None) => None,
    }
}

impl Cx<'_> {
    pub fn layout_table(
        &mut self,
        table: &Table,
        path: Option<&Path>,
        width: f32,
        depth: u8,
    ) -> TableLayout {
        let width = width.max(1.0);
        // The grid: as given, widened for rows that have more columns.
        let mut need = 0usize;
        for row in &table.rows {
            let mut n = row.grid_before as usize + row.grid_after as usize;
            for cell in &row.cells {
                n += cell.span.max(1) as usize;
            }
            need = need.max(n.min(MAX_COLUMNS));
        }
        let mut widths: Vec<f32> = table
            .columns
            .iter()
            .take(MAX_COLUMNS)
            .map(|w| pt(*w).max(0.0))
            .collect();
        if widths.len() < need {
            let each = if widths.iter().sum::<f32>() > 0.0 {
                widths.iter().sum::<f32>() / widths.len() as f32
            } else {
                width / need.max(1) as f32
            };
            widths.resize(need, each);
        }
        if widths.iter().sum::<f32>() <= 0.0 {
            let each = width / widths.len().max(1) as f32;
            widths.iter_mut().for_each(|w| *w = each);
        }
        let ncols = widths.len();
        let mut cols = Vec::with_capacity(ncols + 1);
        let mut at = 0.0;
        cols.push(0.0);
        for w in &widths {
            at += w;
            cols.push(at);
        }
        let total = at;

        let props = &table.props;
        let margin = pt(props.cell_margins[0]).max(0.0);
        let x = match props.align {
            Align::Center => (width - total) / 2.0 + pt(props.indent),
            Align::Right => width - total + pt(props.indent),
            _ => pt(props.indent) - margin,
        };

        let nrows = table.rows.len();
        let mut owner: Vec<Vec<Option<usize>>> = vec![vec![None; ncols]; nrows];
        let mut cells: Vec<CellLayout> = Vec::new();
        for (r, row) in table.rows.iter().enumerate() {
            let mut col = (row.grid_before as usize).min(ncols);
            for (c, cell) in row.cells.iter().enumerate() {
                if col >= ncols {
                    break;
                }
                let span = (cell.span.max(1) as usize).min(ncols - col);
                let continued = (cell.merge == VMerge::Continue && r > 0)
                    .then(|| owner[r - 1][col])
                    .flatten()
                    .filter(|&ci| cells[ci].col0 == col && cells[ci].cols == span);
                let id = if let Some(start) = continued {
                    cells[start].rows += 1;
                    start
                } else {
                    let [ml, mt, mr, mb] = cell.margins.map(|m| pt(m).max(0.0));
                    let inner = (cols[col + span] - cols[col] - ml - mr).max(1.0);
                    let paths = match path {
                        Some(table_path) => Paths::Cell(table_path.clone(), r, c),
                        None => Paths::None,
                    };
                    let stack = self.layout_stack(&cell.blocks, &paths, inner, depth);
                    cells.push(CellLayout {
                        row0: r,
                        rows: 1,
                        col0: col,
                        cols: span,
                        stack,
                        margins: [ml, mt, mr, mb],
                        voff: 0.0,
                        shading: cell.shading,
                        borders: cell.borders,
                        align: cell.align,
                    });
                    cells.len() - 1
                };
                for slot in &mut owner[r][col..col + span] {
                    *slot = Some(id);
                }
                col += span;
            }
        }

        // Row heights: what the cells that are in one row need, then what
        // the cells merged over several rows still lack.
        let mut heights = vec![0.0f32; nrows];
        for cell in &cells {
            if cell.rows == 1 {
                let need = cell.stack.height + cell.margins[1] + cell.margins[3];
                heights[cell.row0] = heights[cell.row0].max(need);
            }
        }
        for (r, row) in table.rows.iter().enumerate() {
            heights[r] = match row.height {
                RowHeight::Auto => heights[r],
                RowHeight::AtLeast(v) => heights[r].max(pt(v)),
                RowHeight::Exactly(v) => pt(v).max(0.0),
            }
            .min(MAX_ROW);
        }
        for cell in &cells {
            if cell.rows > 1 {
                let need = cell.stack.height + cell.margins[1] + cell.margins[3];
                let have: f32 = heights[cell.row0..cell.row0 + cell.rows].iter().sum();
                if need > have {
                    let last = cell.row0 + cell.rows - 1;
                    if !matches!(table.rows[last].height, RowHeight::Exactly(_)) {
                        heights[last] += (need - have).min(MAX_ROW);
                    }
                }
            }
        }
        let mut rows = Vec::with_capacity(nrows);
        let mut top = 0.0;
        for (r, row) in table.rows.iter().enumerate() {
            rows.push(RowLayout {
                top,
                height: heights[r],
                exact: matches!(row.height, RowHeight::Exactly(_)),
                cant_split: row.cant_split,
                header: row.header,
            });
            top += heights[r];
        }
        for cell in &mut cells {
            let group = rows[cell.row0 + cell.rows - 1].top
                + rows[cell.row0 + cell.rows - 1].height
                - rows[cell.row0].top;
            let room = group - cell.margins[1] - cell.margins[3] - cell.stack.height;
            cell.voff = match cell.align {
                CellAlign::Top => 0.0,
                CellAlign::Center => room / 2.0,
                CellAlign::Bottom => room,
            }
            .max(0.0);
        }
        TableLayout {
            x,
            cols,
            rows,
            cells,
            owner,
        }
    }

    /// Draw rows `from..to` of a table nested in a stack.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_nested_table(
        &mut self,
        table: &TableLayout,
        ox: f32,
        oy: f32,
        s0: f32,
        s1: f32,
        ec: &EmitCtx,
        out: &mut Out<'_>,
    ) {
        let slices: Vec<Slice> = table
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.top >= s0 && row.top < s1)
            .map(|(r, row)| Slice {
                row: r,
                c0: 0.0,
                c1: row.height,
                repeat: false,
            })
            .collect();
        if let Some(first) = slices.first() {
            let y = oy + table.rows[first.row].top;
            self.emit_row_slices(table, &slices, ox, y, ec, out);
        }
    }

    /// Put slices of rows on the page one under the other from `y_start`;
    /// returns the height they take.
    pub fn emit_row_slices(
        &mut self,
        table: &TableLayout,
        slices: &[Slice],
        ox: f32,
        y_start: f32,
        ec: &EmitCtx,
        out: &mut Out<'_>,
    ) -> f32 {
        let ncols = table.cols.len().saturating_sub(1);
        let mut y = y_start;
        let mut tops = Vec::with_capacity(slices.len() + 1);
        for slice in slices {
            tops.push(y);
            let row = &table.rows[slice.row];
            let (y_top, y_bot) = (y, y + (slice.c1 - slice.c0));
            let mut c = 0;
            while c < ncols {
                let Some(ci) = table.owner[slice.row][c] else {
                    c += 1;
                    continue;
                };
                let cell = &table.cells[ci];
                c = cell.col0 + cell.cols;
                let (x0, x1) = (
                    ox + table.cols[cell.col0],
                    ox + table.cols[cell.col0 + cell.cols],
                );
                let rect = Rect::new(x0, y_top, x1, y_bot);
                if let Some(color) = cell.shading {
                    out.layers.shade.push(Item::Fill { rect, color });
                }
                if cell.stack.entries.is_empty() {
                    continue;
                }
                let group = row.top - table.rows[cell.row0].top;
                let (c0g, c1g) = (group + slice.c0, group + slice.c1);
                let shift = cell.margins[1] + cell.voff;
                let s0 = if c0g <= 0.01 {
                    f32::NEG_INFINITY
                } else {
                    c0g - shift
                };
                let last_of_group = slice.row + 1 == cell.row0 + cell.rows;
                let s1 = if last_of_group && slice.c1 >= row.height - 0.01 {
                    f32::INFINITY
                } else {
                    c1g - shift
                };
                let content_y = y_top - c0g + shift;
                let offset = if s0.is_finite() { s0 } else { 0.0 };
                let clip = row.exact || slice.c0 > 0.01 || slice.c1 < row.height - 0.01;
                if clip {
                    out.layers.body.push(Item::PushClip(rect));
                }
                let inner = Rect::new(
                    x0 + cell.margins[0],
                    y_top,
                    (x1 - cell.margins[2]).max(x0 + cell.margins[0]),
                    y_bot,
                );
                let cell_ec = EmitCtx {
                    bounds: Some(rect),
                    column: inner,
                    repeat: ec.repeat || slice.repeat,
                    ..*ec
                };
                self.emit_stack(
                    &cell.stack,
                    x0 + cell.margins[0],
                    content_y + offset,
                    s0,
                    s1,
                    &cell_ec,
                    out,
                );
                if clip {
                    out.layers.body.push(Item::PopClip);
                }
            }
            y = y_bot;
        }
        tops.push(y);

        // Borders: each edge once, from the cells on both sides of it.
        let cell_border = |ci: Option<usize>, side: usize| -> Option<&Border> {
            ci.and_then(|ci| table.cells[ci].borders[side].as_ref())
        };
        for (i, slice) in slices.iter().enumerate() {
            let owners = &table.owner[slice.row];
            let (y0, y1) = (tops[i], tops[i + 1]);
            for g in 0..=ncols {
                let left = if g > 0 { owners[g - 1] } else { None };
                let right = if g < ncols { owners[g] } else { None };
                if left.is_some() && left == right {
                    continue;
                }
                if let Some(b) = resolve(cell_border(left, 2), cell_border(right, 0)) {
                    self.rule(
                        out,
                        &b,
                        (ox + table.cols[g], y0 - b.width / 2.0),
                        (ox + table.cols[g], y1 + b.width / 2.0),
                    );
                }
            }
        }
        for i in 0..=slices.len() {
            let above = i
                .checked_sub(1)
                .map(|j| (slices[j], &table.owner[slices[j].row]));
            let below = slices.get(i).map(|s| (*s, &table.owner[s.row]));
            // Not at an edge that is only a cut through a row.
            if let Some((s, _)) = &below
                && i == 0
                && s.c0 > 0.01
            {
                continue;
            }
            if let Some((s, _)) = &above
                && i == slices.len()
                && s.c1 < table.rows[s.row].height - 0.01
            {
                continue;
            }
            let y = tops[i];
            let mut c = 0;
            while c < ncols {
                let a = above.as_ref().and_then(|(_, owners)| owners[c]);
                let b = below.as_ref().and_then(|(_, owners)| owners[c]);
                let internal = a.is_some() && a == b;
                // A merged cell continuing past the page edge has no edge.
                let a_goes_on = i == slices.len()
                    && a.is_some_and(|ci| {
                        let cell = &table.cells[ci];
                        slices[i - 1].row + 1 < cell.row0 + cell.rows
                    });
                let border = if internal || a_goes_on {
                    None
                } else {
                    resolve(cell_border(a, 3), cell_border(b, 1))
                };
                let mut end = c + 1;
                while end < ncols {
                    let a2 = above.as_ref().and_then(|(_, owners)| owners[end]);
                    let b2 = below.as_ref().and_then(|(_, owners)| owners[end]);
                    let border2 =
                        if (a2.is_some() && a2 == b2) || (a2.is_some() && a2 == a && a_goes_on) {
                            None
                        } else {
                            resolve(cell_border(a2, 3), cell_border(b2, 1))
                        };
                    if border2 != border {
                        break;
                    }
                    end += 1;
                }
                if let Some(b) = border {
                    self.rule(
                        out,
                        &b,
                        (ox + table.cols[c] - b.width / 2.0, y),
                        (ox + table.cols[end] + b.width / 2.0, y),
                    );
                }
                c = end;
            }
        }
        y - y_start
    }

    fn rule(&self, out: &mut Out<'_>, border: &Border, from: (f32, f32), to: (f32, f32)) {
        out.layers.rules.push(Item::Rule {
            from,
            to,
            width: border.width.max(0.25),
            color: border_color(border),
            style: rule_style(border.style),
        });
    }
}

#[allow(dead_code)]
fn _layer_used(_: Layer) {}
