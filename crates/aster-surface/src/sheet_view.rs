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
    Cell, CellRange, CellRef, DisplayKind, HAlign, MAX_COLUMNS, MAX_ROWS, Sheet, Workbook,
    column_name,
};
use aster_text::{Align, Family, Paragraph, Rules, Shaper, Spec, Style as TextStyle};
use vello_cpu::kurbo::{Affine, Rect, Shape, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::preview::draw_text;
use crate::text_surface::{DEFAULT_LANGUAGE, HAN_FALLBACK_FAMILIES, Rgba8};

/// Text size in cells and headers, logical pixels.
const TEXT_SIZE: f32 = 13.0;
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
    selected: CellRef,
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
}

pub struct SheetView {
    workbook: Arc<Workbook>,
    sheet: usize,
    places: Vec<Place>,
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
}

/// A cell's text, laid out.
struct Laid {
    paragraph: Paragraph,
    align: HAlign,
    kind: DisplayKind,
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
        };
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

    /// The selected cell's address (its merged area, if any) and what the
    /// formula bar shows for it.
    pub fn selection_text(&self) -> (String, String) {
        let at = self.selected();
        let Some(sheet) = self.sheet() else {
            return (at.to_string(), String::new());
        };
        let address = match sheet.merge_at(at) {
            Some(merge) => merge.to_string(),
            None => at.to_string(),
        };
        let content = sheet
            .cell(at)
            .map(|cell| self.workbook.input_text(cell))
            .unwrap_or_default();
        (address, content)
    }

    fn place(&self) -> Place {
        self.places.get(self.sheet).copied().unwrap_or_default()
    }

    fn place_mut(&mut self) -> &mut Place {
        let index = self.sheet.min(self.places.len() - 1);
        &mut self.places[index]
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

    /// The cell under a point (physical pixels), if it is in the cells area.
    pub fn cell_at(&self, x: f64, y: f64) -> Option<CellRef> {
        let sheet = self.sheet()?;
        let (x, y) = (x / self.s(), y / self.s());
        let header_width = self.header_width();
        let place = self.place();
        let row = (y >= HEADER_HEIGHT)
            .then(|| sheet.row_sizes.index_at(y - HEADER_HEIGHT + place.scroll_y));
        let col =
            (x >= header_width).then(|| sheet.columns.index_at(x - header_width + place.scroll_x));
        match (row, col) {
            (Some(row), Some(col)) => Some(CellRef::new(row, col)),
            // A row number: the first cell of that row on screen.
            (Some(row), None) => Some(CellRef::new(row, sheet.columns.index_at(place.scroll_x))),
            // A column letter: that column's cell in the top row on screen.
            (None, Some(col)) => Some(CellRef::new(sheet.row_sizes.index_at(place.scroll_y), col)),
            (None, None) => None,
        }
    }

    /// Select the cell under a point (physical pixels). Returns whether the
    /// selection changed.
    pub fn press(&mut self, x: f64, y: f64) -> bool {
        match self.cell_at(x, y) {
            Some(at) => self.select(at),
            None => false,
        }
    }

    /// Select a cell (the top-left of its merged area) and scroll it into
    /// view.
    pub fn select(&mut self, at: CellRef) -> bool {
        let at = CellRef::new(at.row.min(MAX_ROWS - 1), at.col.min(MAX_COLUMNS - 1));
        let at = self
            .sheet()
            .and_then(|sheet| sheet.merge_at(at))
            .map_or(at, |merge| merge.first);
        let changed = self.place().selected != at;
        self.place_mut().selected = at;
        self.ensure_visible();
        changed
    }

    fn ensure_visible(&mut self) {
        let Some(sheet) = self.sheet() else {
            return;
        };
        let at = self.place().selected;
        let range = sheet
            .merge_at(at)
            .copied()
            .unwrap_or_else(|| CellRange::single(at));
        let left = sheet.columns.offset(range.first.col);
        let right = sheet.columns.offset(range.last.col + 1);
        let top = sheet.row_sizes.offset(range.first.row);
        let bottom = sheet.row_sizes.offset(range.last.row + 1);
        let (area_w, area_h) = self.area();
        let place = self.place_mut();
        if right > place.scroll_x + area_w {
            place.scroll_x = right - area_w;
        }
        if left < place.scroll_x {
            place.scroll_x = left;
        }
        if bottom > place.scroll_y + area_h {
            place.scroll_y = bottom - area_h;
        }
        if top < place.scroll_y {
            place.scroll_y = top;
        }
        self.clamp();
    }

    /// Move the selection. Returns whether it moved.
    pub fn key(&mut self, key: SheetKey) -> bool {
        let Some(sheet) = self.sheet() else {
            return false;
        };
        let at = self.place().selected;
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
                    .and_then(|row| row.cells.last())
                    .map_or(at.col, |cell| cell.col),
            ),
            SheetKey::Start => CellRef::default(),
            SheetKey::Last => used.map_or(CellRef::default(), |used| used.last),
            SheetKey::Jump(direction) => jump(sheet, at, direction),
        };
        self.select(target)
    }

    /// Text of the cell for a test or a screen reader.
    pub fn text(&self) -> String {
        self.selection_text().1
    }

    fn laid(&mut self, at: CellRef, cell: &Cell) -> Option<Arc<Laid>> {
        let key = (self.sheet, at);
        if let Some(found) = self.cells.get(&key) {
            return found.clone();
        }
        if self.cells.len() >= CACHE_LIMIT {
            self.cells.clear();
        }
        let laid = self.workbook.display(cell).map(|display| {
            // Without "wrap text" (not read yet) a cell is one line.
            let text = display.text.replace(['\r', '\n'], " ");
            let color = match display.kind {
                DisplayKind::Uncomputed => self.style.secondary,
                _ => self.style.text,
            };
            Arc::new(Laid {
                paragraph: lay_out(&mut self.shaper, &text, color, self.scale, Align::Start),
                align: display.align,
                kind: display.kind,
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
            self.style.header_text,
            self.scale,
            Align::Start,
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
            self.style.text,
            self.scale,
            Align::Start,
        ));
        self.hashes = Some(hashes.clone());
        hashes
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
        }
    }

    /// Draw what is in the window and return the pixels, premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        if !self.needs_render() {
            return &self.pixmap;
        }
        self.drawn = Some(self.state());
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
        let first_row = sheet.row_sizes.index_at(place.scroll_y);
        let last_row = sheet.row_sizes.index_at(place.scroll_y + area_h);
        let first_col = sheet.columns.index_at(place.scroll_x);
        let last_col = sheet.columns.index_at(place.scroll_x + area_w);
        let view = CellRange::new(
            CellRef::new(first_row, first_col),
            CellRef::new(last_row, last_col),
        );
        // Logical sheet position to physical window position.
        let x_of = |col: u32| (header_w + sheet.columns.offset(col) - place.scroll_x) * s;
        let y_of = |row: u32| (HEADER_HEIGHT + sheet.row_sizes.offset(row) - place.scroll_y) * s;
        let line = (s.round()).max(1.0);
        let (left, top) = (header_w * s, HEADER_HEIGHT * s);

        // Grid lines.
        self.ctx.set_paint(color(style.grid));
        for col in first_col..=last_col + 1 {
            if col > first_col && sheet.columns.size(col - 1) == 0.0 {
                continue;
            }
            let x = x_of(col).round() - line;
            if x >= left - line {
                self.ctx.fill_rect(&Rect::new(x, top, x + line, h));
            }
        }
        for row in first_row..=last_row + 1 {
            if row > first_row && sheet.row_sizes.size(row - 1) == 0.0 {
                continue;
            }
            let y = y_of(row).round() - line;
            if y >= top - line {
                self.ctx.fill_rect(&Rect::new(left, y, w, y + line));
            }
        }

        // Merged areas on screen: cover the lines inside them.
        let merges: Vec<CellRange> = sheet
            .merges
            .iter()
            .filter(|merge| merge.intersects(&view))
            .copied()
            .collect();
        let clip_cells = Rect::new(left, top, w, h);
        self.ctx.push_clip_layer(&clip_cells.to_path(0.1));
        self.ctx.set_paint(color(style.background));
        for merge in &merges {
            let x0 = x_of(merge.first.col).round();
            let y0 = y_of(merge.first.row).round();
            let x1 = x_of(merge.last.col + 1).round() - line;
            let y1 = y_of(merge.last.row + 1).round() - line;
            self.ctx.fill_rect(&Rect::new(x0, y0, x1, y1));
        }

        // Cell texts: merged areas show their top-left cell.
        let mut texts: Vec<(CellRef, Rect)> = Vec::new();
        for (at, _) in sheet.cells_in(view) {
            if merges.iter().any(|merge| merge.contains(at)) {
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
            if sheet.cell(merge.first).is_some() {
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
            let Some(laid) = self.laid(at, cell) else {
                continue;
            };
            let merged = merges.iter().any(|merge| merge.first == at);
            // Left-aligned text runs on over empty cells to its right, as in
            // Excel.
            let mut room = rect;
            if !merged && laid.kind == DisplayKind::Text && laid.align == HAlign::Left {
                let stop = sheet
                    .next_in_row(at)
                    .map_or(last_col + 1, |next| next.col.min(last_col + 1));
                room.x1 = x_of(stop).max(rect.x1);
            }
            self.draw_cell(&laid, rect, room, s);
        }
        self.ctx.pop_layer();

        // The selected cell.
        let selected = place.selected;
        let span = sheet
            .merge_at(selected)
            .copied()
            .unwrap_or_else(|| CellRange::single(selected));
        let border = (2.0 * s).round();
        let outline = Rect::new(
            x_of(span.first.col).round() - line,
            y_of(span.first.row).round() - line,
            x_of(span.last.col + 1).round(),
            y_of(span.last.row + 1).round(),
        );
        self.ctx.push_clip_layer(&clip_cells.to_path(0.1));
        self.ctx.set_paint(color(style.accent));
        self.ctx.set_stroke(Stroke::new(border));
        self.ctx
            .stroke_rect(&outline.inset(-border / 2.0 + line / 2.0));
        self.ctx.pop_layer();

        // Headers over everything else.
        self.ctx.set_paint(color(style.header));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, w, top));
        self.ctx.fill_rect(&Rect::new(0.0, 0.0, left, h));
        self.ctx
            .push_clip_layer(&Rect::new(left, 0.0, w, top).to_path(0.1));
        for col in first_col..=last_col {
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
        self.ctx
            .push_clip_layer(&Rect::new(0.0, top, left, h).to_path(0.1));
        for row in first_row..=last_row {
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
        self.ctx.set_paint(color(style.grid));
        self.ctx.fill_rect(&Rect::new(left - line, 0.0, left, h));
        self.ctx.fill_rect(&Rect::new(0.0, top - line, w, top));
        self.finish()
    }

    fn finish(&mut self) -> &Pixmap {
        self.ctx.flush();
        self.ctx.render(&mut self.pixmap, &mut self.resources);
        &self.pixmap
    }

    /// One cell's text in `rect`, allowed to spread over `room`.
    fn draw_cell(&mut self, laid: &Laid, rect: Rect, room: Rect, s: f64) {
        let pad = PAD * s;
        let paragraph = &laid.paragraph;
        let width = f64::from(paragraph.width());
        let height = f64::from(paragraph.height());
        // Numbers that do not fit show as ### in Excel.
        if laid.kind == DisplayKind::Number && width > rect.width() - 2.0 * pad {
            let hash = self.hashes();
            let each = f64::from(hash.width()).max(1.0);
            let count = ((rect.width() - 2.0 * pad) / each).floor().max(1.0) as usize;
            let y = (rect.y1 - height - s).max(rect.y0);
            let x0 = rect.x1 - pad - each * count as f64;
            self.ctx.push_clip_layer(&rect.to_path(0.1));
            for i in 0..count {
                self.paint(&hash, x0 + each * i as f64, y);
            }
            self.ctx.pop_layer();
            return;
        }
        let x = match laid.align {
            HAlign::Left => rect.x0 + pad,
            HAlign::Right => rect.x1 - pad - width,
            HAlign::Center => (rect.x0 + rect.x1 - width) / 2.0,
        };
        // Excel puts text at the bottom of a cell.
        let y = (rect.y1 - height - s).max(rect.y0);
        let fits = x >= room.x0 && x + width <= room.x1 - pad / 2.0 && height <= rect.height();
        if fits {
            self.paint(paragraph, x, y);
        } else {
            let clip = Rect::new(room.x0, rect.y0, room.x1 - s, rect.y1);
            self.ctx.push_clip_layer(&clip.to_path(0.1));
            self.paint(paragraph, x.max(room.x0 + pad).min(x), y);
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
    let filled = |cell: CellRef| sheet.cell(cell).is_some();
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
            .find(|row| row.cells.binary_search_by_key(&at.col, |c| c.col).is_ok())
            .map(|row| CellRef::new(row.index, at.col)),
        Direction::Up => sheet
            .rows()
            .iter()
            .rev()
            .filter(|row| row.index < at.row)
            .find(|row| row.cells.binary_search_by_key(&at.col, |c| c.col).is_ok())
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

fn lay_out(shaper: &mut Shaper, text: &str, color: Rgba8, scale: f32, align: Align) -> Paragraph {
    let language = aster_text::segment::guess_language(text, DEFAULT_LANGUAGE);
    let base = TextStyle {
        family: Family::SystemUi,
        size: TEXT_SIZE,
        color,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        rise: 0.0,
    };
    shaper.lay_out(&Spec {
        text,
        base,
        spans: &[],
        language,
        width: None,
        scale,
        line_height: 1.25,
        align,
        rules: Rules::for_language(language),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_sheets::{CellStyle, SheetBuilder, SheetKind, Value};

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
        assert!(view.press(header + 5.0, 44.0 + 5.0) || view.selected() == CellRef::default());
        assert_eq!(view.selected(), CellRef::default());
        view.press(header + 64.0 * 2.0 + 5.0, 44.0 + 41.0);
        // Column B is hidden, so the second column on screen is C.
        assert_eq!(view.selected().to_string(), "C2");
        // A column letter selects that column in the top row on screen.
        view.press(header + 5.0, 20.0);
        assert_eq!(view.selected().to_string(), "A1");
    }
}
