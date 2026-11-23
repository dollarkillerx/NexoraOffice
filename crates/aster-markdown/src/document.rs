//! A text document: the text, where the selection is, the commands that change
//! it, and the way back.

use std::ops::Range;

use icu_segmenter::GraphemeClusterSegmenter;

use crate::history::{History, Kind};

/// A selection as two byte offsets into the text. `anchor` is where it started
/// and `focus` is the end that moves, which is also where the caret is drawn.
/// The two are equal when nothing is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub focus: usize,
}

impl Selection {
    pub fn caret(at: usize) -> Self {
        Self {
            anchor: at,
            focus: at,
        }
    }

    pub fn is_caret(self) -> bool {
        self.anchor == self.focus
    }

    /// The selected bytes, in text order whichever way the selection was made.
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// The edits a user can make. Moving the caret is not one of them: the view
/// does that, because it depends on layout, and reports the result with
/// [`Document::set_selection`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    /// Text typed on the keyboard. Runs of these undo together.
    Type(String),
    /// Text committed by an input method. Always one undo step.
    Commit(String),
    /// Text from the clipboard. Always one undo step. Line endings in it are
    /// turned into `\n`.
    Paste(String),
    /// The Return key.
    LineBreak,
    /// Remove the selection, or the character before the caret.
    Backspace,
    /// Remove the selection, or the character after the caret.
    Delete,
    /// Remove the selection and nothing else: cut, after the text was copied.
    DeleteSelection,
}

/// What an edit did, for the view to repeat on what it shows: the bytes in
/// `range` (offsets in the text as it was) became `inserted`, and the selection
/// is now `selection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub range: Range<usize>,
    pub inserted: String,
    pub selection: Selection,
}

/// Names a state of the text, so that a save running in the background can say
/// afterwards which state it wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavePoint(u64);

#[derive(Clone, Debug)]
pub struct Document {
    text: String,
    selection: Selection,
    history: History,
    /// The state the file on disk holds, if the text has been in that state.
    saved: Option<u64>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new("")
    }
}

/// The start of the grapheme cluster that ends at `at`.
fn previous_boundary(text: &str, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    // Clusters never span a line break, so the line is enough context.
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    if line_start == at {
        // The caret is at the start of a line: the break itself goes.
        return at - 1;
    }
    let line = &text[line_start..at];
    let last = GraphemeClusterSegmenter::new()
        .segment_str(line)
        .filter(|boundary| *boundary < line.len())
        .last()
        .unwrap_or(0);
    line_start + last
}

/// The end of the grapheme cluster that starts at `at`.
fn next_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    if text[at..].starts_with('\n') {
        return at + 1;
    }
    let line_end = text[at..].find('\n').map_or(text.len(), |index| at + index);
    let line = &text[at..line_end];
    let first = GraphemeClusterSegmenter::new()
        .segment_str(line)
        .find(|boundary| *boundary > 0)
        .unwrap_or(line.len());
    at + first
}

impl Document {
    /// A document holding `text`, with the caret at the start. Line endings
    /// must already be `\n`; [`crate::decode`] sees to that for files.
    pub fn new(text: &str) -> Self {
        let history = History::default();
        Self {
            text: text.to_owned(),
            selection: Selection::default(),
            saved: Some(history.state()),
            history,
        }
    }

    /// A document holding `text` that has never been saved in this state, such
    /// as one brought back from a recovery journal. It counts as modified until
    /// it is saved, and undoing cannot reach a saved state.
    pub fn unsaved(text: &str) -> Self {
        Self {
            saved: None,
            ..Self::new(text)
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.range()]
    }

    /// Move `offset` back to the nearest character boundary inside the text.
    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// Record where the view put the selection. Moving it ends the current run
    /// of typing: what is typed next is a new undo step.
    pub fn set_selection(&mut self, selection: Selection) {
        let selection = Selection {
            anchor: self.clamp(selection.anchor),
            focus: self.clamp(selection.focus),
        };
        if selection != self.selection {
            self.selection = selection;
            self.history.seal();
        }
    }

    pub fn select_all(&mut self) {
        self.set_selection(Selection {
            anchor: 0,
            focus: self.text.len(),
        });
    }

    /// Replace `range` with `inserted`, record it, and report it.
    fn replace(&mut self, kind: Kind, range: Range<usize>, inserted: &str) -> Change {
        let before = self.selection;
        let removed = self.text[range.clone()].to_owned();
        self.text.replace_range(range.clone(), inserted);
        self.selection = Selection::caret(range.start + inserted.len());
        self.history.record(
            kind,
            range.start,
            removed,
            inserted.to_owned(),
            before,
            self.selection,
        );
        Change {
            range,
            inserted: inserted.to_owned(),
            selection: self.selection,
        }
    }

    /// Carry out an edit. `None` if it changed nothing, such as Backspace at
    /// the start of the text.
    pub fn apply(&mut self, command: EditCommand) -> Option<Change> {
        let selected = self.selection.range();
        let caret = self.selection.focus;
        match command {
            EditCommand::Type(text) => {
                (!text.is_empty()).then(|| self.replace(Kind::Typing, selected, &text))
            }
            EditCommand::Commit(text) => {
                (!text.is_empty()).then(|| self.replace(Kind::Single, selected, &text))
            }
            EditCommand::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                (!text.is_empty()).then(|| self.replace(Kind::Single, selected, &text))
            }
            EditCommand::LineBreak => Some(self.replace(Kind::Single, selected, "\n")),
            EditCommand::Backspace if !selected.is_empty() => {
                Some(self.replace(Kind::Single, selected, ""))
            }
            EditCommand::Backspace => {
                let start = previous_boundary(&self.text, caret);
                (start < caret).then(|| self.replace(Kind::Backspace, start..caret, ""))
            }
            EditCommand::Delete if !selected.is_empty() => {
                Some(self.replace(Kind::Single, selected, ""))
            }
            EditCommand::Delete => {
                let end = next_boundary(&self.text, caret);
                (end > caret).then(|| self.replace(Kind::Delete, caret..end, ""))
            }
            EditCommand::DeleteSelection => {
                (!selected.is_empty()).then(|| self.replace(Kind::Single, selected, ""))
            }
        }
    }

    /// An input-method composition is starting. It types over the selection, so
    /// the selection goes now; the text that is eventually committed joins this
    /// removal as one undo step. `None` if nothing was selected.
    pub fn begin_composition(&mut self) -> Option<Change> {
        let selected = self.selection.range();
        (!selected.is_empty()).then(|| self.replace(Kind::CompositionStart, selected, ""))
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Take back the last step. The selection returns to where it was before
    /// that step.
    pub fn undo(&mut self) -> Option<Change> {
        let entry = self.history.pop_undo()?;
        let range = entry.start..entry.start + entry.inserted.len();
        self.text.replace_range(range.clone(), &entry.removed);
        self.selection = entry.before;
        Some(Change {
            range,
            inserted: entry.removed,
            selection: self.selection,
        })
    }

    /// Do the last undone step again.
    pub fn redo(&mut self) -> Option<Change> {
        let entry = self.history.pop_redo()?;
        let range = entry.start..entry.start + entry.removed.len();
        self.text.replace_range(range.clone(), &entry.inserted);
        self.selection = entry.after;
        Some(Change {
            range,
            inserted: entry.inserted,
            selection: self.selection,
        })
    }

    /// Whether the text differs from what was last saved. Undoing back to the
    /// saved state makes this false again.
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.history.state())
    }

    /// Name the current state, to pass to [`Self::mark_saved`] once the bytes
    /// written from it are safely on disk. Typing after this starts a new undo
    /// step, so the named state stays reachable.
    pub fn save_point(&mut self) -> SavePoint {
        self.history.seal();
        SavePoint(self.history.state())
    }

    /// The file on disk now holds the state `point` names. If the text has
    /// moved on since, the document stays modified.
    pub fn mark_saved(&mut self, point: SavePoint) {
        self.saved = Some(point.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(document: &mut Document, text: &str) {
        for c in text.chars() {
            document.apply(EditCommand::Type(c.to_string()));
        }
    }

    fn at_end(text: &str) -> Document {
        let mut document = Document::new(text);
        document.set_selection(Selection::caret(text.len()));
        document
    }

    #[test]
    fn a_new_document_is_unmodified_with_the_caret_at_the_start() {
        let document = Document::new("hello");
        assert_eq!(document.text(), "hello");
        assert_eq!(document.selection(), Selection::caret(0));
        assert!(!document.is_modified());
        assert!(!document.can_undo() && !document.can_redo());
    }

    #[test]
    fn typing_inserts_at_the_caret_and_reports_the_change() {
        let mut document = at_end("ab");
        let change = document.apply(EditCommand::Type("c".to_owned()));
        assert_eq!(
            change,
            Some(Change {
                range: 2..2,
                inserted: "c".to_owned(),
                selection: Selection::caret(3),
            })
        );
        assert_eq!(document.text(), "abc");
        assert!(document.is_modified());
    }

    #[test]
    fn typing_replaces_the_selection_whichever_way_it_was_made() {
        for (anchor, focus) in [(1, 4), (4, 1)] {
            let mut document = Document::new("hello");
            document.set_selection(Selection { anchor, focus });
            assert_eq!(document.selected_text(), "ell");
            let change = document.apply(EditCommand::Type("u".to_owned())).unwrap();
            assert_eq!(change.range, 1..4);
            assert_eq!(document.text(), "huo");
            assert_eq!(document.selection(), Selection::caret(2));
        }
    }

    #[test]
    fn a_run_of_typing_is_one_undo_step() {
        let mut document = at_end("");
        typed(&mut document, "hello");
        assert_eq!(document.history.depth(), 1);
        let change = document.undo().unwrap();
        assert_eq!(change.range, 0..5);
        assert_eq!(change.inserted, "");
        assert_eq!(document.text(), "");
        assert!(!document.can_undo());
        document.redo();
        assert_eq!(document.text(), "hello");
        assert_eq!(document.selection(), Selection::caret(5));
    }

    #[test]
    fn moving_the_caret_starts_a_new_undo_step() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        document.set_selection(Selection::caret(0));
        typed(&mut document, "xy");
        assert_eq!(document.text(), "xyab");
        document.undo();
        assert_eq!(document.text(), "ab");
        assert_eq!(document.selection(), Selection::caret(0));
        document.undo();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn setting_the_same_selection_does_not_break_the_run() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        document.set_selection(Selection::caret(2));
        typed(&mut document, "cd");
        assert_eq!(document.history.depth(), 1);
    }

    #[test]
    fn a_line_break_is_its_own_step() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        document.apply(EditCommand::LineBreak);
        typed(&mut document, "cd");
        assert_eq!(document.text(), "ab\ncd");
        assert_eq!(document.history.depth(), 3);
        document.undo();
        assert_eq!(document.text(), "ab\n");
        document.undo();
        assert_eq!(document.text(), "ab");
    }

    #[test]
    fn an_input_method_commit_is_always_one_step() {
        let mut document = at_end("");
        document.apply(EditCommand::Commit("你好".to_owned()));
        document.apply(EditCommand::Commit("世界".to_owned()));
        typed(&mut document, "ab");
        assert_eq!(document.text(), "你好世界ab");
        assert_eq!(document.history.depth(), 3);
        document.undo();
        assert_eq!(document.text(), "你好世界");
        document.undo();
        assert_eq!(document.text(), "你好");
        document.undo();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn typing_does_not_merge_into_a_commit_before_it() {
        let mut document = at_end("");
        typed(&mut document, "a");
        document.apply(EditCommand::Commit("日本語".to_owned()));
        assert_eq!(document.history.depth(), 2);
    }

    #[test]
    fn composing_over_a_selection_undoes_as_one_step() {
        let mut document = Document::new("hello world");
        document.set_selection(Selection {
            anchor: 6,
            focus: 11,
        });
        let removed = document.begin_composition().unwrap();
        assert_eq!(removed.range, 6..11);
        assert_eq!(document.text(), "hello ");
        document.apply(EditCommand::Commit("世界".to_owned()));
        assert_eq!(document.text(), "hello 世界");
        assert_eq!(document.history.depth(), 1);
        document.undo();
        assert_eq!(document.text(), "hello world");
        assert_eq!(
            document.selection(),
            Selection {
                anchor: 6,
                focus: 11
            }
        );
        document.redo();
        assert_eq!(document.text(), "hello 世界");
    }

    #[test]
    fn a_cancelled_composition_leaves_the_removal_as_its_own_step() {
        let mut document = Document::new("abc");
        document.set_selection(Selection {
            anchor: 0,
            focus: 3,
        });
        document.begin_composition();
        // The composition was cancelled; the user types something else.
        typed(&mut document, "x");
        assert_eq!(document.text(), "x");
        document.undo();
        assert_eq!(document.text(), "");
        document.undo();
        assert_eq!(document.text(), "abc");
    }

    #[test]
    fn beginning_a_composition_without_a_selection_changes_nothing() {
        let mut document = at_end("abc");
        assert_eq!(document.begin_composition(), None);
        assert!(!document.is_modified());
    }

    #[test]
    fn backspace_removes_whole_characters() {
        // A family emoji is seven code points; e with a combining accent is two.
        let mut document = at_end("a👨‍👩‍👧e\u{301}");
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "a👨‍👩‍👧");
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "a");
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "");
        assert_eq!(document.apply(EditCommand::Backspace), None);
    }

    #[test]
    fn backspace_handles_cjk_and_line_starts() {
        let mut document = at_end("中文\n日本");
        document.apply(EditCommand::Backspace);
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "中文\n");
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "中文");
        document.apply(EditCommand::Backspace);
        assert_eq!(document.text(), "中");
    }

    #[test]
    fn a_run_of_backspaces_is_one_undo_step() {
        let mut document = at_end("hello");
        for _ in 0..3 {
            document.apply(EditCommand::Backspace);
        }
        assert_eq!(document.text(), "he");
        assert_eq!(document.history.depth(), 1);
        let change = document.undo().unwrap();
        assert_eq!(change.inserted, "llo");
        assert_eq!(document.text(), "hello");
        assert_eq!(document.selection(), Selection::caret(5));
    }

    #[test]
    fn delete_removes_forwards_and_merges() {
        let mut document = Document::new("a👨‍👩‍👧b\nc");
        document.set_selection(Selection::caret(1));
        document.apply(EditCommand::Delete);
        assert_eq!(document.text(), "ab\nc");
        document.apply(EditCommand::Delete);
        document.apply(EditCommand::Delete);
        assert_eq!(document.text(), "ac");
        assert_eq!(document.history.depth(), 1);
        document.undo();
        assert_eq!(document.text(), "a👨‍👩‍👧b\nc");
        assert_eq!(document.selection(), Selection::caret(1));

        let mut end = at_end("x");
        assert_eq!(end.apply(EditCommand::Delete), None);
    }

    #[test]
    fn backspace_and_delete_remove_a_selection_as_one_step() {
        for command in [
            EditCommand::Backspace,
            EditCommand::Delete,
            EditCommand::DeleteSelection,
        ] {
            let mut document = Document::new("hello");
            document.set_selection(Selection {
                anchor: 1,
                focus: 4,
            });
            let change = document.apply(command).unwrap();
            assert_eq!(change.range, 1..4);
            assert_eq!(document.text(), "ho");
            document.undo();
            assert_eq!(document.text(), "hello");
            assert_eq!(
                document.selection(),
                Selection {
                    anchor: 1,
                    focus: 4
                }
            );
        }
        let mut caret = at_end("hello");
        assert_eq!(caret.apply(EditCommand::DeleteSelection), None);
    }

    #[test]
    fn paste_is_one_step_and_uses_lf() {
        let mut document = at_end("a");
        document.apply(EditCommand::Paste("x\r\ny\rz".to_owned()));
        assert_eq!(document.text(), "ax\ny\nz");
        typed(&mut document, "!");
        document.undo();
        document.undo();
        assert_eq!(document.text(), "a");
        assert_eq!(document.apply(EditCommand::Paste(String::new())), None);
    }

    #[test]
    fn a_new_edit_discards_what_was_undone() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        document.undo();
        assert!(document.can_redo());
        typed(&mut document, "c");
        assert!(!document.can_redo());
        assert_eq!(document.redo(), None);
        assert_eq!(document.text(), "c");
    }

    #[test]
    fn undo_and_redo_report_changes_a_view_can_replay() {
        // Apply every reported change to a plain string and compare.
        fn replay(mirror: &mut String, change: Option<Change>) {
            if let Some(change) = change {
                mirror.replace_range(change.range, &change.inserted);
            }
        }
        let mut document = at_end("start");
        let mut mirror = document.text().to_owned();
        let commands = [
            EditCommand::Type("a".to_owned()),
            EditCommand::Type("b".to_owned()),
            EditCommand::LineBreak,
            EditCommand::Commit("中文".to_owned()),
            EditCommand::Backspace,
            EditCommand::Paste("p\nq".to_owned()),
            EditCommand::Backspace,
            EditCommand::Backspace,
        ];
        for command in commands {
            let change = document.apply(command);
            replay(&mut mirror, change);
            assert_eq!(mirror, document.text());
        }
        while document.can_undo() {
            let change = document.undo();
            replay(&mut mirror, change);
            assert_eq!(mirror, document.text());
        }
        assert_eq!(mirror, "start");
        while document.can_redo() {
            let change = document.redo();
            replay(&mut mirror, change);
            assert_eq!(mirror, document.text());
        }
        assert_eq!(mirror, "startab\n中p");
    }

    #[test]
    fn undoing_back_to_the_saved_state_clears_the_modified_flag() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        let point = document.save_point();
        document.mark_saved(point);
        assert!(!document.is_modified());

        typed(&mut document, "cd");
        assert!(document.is_modified());
        // Typing after a save is a step of its own.
        document.undo();
        assert_eq!(document.text(), "ab");
        assert!(!document.is_modified());
        document.undo();
        assert!(document.is_modified());
        document.redo();
        assert!(!document.is_modified());
    }

    #[test]
    fn a_save_that_finishes_after_more_typing_leaves_the_document_modified() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        let point = document.save_point();
        typed(&mut document, "c");
        document.mark_saved(point);
        assert!(document.is_modified());
        document.undo();
        assert_eq!(document.text(), "ab");
        assert!(!document.is_modified());
    }

    #[test]
    fn a_saved_state_that_was_overwritten_never_comes_back() {
        let mut document = at_end("");
        typed(&mut document, "ab");
        let point = document.save_point();
        document.mark_saved(point);
        document.undo();
        typed(&mut document, "ab");
        // Same text, but not known to be what is on disk.
        assert_eq!(document.text(), "ab");
        assert!(document.is_modified());
    }

    #[test]
    fn a_recovered_document_stays_modified_until_it_is_saved() {
        let mut document = Document::unsaved("restored");
        assert!(document.is_modified());
        document.set_selection(Selection::caret(8));
        typed(&mut document, "!");
        document.undo();
        // Back to the restored text, which was never saved.
        assert!(document.is_modified());
        let point = document.save_point();
        document.mark_saved(point);
        assert!(!document.is_modified());
    }

    #[test]
    fn selections_are_kept_inside_the_text_and_on_character_boundaries() {
        let mut document = Document::new("中文");
        document.set_selection(Selection {
            anchor: 1,
            focus: 99,
        });
        assert_eq!(
            document.selection(),
            Selection {
                anchor: 0,
                focus: 6
            }
        );
        document.select_all();
        assert_eq!(document.selected_text(), "中文");
    }
}
