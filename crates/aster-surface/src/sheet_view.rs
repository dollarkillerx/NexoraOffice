//! A worksheet on screen: column and row headers, grid lines, cell values,
//! merged cells and the selected cell. Read-only.
//!
//! Only what is inside the window is laid out and drawn, so a sheet of a
//! million rows costs the same as a small one. Laid-out cell texts are kept
//! between frames and dropped when the scale changes.
//!
//! Geometry is in logical pixels (the sheet's own units: a default column is
//! 64 wide); the pixel buffer and pointer positions are physical pixels.

use std::collections::HashMap;
use std::sync::Arc;

use aster_sheets::{
    Anchor, Cell, CellRange, CellRef, DisplayKind, HAlign, Line, LineStyle, MAX_COLUMNS, MAX_ROWS,
    Picture, Placement, Rgba, Sheet, VAlign, Workbook, column_name,
};
use aster_text::{Align, Family, Paragraph, Rules, Shaper, Span, Spec, Style as TextStyle};
use vello_cpu::kurbo::{Affine, Rect, Shape, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::cell_editor::{CellEditor, EditMode};
use crate::picture::{ImageCache, draw_image};
use crate::preview::draw_text;
use crate::text_surface::ImeEvent;
use crate::text_surface::{DEFAULT_LANGUAGE, HAN_FALLBACK_FAMILIES, Rgba8};

/// Text size in headers and picture labels, logical pixels.
const TEXT_SIZE: f32 = 13.0;
/// Logical pixels per point.
const PX_PER_POINT: f32 = 96.0 / 72.0;
/// Cell text is drawn a little smaller than its point size: the fonts
/// standing in for Calibri and 等线 on this system are wider than them, and
/// column widths were set for those.
const TEXT_FIT: f32 = 0.9;
/// One indent level: three spaces of the default font.
const INDENT: f64 = 9.0;
const HEADER_HEIGHT: f64 = 22.0;
/// Space between a cell's edge and its text.
const PAD: f64 = 4.0;
/// Laid-out texts kept at most; past this the cache starts over.
const CACHE_LIMIT: usize = 20_000;
/// Rows and columns that can be scrolled to past the last used cell.
const EXTRA_ROWS: u32 = 50;
const EXTRA_COLS: u32 = 12;

fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Colours of the grid. All opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetStyle {
    pub background: Rgba8,
    pub text: Rgba8,
    /// Formulas without a saved result.
    pub secondary: Rgba8,
    pub grid: Rgba8,
    pub header: Rgba8,
    pub header_text: Rgba8,
    /// Header of the selected row and column.
    pub header_active: Rgba8,
    pub accent: Rgba8,
}

impl Default for SheetStyle {
    fn default() -> Self {
        Self {
            background: [0xff, 0xff, 0xff, 0xff],
            text: [0x1f, 0x1f, 0x1f, 0xff],
            secondary: [0x6b, 0x6b, 0x6b, 0xff],
            grid: [0xe3, 0xe3, 0xe3, 0xff],
            header: [0xf5, 0xf5, 0xf5, 0xff],
            header_text: [0x5c, 0x5c, 0x5c, 0xff],
            header_active: [0xe4, 0xdc, 0xfa, 0xff],
            accent: [0x7c, 0x5c, 0xe0, 0xff],
        }
    }
}

/// Moving the selection with the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    /// To column A of the row.
    Home,
    /// To the last used column of the row.
    End,
    /// To A1.
    Start,
    /// To the last used cell.
    Last,
    /// With the primary modifier: to the edge of the data in that direction.
    Jump(Direction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Where each sheet was left, so switching back returns there.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Place {
    /// The active cell: where typing goes, inside the selection.
    selected: CellRef,
    /// The corner the selection was started from, and the one it was
    /// stretched to; both are `selected` for one cell.
    anchor: CellRef,
    end: CellRef,
    scroll_x: f64,
    scroll_y: f64,
}

/// What a render depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawn {
    sheet: usize,
    place: Place,
    size: (u16, u16),
    scale: f32,
    style: SheetStyle,
    /// Counts changes to the cells and to the editor.
    revision: u64,
    caret_on: bool,
}

/// A column or row whose size is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resize {
    Column(u32),
    Row(u32),
}

/// A filter button in a header cell whose right and bottom edges are at
/// `right` and `bottom` (logical pixels): a square as tall as the row
/// allows, up to 16, inside the cell's right edge. `None` when the cell is
/// too small to hold one.
fn filter_button(right: f64, bottom: f64, width: f64, height: f64) -> Option<Rect> {
    let size = (height - 3.0).min(16.0).min(width - 3.0);
    if size < 8.0 {
        return None;
    }
    let (x1, y1) = (right - 2.0, bottom - 2.0);
    Some(Rect::new(x1 - size, y1 - size, x1, y1))
}

/// The corner that marks a cell with a note.
const COMMENT_MARK: Rgba8 = [0xd9, 0x2d, 0x20, 0xff];

/// The smallest size a drag leaves, logical pixels.
pub const MIN_SIZE: f64 = 4.0;

/// What is under a point of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Cell(CellRef),
    /// A row number.
    Row(u32),
    /// A column letter.
    Column(u32),
    /// The corner above the row numbers: the whole sheet.
    All,
}

pub struct SheetView {
    workbook: Arc<Workbook>,
    sheet: usize,
    places: Vec<Place>,
    /// Where a fill handle drag would fill, while one is going on.
    fill_preview: Option<CellRange>,
    width: u16,
    height: u16,
    scale: f32,
    style: SheetStyle,
    shaper: Shaper,
    /// Laid-out cell texts by sheet and cell; `None` for nothing to draw.
    cells: HashMap<(usize, CellRef), Option<Arc<Laid>>>,
    headers: HashMap<String, Arc<Paragraph>>,
    hashes: Option<Arc<Paragraph>>,
    ctx: RenderContext,
    pixmap: Pixmap,
    resources: Resources,
    glyphs: Vec<Glyph>,
    drawn: Option<Drawn>,
    /// Which styles have a fill or borders, by index.
    decorated: Vec<bool>,
    any_decorated: bool,
    images: ImageCache,
    /// The cell being typed into, if any.
    editor: Option<CellEditor>,
    revision: u64,
    caret_on: bool,
    /// Where the caret was last drawn, for the input method's candidate
    /// window: x, y, width, height, physical pixels.
    ime_area: (f64, f64, f64, f64),
}

/// A cell's text, laid out.
struct Laid {
    paragraph: Paragraph,
    align: HAlign,
    valign: VAlign,
    kind: DisplayKind,
    /// Logical pixels.
    indent: f64,
    wrap: bool,
}

/// A part of the window that scrolls as one: the frozen corner, the frozen
/// rows, the frozen columns or the rest.
struct Pane {
    first: CellRef,
    last: CellRef,
    /// Logical pixels this pane is scrolled by.
    scroll_x: f64,
    scroll_y: f64,
    header_w: f64,
    /// Physical pixels.
    clip: Rect,
    frozen_rows: bool,
    frozen_cols: bool,
}

impl Pane {
    /// Left edge of a column in this pane, physical pixels.
    fn x(&self, sheet: &Sheet, col: u32, s: f64) -> f64 {
        (self.header_w + sheet.columns.offset(col) - self.scroll_x) * s
    }

    /// Top edge of a row in this pane, physical pixels.
    fn y(&self, sheet: &Sheet, row: u32, s: f64) -> f64 {
        (HEADER_HEIGHT + sheet.row_sizes.offset(row) - self.scroll_y) * s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

fn decorations(workbook: &Workbook) -> Vec<bool> {
    workbook
        .styles
        .iter()
        .map(|style| style.is_visible_when_empty())
        .collect()
}

impl SheetView {
    pub fn new(workbook: Arc<Workbook>) -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
        let sheets = workbook.sheets.len().max(1);
        let first = workbook.active_sheet.min(sheets - 1);
        let mut view = Self {
            workbook,
            sheet: 0,
            places: vec![Place::default(); sheets],
            fill_preview: None,
            width: 0,
            height: 0,
            scale: 1.0,
            style: SheetStyle::default(),
            shaper,
            cells: HashMap::new(),
            headers: HashMap::new(),
            hashes: None,
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            resources: Resources::new(),
            glyphs: Vec::new(),
            drawn: None,
            decorated: Vec::new(),
            any_decorated: false,
            images: ImageCache::default(),
            editor: None,
            revision: 0,
            caret_on: true,
            ime_area: (0.0, 0.0, 1.0, 1.0),
        };
        view.decorated = decorations(&view.workbook);
        view.any_decorated = view.decorated.contains(&true);
        // A hidden sheet is not where a file opens.
        let start = if view.is_visible(first) {
            first
        } else {
            view.visible_sheets().first().copied().unwrap_or(0)
        };
        view.sheet = start;
        view
    }

    pub fn workbook(&self) -> &Arc<Workbook> {
        &self.workbook
    }

    /// Show another version of the same workbook, such as one with
    /// calculated results, keeping each sheet's place.
    pub fn set_workbook(&mut self, workbook: Arc<Workbook>) {
        if workbook.sheets.len() != self.workbook.sheets.len() {
            self.places = vec![Place::default(); workbook.sheets.len().max(1)];
            self.sheet = self.sheet.min(workbook.sheets.len().saturating_sub(1));
        }
        self.workbook = workbook;
        self.decorated = decorations(&self.workbook);
        self.any_decorated = self.decorated.contains(&true);
        self.cells.clear();
        self.drawn = None;
    }

    fn is_visible(&self, index: usize) -> bool {
        self.workbook.sheets.get(index).is_some_and(|s| !s.hidden)
    }

    /// Indices of the sheets that are not hidden; all of them if every one
    /// is.
    pub fn visible_sheets(&self) -> Vec<usize> {
        let visible: Vec<usize> = (0..self.workbook.sheets.len())
            .filter(|&i| self.is_visible(i))
            .collect();
        if visible.is_empty() {
            (0..self.workbook.sheets.len()).collect()
        } else {
            visible
        }
    }

    pub fn sheet_index(&self) -> usize {
        self.sheet
    }

    pub fn sheet(&self) -> Option<&Sheet> {
        self.workbook.sheets.get(self.sheet)
    }

    pub fn set_sheet(&mut self, index: usize) -> bool {
        if index == self.sheet || index >= self.workbook.sheets.len() {
            return false;
        }
        self.sheet = index;
        true
    }

    pub fn selected(&self) -> CellRef {
        self.place().selected
    }

    /// The selected cells: from the active cell to the other corner,
    /// widened to whole merged areas at the corners.
    pub fn selection(&self) -> CellRange {
        let place = self.place();
        let mut range = CellRange::new(
            CellRef::new(
                place.anchor.row.min(place.end.row),
                place.anchor.col.min(place.end.col),
            ),
            CellRef::new(
                place.anchor.row.max(place.end.row),
                place.anchor.col.max(place.end.col),
            ),
        );
        if let Some(sheet) = self.sheet() {
            for corner in [place.anchor, place.end] {
                if let Some(merge) = sheet.merge_at(corner) {
                    range = range.union(merge);
                }
            }
        }
        range
    }

    /// The selection's address (a merged area counts as one cell) and what
    /// the formula bar shows: the active cell's content, or the text being
    /// typed.
    pub fn selection_text(&self) -> (String, String) {
        let at = self.selected();
        let Some(sheet) = self.sheet() else {
            return (at.to_string(), String::new());
        };
        let range = self.selection();
        let address = match sheet.merge_at(at) {
            Some(merge) if *merge == range => merge.to_string(),
            _ if range.first == range.last => at.to_string(),
            _ => range.to_string(),
        };
        if let Some(editor) = &self.editor {
            return (address, editor.shown().0);
        }
        let content = sheet
            .cell(at)
            .map(|cell| self.workbook.input_text(cell))
            .unwrap_or_default();
        (address, content)
    }

    /// The workbook, to change cells. Laid-out texts are dropped, since any
    /// cell may have changed.
    pub fn workbook_mut(&mut self) -> &mut Workbook {
        self.cells.clear();
        self.revision += 1;
        Arc::make_mut(&mut self.workbook)
    }

    // --- Typing into a cell ---

    pub fn editor(&self) -> Option<&CellEditor> {
        self.editor.as_ref()
    }

    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    /// Start typing into the active cell: `text` replaces its content, or
    /// with `None` the content is edited in place.
    pub fn begin_edit(&mut self, mode: EditMode, text: Option<String>) {
        let at = self.selected();
        let text = text.unwrap_or_else(|| {
            self.sheet()
                .and_then(|sheet| sheet.cell(at))
                .map(|cell| self.workbook.input_text(cell))
                .unwrap_or_default()
        });
        self.editor = Some(CellEditor::new(at, mode, text));
        self.ensure_visible();
        self.revision += 1;
    }

    /// Change the editor (insert, move the caret ...). Returns what `f`
    /// returns, `None` when nothing is being edited.
    pub fn edit<T>(&mut self, f: impl FnOnce(&mut CellEditor) -> T) -> Option<T> {
        let editor = self.editor.as_mut()?;
        let out = f(editor);
        self.revision += 1;
        Some(out)
    }

    /// An input method event: starts an entry on the active cell when
    /// nothing is being edited and text is being composed or committed.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        if self.editor.is_none() {
            match &event {
                ImeEvent::Preedit { text, .. } if !text.is_empty() => {}
                ImeEvent::Commit(_) => {}
                _ => return false,
            }
            self.begin_edit(EditMode::Enter, Some(String::new()));
        }
        self.edit(|editor| editor.ime(event)).unwrap_or(false)
    }

    /// Stop editing without keeping what was typed.
    pub fn cancel_edit(&mut self) -> bool {
        let had = self.editor.take().is_some();
        if had {
            self.revision += 1;
        }
        had
    }

    /// Stop editing and hand back the cell and what was typed.
    pub fn finish_edit(&mut self) -> Option<(CellRef, String)> {
        let mut editor = self.editor.take()?;
        // A composition still open is committed as it stands.
        if let (shown, Some(_), _) = editor.shown() {
            editor = CellEditor::new(editor.at, editor.mode, shown);
        }
        self.revision += 1;
        Some((editor.at, editor.text().to_owned()))
    }

    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    pub fn is_composing(&self) -> bool {
        self.editor.as_ref().is_some_and(CellEditor::is_composing)
    }

    /// Where the input method's candidate window goes: under the caret.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        if self.needs_render() {
            self.render();
        }
        self.ime_area
    }

    fn place(&self) -> Place {
        self.places.get(self.sheet).copied().unwrap_or_default()
    }

    fn place_mut(&mut self) -> &mut Place {
        let sheets = self.workbook.sheets.len().max(1);
        if self.places.len() < sheets {
            self.places.resize(sheets, Place::default());
        }
        let index = self.sheet.min(self.places.len() - 1);
        &mut self.places[index]
    }

    /// A sheet came in at `index`: it starts at its first cell, the others
    /// keep where they were.
    pub fn sheet_inserted(&mut self, index: usize) {
        self.places
            .insert(index.min(self.places.len()), Place::default());
        self.cells.clear();
        self.revision += 1;
    }

    /// The sheet at `index` went.
    pub fn sheet_removed(&mut self, index: usize) {
        if index < self.places.len() && self.places.len() > 1 {
            self.places.remove(index);
        }
        if self.sheet >= self.workbook.sheets.len() {
            self.sheet = self.workbook.sheets.len().saturating_sub(1);
        }
        self.cells.clear();
        self.revision += 1;
    }

    pub fn set_style(&mut self, style: SheetStyle) {
        if style != self.style {
            self.style = style;
            self.cells.clear();
            self.headers.clear();
            self.hashes = None;
        }
    }

    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height) == (self.width, self.height) && scale == self.scale {
            return;
        }
        if scale != self.scale {
            self.cells.clear();
            self.headers.clear();
            self.hashes = None;
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        self.drawn = None;
        self.clamp();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    fn s(&self) -> f64 {
        f64::from(self.scale)
    }

    /// Width of the row-number column, logical pixels: room for the largest
    /// row number on screen.
    fn header_width(&self) -> f64 {
        let last = self.last_visible_row() + 1;
        let digits = last.to_string().len().max(2) as f64;
        (digits * 8.0 + 14.0).max(36.0)
    }

    fn last_visible_row(&self) -> u32 {
        let Some(sheet) = self.sheet() else {
            return 0;
        };
        let place = self.place();
        let height = f64::from(self.height) / self.s() - HEADER_HEIGHT;
        sheet.row_sizes.index_at(place.scroll_y + height.max(0.0))
    }

    /// The cells area, logical pixels.
    fn area(&self) -> (f64, f64) {
        (
            (f64::from(self.width) / self.s() - self.header_width()).max(0.0),
            (f64::from(self.height) / self.s() - HEADER_HEIGHT).max(0.0),
        )
    }

    /// The last row and column one can scroll to.
    fn extent(&self) -> (u32, u32) {
        let Some(sheet) = self.sheet() else {
            return (0, 0);
        };
        let used = sheet
            .used_range()
            .unwrap_or_else(|| CellRange::single(CellRef::default()));
        let selected = self.place().selected;
        (
            (used.last.row.max(selected.row) + EXTRA_ROWS).min(MAX_ROWS - 1),
            (used.last.col.max(selected.col) + EXTRA_COLS).min(MAX_COLUMNS - 1),
        )
    }

    fn max_scroll(&self) -> (f64, f64) {
        let Some(sheet) = self.sheet() else {
            return (0.0, 0.0);
        };
        let (rows, cols) = self.extent();
        let (area_w, area_h) = self.area();
        (
            (sheet.columns.offset(cols + 1) - area_w).max(0.0),
            (sheet.row_sizes.offset(rows + 1) - area_h).max(0.0),
        )
    }

    fn clamp(&mut self) {
        let (max_x, max_y) = self.max_scroll();
        let place = self.place_mut();
        place.scroll_x = place.scroll_x.clamp(0.0, max_x);
        place.scroll_y = place.scroll_y.clamp(0.0, max_y);
    }

    /// Scroll by physical pixels; returns whether the view moved.
    pub fn scroll_by(&mut self, dx: f64, dy: f64) -> bool {
        let before = self.place();
        let s = self.s();
        let place = self.place_mut();
        place.scroll_x += dx / s;
        place.scroll_y += dy / s;
        self.clamp();
        self.place() != before
    }

    /// Vertical scroll offset and its bound, physical pixels.
    pub fn scroll_range(&self) -> (f64, f64) {
        let (_, max_y) = self.max_scroll();
        (self.place().scroll_y * self.s(), max_y * self.s())
    }

    /// Horizontal scroll offset and its bound, physical pixels.
    pub fn scroll_range_x(&self) -> (f64, f64) {
        let (max_x, _) = self.max_scroll();
        (self.place().scroll_x * self.s(), max_x * self.s())
    }

    /// Where the frozen rows and columns end, logical pixels from the top
    /// left of the cells area.
    fn frozen_size(&self) -> (f64, f64) {
        self.sheet().map_or((0.0, 0.0), |sheet| {
            (
                sheet.columns.offset(sheet.frozen.1),
                sheet.row_sizes.offset(sheet.frozen.0),
            )
        })
    }

    /// Where a column starts and ends on screen, logical pixels from the
    /// window's left: frozen columns do not scroll.
    fn column_span(&self, col: u32) -> (f64, f64) {
        let Some(sheet) = self.sheet() else {
            return (0.0, 0.0);
        };
        let scroll = if col < sheet.frozen.1 {
            0.0
        } else {
            self.place().scroll_x
        };
        let left = self.header_width() + sheet.columns.offset(col) - scroll;
        (left, left + sheet.columns.size(col))
    }

    /// Where a row starts and ends on screen, logical pixels from the top.
    fn row_span(&self, row: u32) -> (f64, f64) {
        let Some(sheet) = self.sheet() else {
            return (0.0, 0.0);
        };
        let scroll = if row < sheet.frozen.0 {
            0.0
        } else {
            self.place().scroll_y
        };
        let top = HEADER_HEIGHT + sheet.row_sizes.offset(row) - scroll;
        (top, top + sheet.row_sizes.size(row))
    }

    /// The column or row whose far edge is under a point (physical pixels)
    /// on the column letters or row numbers: what dragging there resizes.
    pub fn resize_hit(&self, x: f64, y: f64) -> Option<Resize> {
        const NEAR: f64 = 4.0;
        let sheet = self.sheet()?;
        let (x, y) = (x / self.s(), y / self.s());
        let header = self.header_width();
        if y < HEADER_HEIGHT && x >= header {
            let col = self
                .cell_at(x * self.s(), (HEADER_HEIGHT + 1.0) * self.s())?
                .col;
            let (left, right) = self.column_span(col);
            if right - x <= NEAR {
                return Some(Resize::Column(col));
            }
            if x - left <= NEAR {
                return sheet.columns.previous_visible(col).map(Resize::Column);
            }
        } else if x < header && y >= HEADER_HEIGHT {
            let row = self.cell_at((header + 1.0) * self.s(), y * self.s())?.row;
            let (top, bottom) = self.row_span(row);
            if bottom - y <= NEAR {
                return Some(Resize::Row(row));
            }
            if y - top <= NEAR {
                return sheet.row_sizes.previous_visible(row).map(Resize::Row);
            }
        }
        None
    }

    /// Whether a point (physical pixels) is on the fill handle: the small
    /// square at the bottom right corner of the selection.
    pub fn fill_handle_hit(&self, x: f64, y: f64) -> bool {
        const NEAR: f64 = 5.0;
        if self.editor.is_some() {
            return false;
        }
        let range = self.selection();
        if range.rows() == MAX_ROWS || range.cols() == MAX_COLUMNS {
            return false;
        }
        let (x, y) = (x / self.s(), y / self.s());
        let (_, right) = self.column_span(range.last.col);
        let (_, bottom) = self.row_span(range.last.row);
        (x - right).abs() <= NEAR && (y - bottom).abs() <= NEAR
    }

    /// The column of the filter button under a point (physical pixels), if
    /// any: the square at the right of each header cell of the sheet's
    /// autofilter.
    pub fn filter_button_hit(&self, x: f64, y: f64) -> Option<u32> {
        let sheet = self.sheet()?;
        let filter = sheet.auto_filter.as_ref()?;
        if self.editor.is_some() {
            return None;
        }
        let at = match self.hit(x, y)? {
            Hit::Cell(at) => at,
            _ => return None,
        };
        let at = sheet
            .merge_at(at)
            .map_or(at, |m| CellRef::new(m.first.row, m.last.col));
        if at.row != filter.range.first.row
            || !(filter.range.first.col..=filter.range.last.col).contains(&at.col)
        {
            return None;
        }
        let (_, right) = self.column_span(at.col);
        let (_, bottom) = self.row_span(at.row);
        let button = filter_button(
            right,
            bottom,
            sheet.columns.size(at.col),
            sheet.row_sizes.size(at.row),
        )?;
        let (x, y) = (x / self.s(), y / self.s());
        let on = (button.x0..=button.x1).contains(&x) && (button.y0..=button.y1).contains(&y);
        on.then_some(at.col)
    }

    /// Where the filter button of a column is: its bottom left and bottom
    /// right corners' x and their y, physical pixels from the view's top
    /// left, to open its list under it.
    pub fn filter_button_point(&self, col: u32) -> Option<(f64, f64, f64)> {
        let sheet = self.sheet()?;
        let filter = sheet.auto_filter.as_ref()?;
        let (_, right) = self.column_span(col);
        let (_, bottom) = self.row_span(filter.range.first.row);
        let button = filter_button(
            right,
            bottom,
            sheet.columns.size(col),
            sheet.row_sizes.size(filter.range.first.row),
        )?;
        let s = self.s();
        Some((button.x0 * s, button.x1 * s, button.y1 * s))
    }

    /// Where a cell (all of its merged area) is on screen: left, top, right
    /// and bottom, physical pixels from the view's top left.
    pub fn cell_box(&self, at: CellRef) -> Option<(f64, f64, f64, f64)> {
        let sheet = self.sheet()?;
        let (first, last) = sheet
            .merge_at(at)
            .map_or((at, at), |merge| (merge.first, merge.last));
        let (left, _) = self.column_span(first.col);
        let (_, right) = self.column_span(last.col);
        let (top, _) = self.row_span(first.row);
        let (_, bottom) = self.row_span(last.row);
        let s = self.s();
        Some((left * s, top * s, right * s, bottom * s))
    }

    /// The drop-down arrow of the active cell, if it has one and nothing
    /// is being typed: left, right and bottom edges, physical pixels from
    /// the view's top left (where to open its list).
    pub fn dropdown_button_point(&self) -> Option<(f64, f64, f64)> {
        let sheet = self.sheet()?;
        let at = self.selected();
        if self.editor.is_some() || !sheet.has_dropdown(at) {
            return None;
        }
        let (x0, y0, x1, y1) = self.cell_box(at)?;
        let s = self.s();
        let button = filter_button(x1 / s, y1 / s, (x1 - x0) / s, (y1 - y0) / s)?;
        Some((button.x0 * s, button.x1 * s, button.y1 * s))
    }

    /// Whether a point (physical pixels) is on the active cell's
    /// drop-down arrow.
    pub fn dropdown_button_hit(&self, x: f64, y: f64) -> bool {
        let Some((x0, x1, bottom)) = self.dropdown_button_point() else {
            return false;
        };
        let size = x1 - x0;
        (x0..=x1).contains(&x) && (bottom - size..=bottom).contains(&y)
    }

    /// Where the fill handle is, physical pixels from the view's top left.
    pub fn fill_handle_point(&self) -> (f64, f64) {
        let range = self.selection();
        let (_, right) = self.column_span(range.last.col);
        let (_, bottom) = self.row_span(range.last.row);
        (right * self.s(), bottom * self.s())
    }

    /// The range a fill handle dragged to a point (physical pixels) covers:
    /// the selection stretched down, up, right or left, whichever way the
    /// point is further out of it.
    pub fn fill_target(&self, x: f64, y: f64) -> CellRange {
        let range = self.selection();
        let Some(at) = self.cell_at(x, y) else {
            return range;
        };
        let below = at.row.saturating_sub(range.last.row);
        let above = range.first.row.saturating_sub(at.row);
        let right = at.col.saturating_sub(range.last.col);
        let left = range.first.col.saturating_sub(at.col);
        let rows = below.max(above);
        let cols = right.max(left);
        if rows == 0 && cols == 0 {
            return range;
        }
        if rows >= cols {
            if below > 0 {
                CellRange::new(range.first, CellRef::new(at.row, range.last.col))
            } else {
                CellRange::new(CellRef::new(at.row, range.first.col), range.last)
            }
        } else if right > 0 {
            CellRange::new(range.first, CellRef::new(range.last.row, at.col))
        } else {
            CellRange::new(CellRef::new(range.first.row, at.col), range.last)
        }
    }

    /// Show (or stop showing) where a fill handle drag would fill.
    pub fn set_fill_preview(&mut self, range: Option<CellRange>) {
        if self.fill_preview != range {
            self.fill_preview = range;
            self.revision += 1;
        }
    }

    /// The size a column or row gets when its far edge is dragged to a
    /// point (physical pixels), logical pixels; it may be below
    /// [`MIN_SIZE`] or negative, the caller clamps.
    pub fn resized_size(&self, resize: Resize, x: f64, y: f64) -> f64 {
        let size = match resize {
            Resize::Column(col) => x / self.s() - self.column_span(col).0,
            Resize::Row(row) => y / self.s() - self.row_span(row).0,
        };
        size.round()
    }

    /// The cell under a point (physical pixels), if it is in the cells area.
    pub fn cell_at(&self, x: f64, y: f64) -> Option<CellRef> {
        let sheet = self.sheet()?;
        let (x, y) = (x / self.s(), y / self.s());
        let header_width = self.header_width();
        let place = self.place();
        let (frozen_w, frozen_h) = self.frozen_size();
        // Frozen rows and columns do not scroll.
        let row_at = |y: f64| {
            let scroll = if y < frozen_h { 0.0 } else { place.scroll_y };
            sheet.row_sizes.index_at(y + scroll)
        };
        let col_at = |x: f64| {
            let scroll = if x < frozen_w { 0.0 } else { place.scroll_x };
            sheet.columns.index_at(x + scroll)
        };
        let row = (y >= HEADER_HEIGHT).then(|| row_at(y - HEADER_HEIGHT));
        let col = (x >= header_width).then(|| col_at(x - header_width));
        let first_col = || {
            if sheet.frozen.1 > 0 {
                sheet.columns.index_at(0.0)
            } else {
                col_at(frozen_w)
            }
        };
        let first_row = || {
            if sheet.frozen.0 > 0 {
                sheet.row_sizes.index_at(0.0)
            } else {
                row_at(frozen_h)
            }
        };
        match (row, col) {
            (Some(row), Some(col)) => Some(CellRef::new(row, col)),
            // A row number: the first cell of that row on screen.
            (Some(row), None) => Some(CellRef::new(row, first_col())),
            // A column letter: that column's cell in the top row on screen.
            (None, Some(col)) => Some(CellRef::new(first_row(), col)),
            (None, None) => None,
        }
    }

    /// What is under a point (physical pixels).
    pub fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        let s = self.s();
        let in_cells = x / s >= self.header_width();
        let below_headers = y / s >= HEADER_HEIGHT;
        if !in_cells && !below_headers {
            return Some(Hit::All);
        }
        let at = self.cell_at(x, y)?;
        Some(match (in_cells, below_headers) {
            (true, true) => Hit::Cell(at),
            (false, _) => Hit::Row(at.row),
            (true, false) => Hit::Column(at.col),
        })
    }

    /// A click (physical pixels): selects the cell, or the row, column or
    /// sheet whose header it is on; with `extend`, stretches the selection
    /// to there. Returns whether the selection changed.
    pub fn press(&mut self, x: f64, y: f64, extend: bool) -> bool {
        // An entry the sheet refused stays open until it is fixed.
        if self.editor.is_some() {
            return false;
        }
        let Some(hit) = self.hit(x, y) else {
            return false;
        };
        let before = self.place();
        let last_row = MAX_ROWS - 1;
        let last_col = MAX_COLUMNS - 1;
        match hit {
            Hit::Cell(at) if extend => {
                self.extend_to(at);
            }
            Hit::Cell(at) => {
                self.select(at);
            }
            Hit::Row(row) => {
                let first = if extend { self.selected().row } else { row };
                self.select_range(CellRef::new(first, 0), CellRef::new(row, last_col));
            }
            Hit::Column(col) => {
                let first = if extend { self.selected().col } else { col };
                self.select_range(CellRef::new(0, first), CellRef::new(last_row, col));
            }
            Hit::All => self.select_range(CellRef::default(), CellRef::new(last_row, last_col)),
        }
        self.place() != before
    }

    /// The pointer moved with the button down: stretch the selection to
    /// the cell under it.
    pub fn drag(&mut self, x: f64, y: f64) -> bool {
        let s = self.s();
        let x = x.max(self.header_width() * s);
        let y = y.max(HEADER_HEIGHT * s);
        match self.cell_at(x, y) {
            Some(at) => self.extend_to(at),
            None => false,
        }
    }

    /// Select from the active cell `active` to `end`, without scrolling.
    fn select_range(&mut self, active: CellRef, end: CellRef) {
        let place = self.place_mut();
        place.selected = active;
        place.anchor = active;
        place.end = end;
    }

    /// Move the selection's far corner to `at`, keeping the active cell.
    pub fn extend_to(&mut self, at: CellRef) -> bool {
        let at = CellRef::new(at.row.min(MAX_ROWS - 1), at.col.min(MAX_COLUMNS - 1));
        if self.place().end == at {
            return false;
        }
        self.place_mut().end = at;
        self.ensure_cell_visible(at);
        true
    }

    /// Select a cell (the top-left of its merged area) and scroll it into
    /// view.
    pub fn select(&mut self, at: CellRef) -> bool {
        let at = CellRef::new(at.row.min(MAX_ROWS - 1), at.col.min(MAX_COLUMNS - 1));
        let at = self
            .sheet()
            .and_then(|sheet| sheet.merge_at(at))
            .map_or(at, |merge| merge.first);
        let place = self.place();
        let changed = place.selected != at || place.anchor != at || place.end != at;
        let place = self.place_mut();
        place.selected = at;
        place.anchor = at;
        place.end = at;
        self.ensure_visible();
        changed
    }

    fn ensure_visible(&mut self) {
        self.ensure_cell_visible(self.place().selected);
    }

    fn ensure_cell_visible(&mut self, at: CellRef) {
        let Some(sheet) = self.sheet() else {
            return;
        };
        let (frozen_rows, frozen_cols) = sheet.frozen;
        let range = sheet
            .merge_at(at)
            .copied()
            .unwrap_or_else(|| CellRange::single(at));
        let left = sheet.columns.offset(range.first.col);
        let right = sheet.columns.offset(range.last.col + 1);
        let top = sheet.row_sizes.offset(range.first.row);
        let bottom = sheet.row_sizes.offset(range.last.row + 1);
        let (frozen_w, frozen_h) = self.frozen_size();
        let (area_w, area_h) = self.area();
        let place = self.place_mut();
        // Frozen cells are always in view; the others scroll into the part
        // of the window the frozen ones leave.
        if range.first.col >= frozen_cols {
            if right > place.scroll_x + area_w {
                place.scroll_x = right - area_w;
            }
            if left - frozen_w < place.scroll_x {
                place.scroll_x = left - frozen_w;
            }
        }
        if range.first.row >= frozen_rows {
            if bottom > place.scroll_y + area_h {
                place.scroll_y = bottom - area_h;
            }
            if top - frozen_h < place.scroll_y {
                place.scroll_y = top - frozen_h;
            }
        }
        self.clamp();
    }

    /// Move the selection. Returns whether it moved.
    pub fn key(&mut self, key: SheetKey) -> bool {
        self.key_extending(key, false)
    }

    /// Move the selection, or with `extend` its far corner (Shift with the
    /// arrow keys). Returns whether anything moved.
    pub fn key_extending(&mut self, key: SheetKey, extend: bool) -> bool {
        if self.editor.is_some() {
            return false;
        }
        let Some(sheet) = self.sheet() else {
            return false;
        };
        let at = if extend {
            self.place().end
        } else {
            self.place().selected
        };
        // Leaving a merged area starts from its far edge.
        let span = sheet
            .merge_at(at)
            .copied()
            .unwrap_or_else(|| CellRange::single(at));
        let used = sheet.used_range();
        let (_, area_h) = self.area();
        let next_row = |row: u32| sheet.row_sizes.next_visible(row).unwrap_or(row);
        let prev_row = |row: u32| sheet.row_sizes.previous_visible(row).unwrap_or(row);
        let next_col = |col: u32| sheet.columns.next_visible(col).unwrap_or(col);
        let prev_col = |col: u32| sheet.columns.previous_visible(col).unwrap_or(col);
        let target = match key {
            SheetKey::Up => CellRef::new(prev_row(span.first.row), at.col),
            SheetKey::Down => CellRef::new(next_row(span.last.row), at.col),
            SheetKey::Left => CellRef::new(at.row, prev_col(span.first.col)),
            SheetKey::Right => CellRef::new(at.row, next_col(span.last.col)),
            SheetKey::PageUp | SheetKey::PageDown => {
                let top = sheet.row_sizes.offset(at.row);
                let goal = if key == SheetKey::PageUp {
                    top - area_h
                } else {
                    top + area_h
                };
                CellRef::new(sheet.row_sizes.index_at(goal.max(0.0)), at.col)
            }
            SheetKey::Home => CellRef::new(at.row, 0),
            SheetKey::End => CellRef::new(
                at.row,
                sheet
                    .row(at.row)
                    .and_then(|row| row.cells.iter().rev().find(|cell| !cell.is_blank()))
                    .map_or(at.col, |cell| cell.col),
            ),
            SheetKey::Start => CellRef::default(),
            SheetKey::Last => used.map_or(CellRef::default(), |used| used.last),
            SheetKey::Jump(direction) => jump(sheet, at, direction),
        };
        if extend {
            self.extend_to(target)
        } else {
            self.select(target)
        }
    }

    /// Move the active cell inside the selection, as Return and Tab do in
    /// Excel when several cells are selected; with one cell selected, it
    /// moves down or right (up or left when `back`).
    pub fn step(&mut self, across: bool, back: bool) -> bool {
        if self.editor.is_some() {
            return false;
        }
        let range = self.selection();
        if range.first == range.last {
            let key = match (across, back) {
                (false, false) => SheetKey::Down,
                (false, true) => SheetKey::Up,
                (true, false) => SheetKey::Right,
                (true, true) => SheetKey::Left,
            };
            return self.key(key);
        }
        let at = self.selected();
        let (rows, cols) = (
            range.last.row - range.first.row + 1,
            range.last.col - range.first.col + 1,
        );
        // Position inside the range, in reading order (rows or columns).
        let (major, minor, majors, minors) = if across {
            (
                at.row - range.first.row,
                at.col - range.first.col,
                rows,
                cols,
            )
        } else {
            (
                at.col - range.first.col,
                at.row - range.first.row,
                cols,
                rows,
            )
        };
        let total = u64::from(majors) * u64::from(minors);
        let index = u64::from(major) * u64::from(minors) + u64::from(minor);
        let next = if back {
            (index + total - 1) % total
        } else {
            (index + 1) % total
        };
        let (major, minor) = (
            (next / u64::from(minors)) as u32,
            (next % u64::from(minors)) as u32,
        );
        let target = if across {
            CellRef::new(range.first.row + major, range.first.col + minor)
        } else {
            CellRef::new(range.first.row + minor, range.first.col + major)
        };
        self.place_mut().selected = target;
        self.ensure_visible();
        true
    }

    /// Text of the cell for a test or a screen reader.
    pub fn text(&self) -> String {
        self.selection_text().1
    }

    /// A cell's text laid out in its style. `width` is the cell's (or its
    /// merged area's) width, physical pixels, which wrapped text fills.
    fn laid(&mut self, at: CellRef, cell: &Cell, width: f64) -> Option<Arc<Laid>> {
        let key = (self.sheet, at);
        if let Some(found) = self.cells.get(&key) {
            return found.clone();
        }
        if self.cells.len() >= CACHE_LIMIT {
            self.cells.clear();
        }
        let laid = self.workbook.display(cell).map(|display| {
            let style = self.workbook.style(cell.style);
            let font = &style.font;
            let color = match display.kind {
                DisplayKind::Uncomputed => self.style.secondary,
                _ => readable(font.color, style.fill, &self.style),
            };
            let indent = f64::from(style.indent) * INDENT;
            let s = f64::from(self.scale);
            let wrap = style.wrap;
            let text = if wrap {
                display.text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                // Without "wrap text" a cell is one line.
                display.text.replace(['\r', '\n'], " ")
            };
            let text_style = TextStyle {
                family: family(font.name.as_deref()),
                size: font.size * PX_PER_POINT * TEXT_FIT,
                color,
                bold: font.bold,
                italic: font.italic,
                underline: font.underline,
                strike: font.strike,
                rise: 0.0,
            };
            let wrap_width = wrap.then(|| ((width - (2.0 * PAD + indent) * s).max(s)) as f32);
            let align = match display.align {
                HAlign::Left => Align::Start,
                HAlign::Center => Align::Center,
                HAlign::Right => Align::End,
            };
            Arc::new(Laid {
                paragraph: lay_out(
                    &mut self.shaper,
                    &text,
                    text_style,
                    self.scale,
                    align,
                    wrap_width,
                ),
                align: display.align,
                valign: style.valign,
                kind: display.kind,
                indent,
                wrap,
            })
        });
        self.cells.insert(key, laid.clone());
        laid
    }

    fn header_text(&mut self, text: &str) -> Arc<Paragraph> {
        if let Some(found) = self.headers.get(text) {
            return found.clone();
        }
        if self.headers.len() >= CACHE_LIMIT {
            self.headers.clear();
        }
        let paragraph = Arc::new(lay_out(
            &mut self.shaper,
            text,
            ui_text(self.style.header_text, TEXT_SIZE),
            self.scale,
            Align::Start,
            None,
        ));
        self.headers.insert(text.to_owned(), paragraph.clone());
        paragraph
    }

    fn hashes(&mut self) -> Arc<Paragraph> {
        if let Some(hashes) = &self.hashes {
            return hashes.clone();
        }
        let hashes = Arc::new(lay_out(
            &mut self.shaper,
            "#",
            ui_text(self.style.text, TEXT_SIZE),
            self.scale,
            Align::Start,
            None,
        ));
        self.hashes = Some(hashes.clone());
        hashes
    }

    /// A picture's name, for its frame.
    fn label(&mut self, text: &str) -> Arc<Paragraph> {
        let key = format!("\u{1}{text}");
        if let Some(found) = self.headers.get(&key) {
            return found.clone();
        }
        let paragraph = Arc::new(lay_out(
            &mut self.shaper,
            text,
            ui_text(self.style.secondary, TEXT_SIZE),
            self.scale,
            Align::Start,
            None,
        ));
        self.headers.insert(key, paragraph.clone());
        paragraph
    }

    pub fn needs_render(&self) -> bool {
        self.drawn != Some(self.state())
    }

    fn state(&self) -> Drawn {
        Drawn {
            sheet: self.sheet,
            place: self.place(),
            size: (self.width, self.height),
            scale: self.scale,
            style: self.style,
            revision: self.revision,
            caret_on: self.caret_on && self.editor.is_some(),
        }
    }

    /// Draw what is in the window and return the pixels, premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        if !self.needs_render() {
            return &self.pixmap;
        }
        self.drawn = Some(self.state());
        // Typing a date may have added a style.
        if self.decorated.len() != self.workbook.styles.len() {
            self.decorated = decorations(&self.workbook);
            self.any_decorated = self.decorated.contains(&true);
        }
        let workbook = self.workbook.clone();
        let s = self.s();
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let style = self.style;
        self.ctx.reset();
        self.ctx.set_fill_rule(Fill::NonZero);
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(color(style.background));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        let Some(sheet) = workbook.sheets.get(self.sheet) else {
            return self.finish();
        };
        let place = self.place();
        let header_w = self.header_width();
        let (area_w, area_h) = self.area();
        let (frozen_rows, frozen_cols) = sheet.frozen;
        let (frozen_w, frozen_h) = self.frozen_size();
        let line = (s.round()).max(1.0);
        let (left, top) = (header_w * s, HEADER_HEIGHT * s);
        let split_x = (left + frozen_w * s).min(w);
        let split_y = (top + frozen_h * s).min(h);

        // Columns and rows on screen: the frozen ones, then the scrolled ones
        // (first, last, scroll offset, screen span).
        let mut columns: Vec<(u32, u32, f64, f64, f64)> = Vec::new();
        if frozen_cols > 0 && split_x > left {
            let last = sheet.columns.index_at(area_w).min(frozen_cols - 1);
            columns.push((0, last, 0.0, left, split_x));
        }
        if split_x < w {
            let first = sheet
                .columns
                .index_at(frozen_w + place.scroll_x)
                .max(frozen_cols);
            let last = sheet.columns.index_at(place.scroll_x + area_w).max(first);
            columns.push((first, last, place.scroll_x, split_x, w));
        }
        let mut rows: Vec<(u32, u32, f64, f64, f64)> = Vec::new();
        if frozen_rows > 0 && split_y > top {
            let last = sheet.row_sizes.index_at(area_h).min(frozen_rows - 1);
            rows.push((0, last, 0.0, top, split_y));
        }
        if split_y < h {
            let first = sheet
                .row_sizes
                .index_at(frozen_h + place.scroll_y)
                .max(frozen_rows);
            let last = sheet.row_sizes.index_at(place.scroll_y + area_h).max(first);
            rows.push((first, last, place.scroll_y, split_y, h));
        }
        let pane_of = |row_span: &(u32, u32, f64, f64, f64),
                       col_span: &(u32, u32, f64, f64, f64)| Pane {
            first: CellRef::new(row_span.0, col_span.0),
            last: CellRef::new(row_span.1, col_span.1),
            scroll_x: col_span.2,
            scroll_y: row_span.2,
            header_w,
            clip: Rect::new(col_span.3, row_span.3, col_span.4, row_span.4),
            frozen_rows: row_span.0 < frozen_rows,
            frozen_cols: col_span.0 < frozen_cols,
        };
        for row_span in &rows {
            for col_span in &columns {
                self.draw_pane(&workbook, sheet, &pane_of(row_span, col_span));
            }
        }

        // The selection: a tint over the cells other than the active one
        // and an outline round it, in each pane it reaches.
        let selected = place.selected;
        let span = self.selection();
        let active = sheet
            .merge_at(selected)
            .copied()
            .unwrap_or_else(|| CellRange::single(selected));
        if span != active {
            let mut tint = style.accent;
            tint[3] = 0x2a;
            for row_span in &rows {
                for col_span in &columns {
                    let pane = pane_of(row_span, col_span);
                    let rect = |range: &CellRange| {
                        Rect::new(
                            pane.x(sheet, range.first.col, s).round(),
                            pane.y(sheet, range.first.row, s).round(),
                            pane.x(sheet, range.last.col + 1, s).round(),
                            pane.y(sheet, range.last.row + 1, s).round(),
                        )
                    };
                    let whole = rect(&span);
                    let hole = rect(&active);
                    self.ctx.push_clip_layer(&pane.clip.to_path(0.1));
                    self.ctx.set_paint(color(tint));
                    let mut path = whole.to_path(0.1);
                    path.extend(hole.to_path(0.1));
                    self.ctx.set_fill_rule(Fill::EvenOdd);
                    self.ctx.fill_path(&path);
                    self.ctx.set_fill_rule(Fill::NonZero);
                    let border = (2.0 * s).round();
                    self.ctx.set_paint(color(style.accent));
                    self.ctx.set_stroke(Stroke::new(border));
                    self.ctx.stroke_rect(
                        &Rect::new(whole.x0 - line, whole.y0 - line, whole.x1, whole.y1)
                            .inset(-border / 2.0 + line / 2.0),
                    );
                    self.ctx.pop_layer();
                }
            }
        }
        let row_span = rows
            .iter()
            .find(|r| (selected.row < frozen_rows) == (r.0 < frozen_rows));
        let col_span = columns
            .iter()
            .find(|c| (selected.col < frozen_cols) == (c.0 < frozen_cols));
        if let (Some(row_span), Some(col_span)) = (row_span, col_span) {
            let pane = pane_of(row_span, col_span);
            let border = if span == active {
                (2.0 * s).round()
            } else {
                line
            };
            let outline = Rect::new(
                pane.x(sheet, active.first.col, s).round() - line,
                pane.y(sheet, active.first.row, s).round() - line,
                pane.x(sheet, active.last.col + 1, s).round(),
                pane.y(sheet, active.last.row + 1, s).round(),
            );
            self.ctx.push_clip_layer(&pane.clip.to_path(0.1));
            self.ctx.set_paint(color(style.accent));
            self.ctx.set_stroke(Stroke::new(border));
            self.ctx
                .stroke_rect(&outline.inset(-border / 2.0 + line / 2.0));
            // Where a fill handle drag would fill.
            if let Some(fill) = self.fill_preview {
                let rect = Rect::new(
                    pane.x(sheet, fill.first.col, s).round() - line,
                    pane.y(sheet, fill.first.row, s).round() - line,
                    pane.x(sheet, fill.last.col + 1, s).round(),
                    pane.y(sheet, fill.last.row + 1, s).round(),
                );
                self.ctx.set_stroke(Stroke::new(line));
                self.ctx.stroke_rect(&rect.inset(line / 2.0));
            }
            // The fill handle at the selection's bottom right corner.
            let whole = span.rows() == MAX_ROWS || span.cols() == MAX_COLUMNS;
            if !whole && self.editor.is_none() {
                let x = pane.x(sheet, span.last.col + 1, s).round();
                let y = pane.y(sheet, span.last.row + 1, s).round();
                let half = (3.0 * s).round();
                let edge = s.round().max(1.0);
                self.ctx.set_paint(color(style.background));
                self.ctx.fill_rect(&Rect::new(
                    x - half - edge,
                    y - half - edge,
                    x + half + edge,
                    y + half + edge,
                ));
                self.ctx.set_paint(color(style.accent));
                self.ctx
                    .fill_rect(&Rect::new(x - half, y - half, x + half, y + half));
            }
            if self.editor.is_some() {
                let cell = Rect::new(outline.x0 + line, outline.y0 + line, outline.x1, outline.y1);
                self.draw_editor(sheet, cell, pane.clip);
            }
            self.ctx.pop_layer();
        }

        // The print area: a dashed outline in each pane it reaches.
        if !sheet.print_area.is_empty() {
            for row_span in &rows {
                for col_span in &columns {
                    let pane = pane_of(row_span, col_span);
                    for area in &sheet.print_area {
                        let rect = Rect::new(
                            pane.x(sheet, area.first.col, s).round() - line,
                            pane.y(sheet, area.first.row, s).round() - line,
                            pane.x(sheet, area.last.col + 1, s).round(),
                            pane.y(sheet, area.last.row + 1, s).round(),
                        );
                        self.draw_dashed_outline(rect, pane.clip, line, style.header_text);
                    }
                }
            }
        }

        // Where the frozen part ends.
        self.ctx.set_paint(color(style.header_text));
        if frozen_cols > 0 && split_x < w {
            self.ctx
                .fill_rect(&Rect::new(split_x.round() - line, top, split_x.round(), h));
        }
        if frozen_rows > 0 && split_y < h {
            self.ctx
                .fill_rect(&Rect::new(left, split_y.round() - line, w, split_y.round()));
        }

        // Headers over everything else.
        self.ctx.set_paint(color(style.header));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, top));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, left, h));
        for &(first, last, scroll, x0, x1) in &columns {
            self.ctx
                .push_clip_layer(&Rect::new(x0, 0.0, x1, top).to_path(0.1));
            let x_of = |col: u32| (header_w + sheet.columns.offset(col) - scroll) * s;
            for col in first..=last {
                if sheet.columns.size(col) == 0.0 {
                    continue;
                }
                let (x0, x1) = (x_of(col), x_of(col + 1));
                if (span.first.col..=span.last.col).contains(&col) {
                    self.ctx.set_paint(color(style.header_active));
                    self.ctx.fill_rect(&Rect::new(x0, 0.0, x1, top));
                }
                let label = self.header_text(&column_name(col));
                self.draw_centered(&label, Rect::new(x0, 0.0, x1, top));
                self.ctx.set_paint(color(style.grid));
                self.ctx
                    .fill_rect(&Rect::new(x1.round() - line, 0.0, x1.round(), top));
            }
            self.ctx.pop_layer();
        }
        for &(first, last, scroll, y0, y1) in &rows {
            self.ctx
                .push_clip_layer(&Rect::new(0.0, y0, left, y1).to_path(0.1));
            let y_of = |row: u32| (HEADER_HEIGHT + sheet.row_sizes.offset(row) - scroll) * s;
            for row in first..=last {
                if sheet.row_sizes.size(row) == 0.0 {
                    continue;
                }
                let (y0, y1) = (y_of(row), y_of(row + 1));
                if (span.first.row..=span.last.row).contains(&row) {
                    self.ctx.set_paint(color(style.header_active));
                    self.ctx.fill_rect(&Rect::new(0.0, y0, left, y1));
                }
                let label = self.header_text(&(row + 1).to_string());
                self.draw_centered(&label, Rect::new(0.0, y0, left, y1));
                self.ctx.set_paint(color(style.grid));
                self.ctx
                    .fill_rect(&Rect::new(0.0, y1.round() - line, left, y1.round()));
            }
            self.ctx.pop_layer();
        }
        self.ctx.set_paint(color(style.grid));
        self.ctx.fill_rect(&Rect::new(left - line, 0.0, left, h));
        self.ctx.fill_rect(&Rect::new(0.0, top - line, w, top));
        self.finish()
    }

    /// One pane of the sheet: grid, fills, borders, texts and pictures.
    fn draw_pane(&mut self, workbook: &Workbook, sheet: &Sheet, pane: &Pane) {
        let s = self.s();
        let style = self.style;
        let line = (s.round()).max(1.0);
        let clip = pane.clip;
        let x_of = |col: u32| pane.x(sheet, col, s);
        let y_of = |row: u32| pane.y(sheet, row, s);
        let (first_row, last_row) = (pane.first.row, pane.last.row);
        let (first_col, last_col) = (pane.first.col, pane.last.col);
        let view = CellRange::new(pane.first, pane.last);
        self.ctx.push_clip_layer(&clip.to_path(0.1));

        if sheet.show_grid {
            self.ctx.set_paint(color(style.grid));
            for col in first_col..=last_col + 1 {
                if col > first_col && sheet.columns.size(col - 1) == 0.0 {
                    continue;
                }
                let x = x_of(col).round() - line;
                if x >= clip.x0 - line {
                    self.ctx
                        .fill_rect(&Rect::new(x, clip.y0, x + line, clip.y1));
                }
            }
            for row in first_row..=last_row + 1 {
                if row > first_row && sheet.row_sizes.size(row - 1) == 0.0 {
                    continue;
                }
                let y = y_of(row).round() - line;
                if y >= clip.y0 - line {
                    self.ctx
                        .fill_rect(&Rect::new(clip.x0, y, clip.x1, y + line));
                }
            }
        }

        let merges: Vec<CellRange> = sheet
            .merges
            .iter()
            .filter(|merge| merge.intersects(&view))
            .copied()
            .collect();
        let snapped = |range: CellRange| {
            Rect::new(
                x_of(range.first.col).round(),
                y_of(range.first.row).round(),
                x_of(range.last.col + 1).round(),
                y_of(range.last.row + 1).round(),
            )
        };

        // Cells with a fill or borders, found once.
        let mut decorated: Vec<(CellRef, u32)> = Vec::new();
        if self.any_decorated {
            for row in first_row..=last_row {
                if sheet.row_sizes.size(row) == 0.0 {
                    continue;
                }
                for col in first_col..=last_col {
                    if sheet.columns.size(col) == 0.0 {
                        continue;
                    }
                    let at = CellRef::new(row, col);
                    let index = sheet.style_at(at);
                    if self.decorated.get(index as usize).copied().unwrap_or(false) {
                        decorated.push((at, index));
                    }
                }
            }
        }

        // Merged areas cover the lines inside them, with their first cell's
        // fill.
        for merge in &merges {
            let fill = workbook.style(sheet.style_at(merge.first)).fill;
            self.ctx.set_paint(color(fill.unwrap_or(style.background)));
            let rect = snapped(*merge);
            self.ctx.fill_rect(&Rect::new(
                rect.x0,
                rect.y0,
                rect.x1 - if fill.is_some() { 0.0 } else { line },
                rect.y1 - if fill.is_some() { 0.0 } else { line },
            ));
        }
        for &(at, index) in &decorated {
            if merges.iter().any(|merge| merge.contains(at)) {
                continue;
            }
            if let Some(fill) = workbook.styles.get(index as usize).and_then(|st| st.fill) {
                self.ctx.set_paint(color(fill));
                self.ctx.fill_rect(&snapped(CellRange::single(at)));
            }
        }
        for &(at, index) in &decorated {
            let Some(cell_style) = workbook.styles.get(index as usize) else {
                continue;
            };
            let borders = &cell_style.borders;
            if borders.is_empty() {
                continue;
            }
            let merge = merges.iter().find(|merge| merge.contains(at));
            let rect = snapped(CellRange::single(at));
            let inner = |side: Side| {
                merge.is_some_and(|m| match side {
                    Side::Left => at.col > m.first.col,
                    Side::Right => at.col < m.last.col,
                    Side::Top => at.row > m.first.row,
                    Side::Bottom => at.row < m.last.row,
                })
            };
            for (side, border) in [
                (Side::Top, borders.top),
                (Side::Bottom, borders.bottom),
                (Side::Left, borders.left),
                (Side::Right, borders.right),
            ] {
                if let Some(border) = border
                    && !inner(side)
                {
                    self.draw_border(border, side, rect, line);
                }
            }
        }

        // Cell texts: merged areas show their top-left cell.
        let mut texts: Vec<(CellRef, Rect)> = Vec::new();
        for (at, cell) in sheet.cells_in(view) {
            if cell.is_blank() || merges.iter().any(|merge| merge.contains(at)) {
                continue;
            }
            if sheet.columns.size(at.col) == 0.0 || sheet.row_sizes.size(at.row) == 0.0 {
                continue;
            }
            texts.push((
                at,
                Rect::new(
                    x_of(at.col),
                    y_of(at.row),
                    x_of(at.col + 1),
                    y_of(at.row + 1),
                ),
            ));
        }
        for merge in &merges {
            if sheet.is_filled(merge.first) {
                texts.push((
                    merge.first,
                    Rect::new(
                        x_of(merge.first.col),
                        y_of(merge.first.row),
                        x_of(merge.last.col + 1),
                        y_of(merge.last.row + 1),
                    ),
                ));
            }
        }
        for (at, rect) in texts {
            let Some(cell) = sheet.cell(at) else {
                continue;
            };
            let Some(laid) = self.laid(at, cell, rect.width()) else {
                continue;
            };
            let merged = merges.iter().any(|merge| merge.first == at);
            // Unwrapped text runs on over empty cells beside it, as in Excel:
            // to the right when left-aligned, to the left when right-aligned.
            let mut room = rect;
            if !merged && !laid.wrap && laid.kind == DisplayKind::Text {
                match laid.align {
                    HAlign::Left => {
                        let stop = sheet
                            .next_in_row(at)
                            .map_or(last_col + 1, |next| next.col.min(last_col + 1));
                        room.x1 = x_of(stop).max(rect.x1);
                    }
                    HAlign::Right => {
                        let stop = sheet
                            .previous_in_row(at)
                            .map_or(first_col, |previous| (previous.col + 1).max(first_col));
                        room.x0 = x_of(stop).min(rect.x0);
                    }
                    HAlign::Center => {}
                }
            }
            self.draw_cell(&laid, rect, room, s);
        }

        // The autofilter's buttons, at the right of its header cells.
        if let Some(filter) = &sheet.auto_filter {
            let row = filter.range.first.row;
            let first = filter.range.first.col.max(first_col);
            let last = filter.range.last.col.min(last_col);
            if (first_row..=last_row).contains(&row) && sheet.row_sizes.size(row) > 0.0 {
                for col in first..=last {
                    if sheet.columns.size(col) == 0.0 {
                        continue;
                    }
                    let right = x_of(col + 1) / s;
                    let bottom = y_of(row + 1) / s;
                    let height = sheet.row_sizes.size(row);
                    let width = sheet.columns.size(col);
                    if let Some(button) = filter_button(right, bottom, width, height) {
                        let rect = Rect::new(
                            (button.x0 * s).round(),
                            (button.y0 * s).round(),
                            (button.x1 * s).round(),
                            (button.y1 * s).round(),
                        );
                        self.draw_filter_button(rect, filter.filters(col));
                    }
                }
            }
        }

        // The drop-down arrow of the active cell, when a list rule gives
        // it one.
        let active = self.selected();
        if self.editor.is_none() && view.contains(active) && sheet.has_dropdown(active) {
            let area = sheet.merge_at(active);
            let (last_col_of, last_row_of) =
                area.map_or((active.col, active.row), |m| (m.last.col, m.last.row));
            let right = x_of(last_col_of + 1) / s;
            let bottom = y_of(last_row_of + 1) / s;
            let (width, height) = (
                (active.col..=last_col_of)
                    .map(|c| sheet.columns.size(c))
                    .sum::<f64>(),
                (active.row..=last_row_of)
                    .map(|r| sheet.row_sizes.size(r))
                    .sum::<f64>(),
            );
            if let Some(button) = filter_button(right, bottom, width, height) {
                let rect = Rect::new(
                    (button.x0 * s).round(),
                    (button.y0 * s).round(),
                    (button.x1 * s).round(),
                    (button.y1 * s).round(),
                );
                self.draw_filter_button(rect, false);
            }
        }

        // A red corner at the top right of each cell with a note.
        if !sheet.comments.is_empty() {
            let corner = (6.0 * s).round();
            for at in sheet.comments.keys() {
                if !view.contains(*at) {
                    continue;
                }
                let last_col_of = sheet.merge_at(*at).map_or(at.col, |m| m.last.col);
                if sheet.columns.size(at.col) == 0.0 || sheet.row_sizes.size(at.row) == 0.0 {
                    continue;
                }
                let right = x_of(last_col_of + 1) - line;
                let top = y_of(at.row);
                let mut triangle = vello_cpu::kurbo::BezPath::new();
                triangle.move_to((right - corner, top));
                triangle.line_to((right, top));
                triangle.line_to((right, top + corner));
                triangle.close_path();
                self.ctx.set_paint(color(COMMENT_MARK));
                self.ctx.fill_path(&triangle);
            }
        }

        // Pictures float over the cells; each is drawn in the pane its top
        // left corner is fastened in.
        for picture in &sheet.pictures {
            let anchor = match picture.placement {
                Placement::TwoCell { from, .. } | Placement::OneCell { from, .. } => from.cell,
                Placement::Absolute { .. } => CellRef::new(u32::MAX, u32::MAX),
            };
            let frozen_rows = anchor.row < sheet.frozen.0;
            let frozen_cols = anchor.col < sheet.frozen.1;
            if frozen_rows != pane.frozen_rows || frozen_cols != pane.frozen_cols {
                continue;
            }
            let at = |a: &Anchor| (x_of(a.cell.col) + a.dx * s, y_of(a.cell.row) + a.dy * s);
            let rect = match picture.placement {
                Placement::TwoCell { from, to } => {
                    let (x0, y0) = at(&from);
                    let (x1, y1) = at(&to);
                    Rect::new(x0, y0, x1.max(x0), y1.max(y0))
                }
                Placement::OneCell {
                    from,
                    width,
                    height,
                } => {
                    let (x0, y0) = at(&from);
                    Rect::new(x0, y0, x0 + width * s, y0 + height * s)
                }
                Placement::Absolute {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let x0 = (pane.header_w + x - pane.scroll_x) * s;
                    let y0 = (HEADER_HEIGHT + y - pane.scroll_y) * s;
                    Rect::new(x0, y0, x0 + width * s, y0 + height * s)
                }
            };
            if rect.width() < 1.0 || rect.height() < 1.0 || rect.intersect(clip).is_zero_area() {
                continue;
            }
            self.draw_picture(picture, rect);
        }
        self.ctx.pop_layer();
    }

    /// A filter button (physical pixels): a raised square with an arrow
    /// pointing down; a filtered column's is tinted and its arrow takes the
    /// accent colour, with a bar over it.
    fn draw_filter_button(&mut self, rect: Rect, filtered: bool) {
        let style = self.style;
        let s = self.s();
        let line = s.round().max(1.0);
        let face = if filtered {
            style.header_active
        } else {
            style.header
        };
        self.ctx.set_paint(color(face));
        self.ctx.fill_rect(&rect);
        self.ctx.set_paint(color(style.grid));
        self.ctx.set_stroke(Stroke::new(line));
        self.ctx.stroke_rect(&rect.inset(-line / 2.0));
        let mark = if filtered {
            style.accent
        } else {
            style.header_text
        };
        self.ctx.set_paint(color(mark));
        let (cx, cy) = (rect.center().x, rect.center().y);
        let half = (rect.width() * 0.25).max(2.0 * s);
        let mut arrow = vello_cpu::kurbo::BezPath::new();
        let top = if filtered {
            cy - half * 0.3
        } else {
            cy - half * 0.5
        };
        arrow.move_to((cx - half, top));
        arrow.line_to((cx + half, top));
        arrow.line_to((cx, top + half));
        arrow.close_path();
        self.ctx.fill_path(&arrow);
        if filtered {
            let bar = (top - line * 2.0).round();
            self.ctx
                .fill_rect(&Rect::new(cx - half, bar - line, cx + half, bar));
        }
    }

    /// The text being typed, over the active cell (`cell`, physical
    /// pixels), widened to the right to fit it.
    fn draw_editor(&mut self, sheet: &Sheet, cell: Rect, clip: Rect) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let s = self.s();
        let pad = PAD * s;
        let cell_style = self.workbook.style(sheet.style_at(editor.at));
        let font = &cell_style.font;
        let (text, composing, caret) = editor.shown();
        let base = TextStyle {
            family: family(font.name.as_deref()),
            size: font.size * PX_PER_POINT * TEXT_FIT,
            color: readable(font.color, cell_style.fill, &self.style),
            bold: font.bold,
            italic: font.italic,
            underline: font.underline,
            strike: font.strike,
            rise: 0.0,
        };
        let spans: Vec<Span> = composing
            .map(|range| {
                vec![Span {
                    range,
                    style: TextStyle {
                        underline: true,
                        ..base.clone()
                    },
                }]
            })
            .unwrap_or_default();
        let language = aster_text::segment::guess_language(&text, DEFAULT_LANGUAGE);
        let paragraph = self.shaper.lay_out(&Spec {
            text: &text,
            base,
            spans: &spans,
            language,
            width: None,
            scale: self.scale,
            line_height: 1.25,
            align: Align::Start,
            rules: Rules::for_language(language),
        });
        let (pw, ph) = (f64::from(paragraph.width()), f64::from(paragraph.height()));
        let width = cell
            .width()
            .max(pw + 2.0 * pad + 2.0 * s)
            .min((clip.x1 - cell.x0).max(cell.width()));
        let height = cell.height().max(ph + 2.0 * s);
        let rect = Rect::new(cell.x0, cell.y0, cell.x0 + width, cell.y0 + height);
        self.ctx
            .set_paint(color(cell_style.fill.unwrap_or(self.style.background)));
        self.ctx.fill_rect(&rect);
        let border = (2.0 * s).round();
        self.ctx.set_paint(color(self.style.accent));
        self.ctx.set_stroke(Stroke::new(border));
        self.ctx.stroke_rect(&rect.inset(-border / 2.0));
        let x = rect.x0 + pad;
        let y = if paragraph.lines().len() > 1 {
            rect.y0 + s
        } else {
            (rect.y1 - ph - s).max(rect.y0)
        };
        self.ctx.push_clip_layer(&rect.to_path(0.1));
        self.paint(&paragraph, x, y);
        // The caret: before the next character, or after the last.
        let line_height = f64::from(font.size * PX_PER_POINT * TEXT_FIT) * 1.25 * s;
        let next = text[caret..]
            .chars()
            .next()
            .map(|c| caret..caret + c.len_utf8());
        let (cx, cy, ch) = match next.and_then(|r| paragraph.rects(r).into_iter().next()) {
            Some([x0, y0, _, y1]) => (f64::from(x0), f64::from(y0), f64::from(y1 - y0)),
            None => match paragraph.rects(0..caret).last() {
                Some([_, y0, x1, y1]) => (f64::from(*x1), f64::from(*y0), f64::from(y1 - y0)),
                None => (0.0, 0.0, if ph > 0.0 { ph } else { line_height }),
            },
        };
        let caret_rect = Rect::new(x + cx, y + cy, x + cx + s.max(1.0), y + cy + ch);
        if self.caret_on {
            self.ctx.set_paint(color(self.style.text));
            self.ctx.fill_rect(&caret_rect);
        }
        self.ime_area = (
            caret_rect.x0,
            caret_rect.y0,
            caret_rect.width(),
            caret_rect.height(),
        );
        self.ctx.pop_layer();
    }

    /// A picture, or for one that cannot be shown (a chart, a format not
    /// read) a frame with its name.
    fn draw_picture(&mut self, picture: &Picture, rect: Rect) {
        let s = self.s();
        let style = self.style;
        if let Some(image) = picture
            .image
            .as_ref()
            .and_then(|bytes| self.images.get(bytes))
        {
            draw_image(&mut self.ctx, &image, rect, None, Affine::IDENTITY);
            return;
        }
        self.ctx.set_paint(color(style.header));
        self.ctx.fill_rect(&rect);
        self.ctx.set_paint(color(style.grid));
        self.ctx.set_stroke(Stroke::new(s.max(1.0)));
        self.ctx.stroke_rect(&rect.inset(-s / 2.0));
        let label = self.label(&picture.name);
        if f64::from(label.width()) + 8.0 * s <= rect.width()
            && f64::from(label.height()) + 4.0 * s <= rect.height()
        {
            self.draw_centered(&label, rect);
        }
    }

    /// One side of a cell's border, centred on the grid line.
    fn draw_border(&mut self, border: Line, side: Side, rect: Rect, line: f64) {
        let s = self.s();
        let paint = readable_line(border.color, &self.style);
        self.ctx.set_paint(color(paint));
        let width = (border.style.width() * s).round().max(1.0);
        let horizontal = matches!(side, Side::Top | Side::Bottom);
        // The grid line this side sits on: the one left of and above a cell
        // belong to its neighbours.
        let at = match side {
            Side::Top => rect.y0 - line,
            Side::Bottom => rect.y1 - line,
            Side::Left => rect.x0 - line,
            Side::Right => rect.x1 - line,
        };
        let start = (at + line / 2.0 - width / 2.0).round();
        let (from, to) = if horizontal {
            (rect.x0 - line, rect.x1)
        } else {
            (rect.y0 - line, rect.y1)
        };
        let bands: Vec<(f64, f64)> = if border.style == LineStyle::Double {
            let thin = s.round().max(1.0);
            vec![(start, start + thin), (start + width - thin, start + width)]
        } else {
            vec![(start, start + width)]
        };
        let dashes = border.style.dashes();
        for (a, b) in bands {
            let mut segment = |p: f64, q: f64| {
                let rect = if horizontal {
                    Rect::new(p, a, q, b)
                } else {
                    Rect::new(a, p, b, q)
                };
                self.ctx.fill_rect(&rect);
            };
            if dashes.is_empty() {
                segment(from, to);
                continue;
            }
            let mut p = from;
            let mut i = 0;
            while p < to {
                let length = dashes[i % dashes.len()] * s;
                if i % 2 == 0 {
                    segment(p, (p + length).min(to));
                }
                p += length.max(1.0);
                i += 1;
            }
        }
    }

    /// A rectangle's four edges as dashes, `width` thick, only where they
    /// cross `clip`: a print area of a million rows costs what is on
    /// screen.
    fn draw_dashed_outline(&mut self, rect: Rect, clip: Rect, width: f64, paint: Rgba8) {
        let dash = 4.0 * width;
        let period = dash + 3.0 * width;
        self.ctx.push_clip_layer(&clip.to_path(0.1));
        self.ctx.set_paint(color(paint));
        let mut edge = |horizontal: bool, fixed: f64, from: f64, to: f64| {
            let (lo, hi) = if horizontal {
                (clip.x0, clip.x1)
            } else {
                (clip.y0, clip.y1)
            };
            let (across_lo, across_hi) = if horizontal {
                (clip.y0, clip.y1)
            } else {
                (clip.x0, clip.x1)
            };
            if fixed + width < across_lo || fixed > across_hi {
                return;
            }
            let start = from.max(lo - period);
            let mut p = from + ((start - from) / period).floor().max(0.0) * period;
            let end = to.min(hi + period);
            while p < end {
                let q = (p + dash).min(to);
                let piece = if horizontal {
                    Rect::new(p, fixed, q, fixed + width)
                } else {
                    Rect::new(fixed, p, fixed + width, q)
                };
                self.ctx.fill_rect(&piece);
                p += period;
            }
        };
        edge(true, rect.y0, rect.x0, rect.x1);
        edge(true, rect.y1 - width, rect.x0, rect.x1);
        edge(false, rect.x0, rect.y0, rect.y1);
        edge(false, rect.x1 - width, rect.y0, rect.y1);
        self.ctx.pop_layer();
    }

    fn finish(&mut self) -> &Pixmap {
        self.ctx.flush();
        self.ctx.render(&mut self.pixmap, &mut self.resources);
        &self.pixmap
    }

    /// One cell's text in `rect`, allowed to spread over `room`.
    fn draw_cell(&mut self, laid: &Laid, rect: Rect, room: Rect, s: f64) {
        let pad = PAD * s;
        let indent = laid.indent * s;
        let paragraph = &laid.paragraph;
        let width = f64::from(paragraph.width());
        let height = f64::from(paragraph.height());
        let y = match laid.valign {
            VAlign::Top => rect.y0 + s,
            VAlign::Center => (rect.y0 + rect.y1 - height) / 2.0,
            VAlign::Bottom => (rect.y1 - height - s).max(rect.y0),
        };
        // Numbers that do not fit show as ### in Excel.
        if laid.kind == DisplayKind::Number
            && !laid.wrap
            && width > rect.width() - 2.0 * pad - indent
        {
            let hash = self.hashes();
            let each = f64::from(hash.width()).max(1.0);
            let count = ((rect.width() - 2.0 * pad) / each).floor().max(1.0) as usize;
            let y = match laid.valign {
                VAlign::Top => rect.y0 + s,
                VAlign::Center => (rect.y0 + rect.y1 - f64::from(hash.height())) / 2.0,
                VAlign::Bottom => (rect.y1 - f64::from(hash.height()) - s).max(rect.y0),
            };
            let x0 = rect.x1 - pad - each * count as f64;
            self.ctx.push_clip_layer(&rect.to_path(0.1));
            for i in 0..count {
                self.paint(&hash, x0 + each * i as f64, y);
            }
            self.ctx.pop_layer();
            return;
        }
        let x = if laid.wrap {
            // Wrapped lines are aligned by the layout across this width.
            match laid.align {
                HAlign::Right => rect.x0 + pad,
                _ => rect.x0 + pad + indent,
            }
        } else {
            match laid.align {
                HAlign::Left => rect.x0 + pad + indent,
                HAlign::Right => rect.x1 - pad - indent - width,
                HAlign::Center => (rect.x0 + rect.x1 - width) / 2.0,
            }
        };
        let fits = x >= room.x0
            && x + width <= room.x1 - pad / 2.0
            && y >= rect.y0 - 0.5
            && y + height <= rect.y1 + 0.5;
        if fits {
            self.paint(paragraph, x, y);
        } else {
            let clip = Rect::new(room.x0, rect.y0, room.x1 - s, rect.y1);
            self.ctx.push_clip_layer(&clip.to_path(0.1));
            let x = if laid.align == HAlign::Left || laid.wrap {
                x
            } else {
                x.max(room.x0 + pad).min(x)
            };
            self.paint(paragraph, x, y);
            self.ctx.pop_layer();
        }
    }

    fn draw_centered(&mut self, paragraph: &Paragraph, rect: Rect) {
        let x = (rect.x0 + rect.x1 - f64::from(paragraph.width())) / 2.0;
        let y = (rect.y0 + rect.y1 - f64::from(paragraph.height())) / 2.0;
        self.paint(paragraph, x, y);
    }

    fn paint(&mut self, paragraph: &Paragraph, x: f64, y: f64) {
        self.ctx.set_transform(Affine::translate((x, y)));
        draw_text(
            &mut self.ctx,
            &mut self.resources,
            paragraph,
            0.0,
            paragraph.height(),
            self.scale,
            &mut self.glyphs,
        );
        self.ctx.set_transform(Affine::IDENTITY);
    }

    /// Render and encode as PNG, for tests and `--dump`.
    pub fn encode_png(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width.max(1), self.height.max(1));
        self.drawn = None;
        let data = self.render().data().to_vec();
        Pixmap::from_parts(data, width, height)
            .into_png()
            .map_err(|err| err.to_string())
    }
}

/// Ctrl+arrow: to the edge of the block of filled cells, or to the next
/// filled cell, or to the sheet's edge.
fn jump(sheet: &Sheet, at: CellRef, direction: Direction) -> CellRef {
    let filled = |cell: CellRef| sheet.is_filled(cell);
    let step = |cell: CellRef| -> Option<CellRef> {
        match direction {
            Direction::Up => cell.row.checked_sub(1).map(|r| CellRef::new(r, cell.col)),
            Direction::Down => {
                (cell.row + 1 < MAX_ROWS).then(|| CellRef::new(cell.row + 1, cell.col))
            }
            Direction::Left => cell.col.checked_sub(1).map(|c| CellRef::new(cell.row, c)),
            Direction::Right => {
                (cell.col + 1 < MAX_COLUMNS).then(|| CellRef::new(cell.row, cell.col + 1))
            }
        }
    };
    let edge = match direction {
        Direction::Up => CellRef::new(0, at.col),
        Direction::Down => CellRef::new(MAX_ROWS - 1, at.col),
        Direction::Left => CellRef::new(at.row, 0),
        Direction::Right => CellRef::new(at.row, MAX_COLUMNS - 1),
    };
    let Some(next) = step(at) else {
        return at;
    };
    if filled(at) && filled(next) {
        // Inside a block: to its last filled cell.
        let mut cell = next;
        while let Some(after) = step(cell) {
            if !filled(after) {
                break;
            }
            cell = after;
        }
        return cell;
    }
    // To the next filled cell, looking only where there are cells.
    let found = match direction {
        Direction::Down => sheet
            .rows()
            .iter()
            .filter(|row| row.index > at.row)
            .find(|row| sheet.is_filled(CellRef::new(row.index, at.col)))
            .map(|row| CellRef::new(row.index, at.col)),
        Direction::Up => sheet
            .rows()
            .iter()
            .rev()
            .filter(|row| row.index < at.row)
            .find(|row| sheet.is_filled(CellRef::new(row.index, at.col)))
            .map(|row| CellRef::new(row.index, at.col)),
        Direction::Right => sheet
            .next_in_row(at)
            .map(|cell| CellRef::new(at.row, cell.col)),
        Direction::Left => sheet
            .previous_in_row(at)
            .map(|cell| CellRef::new(at.row, cell.col)),
    };
    found.unwrap_or(edge)
}

/// The font a cell asks for, then the system's.
fn family(name: Option<&str>) -> Family {
    match name {
        Some(name) if !name.trim().is_empty() => {
            let name: String = name
                .chars()
                .filter(|c| !matches!(c, '"' | ',' | ';'))
                .collect();
            Family::List(format!("\"{}\", system-ui", name.trim()).into())
        }
        _ => Family::SystemUi,
    }
}

fn ui_text(color: Rgba8, size: f32) -> TextStyle {
    TextStyle {
        family: Family::SystemUi,
        size,
        color,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        rise: 0.0,
    }
}

/// Relative luminance, 0 to 1.
fn luminance(color: [u8; 4]) -> f64 {
    let channel = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2])
}

/// WCAG contrast ratio of two colours.
fn contrast(a: [u8; 4], b: [u8; 4]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The colour a cell's text is drawn in. Automatic text is the window's
/// text colour, or black or white on a fill. Text on no fill that would
/// vanish into the window (black text of a light-made sheet in the dark
/// theme) takes the window's text colour instead.
fn readable(font: Option<Rgba>, fill: Option<Rgba>, style: &SheetStyle) -> Rgba8 {
    match (font, fill) {
        (Some(font), Some(_)) => font,
        (None, Some(fill)) => {
            if luminance(fill) > 0.4 {
                [0x1f, 0x1f, 0x1f, 0xff]
            } else {
                [0xff, 0xff, 0xff, 0xff]
            }
        }
        (Some(font), None) if contrast(font, style.background) >= 1.6 => font,
        _ => style.text,
    }
}

/// A border's colour, kept visible against the window the same way.
fn readable_line(line: Option<Rgba>, style: &SheetStyle) -> Rgba8 {
    match line {
        Some(line) if contrast(line, style.background) >= 1.3 => line,
        _ => style.text,
    }
}

fn lay_out(
    shaper: &mut Shaper,
    text: &str,
    base: TextStyle,
    scale: f32,
    align: Align,
    width: Option<f32>,
) -> Paragraph {
    let language = aster_text::segment::guess_language(text, DEFAULT_LANGUAGE);
    shaper.lay_out(&Spec {
        text,
        base,
        spans: &[],
        language,
        width,
        scale,
        line_height: 1.25,
        align,
        rules: Rules::for_language(language),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_sheets::{Borders, CellStyle, Font, SheetBuilder, SheetKind, Value};

    fn workbook() -> Arc<Workbook> {
        let mut first = SheetBuilder::new("数据", SheetKind::Worksheet);
        for row in 0..100 {
            for col in 0..5 {
                first.push(
                    row,
                    Cell::new(col, Value::Number(f64::from(row * 10 + col))),
                );
            }
        }
        first.push(
            0,
            Cell::new(6, Value::Text("東京都港区の長い住所のテキスト".into())),
        );
        first.push(200, Cell::new(2, Value::Number(1.0)));
        first.merge(CellRange::parse("H2:J4").unwrap());
        first.push(1, Cell::new(7, Value::Text("合并".into())));
        first.columns.set(1, 2, 0.0);
        let hidden = SheetBuilder::new("隠し", SheetKind::Worksheet)
            .hidden(true)
            .finish();
        let empty = Sheet::empty("空", SheetKind::Worksheet);
        Arc::new(Workbook {
            sheets: vec![first.finish(), hidden, empty],
            styles: vec![CellStyle::default()],
            date1904: false,
            active_sheet: 1,
            names: Vec::new(),
        })
    }

    fn view() -> SheetView {
        let mut view = SheetView::new(workbook());
        view.resize(800, 600, 2.0);
        view
    }

    #[test]
    fn opens_on_a_visible_sheet_and_lists_only_those() {
        let view = view();
        assert_eq!(view.visible_sheets(), [0, 2]);
        assert_eq!(view.sheet_index(), 0);
    }

    #[test]
    fn renders_a_sheet_and_an_empty_one() {
        let mut view = view();
        let pixels = view.render().data_as_u8_slice().to_vec();
        // Not a blank page: grid, headers and text were drawn.
        assert!(pixels.chunks(4).any(|p| p != [0xff, 0xff, 0xff, 0xff]));
        assert!(!view.needs_render());
        assert!(view.set_sheet(2));
        assert!(view.needs_render());
        view.render();
        // Tiny and zero-sized windows do not panic.
        view.resize(1, 1, 1.0);
        view.render();
        view.resize(0, 0, 1.0);
        view.render();
    }

    #[test]
    fn keys_move_the_selection_past_hidden_columns_and_merges() {
        let mut view = view();
        assert_eq!(view.selected(), CellRef::default());
        assert!(view.key(SheetKey::Right));
        // Columns B and... B is hidden: straight to C.
        assert_eq!(view.selected().to_string(), "C1");
        view.key(SheetKey::Left);
        assert_eq!(view.selected().to_string(), "A1");
        assert!(!view.key(SheetKey::Left));
        assert!(!view.key(SheetKey::Up));
        view.select(CellRef::parse("I3").unwrap());
        assert_eq!(view.selected().to_string(), "H2");
        assert_eq!(
            view.selection_text(),
            ("H2:J4".to_owned(), "合并".to_owned())
        );
        view.key(SheetKey::Right);
        assert_eq!(view.selected().to_string(), "K2");
        view.select(CellRef::parse("H3").unwrap());
        view.key(SheetKey::Down);
        assert_eq!(view.selected().to_string(), "H5");
        view.key(SheetKey::Start);
        view.key(SheetKey::Jump(Direction::Down));
        assert_eq!(view.selected().to_string(), "A100");
        view.key(SheetKey::Jump(Direction::Down));
        assert_eq!(view.selected().to_string(), "A1048576");
        view.key(SheetKey::Last);
        assert_eq!(view.selected().to_string(), "J201");
    }

    #[test]
    fn selecting_far_away_scrolls_there_and_back() {
        let mut view = view();
        view.select(CellRef::new(5000, 300));
        let (offset, max) = view.scroll_range();
        assert!(offset > 0.0 && offset <= max);
        let (x, _) = view.scroll_range_x();
        assert!(x > 0.0);
        // The cell under the middle of the window is near the selection.
        let under = view.cell_at(700.0, 500.0).unwrap();
        assert!(under.row.abs_diff(5000) < 30 && under.col.abs_diff(300) < 10);
        view.render();
        view.key(SheetKey::Start);
        assert_eq!(view.scroll_range().0, 0.0);
        // Each sheet keeps its own place.
        view.select(CellRef::new(50, 3));
        view.set_sheet(2);
        assert_eq!(view.selected(), CellRef::default());
        view.set_sheet(0);
        assert_eq!(view.selected(), CellRef::new(50, 3));
    }

    #[test]
    fn clicks_pick_cells_and_headers() {
        let mut view = view();
        // At scale 2 the headers are 44 pixels tall.
        assert_eq!(view.cell_at(10.0, 10.0), None);
        let header = view.header_width() * 2.0;
        assert!(
            view.press(header + 5.0, 44.0 + 5.0, false) || view.selected() == CellRef::default()
        );
        assert_eq!(view.selected(), CellRef::default());
        view.press(header + 64.0 * 2.0 + 5.0, 44.0 + 41.0, false);
        // Column B is hidden, so the second column on screen is C.
        assert_eq!(view.selected().to_string(), "C2");
        // A column letter selects that column in the top row on screen.
        view.press(header + 5.0, 20.0, false);
        assert_eq!(view.selected().to_string(), "A1");
    }

    fn pixel(view: &mut SheetView, x: f64, y: f64) -> [u8; 4] {
        let width = usize::from(view.size().0);
        let pixmap = view.render();
        let data = pixmap.data_as_u8_slice();
        let at = (y as usize * width + x as usize) * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    #[test]
    fn a_cell_with_a_note_gets_a_red_corner() {
        let mut book = Arc::unwrap_or_clone(workbook());
        book.sheets[0].comments.insert(
            CellRef::new(2, 3),
            aster_sheets::Comment::note("我", "看这里"),
        );
        let mut view = SheetView::new(Arc::new(book));
        view.set_sheet(0);
        view.resize(600, 400, 1.0);
        let (_, y0, x1, _) = view.cell_box(CellRef::new(2, 3)).unwrap();
        let near = pixel(&mut view, x1 - 3.0, y0 + 1.0);
        assert_eq!(near, [0xd9, 0x2d, 0x20, 0xff], "the corner is red");
        let far = pixel(&mut view, x1 - 3.0, y0 + 12.0);
        assert_ne!(far, [0xd9, 0x2d, 0x20, 0xff]);
    }

    #[test]
    fn the_print_area_is_outlined_with_dashes() {
        let mut book = Arc::unwrap_or_clone(workbook());
        book.sheets[0].print_area = vec![CellRange::parse("B2:C3").unwrap()];
        let mut view = SheetView::new(Arc::new(book));
        view.set_sheet(0);
        view.resize(600, 400, 1.0);
        let style = view.style;
        let (x0, y0, x1, y1) = {
            let (a, b, _, _) = view.cell_box(CellRef::new(1, 1)).unwrap();
            let (_, _, c, d) = view.cell_box(CellRef::new(2, 2)).unwrap();
            (a, b, c, d)
        };
        // Along the top edge some pixels are the outline's colour and some
        // are not: dashes, not a solid line, and not nothing.
        let top: Vec<[u8; 4]> = (x0 as u32 + 1..x1 as u32 - 1)
            .map(|x| pixel(&mut view, f64::from(x), y0 - 1.0))
            .collect();
        let dark = top.iter().filter(|p| **p == style.header_text).count();
        assert!(
            dark > 4 && dark < top.len() - 4,
            "{dark} of {} pixels",
            top.len()
        );
        // The same on the left edge, and none far from the area.
        let left = (y0 as u32 + 1..y1 as u32 - 1)
            .filter(|y| pixel(&mut view, x0 - 1.0, f64::from(*y)) == style.header_text)
            .count();
        assert!(left > 2);
        let away = pixel(&mut view, x1 + 40.0, y0 - 1.0);
        assert_ne!(away, style.header_text);
        // A sheet without one has no outline.
        let mut book = Arc::unwrap_or_clone(workbook());
        book.sheets[0].print_area.clear();
        let mut plain = SheetView::new(Arc::new(book));
        plain.set_sheet(0);
        plain.resize(600, 400, 1.0);
        let dark = (x0 as u32 + 1..x1 as u32 - 1)
            .filter(|x| pixel(&mut plain, f64::from(*x), y0 - 1.0) == style.header_text)
            .count();
        assert_eq!(dark, 0);
    }

    #[test]
    fn a_print_area_the_height_of_the_sheet_draws_fast_and_stays_on_screen() {
        let mut book = Arc::unwrap_or_clone(workbook());
        // Whole columns B to C: a million rows tall.
        book.sheets[0].print_area = vec![CellRange::new(
            CellRef::new(0, 1),
            CellRef::new(aster_sheets::MAX_ROWS - 1, 2),
        )];
        let mut view = SheetView::new(Arc::new(book));
        view.set_sheet(0);
        view.resize(600, 400, 1.0);
        let started = std::time::Instant::now();
        view.render();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Styled cells, a frozen row and column, and a picture.
    fn styled() -> SheetView {
        let red = [0xC0, 0x10, 0x10, 0xff];
        let thick = Line {
            style: LineStyle::Thick,
            color: Some([0x10, 0x20, 0xC0, 0xff]),
        };
        let styles = vec![
            CellStyle::default(),
            CellStyle {
                fill: Some(red),
                font: Arc::new(Font {
                    bold: true,
                    size: 14.0,
                    ..Font::default()
                }),
                ..CellStyle::default()
            },
            CellStyle {
                borders: Arc::new(Borders {
                    bottom: Some(thick),
                    right: Some(Line {
                        style: LineStyle::Dashed,
                        color: None,
                    }),
                    ..Borders::default()
                }),
                wrap: true,
                valign: VAlign::Top,
                ..CellStyle::default()
            },
        ];
        let mut sheet = SheetBuilder::new("样式", SheetKind::Worksheet);
        let mut title = Cell::new(0, Value::Text("标题".into()));
        title.style = 1;
        sheet.push(0, title);
        let mut boxed = Cell::new(1, Value::Text("折り返し のある 長い テキスト".into()));
        boxed.style = 2;
        sheet.push(1, boxed);
        for row in 0..200 {
            sheet.push(row, Cell::new(3, Value::Number(f64::from(row))));
        }
        sheet.frozen = (1, 1);
        sheet.show_grid = false;
        sheet.column_styles = vec![(5, 5, 1)];
        // A blue 4 × 4 picture over C10:D12.
        let mut blue = Pixmap::new(4, 4);
        for pixel in blue.data_mut() {
            *pixel = vello_cpu::peniko::color::PremulRgba8 {
                r: 0,
                g: 0,
                b: 0xff,
                a: 0xff,
            };
        }
        let png = blue.into_png().unwrap();
        sheet.pictures.push(Picture {
            placement: Placement::TwoCell {
                from: Anchor {
                    cell: CellRef::new(9, 2),
                    dx: 0.0,
                    dy: 0.0,
                },
                to: Anchor {
                    cell: CellRef::new(12, 4),
                    dx: 0.0,
                    dy: 0.0,
                },
            },
            image: Some(Arc::from(png)),
            name: "图片 1".into(),
        });
        sheet.pictures.push(Picture {
            placement: Placement::OneCell {
                from: Anchor {
                    cell: CellRef::new(14, 2),
                    dx: 0.0,
                    dy: 0.0,
                },
                width: 120.0,
                height: 60.0,
            },
            image: None,
            name: "グラフ".into(),
        });
        let mut view = SheetView::new(Arc::new(Workbook {
            sheets: vec![sheet.finish()],
            styles,
            date1904: false,
            active_sheet: 0,
            names: Vec::new(),
        }));
        view.resize(800, 600, 1.0);
        view
    }

    #[test]
    fn fills_borders_and_pictures_are_drawn() {
        let mut view = styled();
        let header = view.header_width();
        // A1 is red (away from its text), with no grid lines anywhere.
        let red = pixel(&mut view, header + 60.0, 22.0 + 3.0);
        assert_eq!(red, [0xC0, 0x10, 0x10, 0xff]);
        // The whole of column F takes the column's red fill.
        let f = header + 64.0 * 5.0 + 30.0;
        assert_eq!(
            pixel(&mut view, f, 22.0 + 20.0 * 20.0 + 10.0),
            [0xC0, 0x10, 0x10, 0xff]
        );
        // B2's thick blue bottom border sits on the line between rows 2
        // and 3, three pixels thick.
        let x = header + 64.0 + 30.0;
        let line = 22.0 + 40.0 - 1.0;
        assert_eq!(pixel(&mut view, x, line), [0x10, 0x20, 0xC0, 0xff]);
        assert_eq!(pixel(&mut view, x, line - 1.0), [0x10, 0x20, 0xC0, 0xff]);
        assert_ne!(pixel(&mut view, x, line - 3.0), [0x10, 0x20, 0xC0, 0xff]);
        // No grid: an empty cell's edge is background.
        assert_eq!(
            pixel(
                &mut view,
                header + 64.0 * 7.0 - 1.0,
                22.0 + 20.0 * 20.0 + 5.0
            ),
            [0xff; 4]
        );
        // The picture fills C10:E13 in blue.
        let inside = (header + 64.0 * 3.0, 22.0 + 20.0 * 11.0);
        assert_eq!(pixel(&mut view, inside.0, inside.1), [0, 0, 0xff, 0xff]);
        // A chart is a grey frame.
        let chart = pixel(
            &mut view,
            header + 64.0 * 2.0 + 5.0,
            22.0 + 20.0 * 14.0 + 5.0,
        );
        assert_eq!(chart, color_bytes(SheetStyle::default().header));
    }

    fn color_bytes(rgba: Rgba8) -> [u8; 4] {
        rgba
    }

    #[test]
    fn frozen_rows_and_columns_stay_while_the_rest_scrolls() {
        let mut view = styled();
        let header = view.header_width();
        assert!(view.scroll_by(0.0, 20.0 * 50.0));
        // Row 1 is still at the top: A1's red is where it was.
        assert_eq!(
            pixel(&mut view, header + 60.0, 22.0 + 3.0),
            [0xC0, 0x10, 0x10, 0xff]
        );
        // Below it, the first scrolled row is row 52.
        assert_eq!(
            view.cell_at(header + 100.0, 22.0 + 20.0 + 5.0),
            Some(CellRef::new(51, 1))
        );
        // The frozen column answers clicks without the scroll.
        assert_eq!(
            view.cell_at(header + 10.0, 22.0 + 5.0),
            Some(CellRef::new(0, 0))
        );
        assert_eq!(
            view.cell_at(header + 10.0, 22.0 + 25.0),
            Some(CellRef::new(51, 0))
        );
        // Selecting a cell above the scrolled part brings it back into view
        // below the frozen row, not under it.
        view.select(CellRef::new(10, 3));
        let (offset, _) = view.scroll_range();
        assert_eq!(offset, 20.0 * 9.0);
        // Frozen cells never scroll the view.
        view.scroll_by(0.0, 20.0 * 40.0);
        let before = view.scroll_range();
        view.select(CellRef::new(0, 0));
        assert_eq!(view.scroll_range(), before);
    }

    #[test]
    fn ranges_from_shift_keys_drags_and_headers() {
        let mut view = view();
        view.select(CellRef::parse("C3").unwrap());
        view.key_extending(SheetKey::Down, true);
        view.key_extending(SheetKey::Right, true);
        assert_eq!(view.selection().to_string(), "C3:D4");
        assert_eq!(view.selected().to_string(), "C3");
        assert_eq!(view.selection_text().0, "C3:D4");
        // Return walks the range row by row, Tab... column by column here.
        assert!(view.step(false, false));
        assert_eq!(view.selected().to_string(), "C4");
        view.step(false, false);
        assert_eq!(view.selected().to_string(), "D3");
        view.step(false, true);
        assert_eq!(view.selected().to_string(), "C4");
        assert_eq!(view.selection().to_string(), "C3:D4");
        // A plain move collapses it.
        view.key(SheetKey::Down);
        assert_eq!(view.selection().to_string(), "C5");
        // Dragging from a cell (scale 2: rows 40 px, header 44 px).
        let header = view.header_width() * 2.0;
        view.press(header + 5.0, 44.0 + 5.0, false);
        assert!(view.drag(header + 64.0 * 2.0 * 3.0 + 5.0, 44.0 + 40.0 * 2.0 + 5.0));
        assert_eq!(view.selection().first.to_string(), "A1");
        assert_eq!(view.selection().last.row, 2);
        // A row number selects the whole row, a column letter the column.
        view.press(10.0, 44.0 + 40.0 + 5.0, false);
        assert_eq!(view.selection().to_string(), "A2:XFD2");
        view.press(header + 5.0, 10.0, false);
        assert_eq!(view.selection().to_string(), "A1:A1048576");
        view.press(5.0, 5.0, false);
        assert_eq!(view.selection().to_string(), "A1:XFD1048576");
        view.render();
    }

    #[test]
    fn typing_into_a_cell_draws_an_editor_and_takes_compositions() {
        use crate::cell_editor::EditMode;

        let mut view = view();
        view.select(CellRef::parse("B3").unwrap());
        let before = view.render().data_as_u8_slice().to_vec();
        // A composition with nothing being edited starts an entry.
        assert!(view.ime(ImeEvent::Preedit {
            text: "とうきょう".into(),
            cursor: Some((0, 0)),
        }));
        assert!(view.is_editing() && view.is_composing());
        let drawn = view.render().data_as_u8_slice().to_vec();
        assert_ne!(before, drawn);
        let (x, y, _, h) = view.ime_cursor_area();
        assert!(x > 0.0 && y > 0.0 && h > 0.0);
        view.ime(ImeEvent::Commit("東京".into()));
        assert_eq!(view.selection_text().1, "東京");
        assert_eq!(
            view.finish_edit(),
            Some((CellRef::parse("B3").unwrap(), "東京".to_owned()))
        );
        assert!(!view.is_editing());
        // F2 edits the content as it is, caret at the end.
        view.select(CellRef::parse("A1").unwrap());
        view.begin_edit(EditMode::Edit, None);
        assert_eq!(view.editor().unwrap().text(), "0");
        view.edit(|editor| editor.insert("5"));
        assert_eq!(view.selection_text().1, "05");
        // A composition left open when the entry ends is kept as shown.
        view.ime(ImeEvent::Preedit {
            text: "x".into(),
            cursor: None,
        });
        assert_eq!(view.finish_edit().unwrap().1, "05x");
        // Enabling the input method alone does not start an entry.
        assert!(!view.ime(ImeEvent::Enabled));
        assert!(!view.is_editing());
    }
}
