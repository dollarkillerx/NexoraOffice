//! The caret and the selection of a [`DocView`]: the pointer, the keys,
//! find, and links. Nothing here changes the document.

use aster_layout::Rect as PageRect;
use aster_writer::Pos;

use super::search::{self, Finding};
use super::text::{
    floor_boundary, next_cluster, previous_cluster, sanitized, text_between, word_after, word_at,
    word_before,
};
use super::{DocQuery, DocView, Sel};

/// Where a press began and how much it picked, so that a drag extends it by
/// the same unit.
#[derive(Clone, Debug)]
pub(super) struct Press {
    start: Pos,
    end: Pos,
    /// 1 characters, 2 words, 3 paragraphs.
    unit: u8,
}

/// A move of the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// By grapheme cluster.
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    /// Of the line the caret is on.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
    /// By a window's height.
    PageUp,
    PageDown,
}

impl DocView {
    // --- The selection ---

    /// Where the caret is: the end of the selection that moves.
    pub fn caret(&self) -> Option<&Pos> {
        self.sel.as_ref().map(|sel| &sel.focus)
    }

    /// The two ends of the selection, the fixed one first; `None` when
    /// nothing is selected.
    pub fn selection(&self) -> Option<(&Pos, &Pos)> {
        self.sel
            .as_ref()
            .filter(|sel| sel.anchor != sel.focus)
            .map(|sel| (&sel.anchor, &sel.focus))
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected text: one paragraph per line, pictures and note
    /// references left out, tabs kept.
    pub fn selected_text(&self) -> String {
        match self.selection() {
            Some((a, b)) => text_between(&self.document, a, b),
            None => String::new(),
        }
    }

    /// Select from `anchor` to `focus`, moved onto valid places. Returns
    /// whether anything changed.
    pub fn set_selection(&mut self, anchor: Pos, focus: Pos) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let (Some(anchor), Some(focus)) = (
            sanitized(&self.document, anchor),
            sanitized(&self.document, focus),
        ) else {
            return false;
        };
        self.goal_x = None;
        self.set_sel(anchor, focus)
    }

    fn set_sel(&mut self, anchor: Pos, focus: Pos) -> bool {
        let new = Sel { anchor, focus };
        if self.sel.as_ref() == Some(&new) {
            return false;
        }
        // A caret that goes somewhere else ends the step of typing and drops
        // the look that was waiting for the next text.
        if self.sel.as_ref().map(|s| &s.focus) != Some(&new.focus) {
            self.pending = None;
            self.history.seal();
        }
        self.sel = Some(new);
        self.sel_rects = None;
        self.touch();
        true
    }

    /// Select the whole body.
    pub fn select_all(&mut self) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let (Some(start), Some(end)) = (self.document.start(), self.document.end()) else {
            return false;
        };
        self.goal_x = None;
        self.set_sel(start, end)
    }

    /// Drop the selection, leaving the caret where it was.
    pub fn clear_selection(&mut self) -> bool {
        if self.compose.is_some() {
            return false;
        }
        match self.sel.clone() {
            Some(sel) => self.set_sel(sel.focus.clone(), sel.focus),
            None => false,
        }
    }

    /// Put the caret at `pos` and show it near the top of the window.
    pub fn go_to(&mut self, pos: &Pos) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let Some(pos) = sanitized(&self.document, pos.clone()) else {
            return false;
        };
        self.goal_x = None;
        let changed = self.set_sel(pos.clone(), pos);
        let Some((_, y, _, _)) = self.caret_rect() else {
            return changed;
        };
        let before = self.scroll_y;
        self.scroll_y += y - 2.0 * super::MARGIN * self.s();
        self.clamp();
        if let Some(rect) = self.caret_rect() {
            self.reveal(rect);
        }
        changed || self.scroll_y != before
    }

    /// Scroll to keep the caret in the window.
    pub fn reveal_caret(&mut self) -> bool {
        match self.caret_rect() {
            Some(rect) => self.reveal(rect),
            None => false,
        }
    }

    // --- The pointer ---

    /// The place under a point of the window (physical pixels).
    pub fn pos_at(&self, x: f64, y: f64) -> Option<Pos> {
        let (page, px, py) = self.locate(x, y);
        let pos = self.layout.hit(page, px, py)?;
        sanitized(&self.document, pos)
    }

    /// The text a unit around `pos`: the character's word, its paragraph.
    fn unit_range(&self, pos: &Pos, unit: u8) -> (Pos, Pos) {
        let Some(paragraph) = self.document.paragraph(&pos.path) else {
            return (pos.clone(), pos.clone());
        };
        let text = paragraph.text();
        let (start, end) = match unit {
            2 if !text.is_empty() => {
                let range = word_at(&text, pos.offset);
                (range.start, range.end)
            }
            3 => (0, text.len()),
            _ => (pos.offset, pos.offset),
        };
        (
            Pos::new(pos.path.clone(), start),
            Pos::new(pos.path.clone(), end),
        )
    }

    /// The button went down at a point (physical pixels): `clicks` is 1 for
    /// a click, 2 for a double and 3 for a triple click; with `extend` a
    /// single click grows the selection from where it was. Returns whether
    /// the picture changed.
    pub fn press(&mut self, x: f64, y: f64, clicks: u8, extend: bool) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let Some(pos) = self.pos_at(x, y) else {
            return false;
        };
        self.goal_x = None;
        let unit = clicks.clamp(1, 3);
        if extend && unit == 1 {
            let anchor = self
                .sel
                .as_ref()
                .map_or_else(|| pos.clone(), |sel| sel.anchor.clone());
            self.press = Some(Press {
                start: anchor.clone(),
                end: anchor.clone(),
                unit: 1,
            });
            return self.set_sel(anchor, pos);
        }
        let (start, end) = self.unit_range(&pos, unit);
        self.press = Some(Press {
            start: start.clone(),
            end: end.clone(),
            unit,
        });
        self.set_sel(start, end)
    }

    /// The pointer moved with the button down: extend what the press began
    /// to the point, by the same unit. Returns whether the picture changed.
    pub fn drag(&mut self, x: f64, y: f64) -> bool {
        let Some(press) = self.press.clone() else {
            return false;
        };
        let Some(pos) = self.pos_at(x, y) else {
            return false;
        };
        let (anchor, focus) = if press.unit == 1 {
            (press.start, pos)
        } else {
            let (from, to) = self.unit_range(&pos, press.unit);
            if from < press.start {
                (press.end, from)
            } else {
                (press.start, to.max(press.end))
            }
        };
        self.set_sel(anchor, focus)
    }

    /// With the button held past the top or bottom edge of the window, one
    /// step of scrolling towards it, and the selection extended to what
    /// comes into view. Call it on a timer while the button is down. Returns
    /// whether anything changed.
    pub fn drag_scroll(&mut self, x: f64, y: f64) -> bool {
        let height = f64::from(self.height);
        let s = self.s();
        let over = if y < 0.0 {
            y
        } else if y > height {
            y - height
        } else {
            0.0
        };
        // Faster the further out, from half a line to several lines.
        let step = (over.abs() * 0.5).clamp(LINE_STEP_MIN * s, 6.0 * super::LINE_STEP * s);
        let mut moved = false;
        if over != 0.0 {
            moved = self.scroll_by(step.copysign(over));
        }
        let (x, y) = (
            x.clamp(0.0, f64::from(self.width)),
            y.clamp(0.0, (height - 1.0).max(0.0)),
        );
        self.drag(x, y) || moved
    }

    /// The button went up.
    pub fn release(&mut self) {
        self.press = None;
    }

    // --- The keys ---

    /// Move the caret; with `select` the selection grows from where it was.
    /// Scrolls to keep the caret in the window. Returns whether anything
    /// changed.
    pub fn move_caret(&mut self, movement: Move, select: bool) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let Some(sel) = self.sel.clone() else {
            return false;
        };
        let focus = sel.focus.clone();
        let collapsed = sel.anchor == sel.focus;
        let mut vertical = false;
        let target: Option<Pos> = match movement {
            Move::Left if !select && !collapsed => Some(sel.anchor.clone().min(focus.clone())),
            Move::Right if !select && !collapsed => Some(sel.anchor.clone().max(focus.clone())),
            Move::Left => self.step_cluster(&focus, false),
            Move::Right => self.step_cluster(&focus, true),
            Move::WordLeft => self.step_word(&focus, false),
            Move::WordRight => self.step_word(&focus, true),
            Move::Up | Move::Down => {
                vertical = true;
                let down = movement == Move::Down;
                let goal = self
                    .goal_x
                    .or_else(|| self.layout.caret(&focus).map(|c| c.x))
                    .unwrap_or(0.0);
                self.goal_x = Some(goal);
                let next = if down {
                    self.layout.below(&focus, goal)
                } else {
                    self.layout.above(&focus, goal)
                };
                // Past the first or last line: to the start or the end.
                match next {
                    Some(next) => Some(self.table_cell_in_column(&focus, next, goal)),
                    None if down => self.document.end(),
                    None => self.document.start(),
                }
            }
            Move::LineStart => self.layout.line_start(&focus),
            Move::LineEnd => self.layout.line_end(&focus),
            Move::DocStart => self.document.start(),
            Move::DocEnd => self.document.end(),
            Move::PageUp | Move::PageDown => {
                vertical = true;
                self.page_target(&focus, movement == Move::PageDown)
            }
        };
        if !vertical {
            self.goal_x = None;
        }
        let target = target
            .and_then(|pos| sanitized(&self.document, pos))
            .unwrap_or(focus);
        let anchor = if select { sel.anchor } else { target.clone() };
        let changed = self.set_sel(anchor, target);
        let scrolled = self.reveal_caret();
        changed || scrolled
    }

    /// Going up or down from a paragraph outside a table into one, the
    /// layout gives the first or last cell in reading order: the cell in
    /// the goal column of that row is where the caret goes.
    fn table_cell_in_column(&self, from: &Pos, next: Pos, goal: f32) -> Pos {
        let in_same_table =
            from.path.in_cell().is_some() && from.path.0.first() == next.path.0.first();
        if next.path.in_cell().is_none() || in_same_table {
            return next;
        }
        let Some(caret) = self.layout.caret(&next) else {
            return next;
        };
        match self
            .layout
            .hit(caret.page, goal, caret.top + caret.height / 2.0)
        {
            Some(found) if found.path.0.first() == next.path.0.first() => {
                sanitized(&self.document, found).unwrap_or(next)
            }
            _ => next,
        }
    }

    /// The place a window's height above or below the caret, at the same
    /// distance from the page's edge; the window scrolls by as much.
    fn page_target(&mut self, from: &Pos, down: bool) -> Option<Pos> {
        let goal = self
            .goal_x
            .or_else(|| self.layout.caret(from).map(|c| c.x))
            .unwrap_or(0.0);
        self.goal_x = Some(goal);
        let (_, y, _, h) = self.caret_rect()?;
        let step = if down {
            self.page_step()
        } else {
            -self.page_step()
        };
        let before = self.scroll_y;
        self.scroll_y += step;
        self.clamp();
        let moved = self.scroll_y - before;
        let screen_y = y + h / 2.0 + step - moved;
        let (page, _, py) = self.locate(0.0, screen_y);
        let reached = self.layout.hit(page, goal, py)?;
        // The window cannot go further and neither can the caret.
        if &reached == from && moved == 0.0 {
            return if down {
                self.document.end()
            } else {
                self.document.start()
            };
        }
        Some(reached)
    }

    /// One grapheme cluster on, into the next paragraph at its start.
    fn step_cluster(&self, pos: &Pos, forward: bool) -> Option<Pos> {
        let text = self.document.paragraph(&pos.path)?.text();
        let at = floor_boundary(&text, pos.offset);
        if forward {
            if at < text.len() {
                return Some(Pos::new(pos.path.clone(), next_cluster(&text, at)));
            }
            let next = self.neighbour(&pos.path, true)?;
            Some(Pos::new(next, 0))
        } else {
            if at > 0 {
                return Some(Pos::new(pos.path.clone(), previous_cluster(&text, at)));
            }
            let previous = self.neighbour(&pos.path, false)?;
            let len = self.document.paragraph(&previous)?.len();
            Some(Pos::new(previous, len))
        }
    }

    /// One word on, or back; over a paragraph's end to the next paragraph.
    fn step_word(&self, pos: &Pos, forward: bool) -> Option<Pos> {
        let text = self.document.paragraph(&pos.path)?.text();
        let at = floor_boundary(&text, pos.offset);
        if forward {
            match word_after(&text, at) {
                Some(end) => Some(Pos::new(pos.path.clone(), end)),
                None => self.step_cluster(pos, true),
            }
        } else {
            match word_before(&text, at) {
                Some(start) => Some(Pos::new(pos.path.clone(), start)),
                None => self.step_cluster(pos, false),
            }
        }
    }

    /// The paragraph after or before `path` in reading order.
    fn neighbour(&self, path: &aster_writer::Path, forward: bool) -> Option<aster_writer::Path> {
        let index = self.paths.binary_search(path).ok()?;
        let next = if forward {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };
        self.paths.get(next).cloned()
    }

    // --- Links ---

    /// The target of the link under a point of the window (physical
    /// pixels), if any: a URL, or `#name` for a place in the document.
    pub fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (page, px, py) = self.locate(x, y);
        let rect = self.page_rect(page);
        if x < rect.x0 || x > rect.x1 || y < rect.y0 || y > rect.y1 {
            return None;
        }
        let pos = self.layout.hit(page, px, py)?;
        let paragraph = self.document.paragraph(&pos.path)?;
        let text = paragraph.text();
        let at = floor_boundary(&text, pos.offset);
        let after = text[at..].chars().next().map_or(at, |c| at + c.len_utf8());
        let before = text[..at].char_indices().next_back().map_or(at, |(i, _)| i);
        for (start, end) in [(at, after), (before, at)] {
            if start == end {
                continue;
            }
            let (Some(a), Some(b)) = (
                self.layout.caret(&Pos::new(pos.path.clone(), start)),
                self.layout.caret(&Pos::new(pos.path.clone(), end)),
            ) else {
                continue;
            };
            let same_line = a.page == page && b.page == page && (a.top - b.top).abs() < 1.0;
            if !same_line
                || px < a.x.min(b.x)
                || px > a.x.max(b.x)
                || py < a.top
                || py > a.top + a.height
            {
                continue;
            }
            let mut offset = 0;
            for run in &paragraph.runs {
                let next = offset + run.text.len();
                if start >= offset && start < next {
                    return run.link.as_ref().map(|link| link.target.clone());
                }
                offset = next;
            }
        }
        None
    }

    // --- Find ---

    /// Search the body for the query (replacing any search before). The
    /// current match is the first at or after the caret, wrapping round; the
    /// view does not scroll: see [`DocView::go_to_hit`]. Returns the number
    /// of matches.
    pub fn set_find(&mut self, query: DocQuery) -> usize {
        let (matches, truncated) = search::search(&self.document, &query);
        let pages: Vec<usize> = matches
            .iter()
            .map(|m| self.layout.page_of(&m.start).unwrap_or(0))
            .collect();
        let current = self.caret().and_then(|caret| {
            matches
                .iter()
                .position(|m| &m.start >= caret)
                .or((!matches.is_empty()).then_some(0))
        });
        let count = matches.len();
        self.find = Some(Finding {
            query,
            matches,
            pages,
            current: current.filter(|_| count > 0),
            truncated,
        });
        self.hit_rects.clear();
        self.touch();
        count
    }

    /// Stop searching: the marks go.
    pub fn clear_find(&mut self) {
        if self.find.take().is_some() {
            self.hit_rects.clear();
            self.touch();
        }
    }

    pub fn find_query(&self) -> Option<&DocQuery> {
        self.find.as_ref().map(|f| &f.query)
    }

    /// Number of matches.
    pub fn hit_count(&self) -> usize {
        self.find.as_ref().map_or(0, |f| f.matches.len())
    }

    /// Whether the search stopped at the most matches it keeps.
    pub fn find_truncated(&self) -> bool {
        self.find.as_ref().is_some_and(|f| f.truncated)
    }

    /// The match drawn stronger.
    pub fn current_hit(&self) -> Option<usize> {
        self.find.as_ref().and_then(|f| f.current)
    }

    pub fn hit(&self, index: usize) -> Option<(&Pos, &Pos)> {
        self.find
            .as_ref()
            .and_then(|f| f.matches.get(index))
            .map(|m| (&m.start, &m.end))
    }

    /// Make match `index` the current one (none when out of range).
    pub fn set_current_hit(&mut self, index: Option<usize>) {
        if let Some(find) = &mut self.find {
            let index = index.filter(|i| *i < find.matches.len());
            if index != find.current {
                find.current = index;
                self.touch();
            }
        }
    }

    /// Scroll so match `index` is in the window, in the middle third of it
    /// when it was not. Returns whether the view moved; false also when
    /// there is no such match.
    pub fn go_to_hit(&mut self, index: usize) -> bool {
        let Some(start) = self.hit(index).map(|(start, _)| start.clone()) else {
            return false;
        };
        let Some(caret) = self.layout.caret(&start) else {
            return false;
        };
        let rect = self.page_rect(caret.page);
        let ppp = self.pixels_per_point();
        let top = rect.y0 + f64::from(caret.top) * ppp;
        let bottom = top + f64::from(caret.height) * ppp;
        let x = rect.x0 + f64::from(caret.x) * ppp;
        let before = (self.scroll_x, self.scroll_y);
        let h = f64::from(self.height);
        let pad = h * 0.15;
        if top < pad || bottom > h - pad {
            self.scroll_y += (top + bottom) / 2.0 - h / 3.0;
        }
        let w = f64::from(self.width);
        if x < w * 0.1 || x > w * 0.9 {
            self.scroll_x += x - w / 2.0;
        }
        self.clamp();
        (self.scroll_x, self.scroll_y) != before
    }

    /// Go to the next or previous match, wrapping round: it becomes the
    /// current match and the selection, and is shown. From no current match
    /// the search starts at the caret. False when there are no matches.
    pub fn find_step(&mut self, forward: bool) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let caret = self.caret().cloned();
        let Some(find) = &self.find else {
            return false;
        };
        let count = find.matches.len();
        if count == 0 {
            return false;
        }
        let next = match (find.current, forward) {
            (Some(i), true) => (i.min(count - 1) + 1) % count,
            (Some(i), false) => (i.min(count - 1) + count - 1) % count,
            (None, true) => caret
                .and_then(|c| find.matches.iter().position(|m| m.start >= c))
                .unwrap_or(0),
            (None, false) => caret
                .and_then(|c| find.matches.iter().rposition(|m| m.end <= c))
                .unwrap_or(count - 1),
        };
        let (start, end) = (
            find.matches[next].start.clone(),
            find.matches[next].end.clone(),
        );
        self.set_current_hit(Some(next));
        self.goal_x = None;
        self.set_sel(start, end);
        self.go_to_hit(next);
        true
    }

    /// The marks of the matches on one page: whether it is the current one,
    /// and the rectangle in the page's points.
    pub(super) fn hit_marks(&mut self, page: usize) -> Vec<(bool, PageRect)> {
        let Some(find) = &self.find else {
            return Vec::new();
        };
        let first = find.pages.partition_point(|p| *p < page.saturating_sub(1));
        let wanted: Vec<usize> = find.pages[first..]
            .iter()
            .take_while(|p| **p <= page)
            .enumerate()
            .map(|(n, _)| first + n)
            .collect();
        let current = find.current;
        let mut marks = Vec::new();
        for index in wanted {
            if !self.hit_rects.contains_key(&index) {
                let (start, end) = {
                    let m = &self.find.as_ref().expect("searching").matches[index];
                    (m.start.clone(), m.end.clone())
                };
                self.hit_rects
                    .insert(index, self.layout.selection(&start, &end));
            }
            for (on, rect) in &self.hit_rects[&index] {
                if *on == page {
                    marks.push((current == Some(index), *rect));
                }
            }
        }
        marks
    }

    /// The rectangles of the selection, per page.
    pub(super) fn selection_rects(&mut self) -> &[(usize, PageRect)] {
        if self.sel_rects.is_none() {
            let rects = match self.selection() {
                Some((a, b)) => self.layout.selection(a, b),
                None => Vec::new(),
            };
            self.sel_rects = Some(rects);
        }
        self.sel_rects.as_deref().unwrap_or(&[])
    }
}

/// The least a drag outside the window scrolls per step, logical pixels.
const LINE_STEP_MIN: f64 = 12.0;
