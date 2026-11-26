//! Tracked changes and comments in the view: showing them or not, listing
//! the comments and finding the one under a point, stepping from change to
//! change, accepting and rejecting.
//!
//! Changes are shown as Word shows them (see `aster_layout`): inserted text
//! underlined and deleted text struck through in a colour for each author,
//! a bar in the margin beside every line with a change, commented text on a
//! light background with a mark in the right margin. Switching markup off
//! lays the document out as if every change was accepted. The setting is the
//! view's, not the document's: it is not saved and undo does not touch it.
//!
//! Typing and deleting are never tracked. Accepting and rejecting are edits
//! like any other, one step of undo each.

use std::cell::RefCell;
use std::sync::Arc;

use aster_layout::Rect as PageRect;
use aster_writer::Pos;
use aster_writer::edit::{self, Selection};
use aster_writer::track::ChangeInfo;

use super::DocView;

/// A comment and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentInfo {
    /// What the document's markers refer to.
    pub id: i64,
    pub author: String,
    pub initials: String,
    /// As the file has it (`2024-05-01T09:30:00Z`).
    pub date: Option<String>,
    /// The comment's text, paragraphs on lines of their own.
    pub text: String,
    /// The text it is about: the two ends (equal when the text has gone).
    /// `None` for a comment no text refers to.
    pub range: Option<(Pos, Pos)>,
    /// The page the text starts on, and a rectangle of the first line of it
    /// on that page, in the page's points (a narrow one at the place when the
    /// range is empty). `None` when the text is not laid out on a page.
    pub page: Option<usize>,
    pub anchor: Option<PageRect>,
    /// The colour of the comment's author on the pages.
    pub color: [u8; 4],
}

/// What the view remembers between calls, for what is worked out from the
/// document: valid for one revision of it and one setting of markup.
#[derive(Default)]
pub(super) struct Review {
    cache: RefCell<Option<Cached>>,
}

struct Cached {
    revision: u64,
    markup: bool,
    comments: Arc<Vec<CommentInfo>>,
    changes: bool,
}

impl DocView {
    /// Tracked changes and comments are shown on the pages.
    pub fn show_markup(&self) -> bool {
        self.layouter.markup()
    }

    /// Show or hide tracked changes and comments. Hidden, the pages are
    /// those of the document with every change accepted. Returns whether it
    /// changed.
    pub fn set_show_markup(&mut self, on: bool) -> bool {
        if self.layouter.markup() == on {
            return false;
        }
        self.layouter.set_markup(on);
        let anchor = self.anchor();
        self.relayout();
        self.restore(anchor);
        self.shots.clear();
        self.band = None;
        self.clamp();
        self.touch();
        true
    }

    fn reviewed(&self) -> Arc<Vec<CommentInfo>> {
        self.review_cache().0
    }

    /// The comments and whether there are changes, worked out once for each
    /// revision of the document.
    fn review_cache(&self) -> (Arc<Vec<CommentInfo>>, bool) {
        let markup = self.layouter.markup();
        if let Some(c) = &*self.review.cache.borrow()
            && c.revision == self.revision
            && c.markup == markup
        {
            return (c.comments.clone(), c.changes);
        }
        let comments = Arc::new(self.collect_comments());
        let changes = self.document.has_changes();
        *self.review.cache.borrow_mut() = Some(Cached {
            revision: self.revision,
            markup,
            comments: comments.clone(),
            changes,
        });
        (comments, changes)
    }

    fn collect_comments(&self) -> Vec<CommentInfo> {
        let anchors = self.document.comment_anchors();
        let mut out: Vec<CommentInfo> = Vec::new();
        for anchor in &anchors {
            let Some(comment) = self.document.comment(anchor.id) else {
                continue;
            };
            let (page, rect) = self.anchor_rect(&anchor.start, &anchor.end);
            out.push(CommentInfo {
                id: comment.id,
                author: comment.author.clone(),
                initials: comment.initials.clone(),
                date: comment.date.clone(),
                text: comment.text(),
                range: Some((anchor.start.clone(), anchor.end.clone())),
                page,
                anchor: rect,
                color: self.layout.author_color(&comment.author),
            });
        }
        // Comments no text refers to come last.
        for comment in &self.document.comments {
            if out.iter().any(|c| c.id == comment.id) {
                continue;
            }
            out.push(CommentInfo {
                id: comment.id,
                author: comment.author.clone(),
                initials: comment.initials.clone(),
                date: comment.date.clone(),
                text: comment.text(),
                range: None,
                page: None,
                anchor: None,
                color: self.layout.author_color(&comment.author),
            });
        }
        out
    }

    /// The page the range starts on and its first rectangle there.
    fn anchor_rect(&self, start: &Pos, end: &Pos) -> (Option<usize>, Option<PageRect>) {
        if start != end
            && let Some((page, rect)) = self.layout.selection(start, end).into_iter().next()
        {
            return (Some(page), Some(rect));
        }
        match self.layout.caret(start) {
            Some(caret) => (
                Some(caret.page),
                Some(PageRect::new(
                    caret.x,
                    caret.top,
                    caret.x + 1.0,
                    caret.top + caret.height,
                )),
            ),
            None => (None, None),
        }
    }

    /// The document's comments in the order of their text, with where each
    /// is on the pages.
    pub fn comments(&self) -> Vec<CommentInfo> {
        (*self.reviewed()).clone()
    }

    /// The document has tracked changes (to accept or reject, and to step
    /// through).
    pub fn has_tracked_changes(&self) -> bool {
        self.review_cache().1
    }

    /// The comment whose text or mark in the margin is under a point of the
    /// window (physical pixels); of nested ranges the innermost.
    pub fn comment_at(&self, x: f64, y: f64) -> Option<i64> {
        if !self.is_over_page(x, y) {
            return None;
        }
        let (page, px, py) = self.locate(x, y);
        let comments = self.reviewed();
        let margin = self.layout.page_info(page).map(|info| info.margin);
        let mut found: Option<&CommentInfo> = None;
        for comment in comments.iter() {
            let Some((start, end)) = &comment.range else {
                continue;
            };
            let on_text = start != end
                && self
                    .layout
                    .selection(start, end)
                    .iter()
                    .any(|(p, rect)| *p == page && rect.contains(px, py));
            // The mark is a few points right of the margin, on the first line.
            let on_mark = comment.page == Some(page)
                && margin.is_some_and(|m| px >= m.x1)
                && comment
                    .anchor
                    .is_some_and(|a| py >= a.y0 - 2.0 && py <= a.y1 + 2.0);
            if on_text || on_mark {
                // A later start is nested inside an earlier one.
                if found
                    .and_then(|f| f.range.as_ref())
                    .is_none_or(|(s, _)| s <= start)
                {
                    found = Some(comment);
                }
            }
        }
        found.map(|c| c.id)
    }

    /// Select the text of a comment and scroll to it. A comment whose text
    /// has gone has the caret put where it was.
    pub fn go_to_comment(&mut self, id: i64) -> bool {
        let Some((start, end)) = self
            .reviewed()
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.range.clone())
        else {
            return false;
        };
        self.select_and_show(start, end)
    }

    /// Select the text between two places and bring it into view: the view
    /// scrolls only when the text is not all in the window already.
    fn select_and_show(&mut self, start: Pos, end: Pos) -> bool {
        if self.compose.is_none() && self.in_window(&start, &end) {
            return self.set_selection(start, end);
        }
        let moved = self.go_to(&start);
        let selected = self.set_selection(start, end);
        moved || selected
    }

    /// Whether the lines from `start` to `end` are wholly in the window.
    fn in_window(&self, start: &Pos, end: &Pos) -> bool {
        let rows = |pos: &Pos| {
            let caret = self
                .layout
                .caret_on(self.page_for(pos.path.story), pos)
                .or_else(|| self.layout.caret(pos))?;
            let top =
                self.page_rect(caret.page).y0 + f64::from(caret.top) * self.pixels_per_point();
            Some((top, top + f64::from(caret.height) * self.pixels_per_point()))
        };
        match (rows(start), rows(end)) {
            (Some((top, _)), Some((_, bottom))) => top >= 0.0 && bottom <= f64::from(self.height),
            _ => false,
        }
    }

    /// Every change in reading order.
    pub fn changes(&self) -> Vec<ChangeInfo> {
        self.document.changes()
    }

    /// Select the next change after the selection, and scroll to it; after
    /// the last, the first. `false` when there are no changes.
    pub fn next_change(&mut self) -> bool {
        self.step_change(true)
    }

    /// The change before the selection; before the first, the last.
    pub fn previous_change(&mut self) -> bool {
        self.step_change(false)
    }

    fn step_change(&mut self, forward: bool) -> bool {
        let changes = self.document.changes();
        let Some(sel) = self.selection_value() else {
            return false;
        };
        let (from, to) = sel.ordered();
        // After (or before) the selection as a whole: by where the change
        // starts, then where it ends.
        let key = |c: &ChangeInfo| (c.start.clone(), c.end.clone());
        let now = (from, to);
        let found = if forward {
            changes.iter().find(|c| key(c) > now).or(changes.first())
        } else {
            changes
                .iter()
                .rev()
                .find(|c| key(c) < now)
                .or(changes.last())
        };
        let Some((start, end)) = found.map(key) else {
            return false;
        };
        self.select_and_show(start, end)
    }

    /// Accept the changes the selection touches (those at the caret when
    /// there is no selection). One step of undo. `false` when there was
    /// nothing to accept.
    pub fn accept_changes(&mut self) -> bool {
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            (edit::accept_in(document, &from, &to) > 0).then(|| selection.clone())
        })
    }

    /// Reject the changes the selection touches. A change of formatting
    /// cannot be rejected: it stays as it is.
    pub fn reject_changes(&mut self) -> bool {
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            (edit::reject_in(document, &from, &to) > 0).then(|| selection.clone())
        })
    }

    /// Accept every change in the document.
    pub fn accept_all_changes(&mut self) -> bool {
        self.apply(None, true, |document, selection| {
            (edit::accept_all(document) > 0).then(|| Selection::caret(selection.focus.clone()))
        })
    }

    /// Reject every change in the document.
    pub fn reject_all_changes(&mut self) -> bool {
        self.apply(None, true, |document, selection| {
            (edit::reject_all(document) > 0).then(|| Selection::caret(selection.focus.clone()))
        })
    }
}
