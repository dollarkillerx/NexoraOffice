//! Changing the document through the view: typing, Enter, deleting, Tab,
//! cut, copy and paste, undo and redo, and replace. Every change goes
//! through [`DocView::apply`]: the operation runs on a copy of the document,
//! the step is noted for undo, and the pages are laid out again.

use std::time::{Duration, Instant};

use aster_writer::edit::{self, Selection, StepKind};
use aster_writer::{Block, Break, Document, Path, Pos};

use super::compose::with_paragraph;
use super::text::{
    floor_boundary, next_cluster, previous_cluster, sanitized, word_after, word_before,
};
use super::{DocView, Sel};

/// A pause this long ends a step of typing.
const PAUSE: Duration = Duration::from_millis(1500);

/// How far one deletion goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A grapheme cluster.
    Cluster,
    Word,
    /// To the start (Backspace) or end (Delete) of the line.
    Line,
}

impl DocView {
    // --- State ---

    /// The document has changes since it was read or last saved.
    pub fn is_modified(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Bumped by every change to the document, undo and redo included.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What is in the document now is what the file holds.
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// The document counts as changed (it came back from a recovery record).
    pub fn mark_unsaved(&mut self) {
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub(super) fn selection_value(&self) -> Option<Selection> {
        self.sel
            .as_ref()
            .map(|sel| Selection::new(sel.anchor.clone(), sel.focus.clone()))
    }

    /// Make sure there is a paragraph to put the caret in: a file can have
    /// a body with none. Returns whether there is a caret afterwards.
    pub(super) fn ensure_paragraph(&mut self) -> bool {
        if self.sel.is_some() {
            return true;
        }
        if with_paragraph(&mut self.document) {
            self.relayout();
            self.sel = self.document.start().map(|start| Sel {
                anchor: start.clone(),
                focus: start,
            });
            self.touch();
        }
        self.sel.is_some()
    }

    /// Carry out an edit. `edit` changes a copy of the document and says
    /// where the selection goes, or `None` when it changed nothing. With
    /// `check`, an edit that leaves the document as it was is not a step.
    pub(super) fn apply(
        &mut self,
        kind: Option<StepKind>,
        check: bool,
        edit: impl FnOnce(&mut Document, &Selection) -> Option<Selection>,
    ) -> bool {
        if self.compose.is_some() || !self.ensure_paragraph() {
            return false;
        }
        let Some(selection) = self.selection_value() else {
            return false;
        };
        let before = self.document.clone();
        let mut document = before.clone();
        let Some(after) = edit(&mut document, &selection) else {
            return false;
        };
        if check && document == before {
            return false;
        }
        if self.last_edit.is_some_and(|at| at.elapsed() > PAUSE) {
            self.history.seal();
        }
        self.history.record(&before, &selection, &after, kind);
        self.document = document;
        self.last_edit = Some(Instant::now());
        self.revision += 1;
        self.pending = None;
        self.goal_x = None;
        let fixed = (
            sanitized(&self.document, after.anchor),
            sanitized(&self.document, after.focus),
        );
        self.sel = match fixed {
            (Some(anchor), Some(focus)) => Some(Sel { anchor, focus }),
            _ => self.document.start().map(|start| Sel {
                anchor: start.clone(),
                focus: start,
            }),
        };
        self.relayout();
        self.press = None;
        self.touch();
        self.reveal_caret();
        true
    }

    /// The caret after the selection is taken out (a caret stays).
    fn clear_range(document: &mut Document, selection: &Selection) -> Pos {
        let (from, to) = selection.ordered();
        if selection.is_caret() {
            from
        } else {
            edit::delete(document, &from, &to)
        }
    }

    // --- Typing ---

    /// Type text: it replaces the selection. With only a caret it takes
    /// the look chosen for it, if any. Returns whether the document changed.
    pub fn type_text(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let pending = self.pending.clone();
        let kind = if self.has_selection() {
            None
        } else {
            Some(StepKind::Typing)
        };
        self.apply(kind, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            let look = pending.map(|patch| {
                let mut look = document
                    .paragraph(&at.path)
                    .map(|p| p.look_at(at.offset).clone())
                    .unwrap_or_default();
                patch.apply(&mut look);
                look
            });
            let end = edit::insert_text(document, &at, text, look.as_ref());
            Some(Selection::caret(end))
        })
    }

    /// Enter: the selection goes and the paragraph is split.
    pub fn enter(&mut self) -> bool {
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            Some(Selection::caret(edit::split_paragraph(document, &at)))
        })
    }

    /// A line, page or column break.
    pub fn insert_break(&mut self, kind: Break) -> bool {
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            Some(Selection::caret(edit::insert_break(document, &at, kind)))
        })
    }

    /// Backspace: the selection if there is one, else back by `unit`; at
    /// the start of a paragraph it leaves its list or joins the paragraph
    /// before.
    pub fn backspace(&mut self, unit: Unit) -> bool {
        let Some(sel) = self.sel.clone() else {
            return false;
        };
        let focus = sel.focus.clone();
        let target = if sel.anchor != sel.focus {
            None
        } else {
            let text = self
                .document
                .paragraph(&focus.path)
                .map(|p| p.text())
                .unwrap_or_default();
            let at = floor_boundary(&text, focus.offset);
            match unit {
                Unit::Cluster if at > 0 => Some(previous_cluster(&text, at)),
                Unit::Word => word_before(&text, at),
                Unit::Line => self
                    .layout
                    .line_start(&focus)
                    .filter(|start| start.path == focus.path && start.offset < at)
                    .map(|start| start.offset),
                _ => None,
            }
        };
        let kind = (unit == Unit::Cluster && target.is_some()).then_some(StepKind::DeleteBackward);
        self.apply(kind, false, |document, selection| {
            if !selection.is_caret() {
                return Some(Selection::caret(Self::clear_range(document, selection)));
            }
            let at = selection.focus.clone();
            let pos = match target {
                Some(offset) => edit::delete(document, &Pos::new(at.path.clone(), offset), &at),
                None => edit::delete_backward(document, &at),
            };
            Some(Selection::caret(pos))
        })
    }

    /// Delete: the selection, else forward by `unit`; at the end of a
    /// paragraph the next one is joined.
    pub fn delete_forward(&mut self, unit: Unit) -> bool {
        let Some(sel) = self.sel.clone() else {
            return false;
        };
        let focus = sel.focus.clone();
        let target = if sel.anchor != sel.focus {
            None
        } else {
            let text = self
                .document
                .paragraph(&focus.path)
                .map(|p| p.text())
                .unwrap_or_default();
            let at = floor_boundary(&text, focus.offset);
            match unit {
                Unit::Cluster if at < text.len() => Some(next_cluster(&text, at)),
                Unit::Word => word_after(&text, at),
                Unit::Line => self
                    .layout
                    .line_end(&focus)
                    .filter(|end| end.path == focus.path && end.offset > at)
                    .map(|end| end.offset),
                _ => None,
            }
        };
        let kind = (unit == Unit::Cluster && target.is_some()).then_some(StepKind::DeleteForward);
        self.apply(kind, false, |document, selection| {
            if !selection.is_caret() {
                return Some(Selection::caret(Self::clear_range(document, selection)));
            }
            let at = selection.focus.clone();
            match target {
                Some(offset) => {
                    edit::delete(document, &at, &Pos::new(at.path.clone(), offset));
                }
                None => {
                    edit::delete_forward(document, &at);
                }
            }
            Some(Selection::caret(at))
        })
    }

    /// Tab: in a list item at its start the item goes a level in (Shift
    /// out); in a table to the next cell (the previous with Shift); else a
    /// tab character (Shift does nothing).
    pub fn tab(&mut self, shift: bool) -> bool {
        let Some(sel) = self.sel.clone() else {
            return false;
        };
        if self.compose.is_some() {
            return false;
        }
        let focus = sel.focus.clone();
        let at_start_of_item = sel.anchor == sel.focus
            && focus.offset == 0
            && self
                .document
                .paragraph(&focus.path)
                .is_some_and(|p| p.props.list.is_some());
        if at_start_of_item {
            let delta = if shift { -1 } else { 1 };
            return self.apply(None, true, |document, selection| {
                let (from, to) = selection.ordered();
                edit::change_level(document, &from, &to, delta);
                Some(selection.clone())
            });
        }
        if focus.path.in_cell().is_some() {
            return match self.cell_neighbour(&focus.path, !shift) {
                Some(target) => self.set_selection(target.clone(), target),
                None => false,
            };
        }
        if shift {
            return false;
        }
        self.type_text("\t")
    }

    /// The start of the paragraph in the cell after (or before) the one
    /// `path` is in, in reading order; merged-over cells are skipped.
    fn cell_neighbour(&self, path: &Path, forward: bool) -> Option<Pos> {
        let (table_path, row, cell, _) = path.in_cell()?;
        let table = self.document.block(&table_path)?.as_table()?;
        let mut cells = Vec::new();
        for (r, row) in table.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                if cell.merge != aster_writer::VMerge::Continue {
                    cells.push((r, c));
                }
            }
        }
        let here = cells.iter().position(|&rc| rc == (row, cell))?;
        let next = if forward {
            here.checked_add(1)?
        } else {
            here.checked_sub(1)?
        };
        let &(r, c) = cells.get(next)?;
        let target = &table.rows[r].cells[c];
        let (inner, _) = aster_writer::paragraphs_of(&target.blocks)
            .into_iter()
            .next()?;
        let mut steps = table_path.0.clone();
        steps.extend([r as u32, c as u32]);
        steps.extend(inner.0);
        Some(Pos::new(Path(steps), 0))
    }

    // --- Clipboard ---

    /// What a copy keeps besides the text: the selected paragraphs with
    /// their formatting, for pasting into a Writer document.
    pub fn selected_fragment(&self) -> Option<Vec<Block>> {
        let (a, b) = self.selection()?;
        let blocks = edit::fragment(&self.document, a, b);
        (!blocks.is_empty()).then_some(blocks)
    }

    /// Take the selection out and give it: its text and its fragment.
    pub fn cut(&mut self) -> Option<(String, Option<Vec<Block>>)> {
        if !self.has_selection() {
            return None;
        }
        let text = self.selected_text();
        let fragment = self.selected_fragment();
        self.apply(None, false, |document, selection| {
            Some(Selection::caret(Self::clear_range(document, selection)))
        });
        Some((text, fragment))
    }

    /// Paste plain text: it replaces the selection; line breaks start
    /// paragraphs, tabs are tabs.
    pub fn paste_text(&mut self, text: &str) -> bool {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return false;
        }
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            Some(Selection::caret(edit::insert_text(
                document, &at, &text, None,
            )))
        })
    }

    /// Paste what [`DocView::selected_fragment`] gave, formatting and all.
    pub fn paste_fragment(&mut self, blocks: &[Block]) -> bool {
        if blocks.is_empty() {
            return false;
        }
        self.apply(None, false, |document, selection| {
            let at = Self::clear_range(document, selection);
            Some(Selection::caret(edit::insert_fragment(
                document, &at, blocks,
            )))
        })
    }

    // --- Undo ---

    pub fn undo(&mut self) -> bool {
        if self.compose.is_some() {
            return false;
        }
        match self.history.undo(&self.document) {
            Some((document, selection)) => {
                self.restore_step(document, selection);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.compose.is_some() {
            return false;
        }
        match self.history.redo(&self.document) {
            Some((document, selection)) => {
                self.restore_step(document, selection);
                true
            }
            None => false,
        }
    }

    fn restore_step(&mut self, document: Document, selection: Selection) {
        self.document = document;
        self.revision += 1;
        self.pending = None;
        self.goal_x = None;
        let fix = |pos: Pos| sanitized(&self.document, pos);
        self.sel = match (fix(selection.anchor), fix(selection.focus)) {
            (Some(anchor), Some(focus)) => Some(Sel { anchor, focus }),
            _ => self.document.start().map(|start| Sel {
                anchor: start.clone(),
                focus: start,
            }),
        };
        self.find = None;
        self.relayout();
        self.press = None;
        self.touch();
        self.reveal_caret();
    }

    /// End the step of typing that is going on: the next typed text is a
    /// step of its own.
    pub fn seal_history(&mut self) {
        self.history.seal();
    }

    // --- Replace ---

    /// Replace the current match of the search with `with`. One step of
    /// undo. The search is made again afterwards.
    pub fn replace_current(&mut self, with: &str) -> bool {
        let Some((start, end)) = self
            .current_hit()
            .and_then(|i| self.hit(i))
            .map(|(a, b)| (a.clone(), b.clone()))
        else {
            return false;
        };
        let query = self.find_query().cloned();
        let with = with.replace(['\n', '\r'], " ");
        let done = self.apply(None, false, |document, _| {
            let look = look_for(document, &start);
            edit::delete(document, &start, &end);
            let at = edit::insert_text(document, &start, &with, look.as_ref());
            Some(Selection::caret(at))
        });
        if done && let Some(query) = query {
            self.set_find(query);
            if let Some(next) = self.current_hit() {
                self.go_to_hit(next);
            }
        }
        done
    }

    /// Replace every match with `with`, as one step of undo. Returns how
    /// many there were.
    pub fn replace_all(&mut self, with: &str) -> usize {
        let matches: Vec<(Pos, Pos)> = (0..self.hit_count())
            .filter_map(|i| self.hit(i).map(|(a, b)| (a.clone(), b.clone())))
            .collect();
        if matches.is_empty() {
            return 0;
        }
        let query = self.find_query().cloned();
        let with = with.replace(['\n', '\r'], " ");
        let count = matches.len();
        let done = self.apply(None, false, |document, selection| {
            // From the last to the first: places before are not moved.
            for (start, end) in matches.iter().rev() {
                let look = look_for(document, start);
                edit::delete(document, start, end);
                edit::insert_text(document, start, &with, look.as_ref());
            }
            Some(Selection::caret(selection.focus.clone()))
        });
        if let Some(query) = query {
            self.set_find(query);
        }
        if done { count } else { 0 }
    }
}

/// The look of the first character of the match at `start`, so that the
/// replacement looks like what it replaces.
fn look_for(document: &Document, start: &Pos) -> Option<aster_writer::RunLook> {
    let paragraph = document.paragraph(&start.path)?;
    let text = paragraph.text();
    let at = floor_boundary(&text, start.offset);
    let first = text[at..].chars().next().map_or(0, char::len_utf8);
    Some(paragraph.look_at(at + first).clone())
}
