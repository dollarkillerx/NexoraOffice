//! Tables: structure, grid, and the borders, shading and margins of each
//! cell with the table style, the table and the cell added up.

use std::sync::Arc;

use aster_writer::{
    Align, Block, Border, Cell, CellAlign, Rgba, Row, RowHeight, StyleKind, Table, TableProps,
    Twips, VMerge,
};

use super::dom::{Node, flatten};
use super::props::{Env, Sides, merge_sides, parse_border, parse_sides, shading_of};
use super::styles::{Cond, CondFormat, final_sides};
use super::{PartState, Reader};

/// Tables inside tables are read this deep.
const MAX_TABLE_DEPTH: usize = 12;
/// Grid columns, and cells in a row, that are read.
const MAX_COLUMNS: usize = 4_096;
/// A column's width when nothing says.
const DEFAULT_COLUMN: Twips = 1_440;

/// `tblBorders`: the outer sides and the lines between cells.
#[derive(Clone, Debug, Default)]
pub struct TblBorders {
    pub sides: Sides,
    pub inside_h: Option<Option<Border>>,
    pub inside_v: Option<Option<Border>>,
}

impl TblBorders {
    pub fn merge(&mut self, upper: &TblBorders) {
        merge_sides(&mut self.sides, &upper.sides);
        if upper.inside_h.is_some() {
            self.inside_h = upper.inside_h;
        }
        if upper.inside_v.is_some() {
            self.inside_v = upper.inside_v;
        }
    }

    /// What these borders give a cell at this place in the table.
    fn for_cell(&self, first_row: bool, last_row: bool, first_col: bool, last_col: bool) -> Sides {
        [
            if first_col {
                self.sides[0]
            } else {
                self.inside_v
            },
            if first_row {
                self.sides[1]
            } else {
                self.inside_h
            },
            if last_col {
                self.sides[2]
            } else {
                self.inside_v
            },
            if last_row {
                self.sides[3]
            } else {
                self.inside_h
            },
        ]
    }
}

pub fn parse_tbl_borders(node: &Node<'_>, env: &Env) -> TblBorders {
    TblBorders {
        sides: parse_sides(node, env),
        inside_h: node.child("insideH").and_then(|b| parse_border(b, env)),
        inside_v: node.child("insideV").and_then(|b| parse_border(b, env)),
    }
}

pub fn parse_tc_borders(node: &Node<'_>, env: &Env) -> Sides {
    parse_sides(node, env)
}

/// `tblCellMar` or `tcMar`: left, top, right, bottom.
pub fn parse_cell_margins(node: &Node<'_>) -> [Option<Twips>; 4] {
    let mut out = [None; 4];
    for c in &node.children {
        let at = match c.local() {
            b"left" | b"start" => 0,
            b"top" => 1,
            b"right" | b"end" => 2,
            b"bottom" => 3,
            _ => continue,
        };
        if let Some(w) = c.int("w") {
            out[at] = Some(w.clamp(0, 31_680) as Twips);
        }
    }
    out
}

/// Which parts of a table style a table asks for (`tblLook`).
#[derive(Clone, Copy)]
struct Look {
    first_row: bool,
    last_row: bool,
    first_col: bool,
    last_col: bool,
    no_h_band: bool,
    no_v_band: bool,
}

fn parse_look(node: Option<&Node<'_>>) -> Look {
    // Word's default: 04A0.
    let mut look = Look {
        first_row: true,
        last_row: false,
        first_col: true,
        last_col: false,
        no_h_band: false,
        no_v_band: true,
    };
    let Some(node) = node else {
        return look;
    };
    if let Some(bits) = node
        .attr("val")
        .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
    {
        look = Look {
            first_row: bits & 0x20 != 0,
            last_row: bits & 0x40 != 0,
            first_col: bits & 0x80 != 0,
            last_col: bits & 0x100 != 0,
            no_h_band: bits & 0x200 != 0,
            no_v_band: bits & 0x400 != 0,
        };
    }
    let flag = |name: &str, slot: &mut bool| {
        if let Some(v) = node.attr(name) {
            *slot = matches!(v.trim(), "1" | "true" | "on");
        }
    };
    flag("firstRow", &mut look.first_row);
    flag("lastRow", &mut look.last_row);
    flag("firstColumn", &mut look.first_col);
    flag("lastColumn", &mut look.last_col);
    flag("noHBand", &mut look.no_h_band);
    flag("noVBand", &mut look.no_v_band);
    look
}

struct RawCell<'n, 'a> {
    node: &'n Node<'a>,
    range: std::ops::Range<usize>,
    span: u32,
    merge: VMerge,
    width: Option<Twips>,
    col: usize,
}

struct RawRow<'n, 'a> {
    node: &'n Node<'a>,
    before: u32,
    after: u32,
    cells: Vec<RawCell<'n, 'a>>,
}

fn is_container(node: &Node<'_>) -> bool {
    matches!(node.local(), b"sdt" | b"customXml" | b"smartTag")
}

impl Reader<'_> {
    pub(super) fn table(&mut self, node: &Node<'_>, part: &mut PartState) -> Option<Table> {
        if part.depth >= MAX_TABLE_DEPTH {
            return None;
        }
        let tbl_pr = node.child("tblPr");
        let mut props = TableProps::default();
        let mut own_borders = TblBorders::default();
        let mut own_margins = [None; 4];
        let mut style_name = None;
        let mut look = parse_look(None);
        if let Some(pr) = tbl_pr {
            style_name = pr.child("tblStyle").and_then(Node::val);
            props.style = style_name.as_deref().map(|s| self.env.intern(s));
            props.align = match pr.child("jc").and_then(Node::val).as_deref() {
                Some("center") => Align::Center,
                Some("right" | "end") => Align::Right,
                _ => Align::Left,
            };
            props.indent = pr
                .child("tblInd")
                .and_then(|n| n.int("w"))
                .map_or(0, |w| w.clamp(-100_000, 100_000) as Twips);
            props.fixed_layout = pr
                .child("tblLayout")
                .and_then(|l| l.attr("type"))
                .as_deref()
                == Some("fixed");
            if let Some(b) = pr.child("tblBorders") {
                own_borders = parse_tbl_borders(b, &self.env);
            }
            if let Some(m) = pr.child("tblCellMar") {
                own_margins = parse_cell_margins(m);
            }
            look = parse_look(pr.child("tblLook"));
        }
        let style = style_name
            .as_deref()
            .and_then(|id| self.styles.index_of(id))
            .filter(|&i| self.styles.raw[i].kind == StyleKind::Table)
            .or_else(|| {
                self.styles
                    .raw
                    .iter()
                    .position(|s| s.kind == StyleKind::Table && s.default)
            });

        // Structure.
        let mut items = Vec::new();
        flatten(&node.children, &is_container, &mut items);
        let mut rows: Vec<RawRow<'_, '_>> = Vec::new();
        for item in items.iter().filter(|i| i.node.is("tr")) {
            let tr = item.node;
            let mut row = RawRow {
                node: tr,
                before: 0,
                after: 0,
                cells: Vec::new(),
            };
            if let Some(pr) = tr.child("trPr") {
                row.before = pr
                    .child("gridBefore")
                    .and_then(Node::val)
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .map_or(0, |v| v.clamp(0, MAX_COLUMNS as i64) as u32);
                row.after = pr
                    .child("gridAfter")
                    .and_then(Node::val)
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .map_or(0, |v| v.clamp(0, MAX_COLUMNS as i64) as u32);
            }
            let mut cells = Vec::new();
            flatten(&tr.children, &is_container, &mut cells);
            let mut col = row.before as usize;
            for cell in cells.iter().filter(|c| c.node.is("tc")).take(MAX_COLUMNS) {
                let tc = cell.node;
                let pr = tc.child("tcPr");
                let span = pr
                    .and_then(|p| p.child("gridSpan"))
                    .and_then(Node::val)
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .map_or(1, |v| v.clamp(1, 256) as u32);
                let merge = match pr.and_then(|p| p.child("vMerge")) {
                    None => VMerge::None,
                    Some(m) => match m.val().as_deref() {
                        Some("restart") => VMerge::Start,
                        _ => VMerge::Continue,
                    },
                };
                let width = pr
                    .and_then(|p| p.child("tcW"))
                    .filter(|w| matches!(w.attr("type").as_deref(), None | Some("dxa")))
                    .and_then(|w| w.int("w"))
                    .filter(|w| *w > 0)
                    .map(|w| w.min(1_000_000) as Twips);
                row.cells.push(RawCell {
                    node: tc,
                    range: tc.range(),
                    span,
                    merge,
                    width,
                    col,
                });
                col += span as usize;
            }
            rows.push(row);
        }

        // The grid: the file's, widened with what the cells say where a row
        // needs more columns than it has.
        let mut columns: Vec<Twips> = node
            .child("tblGrid")
            .map(|g| {
                g.each("gridCol")
                    .take(MAX_COLUMNS)
                    .map(|c| {
                        c.int("w")
                            .map_or(DEFAULT_COLUMN, |w| w.clamp(0, 1_000_000) as Twips)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let needed = rows
            .iter()
            .map(|r| {
                let cells_end = r
                    .cells
                    .last()
                    .map_or(r.before as usize, |c| c.col + c.span as usize);
                cells_end + r.after as usize
            })
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS);
        if columns.len() < needed {
            let have = columns.len();
            let mut widths: Vec<Option<Twips>> = vec![None; needed];
            for row in &rows {
                for cell in &row.cells {
                    let Some(w) = cell.width else { continue };
                    let span = cell.span as usize;
                    let end = (cell.col + span).min(needed);
                    for (j, slot) in widths.iter_mut().enumerate().take(end).skip(cell.col) {
                        if j >= have && (span == 1 || slot.is_none()) {
                            *slot = Some(w / span as Twips);
                        }
                    }
                }
            }
            for width in widths.into_iter().skip(have) {
                columns.push(width.unwrap_or(DEFAULT_COLUMN));
            }
        }
        let grid = columns.len();

        // Table-wide style layers and margins.
        let whole = style.map(|s| self.styles.table_format(s, 0));
        let mut margins = props.cell_margins;
        if let Some(whole) = &whole {
            for (m, s) in margins.iter_mut().zip(&whole.margins) {
                if let Some(s) = s {
                    *m = *s;
                }
            }
        }
        for (m, s) in margins.iter_mut().zip(&own_margins) {
            if let Some(s) = s {
                *m = *s;
            }
        }
        props.cell_margins = margins;

        // How far down each merged cell goes, for its bottom edge.
        let row_count = rows.len();
        let mut merged_to: Vec<Vec<usize>> = rows
            .iter()
            .enumerate()
            .map(|(r, row)| vec![r; row.cells.len()])
            .collect();
        for r in (0..row_count).rev() {
            for c in 0..rows[r].cells.len() {
                let (col, span) = (rows[r].cells[c].col, rows[r].cells[c].span);
                if r + 1 < row_count
                    && let Some(below) = rows[r + 1]
                        .cells
                        .iter()
                        .position(|b| b.col == col && b.span == span && b.merge == VMerge::Continue)
                {
                    merged_to[r][c] = merged_to[r + 1][below];
                }
            }
        }

        part.depth += 1;
        let mut out_rows = Vec::with_capacity(row_count);
        for (r, row) in rows.iter().enumerate() {
            let mut out = Row {
                cells: Vec::with_capacity(row.cells.len()),
                grid_before: row.before,
                grid_after: row.after,
                source: Some(row.node.range()),
                ..Row::default()
            };
            if let Some(pr) = row.node.child("trPr") {
                out.header = pr.toggle("tblHeader").unwrap_or(false);
                out.cant_split = pr.toggle("cantSplit").unwrap_or(false);
                if let Some(h) = pr.child("trHeight") {
                    let value = h.int("val").unwrap_or(0).clamp(0, 1_000_000) as Twips;
                    out.height = match h.attr("hRule").as_deref() {
                        Some("exact") => RowHeight::Exactly(value),
                        Some("auto") => RowHeight::Auto,
                        _ if value > 0 => RowHeight::AtLeast(value),
                        _ => RowHeight::Auto,
                    };
                }
            }
            let count = row.cells.len();
            for (c, cell) in row.cells.iter().enumerate() {
                let first_row = r == 0;
                let last_row = merged_to[r][c] + 1 == row_count;
                let first_col = cell.col == 0;
                let last_col = cell.col + cell.span as usize >= grid;
                let format = style.map(|s| {
                    let mask = cond_mask(&look, r, row_count, c, count);
                    self.cell_format(s, mask)
                });
                let cf = format.as_deref();
                let pr = cell.node.child("tcPr");

                // Borders: table style, table, cell; none/nil overrides.
                let mut layers: Sides = [None; 4];
                if let Some(cf) = cf {
                    merge_sides(
                        &mut layers,
                        &cf.borders
                            .for_cell(first_row, last_row, first_col, last_col),
                    );
                    merge_sides(&mut layers, &cf.cell_borders);
                }
                merge_sides(
                    &mut layers,
                    &own_borders.for_cell(first_row, last_row, first_col, last_col),
                );
                if let Some(b) = pr.and_then(|p| p.child("tcBorders")) {
                    merge_sides(&mut layers, &parse_tc_borders(b, &self.env));
                }
                let borders = final_sides(&layers);

                let mut shading: Option<Option<Rgba>> = cf.and_then(|f| f.shading);
                if let Some(s) = pr
                    .and_then(|p| p.child("shd"))
                    .and_then(|s| shading_of(s, &self.env))
                {
                    shading = Some(s);
                }

                let mut cell_margins = margins;
                if let Some(cf) = cf {
                    for i in 0..4 {
                        if let (Some(s), None) = (cf.margins[i], own_margins[i]) {
                            cell_margins[i] = s;
                        }
                    }
                }
                if let Some(m) = pr.and_then(|p| p.child("tcMar")) {
                    for (slot, v) in cell_margins.iter_mut().zip(parse_cell_margins(m)) {
                        if let Some(v) = v {
                            *slot = v;
                        }
                    }
                }
                let align = match pr
                    .and_then(|p| p.child("vAlign"))
                    .and_then(Node::val)
                    .as_deref()
                {
                    Some("center") => CellAlign::Center,
                    Some("bottom") => CellAlign::Bottom,
                    _ => CellAlign::Top,
                };

                let previous = std::mem::replace(&mut part.cell, format.clone());
                let mut blocks = self.blocks(&cell.node.children, part);
                part.cell = previous;
                if blocks.is_empty() {
                    blocks.push(Block::paragraph(self.empty_paragraph(part)));
                }
                out.cells.push(Cell {
                    blocks,
                    span: cell.span,
                    merge: cell.merge,
                    borders,
                    shading: shading.flatten(),
                    margins: cell_margins,
                    align,
                    source: Some(cell.range.clone()),
                });
            }
            out_rows.push(out);
        }
        part.depth -= 1;
        Some(Table {
            columns,
            rows: out_rows,
            props,
            source: Some(node.range()),
        })
    }

    /// The table style's formats for a cell, kept for the cells that have
    /// the same.
    fn cell_format(&mut self, style: usize, mask: u16) -> Arc<CondFormat> {
        if let Some(found) = self.cell_formats.get(&(style, mask)) {
            return found.clone();
        }
        let format = Arc::new(self.styles.table_format(style, mask));
        self.cell_formats.insert((style, mask), format.clone());
        format
    }
}

/// Which conditional formats a cell has: the bits of [`Cond`].
fn cond_mask(look: &Look, row: usize, rows: usize, cell: usize, cells: usize) -> u16 {
    let mut mask = 0;
    let first_row = look.first_row && row == 0;
    let last_row = look.last_row && row + 1 == rows && rows > 1;
    let first_col = look.first_col && cell == 0;
    let last_col = look.last_col && cell + 1 == cells && cells > 1;
    if first_row {
        mask |= Cond::FirstRow.bit();
    }
    if last_row {
        mask |= Cond::LastRow.bit();
    }
    if first_col {
        mask |= Cond::FirstCol.bit();
    }
    if last_col {
        mask |= Cond::LastCol.bit();
    }
    if !look.no_h_band && !first_row && !last_row {
        let at = row - usize::from(look.first_row);
        mask |= if at.is_multiple_of(2) {
            Cond::Band1Horz
        } else {
            Cond::Band2Horz
        }
        .bit();
    }
    if !look.no_v_band && !first_col && !last_col {
        let at = cell - usize::from(look.first_col);
        mask |= if at.is_multiple_of(2) {
            Cond::Band1Vert
        } else {
            Cond::Band2Vert
        }
        .bit();
    }
    if first_row && first_col {
        mask |= Cond::NwCell.bit();
    }
    if first_row && last_col {
        mask |= Cond::NeCell.bit();
    }
    if last_row && first_col {
        mask |= Cond::SwCell.bit();
    }
    if last_row && last_col {
        mask |= Cond::SeCell.bit();
    }
    mask
}
