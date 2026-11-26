//! Tables, links and pictures: the commands that make and change them, what
//! the menus need to know about them, and the picture as an object that is
//! selected with a click and resized by its corners.
//!
//! **Tables.** Every command is one step of undo and leaves the caret where
//! the operation in `aster_writer::edit` says. A table made here is as wide
//! as the space it sits in ([`edit::available_width`]); a column added to
//! one that then does not fit scales the table's columns down in proportion
//! ([`edit::fit_table`]).
//!
//! **Links.** A link is set on the selection, or, with only a caret, on the
//! whole stretch of text that already has a link around the caret. The
//! dialog's text and address are read by [`DocView::link_info`] and written
//! back by [`DocCommand::EditLink`].
//!
//! **Pictures.** A click on a picture selects it: the selection is its one
//! object character, so Delete and Backspace remove it and typing replaces
//! it. A picture in the line (not floating) shows a handle at each corner;
//! dragging one resizes it keeping its proportions: the new size is the
//! distance from the pointer to the opposite corner as it was when the
//! button went down. A picture in the line stays where the line puts it, so
//! its top left corner stays and the others move with the size. The
//! document changes while the pointer moves and the whole drag is one step
//! of undo, recorded when the button goes up. Floating pictures can be selected and deleted,
//! but have no handles and cannot be moved.

use std::sync::Arc;

use aster_layout::{Item, Rect as PageRect};
use aster_writer::edit::{self, PictureError, Selection};
use aster_writer::{Document, DrawingContent, Inline, Link, OBJECT, Path, Pos, paragraphs_of};

use super::DocView;
use super::text::{sanitized, text_between};

/// EMU in a point, and in a twip.
const EMU_PT: f64 = 12_700.0;
const EMU_TWIP: i64 = 635;
/// EMU in a pixel at 96 to the inch.
const EMU_PIXEL: i64 = 9_525;
/// The smallest a picture is made by dragging, points on a side.
const MIN_SIDE: f64 = 8.0;
/// Logical pixels: a handle's side, and how far from its centre it is hit.
const HANDLE: f64 = 7.0;
const HANDLE_HIT: f64 = 6.0;

/// A corner of a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ];

    fn of(self, rect: &PageRect) -> (f32, f32) {
        match self {
            Corner::TopLeft => (rect.x0, rect.y0),
            Corner::TopRight => (rect.x1, rect.y0),
            Corner::BottomRight => (rect.x1, rect.y1),
            Corner::BottomLeft => (rect.x0, rect.y1),
        }
    }

    fn opposite(self) -> Corner {
        match self {
            Corner::TopLeft => Corner::BottomRight,
            Corner::TopRight => Corner::BottomLeft,
            Corner::BottomRight => Corner::TopLeft,
            Corner::BottomLeft => Corner::TopRight,
        }
    }
}

/// The picture that is selected as an object.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectInfo {
    /// Just before its object character.
    pub pos: Pos,
    /// In the line, not floating: only these can be resized.
    pub inline: bool,
    /// Width and height, EMU.
    pub size: (i64, i64),
    /// Where it is drawn: the page and the rectangle in its points.
    pub page: usize,
    pub rect: PageRect,
}

/// What the link dialog starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkInfo {
    /// The selected text, or the text of the link the caret is in; empty
    /// when there is none (or it runs over several paragraphs, or is a
    /// picture).
    pub text: String,
    /// The address, when the selection (or the caret) is in one link.
    pub target: Option<String>,
    pub tooltip: Option<String>,
}

/// A resize in progress.
pub(super) struct Resize {
    pos: Pos,
    /// The document and selection as they were when the pointer went down.
    before: Document,
    selection: Selection,
    page: usize,
    /// The corner that is held, and where the other one is, in page points.
    corner: Corner,
    fixed: (f32, f32),
    start: (i64, i64),
    size: (i64, i64),
}

impl DocView {
    // --- Tables ---

    /// The caret is in a table cell.
    pub fn in_table(&self) -> bool {
        self.caret()
            .is_some_and(|caret| edit::cell_at(&self.document, caret).is_some())
    }

    /// A change to the table the selection is in; the caret goes where
    /// `change` says. Nothing happens outside tables.
    fn table_edit(&mut self, change: impl FnOnce(&mut Document, &Pos, &Pos) -> Pos) -> bool {
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            edit::cell_at(document, &from)?;
            Some(Selection::caret(change(document, &from, &to)))
        })
    }

    /// A table of `rows` by `columns` where the caret is (at the start of
    /// the selection): see [`edit::insert_table`] for where it goes. The
    /// caret goes into its first cell. Returns whether a table was made
    /// (not for no rows or columns, or more than the format allows).
    pub fn insert_table(&mut self, rows: usize, columns: usize) -> bool {
        self.apply(None, false, |document, selection| {
            let (at, _) = selection.ordered();
            let width = edit::available_width(document, &at);
            let caret = edit::insert_table(document, &at, rows, columns, width);
            (caret != at).then(|| Selection::caret(caret))
        })
    }

    /// Rows above or below the selection (one for a caret, as many as the
    /// selection covers otherwise).
    pub fn insert_rows(&mut self, above: bool) -> bool {
        self.table_edit(|document, from, to| edit::insert_rows(document, from, to, above))
    }

    /// Columns to the left or right of the selection. The table is scaled
    /// to fit the space it sits in when it has become wider.
    pub fn insert_columns(&mut self, left: bool) -> bool {
        self.table_edit(|document, from, to| {
            let caret = edit::insert_columns(document, from, to, left);
            if let Some(cell) = edit::cell_at(document, &caret) {
                edit::fit_table(document, &cell.table);
            }
            caret
        })
    }

    /// Take out the rows the selection covers (the whole table when it
    /// covers them all).
    pub fn delete_rows(&mut self) -> bool {
        self.table_edit(edit::delete_rows)
    }

    pub fn delete_columns(&mut self) -> bool {
        self.table_edit(edit::delete_columns)
    }

    pub fn delete_table(&mut self) -> bool {
        self.table_edit(|document, from, _| edit::delete_table(document, from))
    }

    /// Tab in the last cell of a table: a row is added below and the caret
    /// goes into its first cell. False when the caret is not in a table.
    pub(super) fn add_row_after_last_cell(&mut self) -> bool {
        let Some(focus) = self.caret().cloned() else {
            return false;
        };
        let Some(table) = edit::cell_at(&self.document, &focus).map(|cell| cell.table) else {
            return false;
        };
        let Some(last) = self.last_cell_start(&table) else {
            return false;
        };
        self.apply(None, false, |document, _| {
            Some(Selection::caret(edit::insert_rows(
                document, &last, &last, false,
            )))
        })
    }

    /// The start of the first paragraph of the last cell of the last row.
    fn last_cell_start(&self, table: &Path) -> Option<Pos> {
        let t = self.document.block(table)?.as_table()?;
        let row = t.rows.len().checked_sub(1)?;
        let cell = t.rows[row].cells.len().checked_sub(1)?;
        let (inner, _) = paragraphs_of(&t.rows[row].cells[cell].blocks)
            .into_iter()
            .next()?;
        let mut steps = table.steps.clone();
        steps.extend([row as u32, cell as u32]);
        steps.extend(inner.steps);
        Some(Pos::new(table.with_steps(steps), 0))
    }

    // --- Links ---

    /// The text the link commands act on: the selection, or the stretch
    /// with a link around the caret.
    fn link_scope(&self) -> Option<(Pos, Pos)> {
        let sel = self.sel.as_ref()?;
        if sel.anchor != sel.focus {
            let (a, b) = if sel.anchor <= sel.focus {
                (sel.anchor.clone(), sel.focus.clone())
            } else {
                (sel.focus.clone(), sel.anchor.clone())
            };
            return Some((a, b));
        }
        let (from, to, _) = edit::link_range(&self.document, &sel.focus)?;
        Some((from, to))
    }

    /// The link all the text of `from..to` has, if it is one link.
    fn link_over(&self, from: &Pos, to: &Pos) -> Option<Arc<Link>> {
        let mut found: Option<Arc<Link>> = None;
        for (path, paragraph) in self.document.paragraphs_in(from.path.story) {
            if path < from.path || path > to.path {
                continue;
            }
            let a = if path == from.path { from.offset } else { 0 };
            let b = if path == to.path {
                to.offset
            } else {
                usize::MAX
            };
            let mut start = 0;
            for run in &paragraph.runs {
                let end = start + run.text.len();
                if !run.text.is_empty() && end > a && start < b {
                    match (&found, &run.link) {
                        (_, None) => return None,
                        (None, Some(link)) => found = Some(link.clone()),
                        (Some(have), Some(link)) if **have != **link => return None,
                        _ => {}
                    }
                }
                start = end;
            }
        }
        found
    }

    /// What the link dialog shows for the selection or the caret.
    pub fn link_info(&self) -> LinkInfo {
        let Some((from, to)) = self.link_scope() else {
            return LinkInfo::default();
        };
        let text = text_between(&self.document, &from, &to);
        let text = if text.contains('\n') {
            String::new()
        } else {
            text
        };
        let link = self.link_over(&from, &to);
        LinkInfo {
            text,
            target: link.as_ref().map(|l| l.target.clone()),
            tooltip: link.and_then(|l| l.tooltip.clone()),
        }
    }

    /// The address of the link the caret is in, or all the selection has.
    pub fn link_at_caret(&self) -> Option<String> {
        self.link_info().target
    }

    /// Put `target` on the selection or the link around the caret, or take
    /// the link off with `None`. With a caret outside links the address
    /// itself is typed as a link.
    pub(super) fn set_link_command(&mut self, target: Option<String>) -> bool {
        let target = target
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        let Some((from, to)) = self.link_scope() else {
            return match target {
                Some(target) => self.insert_linked_text(&target.clone(), &target),
                None => false,
            };
        };
        let tooltip = self.link_info().tooltip;
        self.apply(None, true, |document, selection| {
            let link = target.map(|target| Link { target, tooltip });
            edit::set_link(document, &from, &to, link);
            Some(selection.clone())
        })
    }

    /// The dialog's result: `target` is the address (empty to remove the
    /// link), `text` what the linked text says. With a selection or a caret
    /// in a link, the text is replaced when it differs from what is there
    /// (empty text keeps it); otherwise `text` (the address when empty) is
    /// typed at the caret as a link. One step of undo.
    pub(super) fn edit_link(&mut self, text: &str, target: &str) -> bool {
        let target = target.trim();
        let Some((from, to)) = self.link_scope() else {
            if target.is_empty() {
                return false;
            }
            let text = if text.trim().is_empty() { target } else { text };
            return self.insert_linked_text(text, target);
        };
        if target.is_empty() {
            return self.set_link_command(None);
        }
        let tooltip = self.link_info().tooltip;
        let link = Link {
            target: target.to_owned(),
            tooltip,
        };
        let old = text_between(&self.document, &from, &to);
        let keep = text.trim().is_empty() || text == old;
        self.apply(None, true, |document, selection| {
            if keep {
                edit::set_link(document, &from, &to, Some(link));
                return Some(selection.clone());
            }
            let at = edit::delete(document, &from, &to);
            let end = edit::insert_link(document, &at, text, link);
            Some(Selection::caret(end))
        })
    }

    /// Type `text` as a link to `target` in place of the selection.
    pub(super) fn insert_linked_text(&mut self, text: &str, target: &str) -> bool {
        let target = target.trim();
        if text.is_empty() || target.is_empty() {
            return false;
        }
        let link = Link {
            target: target.to_owned(),
            tooltip: None,
        };
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            let end = edit::insert_link(document, &at, text, link);
            (end != at).then(|| Selection::caret(end))
        })
    }

    // --- Pictures ---

    /// Put a picture (the bytes of a PNG, JPEG, GIF, BMP or TIFF file, and
    /// its size in pixels) in the line at the caret, in place of the
    /// selection. It is as big as the pixels are at 96 to the inch, scaled
    /// down, keeping its proportions, to the width the text has there. The
    /// caret goes after it. `Ok(false)` while an input method is composing.
    pub fn insert_picture(
        &mut self,
        image: Arc<[u8]>,
        pixels: (u32, u32),
        alt: Option<String>,
    ) -> Result<bool, PictureError> {
        if self.compose.is_some() {
            return Ok(false);
        }
        if pixels.0 == 0 || pixels.1 == 0 {
            return Err(PictureError::BadSize);
        }
        let mut failed = None;
        let done = self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            let (mut w, mut h) = (
                i64::from(pixels.0) * EMU_PIXEL,
                i64::from(pixels.1) * EMU_PIXEL,
            );
            let room = i64::from(edit::space_for_block(document, &at.path)) * EMU_TWIP;
            if w > room && room > 0 {
                h = (h as f64 * room as f64 / w as f64).round().max(1.0) as i64;
                w = room;
            }
            match edit::insert_picture(document, &at, image, (w, h), alt) {
                Ok(end) => Some(Selection::caret(end)),
                Err(error) => {
                    failed = Some(error);
                    None
                }
            }
        });
        match failed {
            Some(error) => Err(error),
            None => Ok(done),
        }
    }

    /// The picture that is selected as an object, if one is.
    pub fn selected_object(&self) -> Option<ObjectInfo> {
        let sel = self.sel.as_ref()?;
        let (a, b) = if sel.anchor <= sel.focus {
            (&sel.anchor, &sel.focus)
        } else {
            (&sel.focus, &sel.anchor)
        };
        if a.path != b.path || b.offset != a.offset + OBJECT.len() {
            return None;
        }
        let paragraph = self.document.paragraph(&a.path)?;
        let mut start = 0;
        for run in &paragraph.runs {
            if start == a.offset {
                let Inline::Drawing(drawing) = &run.kind else {
                    return None;
                };
                let DrawingContent::Picture(picture) = &drawing.content else {
                    return None;
                };
                let (page, rect) = self.picture_rect(&picture.image, drawing.size, a)?;
                return Some(ObjectInfo {
                    pos: a.clone(),
                    inline: drawing.float.is_none(),
                    size: drawing.size,
                    page,
                    rect,
                });
            }
            start += run.text.len();
        }
        None
    }

    /// Where the picture with these bytes and this size is drawn: the one
    /// nearest the object character at `at` when there are several.
    fn picture_rect(
        &self,
        image: &Arc<[u8]>,
        size: (i64, i64),
        at: &Pos,
    ) -> Option<(usize, PageRect)> {
        let (w, h) = (size.0 as f64 / EMU_PT, size.1 as f64 / EMU_PT);
        let mut found: Vec<(usize, PageRect)> = Vec::new();
        for (index, page) in self.layout.pages.iter().enumerate() {
            for item in &page.items {
                if let Item::Picture(p) = item
                    && Arc::ptr_eq(&p.image, image)
                    && (f64::from(p.rect.width()) - w).abs() < 1.0
                    && (f64::from(p.rect.height()) - h).abs() < 1.0
                {
                    found.push((index, p.rect));
                }
            }
        }
        let caret = self.layout.caret(at);
        found.into_iter().min_by(|(pa, ra), (pb, rb)| {
            let distance = |page: usize, rect: &PageRect| match caret {
                Some(c) if c.page == page => {
                    (rect.x0 - c.x).abs() + (rect.y1 - (c.top + c.height)).abs()
                }
                _ => f32::MAX,
            };
            distance(*pa, ra).total_cmp(&distance(*pb, rb))
        })
    }

    /// The picture under a point of the window (physical pixels): its place
    /// in the text, so that it can be selected.
    fn object_at(&self, x: f64, y: f64) -> Option<(Pos, Pos)> {
        if !self.is_over_page(x, y) {
            return None;
        }
        let (page, px, py) = self.locate(x, y);
        let items = &self.layout.pages.get(page)?.items;
        let hit = items.iter().rev().find_map(|item| match item {
            Item::Picture(p) if p.rect.contains(px, py) => Some(p),
            _ => None,
        })?;
        let near = self.layout.hit(page, px, py);
        let size = |s: (i64, i64)| {
            (s.0 as f64 / EMU_PT - f64::from(hit.rect.width())).abs() < 1.0
                && (s.1 as f64 / EMU_PT - f64::from(hit.rect.height())).abs() < 1.0
        };
        let matching = |path: &Path, offset: usize| -> bool {
            let Some(p) = self.document.paragraph(path) else {
                return false;
            };
            let mut start = 0;
            for run in &p.runs {
                if start == offset {
                    return matches!(
                        &run.kind,
                        Inline::Drawing(d)
                            if size(d.size)
                                && matches!(&d.content, DrawingContent::Picture(pic)
                                    if Arc::ptr_eq(&pic.image, &hit.image))
                    );
                }
                start += run.text.len();
            }
            false
        };
        // The picture the click is on is next to the place it hits (in the
        // line, one of the two sides of it); a floating one may be anchored
        // anywhere, and is looked for in the whole body.
        let mut candidates: Vec<Pos> = Vec::new();
        if let Some(near) = &near
            && let Some(p) = self.document.paragraph(&near.path)
        {
            let mut start = 0;
            for run in &p.runs {
                if matches!(run.kind, Inline::Drawing(_)) {
                    candidates.push(Pos::new(near.path.clone(), start));
                }
                start += run.text.len();
            }
            let at = near.offset;
            candidates.sort_by_key(|c| c.offset.abs_diff(at));
        }
        let found = candidates
            .into_iter()
            .find(|c| matching(&c.path, c.offset))
            .or_else(|| {
                self.document
                    .paragraphs()
                    .into_iter()
                    .find_map(|(path, p)| {
                        let mut start = 0;
                        for run in &p.runs {
                            if matches!(run.kind, Inline::Drawing(_)) && matching(&path, start) {
                                return Some(Pos::new(path.clone(), start));
                            }
                            start += run.text.len();
                        }
                        None
                    })
            })?;
        let end = Pos::new(found.path.clone(), found.offset + OBJECT.len());
        Some((found, end))
    }

    /// Select the picture under a point as an object. False when there is
    /// none, or while composing.
    fn select_object_at(&mut self, x: f64, y: f64) -> bool {
        let Some((start, end)) = self.object_at(x, y) else {
            return false;
        };
        let (Some(start), Some(end)) = (
            sanitized(&self.document, start),
            sanitized(&self.document, end),
        ) else {
            return false;
        };
        self.goal_x = None;
        self.press = None;
        self.set_sel(start, end);
        true
    }

    /// The handle (a corner of the selected picture) under a point of the
    /// window, for the pointer's shape.
    pub fn handle_at(&self, x: f64, y: f64) -> Option<Corner> {
        let info = self.selected_object().filter(|info| info.inline)?;
        let rect = self.page_rect(info.page);
        let ppp = self.pixels_per_point();
        let reach = HANDLE_HIT * self.s();
        Corner::ALL.into_iter().find(|corner| {
            let (cx, cy) = corner.of(&info.rect);
            let (sx, sy) = (rect.x0 + f64::from(cx) * ppp, rect.y0 + f64::from(cy) * ppp);
            (x - sx).abs() <= reach && (y - sy).abs() <= reach
        })
    }

    /// A press that begins a resize or selects a picture. `Some(changed)`
    /// when the press was about a picture, `None` when it was not.
    pub(super) fn press_object(&mut self, x: f64, y: f64) -> Option<bool> {
        if let Some(corner) = self.handle_at(x, y)
            && let Some(info) = self.selected_object()
            && let Some(selection) = self.selection_value()
        {
            self.press = None;
            self.resizing = Some(Resize {
                pos: info.pos,
                before: self.document.clone(),
                selection,
                page: info.page,
                corner,
                fixed: corner.opposite().of(&info.rect),
                start: info.size,
                size: info.size,
            });
            return Some(false);
        }
        self.select_object_at(x, y).then_some(true)
    }

    /// The pointer moved while a picture is being resized. `None` when none
    /// is.
    pub(super) fn drag_object(&mut self, x: f64, y: f64) -> Option<bool> {
        let resize = self.resizing.as_ref()?;
        let rect = self.page_rect(resize.page);
        let ppp = self.pixels_per_point();
        let (px, py) = (((x - rect.x0) / ppp) as f32, ((y - rect.y0) / ppp) as f32);
        let held = resize.corner.of(&PageRect::new(0.0, 0.0, 1.0, 1.0));
        // Which side of the fixed corner the held one is on.
        let (sx, sy) = (
            if held.0 > 0.5 { 1.0 } else { -1.0 },
            if held.1 > 0.5 { 1.0 } else { -1.0 },
        );
        let (w0, h0) = (
            (resize.start.0 as f64).max(1.0),
            (resize.start.1 as f64).max(1.0),
        );
        let dx = (f64::from(px - resize.fixed.0) * sx).max(0.0) * EMU_PT;
        let dy = (f64::from(py - resize.fixed.1) * sy).max(0.0) * EMU_PT;
        let least = MIN_SIDE * EMU_PT / w0.min(h0);
        let scale = (dx / w0).max(dy / h0).max(least);
        let size = (
            (w0 * scale).round().max(1.0) as i64,
            (h0 * scale).round().max(1.0) as i64,
        );
        if size == resize.size {
            return Some(false);
        }
        let mut document = resize.before.clone();
        edit::resize_drawing(&mut document, &resize.pos, size);
        if let Some(resize) = &mut self.resizing {
            resize.size = size;
        }
        self.document = document;
        self.revision += 1;
        self.relayout();
        self.touch();
        Some(true)
    }

    /// The button went up after a resize: it becomes one step of undo.
    /// Returns whether a resize was going on.
    pub(super) fn finish_resize(&mut self) -> bool {
        let Some(resize) = self.resizing.take() else {
            return false;
        };
        if resize.size != resize.start
            && let Some(after) = self.selection_value()
        {
            if self
                .last_edit
                .is_some_and(|at| at.elapsed() > super::modify::PAUSE)
            {
                self.history.seal();
            }
            self.history
                .record(&resize.before, &resize.selection, &after, None);
            self.history.seal();
            self.last_edit = Some(std::time::Instant::now());
        }
        self.touch();
        true
    }

    /// Whether a picture is being resized.
    pub fn is_resizing(&self) -> bool {
        self.resizing.is_some()
    }

    /// Paint the frame and handles of the selected picture on one page.
    pub(super) fn draw_object(&mut self, index: usize, page: vello_cpu::kurbo::Rect, ppp: f64) {
        let Some(info) = self.selected_object().filter(|info| info.page == index) else {
            return;
        };
        let s = self.s();
        let style = self.style.clone();
        let r = vello_cpu::kurbo::Rect::new(
            page.x0 + f64::from(info.rect.x0) * ppp,
            page.y0 + f64::from(info.rect.y0) * ppp,
            page.x0 + f64::from(info.rect.x1) * ppp,
            page.y0 + f64::from(info.rect.y1) * ppp,
        );
        let line = (1.5 * s).round().max(1.0);
        self.ctx.set_paint(super::color(style.object));
        for edge in [
            vello_cpu::kurbo::Rect::new(r.x0 - line, r.y0 - line, r.x1 + line, r.y0),
            vello_cpu::kurbo::Rect::new(r.x0 - line, r.y1, r.x1 + line, r.y1 + line),
            vello_cpu::kurbo::Rect::new(r.x0 - line, r.y0, r.x0, r.y1),
            vello_cpu::kurbo::Rect::new(r.x1, r.y0, r.x1 + line, r.y1),
        ] {
            self.ctx.fill_rect(&edge);
        }
        if !info.inline {
            return;
        }
        let half = (HANDLE * s / 2.0).round().max(2.0);
        for corner in Corner::ALL {
            let (cx, cy) = corner.of(&info.rect);
            let (x, y) = (
                (page.x0 + f64::from(cx) * ppp).round(),
                (page.y0 + f64::from(cy) * ppp).round(),
            );
            self.ctx.set_paint(super::color(style.object));
            self.ctx.fill_rect(&vello_cpu::kurbo::Rect::new(
                x - half,
                y - half,
                x + half,
                y + half,
            ));
            self.ctx.set_paint(vello_cpu::peniko::Color::WHITE);
            self.ctx.fill_rect(&vello_cpu::kurbo::Rect::new(
                x - half + line,
                y - half + line,
                x + half - line,
                y + half - line,
            ));
        }
    }
}
