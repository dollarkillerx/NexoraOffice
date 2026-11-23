//! The Aster Office shell: binds the shell logic in the library half of this
//! crate to the Slint UI under `ui/`, and hosts the custom-drawn text surface
//! from `aster-surface` in Writer and Markdown tabs.
//!
//! Input-method events never reach a Slint `FocusScope`, so they are taken from
//! winit through Slint's `unstable-winit-030` window-event hook, exactly as the
//! Phase 0 gate G1 spike does.
//!
//! Files are read and written on worker threads. Their results come back to
//! this thread through the event loop.
//!
//! Every Markdown document with unsaved changes has a recovery record. Its
//! edits go to the record writer's thread as they are made; this thread never
//! waits for the disk. A record is deleted when its document is saved or
//! closed, so the records found at start-up are what a crash left behind, and
//! the user is asked what to do with them.
//!
//! A Markdown tab shows its source, its preview, or both side by side. The
//! source is highlighted as it is typed; the preview is laid out on a worker
//! thread shortly after typing stops, and only drawn here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use aster_markdown::{DecodeError, Rope, SavePoint, TextFormat};
use aster_platform::dialogs::{self, FileType};
use aster_shell::cli::{self, Options};
use aster_shell::files::{self, LoadError};
use aster_shell::i18n::{self, Language};
use aster_shell::icons::Icon;
use aster_shell::recovery::{self, Age, Candidate, RecordInfo};
use aster_shell::sample::{RECENT_FILES, WORKSPACE_TREE};
use aster_shell::stats::{ImeState, TextStats, group_digits};
use aster_shell::tabs::{DocKind, TabId, Tabs};
use aster_shell::theme::{self, Rgba, ThemeMode};
use aster_shell::toolbar::{self, Command, Control, ViewMode, ViewState, zoom_step};
use aster_storage::{
    Contents, Event, FileState, Fingerprint, RecordKey, RecordWriter, RecoveryRoot,
};
use aster_surface::host::{self, to_px};
use aster_surface::preview::{PreviewLayout, PreviewWorker};
use aster_surface::{Action, ImeEvent, Surface};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{
    CloseRequestResponse, Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use aster_shell::content::{
    self, DiskWarning, JournalStep, MarkdownEditor, Opened, Prepared, WriterView, rope_bytes,
};

slint::include_modules!();

#[cfg(target_os = "macos")]
mod macos;

const CARET_BLINK: Duration = Duration::from_millis(530);
/// How long typing has to pause before the preview is laid out again.
const PREVIEW_DELAY: Duration = Duration::from_millis(250);
/// Width kept free for the window buttons when the tab strip is the title bar.
const WINDOW_BUTTONS_INSET: f32 = 76.0;
/// Extensions offered in the open and save dialogs.
const TEXT_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];

/// What a tab holds. Tabs of the other kinds hold nothing yet.
enum Content {
    // Boxed: both are several kilobytes, and tabs are moved around.
    Writer(Box<WriterView>),
    Markdown(Box<MarkdownEditor>),
}

impl Content {
    fn surface(&mut self) -> &mut dyn Surface {
        match self {
            Content::Writer(view) => &mut view.surface,
            Content::Markdown(editor) => &mut editor.surface,
        }
    }

    fn stats(&self) -> TextStats {
        match self {
            Content::Writer(view) => view.stats,
            Content::Markdown(editor) => editor.stats,
        }
    }
}

/// What the question on screen is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// Closing a tab whose document has unsaved changes.
    CloseTab(TabId),
    /// Quitting while documents have unsaved changes.
    Quit,
}

/// What happens once a save has succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterSave {
    Nothing,
    CloseTab,
}

/// What became of a request to close a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    Closed,
    /// The document has unsaved changes; the user has to say what to do.
    NeedsAnswer(TabId),
    NoSuchTab,
}

/// Everything a worker thread needs to write a document, and what to tell the
/// document afterwards.
struct SaveJob {
    id: TabId,
    path: PathBuf,
    text: Rope,
    format: TextFormat,
    point: SavePoint,
}

struct Shell {
    tabs: Tabs,
    contents: HashMap<TabId, Content>,
    tokens: theme::Tokens,
    language: Language,
    ime: ImeState,
    caret_on: bool,
    last_blink: Instant,
    pending: Option<Pending>,
    /// Writes recovery records. `None` when the system gives no folder for
    /// them; documents are then not protected against a crash.
    writer: Option<RecordWriter>,
    next_key: RecordKey,
    /// Records found at start-up that the user has not decided about yet.
    candidates: Vec<Candidate>,
    /// Lays out Markdown previews. `None` in tests.
    previews: Option<PreviewWorker>,
    /// Files being read and prepared on a worker thread.
    opening: Vec<PathBuf>,
    /// Runs once typing has paused, to bring the preview up to date.
    preview_timer: Timer,
}

impl Shell {
    fn new(mode: ThemeMode, language: Language) -> Self {
        Self {
            tabs: Tabs::new(),
            contents: HashMap::new(),
            tokens: theme::Tokens::of(mode),
            language,
            ime: ImeState::default(),
            caret_on: true,
            last_blink: Instant::now(),
            pending: None,
            writer: None,
            next_key: 0,
            candidates: Vec::new(),
            previews: None,
            opening: Vec::new(),
            preview_timer: Timer::default(),
        }
    }

    fn active_content(&mut self) -> Option<&mut Content> {
        let id = self.tabs.active_id()?;
        self.contents.get_mut(&id)
    }

    /// The surface of the tab in front, if it has one.
    fn active_surface(&mut self) -> Option<&mut dyn Surface> {
        self.active_content().map(Content::surface)
    }

    fn active_writer(&mut self) -> Option<&mut WriterView> {
        match self.active_content()? {
            Content::Writer(view) => Some(&mut **view),
            Content::Markdown(_) => None,
        }
    }

    fn active_markdown(&mut self) -> Option<&mut MarkdownEditor> {
        match self.active_content()? {
            Content::Markdown(editor) => Some(&mut **editor),
            Content::Writer(_) => None,
        }
    }

    fn markdown(&mut self, id: TabId) -> Option<&mut MarkdownEditor> {
        match self.contents.get_mut(&id)? {
            Content::Markdown(editor) => Some(&mut **editor),
            Content::Writer(_) => None,
        }
    }

    fn wake_caret(&mut self) {
        self.caret_on = true;
        self.last_blink = Instant::now();
    }

    /// The tab in front is about to change: a composition must not be left
    /// behind in a document that can no longer receive its commit.
    fn leave_active_document(&mut self) {
        match self.active_content() {
            Some(Content::Writer(view)) if view.surface.is_composing() => {
                view.surface.ime(ImeEvent::Disabled);
            }
            Some(Content::Markdown(editor)) if editor.surface.is_composing() => {
                editor.ime(ImeEvent::Disabled);
            }
            _ => {}
        }
    }

    fn open_untitled(&mut self, kind: DocKind) -> TabId {
        self.leave_active_document();
        let strings = self.language.strings();
        let id = self.tabs.open_untitled(kind, strings.untitled(kind));
        match kind {
            DocKind::Writer => {
                let view = WriterView::new(strings.writer_sample, &self.tokens);
                self.contents.insert(id, Content::Writer(Box::new(view)));
            }
            DocKind::Markdown => {
                let editor =
                    MarkdownEditor::new("", TextFormat::default(), None, None, &self.tokens);
                self.contents
                    .insert(id, Content::Markdown(Box::new(editor)));
            }
            DocKind::Sheets | DocKind::Slides | DocKind::Pdf => {}
        }
        id
    }

    /// Open an entry of the sample list. These are not files: the tab gets text
    /// that says so and has no path, so saving it asks where to.
    fn open_recent(&mut self, index: usize) {
        let Some(file) = RECENT_FILES.get(index) else {
            return;
        };
        self.leave_active_document();
        if self.tabs.activate_existing(file.kind, file.title).is_some() {
            return;
        }
        let id = self.tabs.open(file.kind, file.title);
        let text = i18n::fill(self.language.strings().sample_document, "title", file.title);
        match file.kind {
            DocKind::Writer => {
                let view = WriterView::new(&text, &self.tokens);
                self.contents.insert(id, Content::Writer(Box::new(view)));
            }
            DocKind::Markdown => {
                let editor =
                    MarkdownEditor::new(&text, TextFormat::default(), None, None, &self.tokens);
                self.contents
                    .insert(id, Content::Markdown(Box::new(editor)));
            }
            DocKind::Sheets | DocKind::Slides | DocKind::Pdf => {}
        }
    }

    /// The tab that already shows this file, if any.
    fn tab_of_file(&self, path: &Path) -> Option<TabId> {
        self.contents
            .iter()
            .find_map(|(id, content)| match content {
                Content::Markdown(editor) => editor
                    .path
                    .as_deref()
                    .is_some_and(|open| files::same_file(open, path))
                    .then_some(*id),
                Content::Writer(_) => None,
            })
    }

    /// Show a file that has been read from disk. A file that is already open is
    /// brought to the front instead; its tab keeps whatever was typed into it.
    fn open_file(&mut self, path: PathBuf, opened: Opened) -> TabId {
        self.leave_active_document();
        if let Some(id) = self.tab_of_file(&path) {
            self.tabs.activate(id);
            return id;
        }
        let id = self
            .tabs
            .open(DocKind::Markdown, files::display_name(&path));
        let Opened {
            prepared,
            format,
            fingerprint,
        } = opened;
        let editor = MarkdownEditor::from_prepared(
            prepared,
            format,
            Some(path),
            Some(fingerprint),
            &self.tokens,
        );
        self.contents
            .insert(id, Content::Markdown(Box::new(editor)));
        id
    }

    /// Bring back a document from a record left by an earlier run, in a tab of
    /// its own, and keep writing to that record. Unreadable records stay where
    /// they are. Returns the new tab.
    fn restore(&mut self, candidate: Candidate) -> Option<TabId> {
        let Candidate {
            record,
            info: Some(info),
            text: Ok(text),
            prepared,
            disk,
            ..
        } = candidate
        else {
            return None;
        };
        let prepared = prepared
            .and_then(|prepared| prepared.downcast::<Prepared>().ok())
            .map_or_else(|| content::prepare(text.clone()), |prepared| *prepared);
        self.leave_active_document();
        let warning = match disk {
            Some(FileState::Changed | FileState::Unreadable(_)) => Some(DiskWarning::Changed),
            Some(FileState::Missing) => Some(DiskWarning::Missing),
            Some(FileState::Unchanged) | None => None,
        };
        let title = if info.title.is_empty() {
            info.path
                .as_deref()
                .map_or_else(String::new, files::display_name)
        } else {
            info.title.clone()
        };
        let id = self.tabs.open(DocKind::Markdown, title);
        let mut editor = MarkdownEditor::recovered(
            prepared,
            info.format,
            info.path.clone(),
            info.disk,
            warning,
            &self.tokens,
        );
        if let Some(writer) = &self.writer {
            let key = self.next_key;
            self.next_key += 1;
            // The restored text becomes the record's new checkpoint.
            writer.adopt(
                key,
                record,
                info.to_meta(),
                Contents::later(move || rope_bytes(&text)),
            );
            editor.record = Some(key);
        }
        self.contents
            .insert(id, Content::Markdown(Box::new(editor)));
        self.sync_modified();
        Some(id)
    }

    /// What the recovery record of a tab's document says about it.
    fn record_info(&self, id: TabId, editor: &MarkdownEditor) -> RecordInfo {
        RecordInfo {
            title: self
                .tabs
                .get(id)
                .map(|tab| tab.title.clone())
                .unwrap_or_default(),
            path: editor.path.clone(),
            format: editor.format,
            disk: editor.disk,
        }
    }

    /// Hand every Markdown document's new edits to the record writer, and
    /// start or delete records as documents gain or lose unsaved changes.
    /// Cheap: it encodes a few bytes per edit and queues them.
    fn sync_records(&mut self) {
        let ids: Vec<TabId> = self.contents.keys().copied().collect();
        for id in ids {
            let Some(Content::Markdown(editor)) = self.contents.get(&id) else {
                continue;
            };
            let info = self.record_info(id, editor);
            let next_key = &mut self.next_key;
            let Some(Content::Markdown(editor)) = self.contents.get_mut(&id) else {
                continue;
            };
            let previous = editor.record;
            let step = editor.journal_step(|| {
                *next_key += 1;
                *next_key - 1
            });
            let Some(writer) = &self.writer else {
                continue;
            };
            match (step, editor.record.or(previous)) {
                (JournalStep::Nothing, _) | (_, None) => {}
                (JournalStep::Open(text), Some(key)) => {
                    writer.open(
                        key,
                        info.to_meta(),
                        Contents::later(move || rope_bytes(&text)),
                    );
                }
                (JournalStep::Append(entries, checkpoint), Some(key)) => {
                    for entry in entries {
                        writer.append(key, entry);
                    }
                    if let Some(text) = checkpoint {
                        writer.checkpoint(key, Contents::later(move || rope_bytes(&text)));
                    }
                }
                (JournalStep::Discard, Some(key)) => writer.discard(key),
            }
        }
    }

    fn activate(&mut self, index: usize) {
        if let Some(id) = self.tabs.tabs().get(index).map(|tab| tab.id) {
            self.leave_active_document();
            self.tabs.activate(id);
        }
    }

    /// Close a tab, unless its document has changes that would be lost.
    fn close(&mut self, index: usize) -> Closing {
        let Some(id) = self.tabs.tabs().get(index).map(|tab| tab.id) else {
            return Closing::NoSuchTab;
        };
        if self
            .markdown(id)
            .is_some_and(|editor| editor.document.is_modified())
        {
            return Closing::NeedsAnswer(id);
        }
        self.force_close(id);
        Closing::Closed
    }

    /// Close a tab whatever state its document is in. Its recovery record goes
    /// with it: closing is the user's decision, not a crash.
    fn force_close(&mut self, id: TabId) {
        self.tabs.close(id);
        if let Some(Content::Markdown(editor)) = self.contents.remove(&id)
            && let (Some(writer), Some(key)) = (&self.writer, editor.record)
        {
            writer.discard(key);
        }
    }

    /// Delete every recovery record: the user chose to quit without saving.
    fn discard_all_records(&mut self) {
        for content in self.contents.values_mut() {
            if let Content::Markdown(editor) = content
                && let Some(key) = editor.record.take()
                && let Some(writer) = &self.writer
            {
                writer.discard(key);
            }
        }
    }

    fn show_start_page(&mut self) {
        self.leave_active_document();
        self.tabs.show_start_page();
    }

    fn set_theme(&mut self, mode: ThemeMode) {
        self.tokens = theme::Tokens::of(mode);
        for content in self.contents.values_mut() {
            match content {
                Content::Writer(view) => view.restyle(&self.tokens),
                Content::Markdown(editor) => editor.restyle(&self.tokens),
            }
        }
    }

    /// Carry out a toolbar or status-bar command. Returns whether anything changed.
    fn run(&mut self, command: Command) -> bool {
        let Some(kind) = self.tabs.active().map(|tab| tab.kind) else {
            return false;
        };
        if !command.is_available(kind) {
            return false;
        }
        match (self.active_content(), command) {
            (Some(Content::Markdown(editor)), Command::Undo) => editor.undo(),
            (Some(Content::Markdown(editor)), Command::Redo) => editor.redo(),
            (
                Some(Content::Markdown(editor)),
                Command::ViewSource | Command::ViewSplit | Command::ViewPreview,
            ) => {
                let mode = ViewMode::of(command).unwrap_or_default();
                if editor.view_mode == mode {
                    return false;
                }
                // The preview takes no typing, so a composition cannot go on.
                if !mode.shows_source() {
                    editor.ime(ImeEvent::Disabled);
                }
                editor.set_view_mode(mode);
                true
            }
            (
                Some(Content::Writer(view)),
                Command::ZoomIn | Command::ZoomOut | Command::ZoomReset,
            ) => {
                view.zoom_percent = match command {
                    Command::ZoomIn => zoom_step(view.zoom_percent, true),
                    Command::ZoomOut => zoom_step(view.zoom_percent, false),
                    _ => 100,
                };
                view.surface.set_zoom(view.zoom_percent as f32 / 100.0);
                true
            }
            _ => false,
        }
    }

    /// Bring the tab strip's "modified" marks in line with the documents.
    /// Returns whether any mark changed.
    fn sync_modified(&mut self) -> bool {
        let mut changed = false;
        for (id, content) in &self.contents {
            if let Content::Markdown(editor) = content {
                changed |= self.tabs.set_modified(*id, editor.document.is_modified());
            }
        }
        changed
    }

    /// Where saving a document would write, or a name to suggest if it has
    /// never been saved.
    fn save_target(&mut self, id: TabId) -> Option<(Option<PathBuf>, PathBuf)> {
        let title = self.tabs.get(id)?.title.clone();
        let editor = self.markdown(id)?;
        let suggested = editor
            .path
            .clone()
            .unwrap_or_else(|| files::suggested_file_name(&title));
        // A restored document whose file changed or vanished is never written
        // over silently: saving asks where to, starting at the old place.
        let existing = editor
            .path
            .clone()
            .filter(|_| editor.disk_warning.is_none());
        Some((existing, suggested))
    }

    /// Take a snapshot of a document for writing to `path`. `None` if there is
    /// no such document or a save of it is already running.
    fn begin_save(&mut self, id: TabId, path: PathBuf) -> Option<SaveJob> {
        let editor = self.markdown(id)?;
        if editor.saving {
            return None;
        }
        editor.saving = true;
        let (text, point) = editor.snapshot();
        Some(SaveJob {
            id,
            path,
            text,
            format: editor.format,
            point,
        })
    }

    /// A save has finished. On success the document is tied to the file it was
    /// written to and the state that was written counts as saved.
    fn finish_save(
        &mut self,
        id: TabId,
        path: &Path,
        point: SavePoint,
        written: Option<Fingerprint>,
    ) {
        let Some(editor) = self.markdown(id) else {
            return;
        };
        editor.saving = false;
        let Some(written) = written else {
            return;
        };
        editor.path = Some(path.to_owned());
        editor.disk = Some(written);
        editor.disk_warning = None;
        // What is on disk now has one line-ending style.
        editor.format.mixed_line_endings = false;
        editor.document.mark_saved(point);
        self.tabs.rename(id, files::display_name(path));
        self.sync_modified();

        // Typed on while the save ran: the record stays, and now describes the
        // file as it is.
        if let Some(Content::Markdown(editor)) = self.contents.get(&id)
            && let (Some(writer), Some(key)) = (&self.writer, editor.record)
        {
            let meta = self.record_info(id, editor).to_meta();
            let remove = RecordInfo::optional_keys()
                .into_iter()
                .filter(|optional| !meta.iter().any(|(key, _)| key == optional))
                .collect();
            writer.set_meta(key, meta, remove);
        }
        // Saved with nothing typed since: the record goes.
        self.sync_records();
    }
}

fn color(value: Rgba) -> Color {
    Color::from_argb_u8(value.3, value.0, value.1, value.2)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

/// Copy the design tokens into the Slint globals.
fn apply_tokens(ui: &AppWindow, tokens: &theme::Tokens) {
    let t = ui.global::<Tokens>();
    let c = &tokens.colors;
    t.set_dark(tokens.mode == ThemeMode::Dark);
    t.set_surface(color(c.surface));
    t.set_sunken(color(c.sunken));
    t.set_panel(color(c.panel));
    t.set_fill(color(c.fill));
    t.set_hover(color(c.hover));
    t.set_pressed(color(c.pressed));
    t.set_hairline(color(c.hairline));
    t.set_text(color(c.text));
    t.set_text_secondary(color(c.text_secondary));
    t.set_text_tertiary(color(c.text_tertiary));
    t.set_text_disabled(color(c.text_disabled));
    t.set_accent(color(c.accent));
    t.set_on_accent(color(c.on_accent));
    t.set_accent_soft(color(c.accent_soft));
    t.set_desk(color(c.desk));
    t.set_page(color(c.page));
    t.set_page_edge(color(c.page_edge));

    let m = &tokens.metrics;
    t.set_space_xs(m.space_xs);
    t.set_space_sm(m.space_sm);
    t.set_space_md(m.space_md);
    t.set_space_lg(m.space_lg);
    t.set_space_xl(m.space_xl);
    t.set_radius_sm(m.radius_sm);
    t.set_radius_md(m.radius_md);
    t.set_radius_lg(m.radius_lg);
    t.set_control_height(m.control_height);
    t.set_tab_strip_height(m.tab_strip_height);
    t.set_toolbar_height(m.toolbar_height);
    t.set_status_bar_height(m.status_bar_height);
    t.set_sidebar_width(m.sidebar_width);
    t.set_icon_size(m.icon_size);
    t.set_font_xs(m.font_xs);
    t.set_font_sm(m.font_sm);
    t.set_font_md(m.font_md);
    t.set_font_lg(m.font_lg);
    t.set_font_xl(m.font_xl);
    t.set_font_display(m.font_display);

    apply_native_theme(ui, tokens.mode);
}

/// Make the native parts of the window (title bar, window buttons) follow the
/// theme. Returns false while the native window does not exist yet.
fn apply_native_theme(ui: &AppWindow, mode: ThemeMode) -> bool {
    let native = match mode {
        ThemeMode::Light => winit::window::Theme::Light,
        ThemeMode::Dark => winit::window::Theme::Dark,
    };
    ui.window()
        .with_winit_window(|window| window.set_theme(Some(native)))
        .is_some()
}

/// Set-up that needs the native window, which Slint only creates when the
/// window is first shown. Returns false until it has been done.
fn prepare_native_window(ui: &AppWindow, mode: ThemeMode) -> bool {
    #[cfg(target_os = "macos")]
    ui.window().with_winit_window(macos::claim_title_bar_clicks);
    apply_native_theme(ui, mode)
}

fn apply_icons(ui: &AppWindow) {
    let icons = ui.global::<Icons>();
    icons.set_close(Icon::Close.path().into());
    icons.set_plus(Icon::Plus.path().into());
    icons.set_minus(Icon::Minus.path().into());
    icons.set_chevron_down(Icon::ChevronDown.path().into());
    icons.set_chevron_right(Icon::ChevronRight.path().into());
    icons.set_sidebar(Icon::Sidebar.path().into());
    icons.set_sun(Icon::Sun.path().into());
    icons.set_moon(Icon::Moon.path().into());
    icons.set_folder(Icon::Folder.path().into());
    icons.set_clock(Icon::Clock.path().into());
    icons.set_search(Icon::Search.path().into());
    icons.set_globe(Icon::Globe.path().into());
    icons.set_assistant(Icon::Assistant.path().into());
    icons.set_check(Icon::Check.path().into());
    icons.set_folder_open(Icon::FolderOpen.path().into());
}

/// Copy the interface text of a language into the Slint globals, and rebuild
/// the lists that carry text.
fn apply_language(ui: &AppWindow, language: Language) {
    let strings = language.strings();
    let s = ui.global::<Strings>();
    s.set_tagline(strings.tagline.into());
    s.set_new_document(strings.new_document.into());
    s.set_recent(strings.recent.into());
    s.set_files(strings.files.into());
    s.set_sample(strings.sample.into());
    s.set_sample_data(strings.sample_data.into());
    s.set_placeholder(strings.placeholder.into());
    s.set_placeholder_view(strings.placeholder_view.into());
    s.set_search_files(strings.search_files.into());
    s.set_language(strings.language.into());
    s.set_placeholder_body(strings.placeholder_body.into());
    s.set_ai_title(strings.ai_title.into());
    s.set_ai_body(strings.ai_body.into());
    s.set_ai_point_tools(strings.ai_point_tools.into());
    s.set_ai_point_approval(strings.ai_point_approval.into());
    s.set_ai_point_keys(strings.ai_point_keys.into());
    s.set_ai_input(strings.ai_input.into());
    s.set_menu_file(strings.menu_file.into());
    s.set_menu_edit(strings.menu_edit.into());
    s.set_menu_new_markdown(strings.menu_new_markdown.into());
    s.set_menu_open(strings.menu_open.into());
    s.set_menu_save(strings.menu_save.into());
    s.set_menu_save_as(strings.menu_save_as.into());
    s.set_menu_close_tab(strings.menu_close_tab.into());
    s.set_menu_quit(strings.menu_quit.into());
    s.set_menu_undo(strings.menu_undo.into());
    s.set_menu_redo(strings.menu_redo.into());
    s.set_menu_cut(strings.menu_cut.into());
    s.set_menu_copy(strings.menu_copy.into());
    s.set_menu_paste(strings.menu_paste.into());
    s.set_menu_select_all(strings.menu_select_all.into());
    s.set_open_file(strings.open_file.into());
    s.set_button_save(strings.button_save.into());
    s.set_button_discard(strings.button_discard.into());
    s.set_button_cancel(strings.button_cancel.into());
    s.set_button_ok(strings.button_ok.into());
    s.set_button_quit_anyway(strings.button_quit_anyway.into());
    s.set_recovery_title(strings.recovery_title.into());
    s.set_recovery_body(strings.recovery_body.into());
    s.set_recovery_restore(strings.recovery_restore.into());
    s.set_recovery_discard(strings.recovery_discard.into());
    s.set_recovery_restore_all(strings.recovery_restore_all.into());
    s.set_recovery_later(strings.recovery_later.into());

    ui.set_language_index(language.index() as i32);
    ui.set_kinds(model(
        DocKind::ALL
            .into_iter()
            .map(|kind| KindItem {
                kind: kind.index() as i32,
                name: kind.name().into(),
                tagline: strings.kind_tagline(kind).into(),
                implemented: kind.is_implemented(),
            })
            .collect(),
    ));
    // Japanese text must not be drawn with Simplified Chinese letterforms, nor
    // the other way round.
    host::configure_shell_fonts_for(language.tag());
}

/// Lists that never change while the window is open.
fn apply_static_models(ui: &AppWindow) {
    ui.set_languages(model(
        Language::ALL
            .into_iter()
            .map(|language| SharedString::from(language.native_name()))
            .collect(),
    ));
    ui.set_recent(model(
        RECENT_FILES
            .iter()
            .map(|file| RecentItem {
                title: file.title.into(),
                kind: file.kind.index() as i32,
                location: file.location.into(),
                opened: file.opened.into(),
            })
            .collect(),
    ));
    ui.set_tree(model(
        WORKSPACE_TREE
            .iter()
            .map(|row| TreeItem {
                name: row.name.into(),
                depth: i32::from(row.depth),
                kind: row.kind.map_or(-1, |kind| kind.index() as i32),
                expanded: row.expanded,
            })
            .collect(),
    ));
}

fn tool_item(item: &toolbar::Item) -> ToolItem {
    let mut out = ToolItem {
        enabled: item.enabled,
        active: item.active,
        command: item.command.map_or(-1, Command::id),
        ..ToolItem::default()
    };
    match &item.control {
        Control::Button(icon) => {
            out.control = 0;
            out.icon = icon.path().into();
        }
        Control::Letter(text, style) => {
            out.control = 1;
            out.text = (*text).into();
            out.bold = style.bold;
            out.italic = style.italic;
            out.underline = style.underline;
            out.strike = style.strike;
        }
        Control::Select { value, width } => {
            out.control = 2;
            out.text = value.into();
            out.width = *width;
        }
        Control::Value { text, width } => {
            out.control = 3;
            out.text = text.into();
            out.width = *width;
        }
        Control::Separator => out.control = 4,
        Control::Spacer => out.control = 5,
    }
    out
}

/// The tab strip.
fn refresh_tabs(ui: &AppWindow, shell: &Shell) {
    ui.set_tabs(model(
        shell
            .tabs
            .tabs()
            .iter()
            .map(|tab| TabItem {
                title: tab.title.as_str().into(),
                modified: tab.modified,
                kind: tab.kind.index() as i32,
            })
            .collect(),
    ));
    ui.set_active_index(shell.tabs.active_index().map_or(-1, |index| index as i32));
}

/// Push everything that depends on which tab is in front.
fn refresh_chrome(ui: &AppWindow, shell: &mut Shell) {
    shell.sync_modified();
    refresh_tabs(ui, shell);

    let active = shell.tabs.active().map(|tab| (tab.kind, tab.title.clone()));
    match active {
        Some((kind, title)) => {
            ui.set_active_kind(kind.index() as i32);
            ui.set_active_title(title.into());
            let strings = shell.language.strings();
            ui.set_placeholder_title(
                i18n::fill(strings.placeholder_title, "name", kind.name()).into(),
            );
            ui.set_placeholder_plan(strings.plan(kind).into());
        }
        None => {
            ui.set_active_kind(-1);
            ui.set_active_title(SharedString::new());
        }
    }
    ui.set_editing(shell.active_markdown().is_some());
    refresh_toolbar(ui, shell);
    refresh_status(ui, shell);
}

fn refresh_toolbar(ui: &AppWindow, shell: &mut Shell) {
    let (can_undo, can_redo) = shell.active_markdown().map_or((false, false), |editor| {
        (editor.document.can_undo(), editor.document.can_redo())
    });
    ui.set_can_undo(can_undo);
    ui.set_can_redo(can_redo);
    let Some(kind) = shell.tabs.active().map(|tab| tab.kind) else {
        ui.set_toolbar_items(model(Vec::new()));
        return;
    };
    let view = ViewState {
        zoom_percent: shell.active_writer().map_or(100, |view| view.zoom_percent),
        can_undo,
        can_redo,
        view_mode: shell
            .active_markdown()
            .map_or(ViewMode::Edit, |editor| editor.view_mode),
    };
    ui.set_toolbar_items(model(
        toolbar::toolbar(kind, view, shell.language.strings())
            .iter()
            .map(tool_item)
            .collect(),
    ));
}

/// What the status bar says about a file's encoding and line endings.
fn format_label(format: &TextFormat) -> String {
    let mut label = String::from("UTF-8");
    if format.bom {
        label.push_str(" BOM");
    }
    label.push_str(" · ");
    label.push_str(format.line_ending.label());
    label
}

fn refresh_status(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let kind = shell.tabs.active().map(|tab| tab.kind);
    let ime = shell.ime;
    ui.set_status_kind(
        kind.map_or(strings.start, |kind| strings.kind_label(kind))
            .into(),
    );
    ui.set_status_note(
        match kind {
            Some(DocKind::Writer) => strings.plain_text_note,
            Some(DocKind::Markdown) => strings.markdown_note,
            Some(_) => strings.placeholder_view,
            None => "",
        }
        .into(),
    );

    // Zoom exists on Writer tabs only; file details on Markdown tabs only.
    match shell.active_content() {
        Some(Content::Writer(view)) => {
            ui.set_status_zoom(format!("{}%", view.zoom_percent).into());
            ui.set_zoom_enabled(true);
            ui.set_status_format(SharedString::new());
            ui.set_status_notice(SharedString::new());
        }
        Some(Content::Markdown(editor)) => {
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_format(format_label(&editor.format).into());
            let notice = if let Some(reason) = &editor.record_failed {
                i18n::fill(strings.notice_record_failed, "reason", reason)
            } else if let Some(warning) = editor.disk_warning {
                match warning {
                    DiskWarning::Changed => strings.notice_disk_changed.to_owned(),
                    DiskWarning::Missing => strings.notice_disk_missing.to_owned(),
                }
            } else if editor.link_refused {
                strings.notice_link_refused.to_owned()
            } else if editor.format.mixed_line_endings {
                i18n::fill(
                    strings.mixed_line_endings,
                    "style",
                    editor.format.line_ending.label(),
                )
            } else {
                String::new()
            };
            ui.set_status_notice(notice.into());
        }
        None => {
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_format(SharedString::new());
            ui.set_status_notice(SharedString::new());
        }
    }

    match shell.active_content() {
        Some(content) => {
            let stats = content.stats();
            let count = |template: &str, value: usize| {
                SharedString::from(i18n::fill(template, "count", &group_digits(value)))
            };
            ui.set_status_words(count(strings.word_count, stats.words));
            ui.set_status_chars(count(strings.char_count, stats.chars));
            let composing = content.surface().is_composing();
            let state = if composing { ImeState::Composing } else { ime };
            ui.set_status_ime(strings.ime(state).into());
            ui.set_status_composing(composing);
        }
        None => {
            ui.set_status_words(SharedString::new());
            ui.set_status_chars(SharedString::new());
            ui.set_status_ime(SharedString::new());
            ui.set_status_composing(false);
        }
    }

    // A file being opened says so above anything else.
    if let Some(path) = shell.opening.last() {
        let name = files::display_name(path);
        ui.set_status_notice(i18n::fill(strings.notice_opening, "name", &name).into());
    }
}

/// Count the words of the tab in front again on another thread, if an edit
/// could not be counted locally and its text is too large to count here.
fn schedule_count(ui: &AppWindow, shell: &mut Shell) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    if !editor.stats_stale || editor.counting {
        return;
    }
    editor.counting = true;
    let text = editor.document.text().clone();
    let version = editor.text_version();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let stats = TextStats::of_rope(&text);
        drop(text);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    if let Some(editor) = shell.markdown(id) {
                        editor.counting = false;
                        editor.counted(version, stats);
                    }
                    // The text may have changed again meanwhile.
                    schedule_count(ui, shell);
                    refresh_status(ui, shell);
                }),
            );
        });
    });
}

/// Render the document in front, hand the frame to Slint, and move the
/// candidate window. A Markdown tab also gets its preview drawn, and the two
/// views lined up.
fn present(ui: &AppWindow, shell: &mut Shell) {
    let scale = ui.window().scale_factor();
    let width = to_px(ui.get_surface_width(), scale);
    let height = to_px(ui.get_surface_height(), scale);
    let focused = ui.get_surface_focused();
    let caret_on = shell.caret_on && focused;
    let mode = shell
        .active_markdown()
        .map_or(ViewMode::Edit, |editor| editor.view_mode);
    ui.set_view_mode(match mode {
        ViewMode::Edit => 0,
        ViewMode::Split => 1,
        ViewMode::Preview => 2,
    });
    if mode.shows_preview() {
        present_preview(ui, shell);
    }
    if !mode.shows_source() {
        return;
    }
    if let Some(editor) = shell.active_markdown() {
        editor.decorate();
    }
    let Some(surface) = shell.active_surface() else {
        return;
    };
    if width == 0 || height == 0 {
        return;
    }
    surface.resize(width, height, scale);
    surface.set_caret_on(caret_on);
    if let Some(frame) = host::render_frame(surface) {
        ui.set_frame(frame);
    }
    let (offset, max) = surface.scroll_range();
    ui.set_scroll_offset(offset as f32 / scale);
    ui.set_scroll_max(max as f32 / scale);

    if focused {
        let origin = (ui.get_surface_x(), ui.get_surface_y());
        host::place_candidate_window(ui.window(), origin, surface);
    }
}

/// Draw the preview of the Markdown tab in front, asking for a new layout
/// if its size or colours changed, or if typing has paused since the text
/// changed.
fn present_preview(ui: &AppWindow, shell: &mut Shell) {
    let scale = ui.window().scale_factor();
    let width = to_px(ui.get_preview_width(), scale);
    let height = to_px(ui.get_preview_height(), scale);
    if width == 0 || height == 0 {
        return;
    }
    request_preview(shell, !shell.preview_timer.running());
    let strings = shell.language.strings();
    let Some(editor) = shell.active_markdown() else {
        return;
    };
    let note = if editor.preview_too_large() {
        let limit = format!("{} MB", content::PREVIEW_LIMIT / (1024 * 1024));
        i18n::fill(strings.preview_too_large, "size", &limit)
    } else {
        String::new()
    };
    if ui.get_preview_note().as_str() != note {
        ui.set_preview_note(note.into());
    }
    editor.preview.resize(width, height, scale);
    editor.follow_editor();
    // A caret blink in the source does not change the preview: keep the
    // frame Slint already has.
    if !editor.preview.needs_render() {
        return;
    }
    let frame = {
        let pixmap = editor.preview.render();
        let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            pixmap.data_as_u8_slice(),
            u32::from(width),
            u32::from(height),
        );
        slint::Image::from_rgba8_premultiplied(buffer)
    };
    ui.set_preview_frame(frame);
    let (offset, max) = editor.preview.scroll_range();
    ui.set_preview_scroll_offset(offset as f32 / scale);
    ui.set_preview_scroll_max(max as f32 / scale);
}

/// Hand the preview of the tab in front to the worker if it is out of date.
/// Changes to the text count only when `text_changes` is set.
fn request_preview(shell: &mut Shell, text_changes: bool) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let style = content::preview_style(&shell.tokens);
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    if let Some(job) = editor.preview_job(u64::from(id.0), text_changes, style)
        && let Some(worker) = &shell.previews
    {
        worker.request(job);
    }
}

/// The text of the tab in front changed: lay out its preview again once
/// typing pauses.
fn schedule_preview(ui: &AppWindow, shell: &mut Shell) {
    if !shell
        .active_markdown()
        .is_some_and(|editor| editor.preview_is_behind())
    {
        return;
    }
    let weak = ui.as_weak();
    shell
        .preview_timer
        .start(TimerMode::SingleShot, PREVIEW_DELAY, move || {
            if let Some(ui) = weak.upgrade() {
                deliver(ui, Box::new(|_, shell| request_preview(shell, true)));
            }
        });
}

/// A layout arrived from the preview worker.
fn finish_preview(ui: &AppWindow, shell: &mut Shell, key: u64, layout: PreviewLayout) {
    let Ok(id) = u32::try_from(key).map(TabId) else {
        return;
    };
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    editor.show_preview(layout);
    if shell.tabs.active_id() == Some(id) {
        present(ui, shell);
    }
}

/// Tell the platform to drop any composition it is holding. The input method
/// is switched on again by `present` once a document has the keyboard.
fn stop_platform_composition(ui: &AppWindow) {
    ui.window().with_winit_window(|window| {
        #[cfg(target_os = "macos")]
        macos::discard_marked_text(window);
        window.set_ime_allowed(false);
    });
}

/// After the set of tabs or the tab in front changed.
fn after_tab_change(ui: &AppWindow, shell: &mut Shell) {
    // The document a composition was typed into may be gone or behind another
    // tab; without this its candidate window would stay on screen.
    stop_platform_composition(ui);
    // The preview frame on screen belongs to the tab that was in front.
    if let Some(editor) = shell.active_markdown() {
        editor.preview.invalidate();
    }
    shell.wake_caret();
    refresh_chrome(ui, shell);
    if shell.active_surface().is_some() && shell.pending.is_none() {
        ui.invoke_focus_document();
    }
    present(ui, shell);
}

/// After the text or the view of the document in front changed.
fn after_edit(ui: &AppWindow, shell: &mut Shell, text_changed: bool) {
    shell.wake_caret();
    if text_changed {
        if let Some(view) = shell.active_writer() {
            view.stats = TextStats::of(&view.surface.text());
        }
        if shell.sync_modified() {
            refresh_tabs(ui, shell);
        }
        shell.sync_records();
        // Rebuild the toolbar only when what it shows has changed.
        let (can_undo, can_redo) = shell.active_markdown().map_or((false, false), |editor| {
            (editor.document.can_undo(), editor.document.can_redo())
        });
        if (can_undo, can_redo) != (ui.get_can_undo(), ui.get_can_redo()) {
            refresh_toolbar(ui, shell);
        }
        schedule_preview(ui, shell);
        schedule_count(ui, shell);
    }
    refresh_status(ui, shell);
    present(ui, shell);
}

/// Shortcuts the shell handles before the document sees the key.
fn shell_shortcut(text: &str, primary: bool) -> Option<Command> {
    if !primary {
        return None;
    }
    match text {
        "=" | "+" => Some(Command::ZoomIn),
        "-" => Some(Command::ZoomOut),
        "0" => Some(Command::ZoomReset),
        _ => None,
    }
}

/// Put a question or a message over the window. See `ui/dialog.slint` for the
/// kinds.
///
/// A composition in progress is dropped first, and the platform is told to stop
/// composing: otherwise its candidate window stays on screen over the dialog,
/// and a commit could arrive in a document the user is not looking at. The
/// input method comes back when the document has the keyboard again.
fn show_dialog(ui: &AppWindow, shell: &mut Shell, kind: i32, title: String, body: String) {
    shell.leave_active_document();
    stop_platform_composition(ui);
    ui.set_dialog_title(title.into());
    ui.set_dialog_body(body.into());
    ui.set_dialog_kind(kind);
    present(ui, shell);
    refresh_status(ui, shell);
}

fn hide_dialog(ui: &AppWindow, shell: &mut Shell) {
    shell.pending = None;
    ui.set_dialog_kind(0);
    if shell.active_surface().is_some() {
        ui.invoke_focus_document();
    }
}

fn text_file_types(strings: &'static i18n::Strings) -> [FileType; 1] {
    [FileType {
        name: strings.file_type_text,
        extensions: TEXT_EXTENSIONS,
    }]
}

// --- Work that leaves this thread and comes back ---------------------------

thread_local! {
    /// The shell, for results arriving from worker threads. Handlers set up in
    /// `run` hold their own reference; this one exists because a closure sent
    /// across threads cannot carry an `Rc`.
    static SHELL: RefCell<Option<Rc<RefCell<Shell>>>> = const { RefCell::new(None) };
}

type Delivery = Box<dyn FnOnce(&AppWindow, &mut Shell)>;

/// Run `job` with the shell, on this thread. If the shell is in use right now
/// — a handler is in the middle of something — try again shortly instead of
/// dropping the result.
fn deliver(ui: AppWindow, job: Delivery) {
    let shell = SHELL.with(|cell| cell.borrow().clone());
    let Some(shell) = shell else {
        return;
    };
    match shell.try_borrow_mut() {
        Ok(mut shell) => job(&ui, &mut shell),
        Err(_) => Timer::single_shot(Duration::from_millis(30), move || deliver(ui, job)),
    };
}

/// Read a file on a worker thread and open it in a tab when it arrives.
fn spawn_load(ui: &AppWindow, path: PathBuf) {
    // Say so in the status bar while it takes: a large file takes seconds.
    let opening = path.clone();
    deliver(
        ui.clone_strong(),
        Box::new(move |ui, shell| {
            shell.opening.push(opening);
            refresh_status(ui, shell);
        }),
    );
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Reading, decoding and preparing a large file all happen here.
        let result = files::load(&path).map(content::open);
        // The window may be gone by the time the file is read.
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| finish_load(ui, shell, path, result)),
            );
        });
    });
}

/// Look for records a crash left behind, on a worker thread, and ask the user
/// about them once they are loaded.
fn spawn_recovery_scan(ui: &AppWindow, root: RecoveryRoot) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found = recovery::find(&root).map(|mut candidates| {
            // Prepare restorable documents here, so restoring one does not
            // stall the window however large it is.
            for candidate in &mut candidates {
                if let Ok(text) = &candidate.text {
                    let prepared: Box<dyn std::any::Any + Send> =
                        Box::new(content::prepare(text.clone()));
                    candidate.prepared = Some(prepared);
                }
            }
            candidates
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| match found {
                    Ok(candidates) if candidates.is_empty() => {}
                    Ok(candidates) => {
                        shell.candidates = candidates;
                        refresh_recovery(ui, shell);
                    }
                    // Nothing to offer; editing still works and still writes
                    // records.
                    Err(err) => eprintln!("aster-shell: cannot look for recovery records: {err}"),
                }),
            );
        });
    });
}

/// The words for how long ago a record was written.
fn age_label(strings: &i18n::Strings, written: Option<std::time::SystemTime>) -> String {
    let Some(elapsed) = written.and_then(|written| written.elapsed().ok()) else {
        return String::new();
    };
    match recovery::age(elapsed) {
        Age::JustNow => strings.recovery_just_now.to_owned(),
        Age::Minutes(n) => i18n::fill(strings.recovery_minutes, "count", &n.to_string()),
        Age::Hours(n) => i18n::fill(strings.recovery_hours, "count", &n.to_string()),
        Age::Days(n) => i18n::fill(strings.recovery_days, "count", &n.to_string()),
    }
}

/// One row of the recovery question.
fn recovery_item(strings: &i18n::Strings, candidate: &Candidate) -> RecoveryItem {
    let info = candidate.info.as_ref();
    let title = info
        .map(|info| {
            if info.title.is_empty() {
                info.path
                    .as_deref()
                    .map_or_else(String::new, files::display_name)
            } else {
                info.title.clone()
            }
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "?".to_owned());
    let detail = match info.and_then(|info| info.path.as_deref()) {
        Some(path) => path.to_string_lossy().into_owned(),
        None if info.is_some() => strings.recovery_new_document.to_owned(),
        None => String::new(),
    };
    let note = match (&candidate.text, &candidate.disk) {
        (Err(reason), _) => i18n::fill(strings.recovery_unreadable, "reason", reason),
        (Ok(_), Some(FileState::Changed)) => strings.recovery_disk_changed.to_owned(),
        (Ok(_), Some(FileState::Missing)) => strings.recovery_disk_missing.to_owned(),
        (Ok(_), Some(FileState::Unreadable(reason))) => {
            i18n::fill(strings.recovery_disk_unreadable, "reason", reason)
        }
        (Ok(_), Some(FileState::Unchanged) | None) => String::new(),
    };
    RecoveryItem {
        title: title.into(),
        detail: detail.into(),
        age: age_label(strings, candidate.last_written).into(),
        note: note.into(),
        readable: candidate.is_readable(),
    }
}

/// Show the recovery question, or take it away once every record is decided.
fn refresh_recovery(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let items: Vec<RecoveryItem> = shell
        .candidates
        .iter()
        .map(|candidate| recovery_item(strings, candidate))
        .collect();
    let open = !items.is_empty();
    if open {
        shell.leave_active_document();
        stop_platform_composition(ui);
    }
    ui.set_recovery_items(model(items));
    if !open && shell.active_surface().is_some() && shell.pending.is_none() {
        ui.invoke_focus_document();
    }
}

/// The record writer could not write a document's record.
fn record_failed(ui: &AppWindow, shell: &mut Shell, key: RecordKey, reason: String) {
    for content in shell.contents.values_mut() {
        if let Content::Markdown(editor) = content
            && editor.record == Some(key)
        {
            editor.record_failed = Some(reason.clone());
        }
    }
    refresh_status(ui, shell);
}

fn finish_load(
    ui: &AppWindow,
    shell: &mut Shell,
    path: PathBuf,
    result: Result<Opened, LoadError>,
) {
    if let Some(at) = shell.opening.iter().position(|opening| opening == &path) {
        shell.opening.remove(at);
    }
    refresh_status(ui, shell);
    match result {
        Ok(opened) => {
            shell.open_file(path, opened);
            after_tab_change(ui, shell);
        }
        Err(err) => {
            let strings = shell.language.strings();
            let reason = match err {
                LoadError::Decode(DecodeError::NotUtf8 { .. }) => strings.not_utf8.to_owned(),
                LoadError::Decode(DecodeError::Utf16) => strings.is_utf16.to_owned(),
                LoadError::Io(err) => err.to_string(),
            };
            let title = i18n::fill(strings.open_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, reason);
        }
    }
}

/// Write a document on a worker thread.
fn spawn_save(ui: &AppWindow, job: SaveJob, after: AfterSave) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = files::save(&job.path, &job.text, &job.format);
        let (id, path, point) = (job.id, job.path, job.point);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| finish_save(ui, shell, id, path, point, result, after)),
            );
        });
    });
}

fn finish_save(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    path: PathBuf,
    point: SavePoint,
    result: std::io::Result<Fingerprint>,
    after: AfterSave,
) {
    shell.finish_save(id, &path, point, result.as_ref().ok().copied());
    match result {
        Ok(_) if after == AfterSave::CloseTab => {
            shell.force_close(id);
            after_tab_change(ui, shell);
        }
        Ok(_) => {
            refresh_chrome(ui, shell);
            present(ui, shell);
        }
        Err(err) => {
            let strings = shell.language.strings();
            let title = i18n::fill(strings.save_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, err.to_string());
        }
    }
}

/// Save the document in tab `id`, asking where to if it has no file yet or if
/// `ask` is set. The shell is not borrowed while the system dialog is open, and
/// the dialog is awaited in a task of its own, never inside the event handler
/// that asked for it: see `dialogs`.
fn save_document(
    ui: &AppWindow,
    shell: &Rc<RefCell<Shell>>,
    id: TabId,
    ask: bool,
    after: AfterSave,
) {
    let (target, strings) = {
        let Ok(mut shell) = shell.try_borrow_mut() else {
            return;
        };
        (shell.save_target(id), shell.language.strings())
    };
    let Some((existing, suggested)) = target else {
        return;
    };
    match existing {
        Some(path) if !ask => start_save(ui, shell, id, path, after),
        _ => {
            let (weak, shell) = (ui.as_weak(), shell.clone());
            let _ = slint::spawn_local(async move {
                let types = text_file_types(strings);
                let chosen =
                    dialogs::pick_file_to_save(strings.save_dialog, &suggested, &types).await;
                // Cancelled: nothing is saved and nothing is closed.
                if let Some(path) = chosen
                    && let Some(ui) = weak.upgrade()
                {
                    start_save(&ui, &shell, id, path, after);
                }
            });
        }
    }
}

fn start_save(
    ui: &AppWindow,
    shell: &Rc<RefCell<Shell>>,
    id: TabId,
    path: PathBuf,
    after: AfterSave,
) {
    let Ok(mut shell) = shell.try_borrow_mut() else {
        return;
    };
    if let Some(job) = shell.begin_save(id, path) {
        spawn_save(ui, job, after);
    }
}

/// Close the tab at `index`, or ask first if that would lose changes.
fn close_tab(ui: &AppWindow, shell: &mut Shell, index: usize) {
    match shell.close(index) {
        Closing::Closed => after_tab_change(ui, shell),
        Closing::NeedsAnswer(id) => {
            let strings = shell.language.strings();
            let name = shell.tabs.get(id).map_or("", |tab| tab.title.as_str());
            let title = i18n::fill(strings.unsaved_title, "name", name);
            shell.pending = Some(Pending::CloseTab(id));
            // Show the document the question is about.
            shell.activate(index);
            refresh_chrome(ui, shell);
            present(ui, shell);
            show_dialog(ui, shell, 1, title, strings.unsaved_body.to_owned());
        }
        Closing::NoSuchTab => {}
    }
}

/// Quit, or ask first if documents have unsaved changes. Returns whether the
/// application is going to quit now.
fn request_quit(ui: &AppWindow, shell: &mut Shell) -> bool {
    shell.sync_modified();
    let unsaved = shell.tabs.modified_count();
    if unsaved == 0 {
        return true;
    }
    let strings = shell.language.strings();
    shell.pending = Some(Pending::Quit);
    show_dialog(
        ui,
        shell,
        3,
        i18n::fill(strings.quit_title, "count", &unsaved.to_string()),
        strings.quit_body.to_owned(),
    );
    false
}

fn select_backend() -> Result<(), slint::PlatformError> {
    let selector = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into());
    // On macOS the tab strip is drawn into the title bar, next to the window
    // buttons. Windows and Linux keep their native title bar above the strip.
    #[cfg(target_os = "macos")]
    let selector = selector.with_winit_window_attributes_hook(|attributes| {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attributes
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true)
    });
    selector.select()
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    select_backend()?;
    // Following the system language is not built yet; without `--lang` the
    // shell starts in Simplified Chinese.
    let language = options.language.unwrap_or(Language::ZhHans);
    host::configure_shell_fonts_for(language.tag());

    let ui = AppWindow::new()?;
    let shell = Rc::new(RefCell::new(Shell::new(options.theme, language)));
    SHELL.with(|cell| *cell.borrow_mut() = Some(shell.clone()));
    {
        let mut shell = shell.borrow_mut();
        for kind in options.open {
            shell.open_untitled(kind);
        }
        apply_tokens(&ui, &shell.tokens);
        apply_icons(&ui);
        apply_static_models(&ui);
        apply_language(&ui, shell.language);
        refresh_chrome(&ui, &mut shell);
    }
    if cfg!(target_os = "macos") {
        ui.set_leading_inset(WINDOW_BUTTONS_INSET);
    }
    ui.set_assistant_open(options.assistant);

    // Recovery records: start the writer, then look for what a crash left.
    match aster_platform::paths::recovery_dir() {
        Some(dir) => {
            let root = RecoveryRoot::new(dir);
            let weak = ui.as_weak();
            let writer = RecordWriter::spawn(root.clone(), move |event| {
                if let Event::Failed { key, reason } = event {
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        deliver(
                            ui,
                            Box::new(move |ui, shell| record_failed(ui, shell, key, reason)),
                        );
                    });
                }
            });
            shell.borrow_mut().writer = Some(writer);
            spawn_recovery_scan(&ui, root);
        }
        None => {
            eprintln!("aster-shell: no folder for recovery records; unsaved work is not protected")
        }
    }

    // Previews are laid out on their own thread.
    {
        let weak = ui.as_weak();
        let worker = PreviewWorker::spawn(move |key, _generation, layout| {
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| finish_preview(ui, shell, key, layout)),
                );
            });
        });
        shell.borrow_mut().previews = Some(worker);
    }

    // Most handlers below follow the same shape: get the window and the shell,
    // change the shell, then push what changed back into the window.
    macro_rules! handler {
        (|$ui:ident, $shell:ident $(, $arg:ident)*| $body:block) => {{
            let shell = shell.clone();
            let weak = ui.as_weak();
            move |$($arg),*| {
                let (Some($ui), Ok(mut $shell)) = (weak.upgrade(), shell.try_borrow_mut()) else {
                    return Default::default();
                };
                let $shell = &mut *$shell;
                $body
            }
        }};
    }

    // Input-method events never reach a Slint FocusScope, so take them from winit.
    ui.window().on_winit_window_event({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move |_window, event| {
            let winit::event::WindowEvent::Ime(ime) = event else {
                return EventResult::Propagate;
            };
            let (Some(ui), Ok(mut shell)) = (weak.upgrade(), shell.try_borrow_mut()) else {
                return EventResult::Propagate;
            };
            let shell = &mut *shell;
            let event = host::ime_event(ime);
            match &event {
                ImeEvent::Enabled => shell.ime = ImeState::Enabled,
                ImeEvent::Disabled => shell.ime = ImeState::Disabled,
                ImeEvent::Preedit { .. } | ImeEvent::Commit(_) => shell.ime = ImeState::Enabled,
            }
            if !ui.get_surface_focused() {
                return EventResult::Propagate;
            }
            if shell
                .active_markdown()
                .is_some_and(|editor| !editor.view_mode.shows_source())
            {
                return EventResult::Propagate;
            }
            let text_changed = match shell.active_content() {
                Some(Content::Writer(view)) => {
                    let committed = matches!(event, ImeEvent::Commit(_));
                    view.surface.ime(event);
                    committed
                }
                Some(Content::Markdown(editor)) => editor.ime(event),
                None => return EventResult::Propagate,
            };
            after_edit(&ui, shell, text_changed);
            EventResult::PreventDefault
        }
    });

    ui.on_key_pressed(handler!(|ui, shell, text, shift, primary, alt| {
        let is_writer = shell.active_writer().is_some();
        if is_writer && let Some(command) = shell_shortcut(text.as_str(), primary) {
            if shell.run(command) {
                refresh_toolbar(&ui, shell);
                after_edit(&ui, shell, false);
            }
            return true;
        }
        let Some(action) = host::decode_key(text.as_str(), shift, primary, alt) else {
            return false;
        };
        // With only the preview on screen there is nothing to type into.
        if shell
            .active_markdown()
            .is_some_and(|editor| !editor.view_mode.shows_source())
        {
            return false;
        }
        let text_changed = match shell.active_content() {
            Some(Content::Writer(view)) => {
                view.surface.apply(action);
                true
            }
            Some(Content::Markdown(editor)) => editor.key(action),
            None => return false,
        };
        after_edit(&ui, shell, text_changed);
        true
    }));

    ui.on_pointer_down(handler!(|ui, shell, x, y, shift| {
        let scale = f64::from(ui.window().scale_factor());
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        match shell.active_content() {
            Some(Content::Writer(view)) => view.surface.pointer_down(x, y, shift),
            Some(Content::Markdown(editor)) => editor.pointer_down(x, y, shift),
            None => return,
        }
        after_edit(&ui, shell, false);
    }));

    ui.on_pointer_drag(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        match shell.active_content() {
            Some(Content::Writer(view)) => view.surface.pointer_drag(x, y),
            Some(Content::Markdown(editor)) => editor.pointer_drag(x, y),
            None => return,
        }
        present(&ui, shell);
    }));

    ui.on_scrolled(handler!(|ui, shell, delta| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(surface) = shell.active_surface()
            && surface.scroll_by(-f64::from(delta) * scale)
        {
            present(&ui, shell);
        }
    }));

    ui.on_preview_scrolled(handler!(|ui, shell, delta| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(editor) = shell.active_markdown()
            && editor.scroll_preview(-f64::from(delta) * scale)
        {
            present(&ui, shell);
        }
    }));

    ui.on_preview_moved(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let over_link = shell.active_markdown().is_some_and(|editor| {
            editor
                .preview
                .link_at(f64::from(x) * scale, f64::from(y) * scale)
                .is_some()
        });
        if over_link != ui.get_preview_link_hover() {
            ui.set_preview_link_hover(over_link);
        }
    }));

    ui.on_preview_clicked(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let Some(url) = editor
            .preview
            .link_at(f64::from(x) * scale, f64::from(y) * scale)
        else {
            return;
        };
        let refused = match aster_platform::links::open(&url) {
            Ok(()) => false,
            Err(aster_platform::links::OpenError::Refused) => true,
            Err(err) => {
                eprintln!("aster-shell: {err}");
                false
            }
        };
        editor.link_refused = refused;
        refresh_status(&ui, shell);
    }));

    ui.on_tab_activate(handler!(|ui, shell, index| {
        shell.activate(usize::try_from(index).unwrap_or(usize::MAX));
        after_tab_change(&ui, shell);
    }));

    ui.on_tab_close(handler!(|ui, shell, index| {
        close_tab(&ui, shell, usize::try_from(index).unwrap_or(usize::MAX));
    }));

    ui.on_new_document(handler!(|ui, shell, kind| {
        if let Some(kind) = usize::try_from(kind).ok().and_then(DocKind::from_index) {
            shell.open_untitled(kind);
            after_tab_change(&ui, shell);
        }
    }));

    ui.on_open_recent(handler!(|ui, shell, index| {
        shell.open_recent(usize::try_from(index).unwrap_or(usize::MAX));
        after_tab_change(&ui, shell);
    }));

    ui.on_show_start(handler!(|ui, shell| {
        shell.show_start_page();
        after_tab_change(&ui, shell);
    }));

    ui.on_toggle_theme(handler!(|ui, shell| {
        shell.set_theme(shell.tokens.mode.toggled());
        apply_tokens(&ui, &shell.tokens);
        present(&ui, shell);
    }));

    ui.on_set_language(handler!(|ui, shell, index| {
        if let Some(language) = usize::try_from(index).ok().and_then(Language::from_index) {
            // Titles and text of documents that are already open stay as they are.
            shell.language = language;
            apply_language(&ui, language);
            refresh_chrome(&ui, shell);
            if !shell.candidates.is_empty() {
                refresh_recovery(&ui, shell);
            }
        }
    }));

    ui.on_toolbar_command(handler!(|ui, shell, id| {
        if let Some(command) = Command::from_id(id)
            && shell.run(command)
        {
            if ViewMode::of(command).is_some() {
                if command == Command::ViewPreview {
                    stop_platform_composition(&ui);
                }
                refresh_toolbar(&ui, shell);
                refresh_status(&ui, shell);
                ui.invoke_focus_document();
                present(&ui, shell);
                return;
            }
            refresh_toolbar(&ui, shell);
            after_edit(&ui, shell, true);
        }
    }));

    for (command, register) in [
        (
            Command::ZoomOut,
            AppWindow::on_zoom_out as fn(&AppWindow, Box<dyn FnMut()>),
        ),
        (Command::ZoomReset, AppWindow::on_zoom_reset),
        (Command::ZoomIn, AppWindow::on_zoom_in),
        (Command::Undo, AppWindow::on_edit_undo),
        (Command::Redo, AppWindow::on_edit_redo),
    ] {
        register(
            &ui,
            Box::new(handler!(|ui, shell| {
                if shell.run(command) {
                    refresh_toolbar(&ui, shell);
                    after_edit(&ui, shell, true);
                }
            })),
        );
    }

    // --- Files ---

    ui.on_file_new(handler!(|ui, shell| {
        shell.open_untitled(DocKind::Markdown);
        after_tab_change(&ui, shell);
    }));

    ui.on_file_open({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move || {
            // The shell is not borrowed while the system dialog is open.
            let Ok(strings) = shell.try_borrow().map(|shell| shell.language.strings()) else {
                return;
            };
            let weak = weak.clone();
            // Never block on a dialog inside an event handler: see `dialogs`.
            let _ = slint::spawn_local(async move {
                let types = text_file_types(strings);
                if let Some(path) = dialogs::pick_file_to_open(strings.open_dialog, &types).await
                    && let Some(ui) = weak.upgrade()
                {
                    spawn_load(&ui, path);
                }
            });
        }
    });

    for (ask, register) in [
        (
            false,
            AppWindow::on_file_save as fn(&AppWindow, Box<dyn FnMut()>),
        ),
        (true, AppWindow::on_file_save_as),
    ] {
        let shell = shell.clone();
        let weak = ui.as_weak();
        register(
            &ui,
            Box::new(move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let Ok(id) = shell.try_borrow().map(|shell| shell.tabs.active_id()) else {
                    return;
                };
                if let Some(id) = id {
                    save_document(&ui, &shell, id, ask, AfterSave::Nothing);
                }
            }),
        );
    }

    ui.on_file_close(handler!(|ui, shell| {
        if let Some(index) = shell.tabs.active_index() {
            close_tab(&ui, shell, index);
        }
    }));

    ui.on_quit_requested(handler!(|ui, shell| {
        if request_quit(&ui, shell) {
            // Fails only when no event loop is running, and then there is
            // nothing to quit.
            let _ = slint::quit_event_loop();
        }
    }));

    ui.window().on_close_requested({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move || {
            let (Some(ui), Ok(mut shell)) = (weak.upgrade(), shell.try_borrow_mut()) else {
                return CloseRequestResponse::KeepWindowShown;
            };
            if request_quit(&ui, &mut shell) {
                CloseRequestResponse::HideWindow
            } else {
                CloseRequestResponse::KeepWindowShown
            }
        }
    });

    // --- Clipboard and selection ---

    ui.on_edit_cut(handler!(|ui, shell| {
        if let Some(text) = shell.active_markdown().and_then(MarkdownEditor::cut) {
            ui.invoke_copy_to_clipboard(text.into());
            after_edit(&ui, shell, true);
        }
    }));

    ui.on_edit_copy(handler!(|ui, shell| {
        if let Some(text) = shell.active_markdown().and_then(|editor| editor.copy()) {
            ui.invoke_copy_to_clipboard(text.into());
        }
    }));

    ui.on_edit_paste(handler!(|ui, shell| {
        if shell.active_markdown().is_none() {
            return;
        }
        let text = ui.invoke_clipboard_text();
        if let Some(editor) = shell.active_markdown()
            && editor.paste(text.as_str())
        {
            after_edit(&ui, shell, true);
        }
    }));

    ui.on_edit_select_all(handler!(|ui, shell| {
        match shell.active_content() {
            Some(Content::Writer(view)) => view.surface.apply(Action::SelectAll),
            Some(Content::Markdown(editor)) => editor.select_all(),
            None => return,
        }
        after_edit(&ui, shell, false);
    }));

    // --- Answers to the question on screen ---

    ui.on_dialog_save({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let pending = {
                let Ok(mut shell) = shell.try_borrow_mut() else {
                    return;
                };
                let pending = shell.pending;
                hide_dialog(&ui, &mut shell);
                pending
            };
            if let Some(Pending::CloseTab(id)) = pending {
                save_document(&ui, &shell, id, false, AfterSave::CloseTab);
            }
        }
    });

    ui.on_dialog_discard(handler!(|ui, shell| {
        let pending = shell.pending;
        hide_dialog(&ui, shell);
        match pending {
            Some(Pending::CloseTab(id)) => {
                shell.force_close(id);
                after_tab_change(&ui, shell);
            }
            Some(Pending::Quit) => {
                // Quitting without saving is a decision, not a crash: nothing
                // is left to recover.
                shell.discard_all_records();
                let _ = slint::quit_event_loop();
            }
            None => {}
        }
    }));

    ui.on_dialog_cancel(handler!(|ui, shell| {
        hide_dialog(&ui, shell);
    }));

    // --- The recovery question ---

    ui.on_recovery_restore(handler!(|ui, shell, index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if index >= shell.candidates.len() || !shell.candidates[index].is_readable() {
            return;
        }
        let candidate = shell.candidates.remove(index);
        shell.restore(candidate);
        refresh_recovery(&ui, shell);
        after_tab_change(&ui, shell);
    }));

    ui.on_recovery_discard(handler!(|ui, shell, index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if index >= shell.candidates.len() {
            return;
        }
        let candidate = shell.candidates.remove(index);
        match &shell.writer {
            Some(writer) => writer.discard_found(candidate.record),
            None => {
                let _ = candidate.record.discard();
            }
        }
        refresh_recovery(&ui, shell);
    }));

    ui.on_recovery_restore_all(handler!(|ui, shell| {
        let (readable, unreadable): (Vec<Candidate>, Vec<Candidate>) =
            std::mem::take(&mut shell.candidates)
                .into_iter()
                .partition(Candidate::is_readable);
        // Unreadable records stay on the list: only the user may delete them.
        shell.candidates = unreadable;
        for candidate in readable {
            shell.restore(candidate);
        }
        refresh_recovery(&ui, shell);
        after_tab_change(&ui, shell);
    }));

    ui.on_recovery_later(handler!(|ui, shell| {
        // The records stay on disk, unlocked, and are offered again at the
        // next start.
        shell.candidates.clear();
        refresh_recovery(&ui, shell);
    }));

    ui.on_drag_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().with_winit_window(|window| {
                    // Fails when the platform cannot start a drag from here;
                    // the native title bar, where there is one, still works.
                    let _ = window.drag_window();
                });
            }
        }
    });

    ui.on_toggle_maximize({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let window = ui.window();
                window.set_maximized(!window.is_maximized());
            }
        }
    });

    // Picks up resizes, scale changes and focus changes, and blinks the caret.
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(33), {
        let mut was_focused = false;
        let mut was_fullscreen = false;
        let mut native_ready = false;
        handler!(|ui, shell| {
            if !native_ready {
                native_ready = prepare_native_window(&ui, shell.tokens.mode);
            }
            let fullscreen = ui.window().is_fullscreen();
            if fullscreen != was_fullscreen && cfg!(target_os = "macos") {
                // A full-screen window has no window buttons to make room for.
                was_fullscreen = fullscreen;
                ui.set_leading_inset(if fullscreen {
                    0.0
                } else {
                    WINDOW_BUTTONS_INSET
                });
            }

            let scale = ui.window().scale_factor();
            let size = (
                to_px(ui.get_surface_width(), scale),
                to_px(ui.get_surface_height(), scale),
                scale,
            );
            let preview_size = (
                to_px(ui.get_preview_width(), scale),
                to_px(ui.get_preview_height(), scale),
                scale,
            );
            let mode = shell
                .active_markdown()
                .map_or(ViewMode::Edit, |editor| editor.view_mode);
            let preview_moved = shell.active_markdown().is_some_and(|editor| {
                mode.shows_preview() && editor.preview.size() != preview_size
            });
            let focused = ui.get_surface_focused();
            let blink_due = shell.last_blink.elapsed() >= CARET_BLINK;
            let caret_on = shell.caret_on;
            let Some(surface) = shell.active_surface() else {
                return;
            };
            // With the preview alone on screen, the source has no size and
            // no caret to blink.
            let editor_moved = mode.shows_source() && size != surface.size();
            let mut dirty = editor_moved || focused != was_focused || preview_moved;
            was_focused = focused;
            if blink_due {
                shell.last_blink = Instant::now();
                shell.caret_on = !caret_on;
                dirty = dirty || (focused && mode.shows_source());
            }
            if dirty {
                present(&ui, shell);
            }
        })
    });

    // Files named on the command line.
    for path in options.files {
        let path = std::path::absolute(&path).unwrap_or(path);
        spawn_load(&ui, path);
    }

    ui.run()?;
    // Carry out what the record writer still has queued (deleting the records
    // of documents that were saved or discarded) before the process ends.
    if let Ok(mut shell) = shell.try_borrow_mut() {
        drop(shell.writer.take());
        shell.candidates.clear();
    }
    SHELL.with(|cell| cell.borrow_mut().take());
    Ok(())
}

fn main() -> ExitCode {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("aster-shell: {err}\n{}", cli::USAGE);
            return ExitCode::FAILURE;
        }
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("aster-shell: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_markdown::{EditCommand, LineEnding};

    fn shell() -> Shell {
        Shell::new(ThemeMode::Light, Language::ZhHans)
    }

    fn decoded(text: &str) -> Opened {
        content::open(files::Loaded {
            decoded: aster_markdown::decode(text.as_bytes()).expect("valid UTF-8"),
            fingerprint: aster_storage::Fingerprint {
                len: text.len() as u64,
                checksum: aster_storage::checksum(text.as_bytes()),
                modified_nanos: None,
            },
        })
    }

    fn written() -> Option<Fingerprint> {
        Some(Fingerprint {
            len: 0,
            checksum: 0,
            modified_nanos: None,
        })
    }

    fn type_into_active(shell: &mut Shell, text: &str) {
        let editor = shell.active_markdown().expect("a Markdown tab in front");
        assert!(editor.edit(EditCommand::Type(text.to_owned())));
    }

    #[test]
    fn only_text_tabs_have_content() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Sheets);
        assert!(shell.active_content().is_none());
        shell.open_untitled(DocKind::Writer);
        assert!(shell.active_writer().is_some());
        assert!(shell.active_markdown().is_none());
        shell.open_untitled(DocKind::Markdown);
        assert!(shell.active_markdown().is_some());
        assert!(shell.active_writer().is_none());
        shell.show_start_page();
        assert!(shell.active_surface().is_none());
        assert_eq!(shell.contents.len(), 2);
    }

    #[test]
    fn closing_a_tab_drops_its_content() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.open_untitled(DocKind::Markdown);
        assert_eq!(shell.close(0), Closing::Closed);
        assert_eq!(shell.contents.len(), 1);
        assert!(shell.active_markdown().is_some());
        assert_eq!(shell.close(0), Closing::Closed);
        assert!(shell.contents.is_empty());
        assert!(shell.tabs.showing_start_page());
        assert_eq!(shell.close(5), Closing::NoSuchTab);
    }

    #[test]
    fn a_modified_document_is_not_closed_without_an_answer() {
        let mut shell = shell();
        let id = shell.open_untitled(DocKind::Markdown);
        type_into_active(&mut shell, "draft");
        assert_eq!(shell.close(0), Closing::NeedsAnswer(id));
        assert_eq!(shell.tabs.len(), 1);
        assert!(shell.sync_modified());
        assert_eq!(shell.tabs.modified_count(), 1);
        // Undoing the edit makes it safe to close again.
        assert!(shell.run(Command::Undo));
        assert!(shell.sync_modified());
        assert_eq!(shell.close(0), Closing::Closed);
        // And the user can always insist.
        let id = shell.open_untitled(DocKind::Markdown);
        type_into_active(&mut shell, "x");
        shell.force_close(id);
        assert!(shell.tabs.is_empty() && shell.contents.is_empty());
    }

    #[test]
    fn reopening_a_recent_file_reuses_its_tab() {
        let mut shell = shell();
        shell.open_recent(0);
        shell.open_untitled(DocKind::Slides);
        shell.open_recent(0);
        assert_eq!(shell.tabs.len(), 2);
        assert_eq!(shell.tabs.active_index(), Some(0));
        shell.open_recent(RECENT_FILES.len());
        assert_eq!(shell.tabs.len(), 2);
    }

    #[test]
    fn a_file_is_opened_in_one_tab_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "# 笔记\r\n").unwrap();

        let mut shell = shell();
        let first = shell.open_file(path.clone(), decoded("# 笔记\r\n"));
        assert_eq!(
            shell.tabs.active().map(|tab| tab.title.as_str()),
            Some("notes.md")
        );
        let editor = shell.active_markdown().expect("opened as Markdown");
        assert_eq!(editor.document.text(), "# 笔记\n");
        assert_eq!(editor.format.line_ending, LineEnding::CrLf);
        assert!(!editor.document.is_modified());

        // The same file through another spelling of its path, after an edit.
        type_into_active(&mut shell, "x");
        shell.show_start_page();
        let again = shell.open_file(dir.path().join(".").join("notes.md"), decoded("ignored"));
        assert_eq!(again, first);
        assert_eq!(shell.tabs.len(), 1);
        assert_eq!(
            shell
                .active_markdown()
                .map(|editor| editor.document.text().to_string()),
            Some("x# 笔记\n".to_owned())
        );
    }

    #[test]
    fn saving_ties_an_untitled_document_to_its_file() {
        let mut shell = shell();
        let id = shell.open_untitled(DocKind::Markdown);
        type_into_active(&mut shell, "内容");
        shell.sync_modified();

        let (existing, suggested) = shell.save_target(id).expect("a Markdown tab");
        assert_eq!(existing, None);
        assert_eq!(suggested, PathBuf::from("未命名笔记 1.md"));

        let path = PathBuf::from("/tmp/会议.md");
        let job = shell
            .begin_save(id, path.clone())
            .expect("nothing is saving");
        assert_eq!(job.text, "内容");
        // A second save cannot start while the first is running.
        assert!(shell.begin_save(id, path.clone()).is_none());

        shell.finish_save(id, &path, job.point, written());
        assert_eq!(
            shell.tabs.get(id).map(|tab| tab.title.as_str()),
            Some("会议.md")
        );
        assert_eq!(shell.tabs.modified_count(), 0);
        assert_eq!(
            shell.save_target(id),
            Some((Some(path.clone()), path.clone()))
        );
        assert_eq!(shell.close(0), Closing::Closed);
    }

    #[test]
    fn a_failed_save_leaves_the_document_modified_and_untitled() {
        let mut shell = shell();
        let id = shell.open_untitled(DocKind::Markdown);
        type_into_active(&mut shell, "内容");
        let path = PathBuf::from("/no/such/folder/a.md");
        let job = shell
            .begin_save(id, path.clone())
            .expect("nothing is saving");
        shell.finish_save(id, &path, job.point, None);
        let editor = shell.markdown(id).expect("still open");
        assert!(editor.document.is_modified());
        assert_eq!(editor.path, None);
        assert!(!editor.saving);
        assert_eq!(shell.close(0), Closing::NeedsAnswer(id));
    }

    #[test]
    fn typing_during_a_save_keeps_the_document_modified() {
        let mut shell = shell();
        let id = shell.open_untitled(DocKind::Markdown);
        type_into_active(&mut shell, "a");
        let path = PathBuf::from("/tmp/a.md");
        let job = shell
            .begin_save(id, path.clone())
            .expect("nothing is saving");
        type_into_active(&mut shell, "b");
        shell.finish_save(id, &path, job.point, written());
        assert_eq!(shell.tabs.modified_count(), 1);
    }

    #[test]
    fn switching_tabs_abandons_a_composition() {
        for kind in [DocKind::Writer, DocKind::Markdown] {
            let mut shell = shell();
            let first = shell.open_untitled(kind);
            let preedit = ImeEvent::Preedit {
                text: "nihao".to_owned(),
                cursor: None,
            };
            let before = match shell.active_content().expect("a text tab") {
                Content::Writer(view) => {
                    view.surface.ime(preedit);
                    view.surface.text()
                }
                Content::Markdown(editor) => {
                    editor.ime(preedit);
                    editor.document.text().to_string()
                }
            };
            shell.open_untitled(DocKind::Writer);
            let surface = shell
                .contents
                .get_mut(&first)
                .expect("still open")
                .surface();
            assert!(!surface.is_composing(), "{kind:?}");
            assert_eq!(surface.text(), before, "{kind:?}");
        }
    }

    #[test]
    fn zoom_commands_only_act_on_a_writer_tab() {
        let mut shell = shell();
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Sheets);
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Markdown);
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Writer);
        assert!(shell.run(Command::ZoomIn));
        assert!(shell.run(Command::ZoomIn));
        let view = shell.active_writer().expect("writer tab");
        assert_eq!(view.zoom_percent, 125);
        assert_eq!(view.surface.zoom(), 1.25);
        assert!(shell.run(Command::ZoomReset));
        assert_eq!(shell.active_writer().expect("writer tab").zoom_percent, 100);
        // Commands that are not built yet change nothing, and a Writer tab has
        // no history.
        assert!(!shell.run(Command::Bold));
        assert!(!shell.run(Command::Undo));
    }

    #[test]
    fn zoom_is_kept_per_document() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.run(Command::ZoomIn);
        shell.open_untitled(DocKind::Writer);
        assert_eq!(shell.active_writer().expect("second").zoom_percent, 100);
        shell.activate(0);
        assert_eq!(shell.active_writer().expect("first").zoom_percent, 110);
    }

    #[test]
    fn undo_and_redo_run_through_the_shell() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Markdown);
        assert!(!shell.run(Command::Undo));
        type_into_active(&mut shell, "abc");
        assert!(shell.run(Command::Undo));
        assert_eq!(
            shell
                .active_markdown()
                .map(|e| e.document.text().to_string()),
            Some(String::new())
        );
        assert!(shell.run(Command::Redo));
        assert_eq!(
            shell
                .active_markdown()
                .map(|e| e.document.text().to_string()),
            Some("abc".to_owned())
        );
        assert!(!shell.run(Command::Redo));
    }

    #[test]
    fn shortcuts_need_the_primary_modifier() {
        assert_eq!(shell_shortcut("=", true), Some(Command::ZoomIn));
        assert_eq!(shell_shortcut("-", true), Some(Command::ZoomOut));
        assert_eq!(shell_shortcut("0", true), Some(Command::ZoomReset));
        assert_eq!(shell_shortcut("=", false), None);
        assert_eq!(shell_shortcut("a", true), None);
    }

    #[test]
    fn new_documents_are_named_in_the_interface_language() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.language = Language::En;
        let english = shell.open_untitled(DocKind::Writer);
        shell.language = Language::Ja;
        shell.open_untitled(DocKind::Sheets);
        let titles: Vec<_> = shell
            .tabs
            .tabs()
            .iter()
            .map(|tab| tab.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["未命名文档 1", "Untitled document 2", "無題のシート 1"]
        );
        // The text of a new Writer document follows the language too.
        let surface = shell.contents.get_mut(&english).expect("open").surface();
        assert!(surface.text().starts_with("Aster Office shell"));
        // A new Markdown document is empty.
        shell.open_untitled(DocKind::Markdown);
        assert_eq!(shell.active_markdown().map(|e| e.document.len()), Some(0));
    }

    #[test]
    fn the_status_bar_names_encoding_and_line_endings() {
        assert_eq!(format_label(&TextFormat::default()), "UTF-8 · LF");
        let windows = TextFormat {
            line_ending: LineEnding::CrLf,
            bom: true,
            mixed_line_endings: false,
        };
        assert_eq!(format_label(&windows), "UTF-8 BOM · CRLF");
    }

    #[test]
    fn every_control_maps_to_a_ui_item() {
        for kind in DocKind::ALL {
            for item in toolbar::toolbar(kind, ViewState::default(), &i18n::ZH_HANS) {
                let ui_item = tool_item(&item);
                assert!((0..=5).contains(&ui_item.control));
                assert_eq!(ui_item.enabled, item.enabled);
                if let Some(command) = item.command {
                    assert_eq!(Command::from_id(ui_item.command), Some(command));
                } else {
                    assert_eq!(ui_item.command, -1);
                }
            }
        }
    }
}
