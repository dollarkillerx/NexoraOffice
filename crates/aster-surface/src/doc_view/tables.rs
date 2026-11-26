//! Tables in the view: merging and splitting cells, the selection drawn as
//! whole cells, and dragging the lines of a table.
//!
//! **Cells.** Each command is one step of undo and works in any story. The
//! `can_` questions are for enabling menu items; they say no while an input
//! method is composing, as the commands do nothing then. What a merge acts on
//! is the selection: dragging from one cell to another selects the block of
//! cells between them (widened over merged cells, see
//! [`aster_writer::edit::cell_range`]), and a merge takes exactly that block.
//!
//! **Selecting cells.** A selection whose ends are in two different cells of
//! one table is drawn as the whole cells of the block instead of the lines of
//! text in them, so that empty cells and the space round the text are
//! covered too. A selection inside one cell is text and is drawn as text.
//!
//! **Lines.** Over a column line of a table, or the bottom of a row, the
//! pointer is on a handle ([`Handle::ColumnBorder`], [`Handle::RowBorder`]);
//! dragging it shows a guide line where the line will be and changes the
//! document when the button goes up, as one step of undo: a column line with
//! [`edit::move_column_border`] (the neighbours trade width), the bottom of a
//! row with [`edit::set_row_height`]. The row gets *at least* the height the
//! pointer made it, or *exactly* when it already was an exact row; a row can
//! not be made lower than what is in it, whatever the guide shows. Lines
//! hidden by a merged cell are not handles. The tables of a header or footer
//! can be dragged while the caret is in it, those of the body and the notes
//! while it is not.

use aster_layout::{Rect as PageRect, TableBorder};
use aster_writer::edit::{self, MAX_ROW_HEIGHT, MIN_EXACT_ROW, Selection};
use aster_writer::{RowHeight, Twips};
use vello_cpu::kurbo::Rect;

use super::objects::Handle;
use super::{DocStyle, DocView, color};

/// How far from a line the pointer is on it, logical pixels.
const BORDER_REACH: f64 = 3.0;
/// Twips in a point.
const TWIPS_PT: f64 = 20.0;

/// A line of a table being dragged.
pub(super) struct BorderDrag {
    border: TableBorder,
    /// Where the button went down, physical pixels.
    start: (f64, f64),
    /// How far the line has been moved, in twips, as far as the edit would
    /// take it.
    applied: Twips,
}

impl DocView {
    // --- Cells ---

    /// The ends of the selection, in reading order.
    fn ordered_ends(&self) -> Option<(aster_writer::Pos, aster_writer::Pos)> {
        let sel = self.sel.as_ref()?;
        Some(if sel.anchor <= sel.focus {
            (sel.anchor.clone(), sel.focus.clone())
        } else {
            (sel.focus.clone(), sel.anchor.clone())
        })
    }

    /// The selected cells can be merged into one: two or more cells of one
    /// table, none of which the merge would leave a hole beside.
    pub fn can_merge_cells(&self) -> bool {
        if self.compose.is_some() || self.new_marginal.is_some() {
            return false;
        }
        self.ordered_ends()
            .is_some_and(|(from, to)| edit::can_merge_cells(&self.document, &from, &to))
    }

    /// Merge the selected cells into one; the caret goes to its start. One
    /// step of undo. False where [`DocView::can_merge_cells`] is.
    pub fn merge_cells(&mut self) -> bool {
        if !self.can_merge_cells() {
            return false;
        }
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            edit::can_merge_cells(document, &from, &to)
                .then(|| Selection::caret(edit::merge_cells(document, &from, &to)))
        })
    }

    /// The cell the caret is in (the start of the selection) can be cut into
    /// `columns` by `rows` cells: it is a plain cell and the table stays
    /// within the limits (see [`edit::can_split_cell`]).
    pub fn can_split_cell(&self, columns: usize, rows: usize) -> bool {
        if self.compose.is_some() || self.new_marginal.is_some() {
            return false;
        }
        self.ordered_ends()
            .is_some_and(|(from, _)| edit::can_split_cell(&self.document, &from, columns, rows))
    }

    /// Cut the cell the selection starts in into `columns` cells side by side
    /// and `rows` one above the other; the content stays in the first and the
    /// caret goes to its start. One step of undo.
    pub fn split_cell(&mut self, columns: usize, rows: usize) -> bool {
        if !self.can_split_cell(columns, rows) {
            return false;
        }
        self.apply(None, true, |document, selection| {
            let (from, _) = selection.ordered();
            edit::can_split_cell(document, &from, columns, rows)
                .then(|| Selection::caret(edit::split_cell(document, &from, columns, rows)))
        })
    }

    /// The cell the selection starts in is merged (wider than a grid column
    /// or joined with cells below or above).
    pub fn can_unmerge_cell(&self) -> bool {
        if self.compose.is_some() || self.new_marginal.is_some() {
            return false;
        }
        self.ordered_ends()
            .is_some_and(|(from, _)| edit::can_unmerge_cell(&self.document, &from))
    }

    /// Take the merged cell the selection starts in apart again; the caret
    /// goes to the start of the first cell. One step of undo.
    pub fn unmerge_cell(&mut self) -> bool {
        if !self.can_unmerge_cell() {
            return false;
        }
        self.apply(None, true, |document, selection| {
            let (from, _) = selection.ordered();
            edit::can_unmerge_cell(document, &from)
                .then(|| Selection::caret(edit::unmerge_cell(document, &from)))
        })
    }

    /// The rectangles of the selection when it is of whole cells: the block
    /// of cells between its ends, one per page it is on. `None` for a text
    /// selection.
    pub(super) fn cell_selection_rects(&self) -> Option<Vec<(usize, PageRect)>> {
        let (from, to) = self.selection()?;
        let range = edit::cell_range(&self.document, from, to)?;
        Some(self.layout.cell_rects(
            self.page_for(from.path.story),
            &range.table,
            range.rows,
            range.columns,
        ))
    }

    // --- Lines ---

    /// The line of a table under a point of the window (physical pixels), if
    /// the table is one the pointer can work on now.
    pub(super) fn border_at(&self, x: f64, y: f64) -> Option<TableBorder> {
        if !self.is_over_page(x, y) {
            return None;
        }
        let (page, px, py) = self.locate(x, y);
        let story = self.pointer_story(page, px, py)?;
        let reach = (BORDER_REACH * self.s() / self.pixels_per_point()) as f32;
        self.layout.table_border_at(
            page,
            px,
            py,
            reach,
            matches!(story, aster_writer::StoryId::Marginal(_)),
        )
    }

    /// The handle a line drag is on, for the pointer's shape.
    pub(super) fn border_handle(&self, x: f64, y: f64) -> Option<Handle> {
        let border = match &self.border {
            Some(drag) => drag.border.clone(),
            None => self.border_at(x, y)?,
        };
        Some(match border {
            TableBorder::Column { .. } => Handle::ColumnBorder,
            TableBorder::Row { .. } => Handle::RowBorder,
        })
    }

    /// A press on a line of a table begins a drag. `None` when it is not on
    /// one.
    pub(super) fn press_border(&mut self, x: f64, y: f64) -> Option<bool> {
        let border = self.border_at(x, y)?;
        self.press = None;
        self.border = Some(BorderDrag {
            border,
            start: (x, y),
            applied: 0,
        });
        Some(false)
    }

    /// What the line has moved to, in twips, as far as the edit lets it.
    fn border_shift(&self, border: &TableBorder, wanted: Twips) -> Twips {
        match border {
            TableBorder::Column { table, border, .. } => {
                let mut trial = self.document.clone();
                edit::move_column_border(&mut trial, table, *border, wanted)
            }
            TableBorder::Row { height, .. } => {
                let now = (f64::from(*height) * TWIPS_PT).round() as Twips;
                wanted.clamp(-now, (MAX_ROW_HEIGHT - now).max(0))
            }
        }
    }

    /// The pointer moved while a line is dragged. `None` when none is.
    pub(super) fn drag_border(&mut self, x: f64, y: f64) -> Option<bool> {
        let drag = self.border.as_ref()?;
        let ppp = self.pixels_per_point();
        let pixels = match drag.border {
            TableBorder::Column { .. } => x - drag.start.0,
            TableBorder::Row { .. } => y - drag.start.1,
        };
        let wanted = (pixels / ppp * TWIPS_PT).round() as Twips;
        let applied = self.border_shift(&drag.border, wanted);
        if applied == drag.applied {
            return Some(false);
        }
        if let Some(drag) = &mut self.border {
            drag.applied = applied;
        }
        self.touch();
        Some(true)
    }

    /// The button went up after dragging a line: the document changes, as
    /// one step of undo. Returns whether a line was being dragged and the
    /// document changed.
    pub(super) fn finish_border(&mut self) -> bool {
        let Some(drag) = self.border.take() else {
            return false;
        };
        self.touch();
        if drag.applied == 0 || self.compose.is_some() {
            return false;
        }
        let shift = drag.applied;
        match drag.border {
            TableBorder::Column { table, border, .. } => {
                self.apply_in_place(|document, selection| {
                    (edit::move_column_border(document, &table, border, shift) != 0)
                        .then(|| selection.clone())
                })
            }
            TableBorder::Row {
                table, row, height, ..
            } => self.apply_in_place(|document, selection| {
                let at = edit::row_start(document, &table, row)?;
                let now = (f64::from(height) * TWIPS_PT).round() as Twips;
                let exact = document
                    .block(&table)
                    .and_then(|b| b.as_table())
                    .and_then(|t| t.rows.get(row))
                    .is_some_and(|r| matches!(r.height, RowHeight::Exactly(_)));
                let new = (now + shift).max(0);
                let height = if exact {
                    RowHeight::Exactly(new.max(MIN_EXACT_ROW))
                } else {
                    RowHeight::AtLeast(new)
                };
                edit::set_row_height(document, &at, &at, height).then(|| selection.clone())
            }),
        }
    }

    /// Paint the guide line of the line being dragged on one page.
    pub(super) fn draw_border_guide(
        &mut self,
        index: usize,
        page: Rect,
        ppp: f64,
        style: &DocStyle,
    ) {
        let Some(drag) = &self.border else {
            return;
        };
        let shift = f64::from(drag.applied) / TWIPS_PT;
        let thickness = (1.5 * self.s()).round().max(1.0);
        let guide = match &drag.border {
            TableBorder::Column {
                page: on,
                x,
                y0,
                y1,
                ..
            } if *on == index => {
                let at = page.x0 + (f64::from(*x) + shift) * ppp;
                Rect::new(
                    (at - thickness / 2.0).round(),
                    page.y0 + f64::from(*y0) * ppp,
                    (at - thickness / 2.0).round() + thickness,
                    page.y0 + f64::from(*y1) * ppp,
                )
            }
            TableBorder::Row {
                page: on,
                y,
                x0,
                x1,
                ..
            } if *on == index => {
                let at = page.y0 + (f64::from(*y) + shift) * ppp;
                Rect::new(
                    page.x0 + f64::from(*x0) * ppp,
                    (at - thickness / 2.0).round(),
                    page.x0 + f64::from(*x1) * ppp,
                    (at - thickness / 2.0).round() + thickness,
                )
            }
            _ => return,
        };
        self.ctx.set_paint(color(style.object));
        self.ctx.fill_rect(&guide);
    }
}
