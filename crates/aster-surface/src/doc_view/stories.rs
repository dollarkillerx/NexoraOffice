//! Stories other than the body: pointing at notes, entering and leaving
//! headers and footers, making footnotes and page numbers.
//!
//! **The active story** is the story of the caret: the body, a footnote or an
//! endnote, a header or a footer. A single click in a note puts the caret
//! there; a click in the body goes back. A double-click in the strip above or
//! below the body enters the header or footer the page shows (Word's way in);
//! while inside, a single click in the body does nothing, a double-click
//! in the body or [`DocView::leave_marginal`] (Esc) leaves, and a dashed line
//! across each page marks the edge of the strips. Clicks in the strips move
//! between the header and the footer.
//!
//! **Entering changes nothing.** On a page that shows no header (or footer)
//! the strip can be entered all the same; the story is made empty by the
//! first edit made in it, as one step of undo with that edit.
//!
//! **Pages.** A header is on every page of its section, so the view keeps the
//! page the caret is on and asks the layout about places on it.

use aster_layout::Caret;
use aster_writer::edit::{self, Selection};
use aster_writer::{MarginalKind, MarginalSlot, PageField, Pos, StoryId};
use vello_cpu::kurbo::Rect;

use super::{DocStyle, DocView, Sel, color};

/// A header or footer the caret is in that does not exist yet.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct NewMarginal {
    /// The page it was entered on.
    pub page: usize,
    pub section: usize,
    pub kind: MarginalKind,
    pub slot: MarginalSlot,
}

/// What a point of a page is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Area {
    Header,
    Footer,
    Note(StoryId),
    Body,
}

impl DocView {
    // --- Which story ---

    /// The story the caret is in. In a header or footer that does not exist
    /// yet (see the module documentation) it is the one the first edit
    /// will make.
    pub fn active_story(&self) -> StoryId {
        if self.new_marginal.is_some() && self.compose.is_none() {
            return StoryId::Marginal(self.document.marginals.len());
        }
        self.caret_pos().map_or(StoryId::Body, |pos| pos.path.story)
    }

    /// The caret is in a header or a footer.
    pub fn in_marginal(&self) -> bool {
        matches!(self.active_story(), StoryId::Marginal(_))
    }

    /// Whether the header or footer the caret is in is a header or a footer
    /// (for the status bar). `None` outside both.
    pub fn marginal_kind(&self) -> Option<MarginalKind> {
        if let Some(new) = &self.new_marginal
            && self.compose.is_none()
        {
            return Some(new.kind);
        }
        let StoryId::Marginal(index) = self.active_story() else {
            return None;
        };
        let used = |kind: fn(&aster_writer::Section) -> &aster_writer::Marginals| {
            self.document.sections().iter().any(|(_, section)| {
                let set = kind(section);
                [set.default, set.first, set.even].contains(&Some(index))
            })
        };
        if used(|s| &s.headers) {
            Some(MarginalKind::Header)
        } else if used(|s| &s.footers) {
            Some(MarginalKind::Footer)
        } else {
            None
        }
    }

    /// The page the caret is on when it is in a header or footer.
    pub fn marginal_page(&self) -> Option<usize> {
        self.in_marginal()
            .then(|| self.new_marginal.as_ref().map(|n| n.page).or(self.page))
            .flatten()
    }

    /// The page to ask the layout about for a place of `story`.
    pub(super) fn page_for(&self, story: StoryId) -> Option<usize> {
        matches!(story, StoryId::Marginal(_))
            .then_some(self.page)
            .flatten()
    }

    /// Where the caret is, with the page of a header or footer.
    pub(super) fn caret_geometry(&self) -> Option<Caret> {
        if let Some(new) = &self.new_marginal
            && self.compose.is_none()
        {
            return self.virtual_caret(new);
        }
        let pos = self.caret_pos()?;
        self.layout
            .caret_on(self.page_for(pos.path.story), &pos)
            .or_else(|| self.layout.caret(&pos))
    }

    /// Where the caret is shown in a header or footer that is not made yet:
    /// at the start of the empty paragraph it will have.
    fn virtual_caret(&self, new: &NewMarginal) -> Option<Caret> {
        let sections = self.document.sections();
        let section = sections.get(new.section)?.1;
        let height = (self.document.styles.default_run.size * 1.2).max(8.0);
        let margins = &section.margins;
        let x = margins.left.max(0) as f32 / 20.0;
        let top = match new.kind {
            MarginalKind::Header => margins.header.max(0) as f32 / 20.0,
            MarginalKind::Footer => {
                let page = self.layout.pages.get(new.page)?;
                page.height - margins.footer.max(0) as f32 / 20.0 - height
            }
        };
        Some(Caret {
            page: new.page,
            x,
            top,
            height,
        })
    }

    /// What a point of `page` is over.
    pub(super) fn area_at(&self, page: usize, x: f32, y: f32) -> Area {
        let Some(info) = self.layout.page_info(page) else {
            return Area::Body;
        };
        if y < info.header_area.y1 {
            return Area::Header;
        }
        if y > info.footer_area.y0 {
            return Area::Footer;
        }
        match self.layout.story_at(page, x, y) {
            Some(story @ (StoryId::Footnote(_) | StoryId::Endnote(_))) => Area::Note(story),
            _ => Area::Body,
        }
    }

    // --- Entering and leaving ---

    /// Enter the header of the page the caret is on.
    pub fn enter_header(&mut self) -> bool {
        let page = self.current_page();
        self.enter_marginal(page, MarginalKind::Header, None)
    }

    /// Enter the footer of the page the caret is on.
    pub fn enter_footer(&mut self) -> bool {
        let page = self.current_page();
        self.enter_marginal(page, MarginalKind::Footer, None)
    }

    /// Put the caret in the header or footer of `page`: at `at` (a click)
    /// when there is one and it is in the story, else at its start. Where
    /// the page shows none, the caret is in the one the first edit makes.
    fn enter_marginal(&mut self, page: usize, kind: MarginalKind, at: Option<(f32, f32)>) -> bool {
        if self.compose.is_some() {
            return false;
        }
        let Some(info) = self.layout.page_info(page).cloned() else {
            return false;
        };
        if !self.in_marginal() {
            self.body_sel = self.sel.clone();
        }
        self.goal_x = None;
        let existing = match kind {
            MarginalKind::Header => info.header,
            MarginalKind::Footer => info.footer,
        };
        let Some(index) = existing else {
            self.new_marginal = Some(NewMarginal {
                page,
                section: info.section,
                kind,
                slot: info.slot,
            });
            self.page = Some(page);
            self.pending = None;
            self.touch();
            return true;
        };
        let story = StoryId::Marginal(index);
        self.new_marginal = None;
        self.page = Some(page);
        let hit = at.and_then(|(x, y)| self.layout.hit_in(page, story, x, y));
        let Some(pos) = hit.or_else(|| self.document.story_start(story)) else {
            return false;
        };
        self.set_sel_on(pos.clone(), pos, Some(page));
        self.touch();
        self.reveal_caret();
        true
    }

    /// Back to the body, where the caret was before the header or footer
    /// was entered. False when the caret is not in one.
    pub fn leave_marginal(&mut self) -> bool {
        if !self.in_marginal() || self.compose.is_some() {
            return false;
        }
        let back = self
            .body_sel
            .take()
            .and_then(|sel| {
                Some(Sel {
                    anchor: super::text::sanitized(&self.document, sel.anchor)?,
                    focus: super::text::sanitized(&self.document, sel.focus)?,
                })
            })
            .filter(|sel| !matches!(sel.focus.path.story, StoryId::Marginal(_)));
        self.new_marginal = None;
        self.page = None;
        let sel = back.or_else(|| {
            self.document.start().map(|start| Sel {
                anchor: start.clone(),
                focus: start,
            })
        });
        self.goal_x = None;
        self.pending = None;
        self.sel = sel;
        self.sel_rects = None;
        self.history.seal();
        self.touch();
        self.reveal_caret();
        true
    }

    /// Set the selection and the page it is on.
    pub(super) fn set_sel_on(&mut self, anchor: Pos, focus: Pos, page: Option<usize>) -> bool {
        let changed = self.set_sel(anchor, focus);
        if matches!(
            self.sel.as_ref().map(|s| s.focus.path.story),
            Some(StoryId::Marginal(_))
        ) {
            self.page = page;
        }
        changed
    }

    // --- The pointer ---

    /// A press that is not on a plain body paragraph: in a note, in a header
    /// or footer, or in the body while inside a header or footer. `None`
    /// when the press is the body's ordinary one.
    pub(super) fn press_story(&mut self, x: f64, y: f64, clicks: u8, extend: bool) -> Option<bool> {
        // The canvas around the pages is no part of any story.
        if !self.is_over_page(x, y) {
            return None;
        }
        let (page, px, py) = self.locate(x, y);
        let area = self.area_at(page, px, py);
        let inside = self.in_marginal();
        match area {
            Area::Header | Area::Footer => {
                let kind = if area == Area::Header {
                    MarginalKind::Header
                } else {
                    MarginalKind::Footer
                };
                if !inside && clicks < 2 {
                    // Outside, one click there is a click in the body.
                    return None;
                }
                Some(self.press_marginal(page, kind, (px, py), clicks, extend))
            }
            Area::Note(story) => {
                if inside {
                    // A note is not the header: leaving takes a double-click.
                    return Some(clicks >= 2 && self.leave_to(x, y));
                }
                Some(self.press_in(story, page, (px, py), clicks, extend))
            }
            Area::Body if inside => Some(clicks >= 2 && self.leave_to(x, y)),
            Area::Body => None,
        }
    }

    /// Leave the header or footer and put the caret where a click was.
    fn leave_to(&mut self, x: f64, y: f64) -> bool {
        self.leave_marginal();
        let Some(pos) = self.pos_at(x, y) else {
            return true;
        };
        self.set_sel(pos.clone(), pos);
        self.reveal_caret();
        true
    }

    /// A click in the strip of a header or footer, inside one or with
    /// a double-click.
    fn press_marginal(
        &mut self,
        page: usize,
        kind: MarginalKind,
        at: (f32, f32),
        clicks: u8,
        extend: bool,
    ) -> bool {
        let Some(info) = self.layout.page_info(page).cloned() else {
            return false;
        };
        let existing = match kind {
            MarginalKind::Header => info.header,
            MarginalKind::Footer => info.footer,
        };
        match existing {
            Some(index) => {
                // Not in one yet: the double-click that enters it puts the
                // caret there and picks no word.
                let clicks = if self.in_marginal() {
                    clicks
                } else {
                    self.body_sel = self.sel.clone();
                    1
                };
                self.new_marginal = None;
                self.press_in(StoryId::Marginal(index), page, at, clicks.max(1), extend)
            }
            None => self.enter_marginal(page, kind, None),
        }
    }

    /// A press on a paragraph of `story` on `page`, picking by `clicks`.
    fn press_in(
        &mut self,
        story: StoryId,
        page: usize,
        at: (f32, f32),
        clicks: u8,
        extend: bool,
    ) -> bool {
        let Some(pos) = self
            .layout
            .hit_in(page, story, at.0, at.1)
            .and_then(|pos| super::text::sanitized(&self.document, pos))
        else {
            return false;
        };
        self.goal_x = None;
        self.new_marginal = None;
        let unit = clicks.clamp(1, 3);
        let same_story = self
            .sel
            .as_ref()
            .is_some_and(|sel| sel.anchor.path.story == story);
        if extend && unit == 1 && same_story {
            let anchor = self
                .sel
                .as_ref()
                .map_or_else(|| pos.clone(), |s| s.anchor.clone());
            self.press = Some(super::editing::Press::new(
                anchor.clone(),
                anchor.clone(),
                1,
            ));
            return self.set_sel_on(anchor, pos, Some(page));
        }
        let (start, end) = self.unit_range(&pos, unit);
        self.press = Some(super::editing::Press::new(start.clone(), end.clone(), unit));
        self.set_sel_on(start, end, Some(page))
    }

    /// The place under a point in `story`, for a drag that began there.
    pub(super) fn pos_in_story(&self, x: f64, y: f64, story: StoryId) -> Option<Pos> {
        if story.is_body() {
            return self.pos_at(x, y);
        }
        let (page, px, py) = self.locate(x, y);
        let pos = self.layout.hit_in(page, story, px, py)?;
        super::text::sanitized(&self.document, pos)
    }

    // --- Painting ---

    /// The dashed lines that mark where the header and footer strips end,
    /// while the caret is in one.
    pub(super) fn draw_marginal_edges(
        &mut self,
        index: usize,
        rect: Rect,
        ppp: f64,
        style: &DocStyle,
    ) {
        if !self.in_marginal() {
            return;
        }
        let Some(info) = self.layout.page_info(index).cloned() else {
            return;
        };
        let s = self.s();
        let thickness = s.round().max(1.0);
        let (dash, gap) = (6.0 * s, 4.0 * s);
        self.ctx.set_paint(color(style.object));
        for y in [info.header_area.y1, info.footer_area.y0] {
            let top = (rect.y0 + f64::from(y) * ppp).round();
            let mut x = rect.x0;
            while x < rect.x1 {
                let end = (x + dash).min(rect.x1);
                self.ctx.fill_rect(&Rect::new(x, top, end, top + thickness));
                x += dash + gap;
            }
        }
    }

    // --- Making things ---

    /// A footnote at the caret: the reference goes in the text and the caret
    /// goes into the new note. Only in the body; the selection is replaced.
    pub fn insert_footnote(&mut self) -> bool {
        if self.compose.is_some() || self.new_marginal.is_some() || !self.active_story().is_body() {
            return false;
        }
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            edit::insert_footnote(document, &at)
        })
    }

    /// A page-number field at the caret, in any story; the selection is
    /// replaced.
    pub fn insert_page_number(&mut self) -> bool {
        if self.compose.is_some() {
            return false;
        }
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            Some(Selection::caret(edit::insert_page_number(
                document,
                &at,
                PageField::Page,
            )))
        })
    }
}
