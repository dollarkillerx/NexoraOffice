//! What a tab holds: the view of a Writer tab, or the editor of a Markdown tab.
//!
//! This is the binary's side of the crate: it joins the document model from
//! `aster-markdown` to the text surface from `aster-surface`. Neither of those
//! knows about the other.

use std::path::PathBuf;

use aster_markdown::{Change, Document, EditCommand, SavePoint, Selection, TextFormat};
use aster_shell::stats::TextStats;
use aster_shell::theme;
use aster_storage::{Fingerprint, RecordKey};
use aster_surface::{Action, ImeEvent, PageLayout, Palette, TextSurface};

/// Body text size of the Markdown editor, in logical pixels.
const MARKDOWN_FONT_SIZE: f32 = 15.0;

pub fn palette(tokens: &theme::Tokens) -> Palette {
    let c = &tokens.colors;
    Palette {
        desk: c.desk.to_array(),
        page: c.page.to_array(),
        page_edge: c.page_edge.to_array(),
        ink: c.ink.to_array(),
        selection: c.selection.to_array(),
        caret: c.caret.to_array(),
    }
}

/// The sheet of paper a Writer tab shows.
pub fn page_layout(tokens: &theme::Tokens) -> PageLayout {
    let page = &tokens.page;
    PageLayout {
        margin: f64::from(page.desk_margin),
        padding: f64::from(page.padding),
        max_width: Some(f64::from(page.width)),
        min_aspect: Some(f64::from(page.aspect)),
    }
}

/// A Markdown tab has no sheet of paper: the text sits on the window surface,
/// in a column that stops growing where lines would get too long to read.
fn editor_palette(tokens: &theme::Tokens) -> Palette {
    let c = &tokens.colors;
    Palette {
        desk: c.surface.to_array(),
        page: c.surface.to_array(),
        page_edge: [0, 0, 0, 0],
        ..palette(tokens)
    }
}

fn editor_layout(tokens: &theme::Tokens) -> PageLayout {
    let m = &tokens.metrics;
    PageLayout {
        margin: 0.0,
        padding: f64::from(m.space_xl) * 2.0,
        max_width: Some(f64::from(tokens.page.width) + 4.0 * f64::from(m.space_xl)),
        min_aspect: None,
    }
}

/// A Writer tab: the plain-text surface on its own, with no document model, no
/// undo and no file behind it.
pub struct WriterView {
    pub surface: TextSurface,
    pub zoom_percent: u32,
    pub stats: TextStats,
}

impl WriterView {
    pub fn new(text: &str, tokens: &theme::Tokens) -> Self {
        let mut surface = TextSurface::new(text);
        surface.set_palette(palette(tokens));
        surface.set_page_layout(page_layout(tokens));
        Self {
            stats: TextStats::of(text),
            surface,
            zoom_percent: 100,
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.surface.set_palette(palette(tokens));
        self.surface.set_page_layout(page_layout(tokens));
    }
}

/// A Markdown tab. The document owns the text, the selection and the undo
/// history; the surface shows them, and answers what only layout can: where
/// the caret goes when it moves, what is under the pointer.
///
/// Every edit goes to the document first and is then repeated on the surface,
/// so the two never disagree about the text.
pub struct MarkdownEditor {
    pub document: Document,
    pub surface: TextSurface,
    /// The file the document is saved in. `None` until it has been saved once.
    pub path: Option<PathBuf>,
    /// Line endings and byte-order mark to write the file with.
    pub format: TextFormat,
    pub stats: TextStats,
    /// A save is running in the background.
    pub saving: bool,
    /// What the file held when it was last read or written. Stored in the
    /// recovery record, so a restored document can tell whether another
    /// program changed the file in the meantime.
    pub disk: Option<Fingerprint>,
    /// Set on a restored document whose file changed or vanished while it was
    /// not open. Saving then asks where to, instead of overwriting the file.
    pub disk_warning: Option<DiskWarning>,
    /// The document's recovery record, while it has unsaved changes.
    pub record: Option<RecordKey>,
    /// Why the recovery record could not be written, if it could not.
    pub record_failed: Option<String>,
    /// Changes not yet handed to the recovery writer, encoded.
    unjournaled: Vec<Vec<u8>>,
    /// Entries and bytes written to the record since its last checkpoint.
    since_checkpoint: (u64, usize),
}

/// What became of a restored document's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskWarning {
    /// Another program changed it, or it could not be read.
    Changed,
    Missing,
}

/// What the recovery record of a document needs next. See
/// [`MarkdownEditor::journal_step`].
#[derive(Debug, PartialEq, Eq)]
pub enum JournalStep {
    Nothing,
    /// Start a record holding this whole text.
    Open(String),
    /// Add these entries; then, if given, checkpoint this whole text.
    Append(Vec<Vec<u8>>, Option<String>),
    /// The document has nothing unsaved any more: delete the record.
    Discard,
}

/// A record is checkpointed after this many entries...
const CHECKPOINT_ENTRIES: u64 = 256;
/// ...or this many bytes of entries, whichever comes first. A checkpoint
/// copies the whole text, so it is not done on every key press; a replay after
/// a crash then has at most this much to apply.
const CHECKPOINT_BYTES: usize = 256 * 1024;

impl MarkdownEditor {
    /// A document as read from `path`, or a new one when `path` is `None`.
    pub fn new(
        text: &str,
        format: TextFormat,
        path: Option<PathBuf>,
        disk: Option<Fingerprint>,
        tokens: &theme::Tokens,
    ) -> Self {
        let mut surface = TextSurface::new(text);
        surface.set_font_size(MARKDOWN_FONT_SIZE);
        surface.set_palette(editor_palette(tokens));
        surface.set_page_layout(editor_layout(tokens));
        // A file opens at its beginning.
        surface.set_selection(0, 0);
        Self {
            document: Document::new(text),
            surface,
            path,
            format,
            stats: TextStats::of(text),
            saving: false,
            disk,
            disk_warning: None,
            record: None,
            record_failed: None,
            unjournaled: Vec::new(),
            since_checkpoint: (0, 0),
        }
    }

    /// A document restored from a recovery record. It has unsaved changes from
    /// the start: what was recovered has never been saved.
    pub fn recovered(
        text: &str,
        format: TextFormat,
        path: Option<PathBuf>,
        disk: Option<Fingerprint>,
        disk_warning: Option<DiskWarning>,
        tokens: &theme::Tokens,
    ) -> Self {
        Self {
            document: Document::unsaved(text),
            disk_warning,
            ..Self::new(text, format, path, disk, tokens)
        }
    }

    /// Decide what the recovery record needs after the edits since the last
    /// call, and forget those edits. The caller carries the step out; this
    /// keeps `record` in step with it.
    ///
    /// A record exists exactly while the document has unsaved changes. It
    /// starts with the whole text, so the edits that made the document
    /// modified are already in it; it is deleted when the document is saved
    /// or undone back to what is on disk.
    pub fn journal_step(&mut self, new_key: impl FnOnce() -> RecordKey) -> JournalStep {
        let entries = std::mem::take(&mut self.unjournaled);
        match (self.record, self.document.is_modified()) {
            (None, false) => JournalStep::Nothing,
            (None, true) => {
                self.record = Some(new_key());
                self.since_checkpoint = (0, 0);
                JournalStep::Open(self.document.text().to_owned())
            }
            (Some(_), false) => {
                self.record = None;
                self.record_failed = None;
                JournalStep::Discard
            }
            (Some(_), true) if entries.is_empty() => JournalStep::Nothing,
            (Some(_), true) => {
                let (count, bytes) = &mut self.since_checkpoint;
                *count += entries.len() as u64;
                *bytes += entries.iter().map(Vec::len).sum::<usize>();
                let checkpoint = (*count >= CHECKPOINT_ENTRIES || *bytes >= CHECKPOINT_BYTES)
                    .then(|| self.document.text().to_owned());
                if checkpoint.is_some() {
                    self.since_checkpoint = (0, 0);
                }
                JournalStep::Append(entries, checkpoint)
            }
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.surface.set_palette(editor_palette(tokens));
        self.surface.set_page_layout(editor_layout(tokens));
    }

    /// Repeat on the surface an edit the document has made.
    fn show(&mut self, change: Option<Change>) -> bool {
        let Some(change) = change else {
            return false;
        };
        self.unjournaled
            .push(aster_markdown::journal::encode(&change));
        self.surface.replace_range(change.range, &change.inserted);
        self.surface
            .set_selection(change.selection.anchor, change.selection.focus);
        self.surface.reveal_caret();
        self.stats = TextStats::of(self.document.text());
        true
    }

    /// Tell the document where the surface put the selection.
    fn adopt_selection(&mut self) {
        let (anchor, focus) = self.surface.selection();
        self.document.set_selection(Selection { anchor, focus });
    }

    /// Carry out an edit. Returns whether the text changed. While the input
    /// method is composing it owns the keyboard, and edits are ignored.
    pub fn edit(&mut self, command: EditCommand) -> bool {
        if self.surface.is_composing() {
            return false;
        }
        let change = self.document.apply(command);
        self.show(change)
    }

    /// Carry out what a key press decoded to. Returns whether the text changed.
    pub fn key(&mut self, action: Action) -> bool {
        match action {
            Action::Insert(text) => self.edit(EditCommand::Type(text)),
            Action::Newline => self.edit(EditCommand::LineBreak),
            Action::Backspace => self.edit(EditCommand::Backspace),
            Action::Delete => self.edit(EditCommand::Delete),
            movement => {
                // Only layout knows where "up" or "end of line" is.
                self.surface.apply(movement);
                self.adopt_selection();
                false
            }
        }
    }

    pub fn pointer_down(&mut self, x: f64, y: f64, extend: bool) {
        self.surface.pointer_down(x, y, extend);
        if !self.surface.is_composing() {
            self.adopt_selection();
        }
    }

    pub fn pointer_drag(&mut self, x: f64, y: f64) {
        self.surface.pointer_drag(x, y);
        if !self.surface.is_composing() {
            self.adopt_selection();
        }
    }

    /// Handle an input-method event. Returns whether the text changed.
    ///
    /// Composition text lives on the surface only. The document hears of it
    /// once, when it is committed, so a whole composition is one undo step and
    /// nothing typed on the way to it is ever in the history.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        match event {
            ImeEvent::Enabled => false,
            ImeEvent::Preedit { text, cursor } => {
                let mut changed = false;
                if !text.is_empty() && !self.surface.is_composing() {
                    // The composition types over the selection.
                    let removed = self.document.begin_composition();
                    changed = self.show(removed);
                }
                self.surface.compose(&text, cursor);
                changed
            }
            ImeEvent::Commit(text) => {
                // Some platforms commit without first sending an empty preedit.
                self.surface.end_composition();
                self.edit(EditCommand::Commit(text))
            }
            ImeEvent::Disabled => {
                self.surface.end_composition();
                false
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.surface.is_composing() {
            return false;
        }
        let change = self.document.undo();
        self.show(change)
    }

    pub fn redo(&mut self) -> bool {
        if self.surface.is_composing() {
            return false;
        }
        let change = self.document.redo();
        self.show(change)
    }

    pub fn select_all(&mut self) {
        if self.surface.is_composing() {
            return;
        }
        self.document.select_all();
        let selection = self.document.selection();
        self.surface
            .set_selection(selection.anchor, selection.focus);
    }

    /// The selected text, for the clipboard. `None` if nothing is selected.
    pub fn copy(&self) -> Option<String> {
        let text = self.document.selected_text();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// Remove the selected text and return it for the clipboard.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy()?;
        self.edit(EditCommand::DeleteSelection).then_some(text)
    }

    pub fn paste(&mut self, text: &str) -> bool {
        self.edit(EditCommand::Paste(text.to_owned()))
    }

    /// The text to write to disk, and the name of the state it is.
    pub fn snapshot(&mut self) -> (String, SavePoint) {
        (self.document.text().to_owned(), self.document.save_point())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_shell::theme::ThemeMode;

    fn editor(text: &str) -> MarkdownEditor {
        let tokens = theme::Tokens::of(ThemeMode::Light);
        let mut editor = MarkdownEditor::new(text, TextFormat::default(), None, None, &tokens);
        editor.surface.resize(1200, 600, 2.0);
        editor
    }

    /// The invariant everything rests on.
    fn assert_in_step(editor: &MarkdownEditor) {
        assert_eq!(editor.surface.text(), editor.document.text());
        if !editor.surface.is_composing() {
            let selection = editor.document.selection();
            assert_eq!(
                editor.surface.selection(),
                (selection.anchor, selection.focus)
            );
        }
    }

    fn type_text(editor: &mut MarkdownEditor, text: &str) {
        for c in text.chars() {
            editor.key(Action::Insert(c.to_string()));
            assert_in_step(editor);
        }
    }

    fn preedit(text: &str) -> ImeEvent {
        ImeEvent::Preedit {
            text: text.to_owned(),
            cursor: Some((text.len(), text.len())),
        }
    }

    const END: Action = Action::LineEnd { select: false };

    #[test]
    fn a_file_opens_at_its_beginning_unmodified() {
        let editor = editor("# Title\n\nBody\n");
        assert_in_step(&editor);
        assert_eq!(editor.document.selection(), Selection::caret(0));
        assert!(!editor.document.is_modified());
        assert_eq!(editor.stats.words, 2);
    }

    #[test]
    fn typing_and_keys_keep_document_and_surface_in_step() {
        let mut editor = editor("");
        type_text(&mut editor, "hello");
        editor.key(Action::Newline);
        type_text(&mut editor, "world");
        assert_eq!(editor.document.text(), "hello\nworld");
        editor.key(Action::Backspace);
        editor.key(Action::Up { select: false });
        assert_in_step(&editor);
        editor.key(Action::LineStart { select: false });
        editor.key(Action::Delete);
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ello\nworl");
        assert!(editor.document.is_modified());
        assert_eq!(editor.stats.words, 2);
    }

    #[test]
    fn selecting_with_shift_then_typing_replaces_the_selection() {
        let mut editor = editor("hello world");
        editor.key(END);
        for _ in 0..5 {
            editor.key(Action::Left {
                select: true,
                word: false,
            });
        }
        assert_in_step(&editor);
        assert_eq!(editor.copy().as_deref(), Some("world"));
        type_text(&mut editor, "X");
        assert_eq!(editor.document.text(), "hello X");
    }

    #[test]
    fn pointer_selection_reaches_the_document() {
        let mut editor = editor("hello world, this is a line of text");
        editor.pointer_down(120.0, 110.0, false);
        editor.pointer_drag(360.0, 110.0);
        assert_in_step(&editor);
        let selection = editor.document.selection();
        assert!(!selection.is_caret(), "the drag selected nothing");
        assert_eq!(
            editor.copy().as_deref(),
            Some(editor.document.selected_text())
        );
    }

    #[test]
    fn undo_and_redo_restore_text_and_selection_on_both_sides() {
        let mut editor = editor("");
        type_text(&mut editor, "one");
        editor.key(Action::Newline);
        type_text(&mut editor, "two");
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "one\n");
        assert!(editor.undo());
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert!(editor.redo());
        assert!(editor.redo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "one\ntwo");
        assert!(!editor.redo());
    }

    #[test]
    fn a_composition_is_invisible_to_the_document_until_it_commits() {
        let mut editor = editor("ab");
        editor.key(END);
        assert!(!editor.ime(preedit("n")));
        assert!(!editor.ime(preedit("ni")));
        assert!(!editor.ime(preedit("nihao")));
        assert!(editor.surface.is_composing());
        assert_eq!(editor.document.text(), "ab");
        assert!(!editor.document.is_modified());
        assert!(!editor.document.can_undo());

        // Clearing the preedit changes no committed text.
        assert!(!editor.ime(ImeEvent::Preedit {
            text: String::new(),
            cursor: None,
        }));
        assert!(editor.ime(ImeEvent::Commit("你好".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ab你好");

        // One step back takes the whole commit, and nothing else.
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "ab");
        assert!(!editor.document.can_undo());
    }

    #[test]
    fn a_commit_without_a_cleared_preedit_inserts_once() {
        let mut editor = editor("");
        editor.ime(preedit("にほんご"));
        assert!(editor.ime(ImeEvent::Commit("日本語".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "日本語");
        assert!(!editor.surface.is_composing());
    }

    #[test]
    fn keys_and_commands_are_ignored_while_composing() {
        let mut editor = editor("ab");
        editor.key(END);
        type_text(&mut editor, "c");
        editor.ime(preedit("ni"));
        assert!(!editor.key(Action::Backspace));
        assert!(!editor.key(Action::Insert("z".to_owned())));
        assert!(!editor.undo());
        assert!(!editor.paste("x"));
        editor.select_all();
        assert!(editor.surface.is_composing());
        assert_eq!(editor.document.text(), "abc");
    }

    #[test]
    fn a_cancelled_composition_leaves_no_trace() {
        let mut editor = editor("x");
        editor.key(END);
        editor.ime(preedit("zhong"));
        editor.ime(ImeEvent::Disabled);
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "x");
        assert!(!editor.document.is_modified());
        // Typing works again afterwards.
        type_text(&mut editor, "y");
        assert_eq!(editor.document.text(), "xy");
    }

    #[test]
    fn composing_over_a_selection_replaces_it_as_one_undo_step() {
        let mut editor = editor("hello world");
        editor.key(END);
        for _ in 0..5 {
            editor.key(Action::Left {
                select: true,
                word: false,
            });
        }
        assert!(editor.ime(preedit("shijie")));
        assert_eq!(editor.document.text(), "hello ");
        assert!(editor.ime(ImeEvent::Commit("世界".to_owned())));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "hello 世界");
        assert!(editor.undo());
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "hello world");
        assert_eq!(editor.copy().as_deref(), Some("world"));
    }

    #[test]
    fn cut_copy_and_paste() {
        let mut editor = editor("alpha beta");
        assert_eq!(editor.copy(), None);
        assert_eq!(editor.cut(), None);
        editor.select_all();
        assert_in_step(&editor);
        assert_eq!(editor.cut().as_deref(), Some("alpha beta"));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "");
        assert!(editor.paste("中文\r\n日本語"));
        assert_in_step(&editor);
        assert_eq!(editor.document.text(), "中文\n日本語");
        assert!(editor.undo());
        assert!(editor.undo());
        assert_eq!(editor.document.text(), "alpha beta");
    }

    #[test]
    fn the_view_follows_the_caret_when_typing_runs_off_the_bottom() {
        let mut editor = editor("");
        for _ in 0..60 {
            type_text(&mut editor, "line");
            editor.key(Action::Newline);
        }
        let (scroll, max) = editor.surface.scroll_range();
        assert!(max > 0.0 && scroll > 0.0);
        let (_, y, _, height) = editor.surface.ime_cursor_area();
        assert!(y >= 0.0 && y + height <= 600.0, "caret at {y}");
        // Undoing everything brings the view back to the top.
        while editor.undo() {}
        assert_eq!(editor.surface.scroll_range().0, 0.0);
    }

    #[test]
    fn saving_clears_the_modified_flag_for_the_state_that_was_written() {
        let mut editor = editor("");
        type_text(&mut editor, "ab");
        let (text, point) = editor.snapshot();
        assert_eq!(text, "ab");
        // The user keeps typing while the save runs.
        type_text(&mut editor, "c");
        editor.document.mark_saved(point);
        assert!(editor.document.is_modified());
        editor.undo();
        assert!(!editor.document.is_modified());
    }

    /// Apply the steps to a model of the record, the way the writer would.
    #[derive(Default)]
    struct Record {
        open: bool,
        base: String,
        entries: Vec<Vec<u8>>,
    }

    impl Record {
        fn take(&mut self, step: JournalStep) {
            match step {
                JournalStep::Nothing => {}
                JournalStep::Open(text) => {
                    *self = Record {
                        open: true,
                        base: text,
                        entries: Vec::new(),
                    };
                }
                JournalStep::Append(entries, checkpoint) => {
                    assert!(self.open, "appending to a record that is not open");
                    self.entries.extend(entries);
                    if let Some(text) = checkpoint {
                        self.base = text;
                        self.entries.clear();
                    }
                }
                JournalStep::Discard => *self = Record::default(),
            }
        }

        fn text(&self) -> String {
            aster_markdown::journal::rebuild(self.base.as_bytes(), &self.entries).unwrap()
        }
    }

    #[test]
    fn the_record_follows_the_document() {
        let mut editor = editor("saved\n");
        let mut record = Record::default();
        let mut keys = 0..;
        let mut sync = |editor: &mut MarkdownEditor, record: &mut Record| {
            let step = editor.journal_step(|| keys.next().unwrap());
            record.take(step);
        };

        // Nothing unsaved, no record.
        sync(&mut editor, &mut record);
        assert!(!record.open && editor.record.is_none());

        editor.key(END);
        editor.key(Action::Down { select: false });
        type_text(&mut editor, "中文");
        sync(&mut editor, &mut record);
        assert!(record.open);
        assert_eq!(editor.record, Some(0));
        assert_eq!(record.text(), editor.document.text());

        editor.ime(preedit("nihao"));
        // A composition is not an edit.
        sync(&mut editor, &mut record);
        assert_eq!(record.text(), "saved\n中文");
        editor.ime(ImeEvent::Commit("你好".to_owned()));
        editor.undo();
        editor.redo();
        assert!(editor.paste("日本語"));
        sync(&mut editor, &mut record);
        assert_eq!(record.text(), editor.document.text());

        // Saved: the record goes.
        let (_, point) = editor.snapshot();
        editor.document.mark_saved(point);
        sync(&mut editor, &mut record);
        assert!(!record.open && editor.record.is_none());

        // Modified again: a new record, starting from the text as it is.
        type_text(&mut editor, "!");
        sync(&mut editor, &mut record);
        assert_eq!(editor.record, Some(1));
        assert_eq!(record.text(), editor.document.text());

        // Undone back to the saved text: the record goes again.
        editor.undo();
        sync(&mut editor, &mut record);
        assert!(!record.open);
    }

    #[test]
    fn long_editing_checkpoints_the_record() {
        let mut editor = editor("");
        let mut record = Record::default();
        let mut checkpoints = 0;
        for n in 0..(CHECKPOINT_ENTRIES * 3) {
            type_text(&mut editor, if n % 40 == 39 { "\n" } else { "字" });
            let step = editor.journal_step(|| 7);
            if matches!(step, JournalStep::Append(_, Some(_))) {
                checkpoints += 1;
            }
            record.take(step);
            assert!(record.entries.len() as u64 <= CHECKPOINT_ENTRIES);
        }
        assert!(checkpoints >= 2, "{checkpoints} checkpoints");
        assert_eq!(record.text(), editor.document.text());
    }

    #[test]
    fn a_restored_document_has_unsaved_changes_and_a_record_at_once() {
        let tokens = theme::Tokens::of(ThemeMode::Light);
        let mut editor = MarkdownEditor::recovered(
            "restored",
            TextFormat::default(),
            Some(PathBuf::from("/tmp/a.md")),
            None,
            Some(DiskWarning::Changed),
            &tokens,
        );
        assert!(editor.document.is_modified());
        assert_eq!(editor.disk_warning, Some(DiskWarning::Changed));
        assert_eq!(
            editor.journal_step(|| 3),
            JournalStep::Open("restored".to_owned())
        );
    }

    #[test]
    fn writer_and_markdown_views_are_styled_differently() {
        let tokens = theme::Tokens::of(ThemeMode::Dark);
        assert_ne!(palette(&tokens).desk, editor_palette(&tokens).desk);
        assert_eq!(editor_palette(&tokens).desk, editor_palette(&tokens).page);
        assert_eq!(page_layout(&tokens).max_width, Some(794.0));
        assert!(editor_layout(&tokens).min_aspect.is_none());
    }
}
