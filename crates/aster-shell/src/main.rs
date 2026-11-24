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
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aster_markdown::format::Styles;
use aster_markdown::{DecodeError, Encoding, Rope, SavePoint, TextFormat};
use aster_platform::dialogs::{self, FileType};
use aster_shell::cli::{self, Options};
use aster_shell::files::{self, Dropped, LoadError};
use aster_shell::i18n::{self, Language};
use aster_shell::icons::Icon;
use aster_shell::office::{self, SheetTab, SlidesTab};
use aster_shell::recovery::{self, Age, Candidate, RecordInfo};
use aster_shell::sidebar::{self, Look, SIDEBAR_RECENT};
use aster_shell::stats::{ImeState, TextStats, group_digits};
use aster_shell::tabs::{DocKind, TabId, Tabs};
use aster_shell::theme::{self, Rgba, ThemeMode};
use aster_shell::toolbar::{self, CellLook, Command, Control, ViewMode, ViewState, zoom_step};
use aster_storage::{
    Contents, Event, FileState, Fingerprint, RecordKey, RecordWriter, RecoveryRoot,
};
use aster_surface::cell_editor::EditMode;
use aster_surface::host::{self, to_px};
use aster_surface::preview::{PreviewBuilder, PreviewLayout, PreviewWorker};
use aster_surface::{Action, ImeEvent, Surface};
use aster_workspace::{
    FileTree, Filter, RecentFiles, Row, RowKind, Store, WorkspaceState, read_folder,
};
use chrono::Local;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{
    CloseRequestResponse, Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode,
    VecModel,
};

use aster_shell::content::{
    self, DiskWarning, Format, JournalStep, MarkdownEditor, Opened, Prepared, ReplaceJob,
    SearchJob, WriterView, rope_bytes,
};

slint::include_modules!();

mod agent;

#[cfg(target_os = "macos")]
mod macos;

const CARET_BLINK: Duration = Duration::from_millis(530);
/// Documents up to this size get their first preview laid out at once, on
/// this thread, instead of an empty pane while the worker does it.
const PREVIEW_AT_ONCE: usize = 256 * 1024;
/// How long typing has to pause before the preview is laid out again.
const PREVIEW_DELAY: Duration = Duration::from_millis(250);
/// Width kept free for the window buttons when the tab strip is the title bar.
const WINDOW_BUTTONS_INSET: f32 = 76.0;
/// Two clicks on the same folder closer together than this are one double
/// click: the folder opens (or closes) once.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// Extensions offered in the open and save dialogs.
const TEXT_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];
/// Everything the open dialog offers at once.
const SUPPORTED_EXTENSIONS: &[&str] = &["md", "markdown", "txt", "xlsx", "xlsm", "pptx", "pptm"];

/// What a tab holds. Untitled Sheets, Slides and PDF tabs hold nothing yet.
enum Content {
    // Boxed: each is several kilobytes, and tabs are moved around.
    Writer(Box<WriterView>),
    Markdown(Box<MarkdownEditor>),
    /// A workbook from a file, read-only.
    Sheet(Box<SheetTab>),
    /// A presentation from a file, read-only.
    Slides(Box<SlidesTab>),
}

impl Content {
    fn surface(&mut self) -> &mut dyn Surface {
        match self {
            Content::Writer(view) => &mut view.surface,
            Content::Markdown(editor) => &mut editor.surface,
            Content::Sheet(tab) => &mut tab.view,
            Content::Slides(tab) => &mut tab.view,
        }
    }

    fn stats(&self) -> TextStats {
        match self {
            Content::Writer(view) => view.stats,
            Content::Markdown(editor) => editor.stats,
            Content::Sheet(_) | Content::Slides(_) => TextStats::default(),
        }
    }

    /// The file a tab shows, if it came from one.
    fn path(&self) -> Option<&Path> {
        match self {
            Content::Markdown(editor) => editor.path.as_deref(),
            Content::Sheet(tab) => tab.path.as_deref(),
            Content::Slides(tab) => Some(&tab.path),
            Content::Writer(_) => None,
        }
    }
}

/// What the question on screen is about.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    /// Closing a tab whose document has unsaved changes.
    CloseTab(TabId),
    /// Quitting while documents have unsaved changes.
    Quit,
    /// A recent file is no longer there; it may be taken off the list.
    MissingRecent(PathBuf),
    /// Another program changed the file of a document with unsaved changes;
    /// what the file holds now waits in `Shell::disk_loaded`.
    DiskChanged(TabId),
    /// Saving failed because the file's encoding cannot hold a character;
    /// it may be saved in UTF-8 instead.
    SaveInUtf8 {
        id: TabId,
        path: PathBuf,
        after: AfterSave,
    },
    /// Moving this file or folder to the trash, once confirmed.
    Trash(PathBuf),
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
    /// The file's new content, while the user decides whether to load it.
    disk_loaded: Option<(TabId, files::Loaded)>,
    /// Files behind open documents are being looked at on another thread.
    checking_disk: bool,
    /// What the one-line question on screen is about.
    prompt: Option<Prompt>,
    /// Where a selection is being dragged past the preview's edge, in
    /// physical pixels, while `drag_timer` scrolls it.
    preview_drag_at: Option<(f64, f64)>,
    /// Counts double and triple clicks in the source and the preview.
    clicks: content::ClickCounter,
    drag_timer: Timer,
    /// What the encoding question is about, and the encodings it offers.
    encoding_for: Option<(EncodingFor, Vec<Encoding>)>,
    /// Writes recovery records. `None` when the system gives no folder for
    /// them; documents are then not protected against a crash.
    writer: Option<RecordWriter>,
    next_key: RecordKey,
    /// Records found at start-up that the user has not decided about yet.
    candidates: Vec<Candidate>,
    /// Lays out Markdown previews. `None` in tests.
    previews: Option<PreviewWorker>,
    /// The heading level the toolbar's paragraph style field shows.
    shown_heading: u8,
    /// The styles the toolbar shows pressed.
    shown_styles: Styles,
    /// How the toolbar shows the active cell of a workbook.
    shown_cell: CellLook,
    /// Lays out a small document's first preview on this thread, so the
    /// pane is never shown empty. Made and warmed up on a worker thread at
    /// start-up; `None` until then.
    preview_builder: Option<PreviewBuilder>,
    /// Files being read and prepared on a worker thread.
    opening: Vec<PathBuf>,
    /// Runs once typing has paused, to bring the preview up to date.
    preview_timer: Timer,
    /// Runs once typing has paused, to search again with the find bar open.
    find_timer: Timer,
    /// Files opened or saved lately. What is read from disk at start-up is
    /// merged in when it arrives; until then nothing is written back.
    recent: RecentFiles,
    recent_loaded: bool,
    /// The folder in the sidebar.
    tree: Option<FileTree>,
    /// The built-in assistant and MCP; set up once the window exists.
    ai: Option<agent::Assistant>,
    /// The headings of the Markdown document in front, for the sidebar's
    /// outline, and which document and text version they are of.
    outline: Vec<aster_markdown::structure::Heading>,
    outline_of: Option<(TabId, u64)>,
    outline_timer: Timer,
    /// The link dialog is open for this tab, over this range of its text as
    /// it was at this version, for an image or not.
    link_edit: Option<(TabId, std::ops::Range<usize>, u64, bool)>,
    /// Whether the remembered folder has been looked up; until then the
    /// sidebar state is not written back.
    workspace_loaded: bool,
    /// What the sidebar's search field holds.
    filter: Option<Filter>,
    /// The files and tree lines behind the rows on screen, by index.
    sidebar_recent: Vec<PathBuf>,
    start_recent: Vec<PathBuf>,
    tree_rows: Vec<Row>,
    /// The folder last expanded or collapsed, and when: a double click
    /// toggles it once, not open and shut again.
    last_toggle: Option<(PathBuf, Instant)>,
    /// Writes the recent list and the sidebar state. `None` in tests and
    /// when the system gives no data folder.
    store: Option<Store>,
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
            disk_loaded: None,
            checking_disk: false,
            prompt: None,
            preview_drag_at: None,
            clicks: content::ClickCounter::default(),
            drag_timer: Timer::default(),
            encoding_for: None,
            writer: None,
            next_key: 0,
            candidates: Vec::new(),
            previews: None,
            preview_builder: None,
            shown_heading: 0,
            shown_styles: Styles::default(),
            shown_cell: CellLook::default(),
            opening: Vec::new(),
            preview_timer: Timer::default(),
            find_timer: Timer::default(),
            recent: RecentFiles::default(),
            recent_loaded: false,
            tree: None,
            ai: None,
            outline: Vec::new(),
            outline_of: None,
            outline_timer: Timer::default(),
            link_edit: None,
            workspace_loaded: false,
            filter: None,
            sidebar_recent: Vec::new(),
            start_recent: Vec::new(),
            tree_rows: Vec::new(),
            last_toggle: None,
            store: None,
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
            _ => None,
        }
    }

    fn active_markdown(&mut self) -> Option<&mut MarkdownEditor> {
        match self.active_content()? {
            Content::Markdown(editor) => Some(&mut **editor),
            _ => None,
        }
    }

    fn active_sheet(&mut self) -> Option<&mut SheetTab> {
        match self.active_content()? {
            Content::Sheet(tab) => Some(&mut **tab),
            _ => None,
        }
    }

    fn active_slides(&mut self) -> Option<&mut SlidesTab> {
        match self.active_content()? {
            Content::Slides(tab) => Some(&mut **tab),
            _ => None,
        }
    }

    fn markdown(&mut self, id: TabId) -> Option<&mut MarkdownEditor> {
        match self.contents.get_mut(&id)? {
            Content::Markdown(editor) => Some(&mut **editor),
            _ => None,
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
            DocKind::Sheets => {
                let source = office::blank_workbook(self.language.tag());
                // Our own template: it always reads.
                if let Ok(workbook) = aster_ooxml::xlsx::read(source.clone()) {
                    let tab = SheetTab::new(workbook, Arc::new(source), None, &self.tokens);
                    self.contents.insert(id, Content::Sheet(Box::new(tab)));
                }
            }
            DocKind::Slides | DocKind::Pdf => {}
        }
        id
    }

    /// Put a file at the top of the recent list, in its canonical spelling so
    /// it is listed once however it was reached.
    fn remember_file(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if self.recent.add(&path, SystemTime::now()) {
            self.save_recent();
        }
    }

    fn save_recent(&self) {
        if !self.recent_loaded {
            return;
        }
        if let (Some(store), Some(file)) = (&self.store, aster_platform::paths::recent_files()) {
            store.write(file, self.recent.encode().into_bytes());
        }
    }

    fn save_workspace(&self) {
        if !self.workspace_loaded {
            return;
        }
        let state = match &self.tree {
            Some(tree) => WorkspaceState {
                folder: Some(tree.root().to_owned()),
                expanded: tree.expanded(),
            },
            None => WorkspaceState::default(),
        };
        if let (Some(store), Some(file)) = (&self.store, aster_platform::paths::workspace_state()) {
            store.write(file, state.encode().into_bytes());
        }
    }

    /// Show `folder` in the sidebar with `expanded` folders open. Returns the
    /// folders to read.
    fn open_folder(&mut self, folder: &Path, expanded: Vec<PathBuf>) -> Vec<PathBuf> {
        let root = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_owned());
        let mut tree = FileTree::new(root);
        tree.restore_expanded(expanded);
        let wanted = tree.wanted();
        self.tree = Some(tree);
        self.save_workspace();
        wanted
    }

    /// The tab that already shows this file, if any.
    fn tab_of_file(&self, path: &Path) -> Option<TabId> {
        self.contents.iter().find_map(|(id, content)| {
            content
                .path()
                .is_some_and(|open| files::same_file(open, path))
                .then_some(*id)
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

    /// Show a workbook or presentation that has been read from disk.
    fn open_office(&mut self, path: PathBuf, loaded: office::Loaded) -> TabId {
        self.leave_active_document();
        if let Some(id) = self.tab_of_file(&path) {
            self.tabs.activate(id);
            return id;
        }
        let title = files::display_name(&path);
        let content = match loaded {
            office::Loaded::Workbook(workbook, source) => {
                let id = self.tabs.open(DocKind::Sheets, title);
                let tab = SheetTab::new(workbook, source, Some(path), &self.tokens);
                self.contents.insert(id, Content::Sheet(Box::new(tab)));
                return id;
            }
            office::Loaded::Presentation(presentation) => {
                SlidesTab::new(presentation, path, &self.tokens, self.language.strings())
            }
        };
        let id = self.tabs.open(DocKind::Slides, title);
        self.contents.insert(id, Content::Slides(Box::new(content)));
        id
    }

    /// Bring back a document from a record left by an earlier run, in a tab of
    /// its own, and keep writing to that record. Unreadable records stay where
    /// they are. Returns the new tab.
    fn restore(&mut self, candidate: Candidate) -> Option<TabId> {
        if candidate.workbook.is_some() {
            return self.restore_workbook(candidate);
        }
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

    /// Bring back a workbook from its record: the workbook as it was last
    /// written, in a tab of its own with unsaved changes, saving to the file
    /// it belongs to. The record is kept writing.
    fn restore_workbook(&mut self, candidate: Candidate) -> Option<TabId> {
        let Candidate {
            record,
            info: Some(info),
            workbook: Some(bytes),
            ..
        } = candidate
        else {
            return None;
        };
        let workbook = aster_ooxml::xlsx::read(bytes.clone()).ok()?;
        self.leave_active_document();
        let title = if info.title.is_empty() {
            info.path
                .as_deref()
                .map_or_else(String::new, files::display_name)
        } else {
            info.title.clone()
        };
        let id = self.tabs.open(DocKind::Sheets, title);
        let source = Arc::new(bytes);
        let mut tab = SheetTab::new(workbook, source.clone(), info.path.clone(), &self.tokens);
        tab.disk = info.disk;
        tab.mark_recovered();
        if let Some(writer) = &self.writer {
            let key = self.next_key;
            self.next_key += 1;
            writer.adopt(
                key,
                record,
                info.to_meta(),
                Contents::Bytes((*source).clone()),
            );
            tab.record = Some(key);
            tab.recorded = Some((tab.history.version(), std::time::Instant::now()));
        }
        self.contents.insert(id, Content::Sheet(Box::new(tab)));
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
            workbook: false,
        }
    }

    /// What the recovery record of a workbook says about it.
    fn sheet_record_info(&self, id: TabId, tab: &SheetTab) -> RecordInfo {
        RecordInfo {
            title: self
                .tabs
                .get(id)
                .map(|tab| tab.title.clone())
                .unwrap_or_default(),
            path: tab.path.clone(),
            format: TextFormat::default(),
            disk: tab.disk,
            workbook: true,
        }
    }

    /// Write each workbook with unsaved changes to its recovery record, as
    /// saving would write it, at most every few seconds: the whole workbook
    /// is made again each time, on the writer thread. A record goes once
    /// its workbook has no unsaved changes. Called after edits and by a
    /// timer, which writes what changed since the last time.
    fn sync_sheet_records(&mut self) {
        /// The least time between two writes of one workbook's record.
        const EVERY: std::time::Duration = std::time::Duration::from_secs(2);
        let ids: Vec<TabId> = self.contents.keys().copied().collect();
        for id in ids {
            let Some(Content::Sheet(tab)) = self.contents.get(&id) else {
                continue;
            };
            let info = self.sheet_record_info(id, tab);
            let Some(writer) = &self.writer else {
                return;
            };
            let Some(Content::Sheet(tab)) = self.contents.get_mut(&id) else {
                continue;
            };
            if !tab.is_modified() {
                if let Some(key) = tab.record.take() {
                    writer.discard(key);
                }
                tab.recorded = None;
                continue;
            }
            let version = tab.history.version();
            match tab.recorded {
                Some((done, _)) if done == version && tab.record.is_some() => continue,
                Some((_, at)) if at.elapsed() < EVERY => continue,
                _ => {}
            }
            let contents = Contents::maybe(tab.snapshot());
            match tab.record {
                Some(key) => {
                    let meta = info.to_meta();
                    let remove = RecordInfo::optional_keys()
                        .into_iter()
                        .filter(|optional| !meta.iter().any(|(key, _)| key == optional))
                        .collect();
                    writer.set_meta(key, meta, remove);
                    writer.checkpoint(key, contents);
                }
                None => {
                    let key = self.next_key;
                    self.next_key += 1;
                    writer.open(key, info.to_meta(), contents);
                    tab.record = Some(key);
                }
            }
            tab.recorded = Some((version, std::time::Instant::now()));
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
        let modified = match self.contents.get(&id) {
            Some(Content::Markdown(editor)) => editor.is_modified(),
            Some(Content::Sheet(tab)) => tab.is_modified(),
            _ => false,
        };
        if modified {
            return Closing::NeedsAnswer(id);
        }
        self.force_close(id);
        Closing::Closed
    }

    /// Close a tab whatever state its document is in. Its recovery record goes
    /// with it: closing is the user's decision, not a crash.
    fn force_close(&mut self, id: TabId) {
        self.tabs.close(id);
        let record = match self.contents.remove(&id) {
            Some(Content::Markdown(editor)) => editor.record,
            Some(Content::Sheet(tab)) => tab.record,
            _ => None,
        };
        if let (Some(writer), Some(key)) = (&self.writer, record) {
            writer.discard(key);
        }
    }

    /// Delete every recovery record: the user chose to quit without saving.
    fn discard_all_records(&mut self) {
        for content in self.contents.values_mut() {
            let record = match content {
                Content::Markdown(editor) => editor.record.take(),
                Content::Sheet(tab) => tab.record.take(),
                _ => None,
            };
            if let (Some(key), Some(writer)) = (record, &self.writer) {
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
        let strings = self.language.strings();
        for content in self.contents.values_mut() {
            match content {
                Content::Writer(view) => view.restyle(&self.tokens),
                Content::Markdown(editor) => editor.restyle(&self.tokens),
                Content::Sheet(tab) => tab.restyle(&self.tokens),
                Content::Slides(tab) => tab.restyle(&self.tokens, strings),
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
        let tokens = self.tokens;
        match (self.active_content(), command) {
            (Some(Content::Markdown(editor)), command) if format_of(command).is_some() => {
                format_of(command).is_some_and(|format| editor.format(format))
            }
            (Some(Content::Markdown(editor)), Command::Undo) => editor.undo(),
            (Some(Content::Markdown(editor)), Command::Redo) => editor.redo(),
            (Some(Content::Sheet(_)), command) if sheet_formats(command) => {
                let language = self.language.tag();
                let Some(tab) = self.active_sheet() else {
                    return false;
                };
                let mut changes = if tab.view.is_editing() {
                    tab.commit(language)
                } else {
                    Vec::new()
                };
                let started = match command {
                    Command::MergeCells => {
                        changes.extend(tab.merge());
                        true
                    }
                    Command::AutoSum => {
                        changes.extend(tab.auto_sum(language));
                        true
                    }
                    _ => match tab.edit_for(command, language) {
                        Some(edit) => {
                            changes.extend(tab.format(&edit));
                            false
                        }
                        None => false,
                    },
                };
                tab.recalc(&changes);
                started || !changes.is_empty()
            }
            (Some(Content::Sheet(tab)), Command::Undo | Command::Redo) => {
                let changes = if command == Command::Undo {
                    tab.undo()
                } else {
                    tab.redo()
                };
                tab.recalc(&changes);
                !changes.is_empty()
            }
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
                Some(Content::Markdown(editor)),
                Command::ZoomIn | Command::ZoomOut | Command::ZoomReset,
            ) => {
                let percent = match command {
                    Command::ZoomIn => zoom_step(editor.zoom_percent, true),
                    Command::ZoomOut => zoom_step(editor.zoom_percent, false),
                    _ => 100,
                };
                if percent == editor.zoom_percent {
                    return false;
                }
                editor.set_zoom(percent, &tokens);
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
            match content {
                Content::Markdown(editor) => {
                    changed |= self.tabs.set_modified(*id, editor.is_modified());
                }
                Content::Sheet(tab) => {
                    changed |= self.tabs.set_modified(*id, tab.is_modified());
                }
                _ => {}
            }
        }
        changed
    }

    /// Where saving a document would write, or a name to suggest if it has
    /// never been saved.
    fn save_target(&mut self, id: TabId) -> Option<(Option<PathBuf>, PathBuf)> {
        let title = self.tabs.get(id)?.title.clone();
        if let Some(Content::Sheet(tab)) = self.contents.get(&id) {
            let suggested = tab.path.clone().unwrap_or_else(|| {
                let mut name = files::suggested_file_name(&title);
                name.set_extension("xlsx");
                name
            });
            return Some((tab.path.clone(), suggested));
        }
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
        // What is on disk now has one line-ending style, and is what the
        // format says.
        editor.format.mixed_line_endings = false;
        editor.format_changed = false;
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
    t.set_selection(color(c.selection));
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
    icons.set_chevron_up(Icon::ChevronUp.path().into());
    icons.set_refresh(Icon::Refresh.path().into());
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
    s.set_outline(strings.outline.into());
    s.set_outline_empty(strings.outline_empty.into());
    s.set_outline_none(strings.outline_none.into());
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
    s.set_menu_export_html(strings.menu_export_html.into());
    s.set_menu_export_pdf(strings.menu_export_pdf.into());
    s.set_menu_close_tab(strings.menu_close_tab.into());
    s.set_menu_quit(strings.menu_quit.into());
    s.set_menu_undo(strings.menu_undo.into());
    s.set_menu_redo(strings.menu_redo.into());
    s.set_menu_cut(strings.menu_cut.into());
    s.set_menu_copy(strings.menu_copy.into());
    s.set_menu_paste(strings.menu_paste.into());
    s.set_menu_select_all(strings.menu_select_all.into());
    s.set_menu_insert_rows(strings.menu_insert_rows.into());
    s.set_menu_insert_columns(strings.menu_insert_columns.into());
    s.set_menu_delete_rows(strings.menu_delete_rows.into());
    s.set_menu_delete_columns(strings.menu_delete_columns.into());
    s.set_menu_hide_rows(strings.menu_hide_rows.into());
    s.set_menu_hide_columns(strings.menu_hide_columns.into());
    s.set_menu_unhide_rows(strings.menu_unhide_rows.into());
    s.set_menu_unhide_columns(strings.menu_unhide_columns.into());
    s.set_sheet_add(strings.sheet_add.into());
    s.set_sheet_rename(strings.sheet_rename.into());
    s.set_sheet_delete(strings.sheet_delete.into());
    s.set_menu_find(strings.menu_find.into());
    s.set_menu_find_next(strings.menu_find_next.into());
    s.set_menu_find_previous(strings.menu_find_previous.into());
    s.set_menu_find_selection(strings.menu_find_selection.into());
    s.set_menu_replace(strings.menu_replace.into());
    s.set_find_placeholder(strings.find_placeholder.into());
    s.set_replace_placeholder(strings.replace_placeholder.into());
    s.set_find_match_case(strings.find_match_case.into());
    s.set_find_whole_word(strings.find_whole_word.into());
    s.set_find_regex(strings.find_regex.into());
    s.set_find_close(strings.find_close.into());
    s.set_replace_one(strings.replace_one.into());
    s.set_replace_all(strings.replace_all.into());
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
    s.set_menu_open_folder(strings.menu_open_folder.into());
    s.set_menu_clear_recent(strings.menu_clear_recent.into());
    s.set_reveal(sidebar::reveal_label(strings).into());
    s.set_recent_remove(strings.recent_remove.into());
    s.set_tree_new_file(strings.tree_new_file.into());
    s.set_tree_new_folder(strings.tree_new_folder.into());
    s.set_tree_rename(strings.tree_rename.into());
    s.set_trash(sidebar::trash_label(strings).into());
    s.set_button_open(strings.button_open.into());
    s.set_button_save_utf8(strings.button_save_utf8.into());
    s.set_menu_reopen_encoding(strings.menu_reopen_encoding.into());
    s.set_menu_convert_utf8(strings.menu_convert_utf8.into());
    s.set_menu_line_endings_lf(strings.menu_line_endings_lf.into());
    s.set_menu_line_endings_crlf(strings.menu_line_endings_crlf.into());
    s.set_menu_go_to_line(strings.menu_go_to_line.into());
    s.set_button_load_disk(strings.button_load_disk.into());
    s.set_button_keep_mine(strings.button_keep_mine.into());
    s.set_recent_empty(strings.recent_empty.into());
    s.set_recent_missing(strings.recent_missing.into());
    s.set_no_folder(strings.no_folder.into());
    s.set_refresh(strings.refresh.into());
    s.set_unsupported(strings.unsupported.into());
    s.set_drop_to_open(strings.drop_to_open.into());
    s.set_no_matches(strings.no_matches.into());
    s.set_menu_format(strings.menu_format.into());
    s.set_format_bold(strings.format_bold.into());
    s.set_format_italic(strings.format_italic.into());
    s.set_format_strike(strings.format_strike.into());
    s.set_format_underline(strings.format_underline.into());
    s.set_format_code(strings.format_code.into());
    s.set_format_code_block(strings.format_code_block.into());
    s.set_format_rule(strings.format_rule.into());
    s.set_format_toc(strings.format_toc.into());
    s.set_format_footnote(strings.format_footnote.into());
    s.set_link_title(strings.link_title.into());
    s.set_image_title(strings.image_title.into());
    s.set_link_label(strings.link_label.into());
    s.set_link_address(strings.link_address.into());
    s.set_image_label(strings.image_label.into());
    s.set_image_address(strings.image_address.into());
    s.set_format_bullet(strings.format_bullet.into());
    s.set_format_numbered(strings.format_numbered.into());
    s.set_format_task(strings.format_task.into());
    s.set_format_quote(strings.format_quote.into());
    s.set_format_link(strings.format_link.into());
    s.set_format_image(strings.format_image.into());
    s.set_format_table(strings.format_table.into());
    s.set_notes_title(strings.notes_title.into());
    s.set_notes_empty(strings.notes_empty.into());
    s.set_notes_show(strings.notes_show.into());
    s.set_notes_hide(strings.notes_hide.into());
    s.set_ai_new_chat(strings.ai_new_chat.into());
    s.set_ai_settings(strings.ai_settings.into());
    s.set_ai_back(strings.ai_back.into());
    s.set_ai_send(strings.ai_send.into());
    s.set_ai_quick_rewrite(strings.ai_quick_rewrite.into());
    s.set_ai_quick_proofread(strings.ai_quick_proofread.into());
    s.set_ai_quick_summarize(strings.ai_quick_summarize.into());
    s.set_ai_quick_continue(strings.ai_quick_continue.into());
    s.set_ai_quick_translate(strings.ai_quick_translate.into());
    s.set_ai_reply_copy(strings.ai_reply_copy.into());
    s.set_ai_reply_insert(strings.ai_reply_insert.into());
    s.set_ai_reply_replace(strings.ai_reply_replace.into());
    s.set_ai_reply_retry(strings.ai_reply_retry.into());
    s.set_ai_chats(strings.ai_chats.into());
    s.set_mcp_disconnect(strings.mcp_disconnect.into());
    s.set_ai_stop(strings.ai_stop.into());
    s.set_ai_input_hint(strings.ai_input_hint.into());
    s.set_ai_empty(strings.ai_empty.into());
    s.set_ai_access_none(strings.ai_access_none.into());
    s.set_level_off(strings.level_off.into());
    s.set_level_read_only(strings.level_read_only.into());
    s.set_level_ask(strings.level_ask.into());
    s.set_level_trusted(strings.level_trusted.into());
    s.set_level_full(strings.level_full.into());
    s.set_level_help(strings.level_help.into());
    s.set_ai_not_configured(strings.ai_not_configured.into());
    s.set_ai_thinking(strings.ai_thinking.into());
    s.set_approve_allow(strings.approve_allow.into());
    s.set_approve_session(strings.approve_session.into());
    s.set_approve_deny(strings.approve_deny.into());
    s.set_approval_timeout(strings.approval_timeout.into());
    s.set_approval_removed_label(strings.approval_removed_label.into());
    s.set_approval_inserted_label(strings.approval_inserted_label.into());
    s.set_ai_settings_title(strings.ai_settings_title.into());
    s.set_ai_provider(strings.ai_provider.into());
    s.set_provider_custom(strings.provider_custom.into());
    s.set_ai_base_url(strings.ai_base_url.into());
    s.set_ai_model(strings.ai_model.into());
    s.set_ai_api_key(strings.ai_api_key.into());
    s.set_ai_key_hint(strings.ai_key_hint.into());
    s.set_ai_insecure(strings.ai_insecure.into());
    s.set_ai_allow_insecure(strings.ai_allow_insecure.into());
    s.set_ai_refresh_models(strings.ai_refresh_models.into());
    s.set_ai_key_save(strings.ai_key_save.into());
    s.set_ai_key_delete(strings.ai_key_delete.into());
    s.set_ai_redact(strings.ai_redact.into());
    s.set_ai_access_title(strings.ai_access_title.into());
    s.set_ai_untitled_access(strings.ai_untitled_access.into());
    s.set_ai_no_folder(strings.ai_no_folder.into());
    s.set_mcp_title(strings.mcp_title.into());
    s.set_mcp_enable(strings.mcp_enable.into());
    s.set_mcp_off_note(strings.mcp_off_note.into());
    s.set_mcp_no_clients(strings.mcp_no_clients.into());
    s.set_mcp_setup_cli(strings.mcp_setup_cli.into());
    s.set_mcp_setup_json(strings.mcp_setup_json.into());
    s.set_mcp_copy(strings.mcp_copy.into());
    s.set_table_edit(strings.table_edit.into());
    s.set_table_row_above(strings.table_row_above.into());
    s.set_table_row_below(strings.table_row_below.into());
    s.set_table_column_left(strings.table_column_left.into());
    s.set_table_column_right(strings.table_column_right.into());
    s.set_table_delete_row(strings.table_delete_row.into());
    s.set_table_delete_column(strings.table_delete_column.into());
    s.set_table_align_left(strings.table_align_left.into());
    s.set_table_align_center(strings.table_align_center.into());
    s.set_table_align_right(strings.table_align_right.into());
    ui.set_heading_names(model(
        toolbar::heading_names(strings)
            .into_iter()
            .map(SharedString::from)
            .collect(),
    ));

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
}

fn tool_item(item: &toolbar::Item) -> ToolItem {
    let mut out = ToolItem {
        enabled: item.enabled,
        active: item.active,
        command: item.command.map_or(-1, Command::id),
        options: model(
            item.options
                .iter()
                .map(|option| SharedString::from(option.as_str()))
                .collect(),
        ),
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
        Control::Swatch {
            icon,
            letter,
            color,
        } => {
            out.control = 6;
            out.icon = icon.map_or(SharedString::new(), |icon| icon.path().into());
            out.text = (*letter).into();
            out.has_swatch = color.is_some();
            if let Some([r, g, b, a]) = color {
                out.swatch = slint::Color::from_argb_u8(*a, *r, *g, *b);
            }
            out.colours = model(
                toolbar::PALETTE
                    .iter()
                    .map(|[r, g, b, a]| slint::Color::from_argb_u8(*a, *r, *g, *b))
                    .collect(),
            );
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
    ui.set_can_save(shell.active_markdown().is_some() || shell.active_sheet().is_some());
    agent::refresh(ui, shell);
    schedule_outline(ui, shell);
    refresh_viewer(ui, shell);
    refresh_toolbar(ui, shell);
    refresh_status(ui, shell);
    refresh_find(ui, shell);
}

fn refresh_toolbar(ui: &AppWindow, shell: &mut Shell) {
    let (can_undo, can_redo) = undo_state(shell);
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
        heading: shell
            .active_markdown()
            .map_or(0, |editor| editor.heading_level()),
        styles: shell
            .active_markdown()
            .map_or_else(Styles::default, |editor| editor.styles()),
        cell: shell
            .active_sheet()
            .map_or_else(CellLook::default, |tab| tab.cell_look()),
    };
    shell.shown_cell = view.cell;
    shell.shown_heading = view.heading;
    shell.shown_styles = view.styles;
    ui.set_in_table(view.styles.table);
    {
        use aster_markdown::format::Block;
        let styles = view.styles;
        ui.set_cell_bold(view.cell.bold);
        ui.set_cell_italic(view.cell.italic);
        ui.set_cell_underline(view.cell.underline);
        ui.set_cell_strike(view.cell.strike);
        ui.set_mark_bold(styles.bold);
        ui.set_mark_italic(styles.italic);
        ui.set_mark_strike(styles.strike);
        ui.set_mark_code(styles.code);
        ui.set_mark_block(match styles.block {
            Some(Block::Bullet) => 1,
            Some(Block::Numbered) => 2,
            Some(Block::Task) => 3,
            Some(Block::Quote) => 4,
            None => 0,
        });
        ui.set_mark_heading(i32::from(view.heading));
    }
    ui.set_toolbar_items(model(
        toolbar::toolbar(kind, view, shell.language.strings())
            .iter()
            .map(tool_item)
            .collect(),
    ));
}

/// What the status bar says about a file's encoding and line endings.
fn format_label(format: &TextFormat) -> String {
    let mut label = String::from(format.encoding.label());
    let has_bom = matches!(
        format.encoding,
        Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be
    );
    if format.bom && has_bom {
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
    // Workbooks can be edited and saved now; presentations are still read
    // only.
    let read_only = shell.active_slides().is_some();
    ui.set_status_note(
        match kind {
            _ if read_only => strings.read_only_note,
            Some(DocKind::Sheets) if shell.active_sheet().is_some() => "",
            Some(DocKind::Writer) => strings.plain_text_note,
            Some(DocKind::Markdown) => strings.markdown_note,
            Some(_) => strings.placeholder_view,
            None => "",
        }
        .into(),
    );

    // Zoom exists on Writer and Markdown tabs; file details on Markdown tabs only.
    match shell.active_content() {
        Some(Content::Writer(view)) => {
            ui.set_status_zoom(format!("{}%", view.zoom_percent).into());
            ui.set_zoom_enabled(true);
            ui.set_status_format(SharedString::new());
            ui.set_status_notice(SharedString::new());
        }
        Some(Content::Markdown(editor)) => {
            ui.set_status_zoom(format!("{}%", editor.zoom_percent).into());
            ui.set_zoom_enabled(true);
            ui.set_status_format(format_label(&editor.format).into());
            ui.set_format_utf8(editor.format.encoding == Encoding::Utf8);
            ui.set_format_crlf(editor.format.line_ending == aster_markdown::LineEnding::CrLf);
            let notice = if let Some(reason) = &editor.record_failed {
                i18n::fill(strings.notice_record_failed, "reason", reason)
            } else if let Some(warning) = editor.disk_warning {
                match warning {
                    DiskWarning::Changed => strings.notice_disk_changed.to_owned(),
                    DiskWarning::Missing => strings.notice_disk_missing.to_owned(),
                    DiskWarning::ChangedWhileOpen => strings.notice_disk_changed_open.to_owned(),
                    DiskWarning::MissingWhileOpen => strings.notice_disk_missing_open.to_owned(),
                }
            } else if editor.reloaded {
                strings.notice_reloaded.to_owned()
            } else if editor.reopened {
                i18n::fill(
                    strings.notice_reopened,
                    "encoding",
                    editor.format.encoding.label(),
                )
            } else if editor.format_changed {
                i18n::fill(
                    strings.notice_format_changed,
                    "format",
                    &format_label(&editor.format),
                )
            } else if editor.find.replacing {
                strings.notice_replacing.to_owned()
            } else if let Some(count) = editor.find.replaced {
                i18n::fill(strings.notice_replaced, "count", &group_digits(count))
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
        Some(Content::Sheet(tab)) => {
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_format("XLSX".into());
            let mut notice = match &tab.calc {
                office::Calc::Running => strings.calc_running.to_owned(),
                office::Calc::Done(0) => String::new(),
                office::Calc::Done(count) => {
                    i18n::fill(strings.calc_done, "count", &group_digits(*count))
                }
                office::Calc::Failed(reason) => i18n::fill(strings.calc_failed, "reason", reason),
            };
            // Excel shows 0 in a circular reference and says where it is.
            if let Some(&(sheet, at)) = tab.circular.first() {
                let name = tab
                    .view
                    .workbook()
                    .sheets
                    .get(sheet)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                notice = i18n::fill(strings.calc_circular, "cell", &format!("{name}!{at}"));
            }
            match &tab.refusal {
                Some(office::Refusal::Holds(what)) => {
                    let what = blocker_label(strings, what);
                    notice = i18n::fill(strings.lines_refused, "what", &what);
                }
                Some(office::Refusal::Full) => notice = strings.lines_full.to_owned(),
                Some(office::Refusal::LastSheet) => notice = strings.sheet_last.to_owned(),
                Some(office::Refusal::SheetHolds(what)) => {
                    let what = blocker_label(strings, what);
                    notice = i18n::fill(strings.sheet_holds, "what", &what);
                }
                None => {
                    if let Some(count) = tab.find.replaced {
                        notice = i18n::fill(strings.notice_replaced, "count", &group_digits(count));
                    }
                }
            }
            ui.set_status_notice(notice.into());
        }
        Some(Content::Slides(_)) => {
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_format("PPTX".into());
            ui.set_status_notice(SharedString::new());
        }
        None => {
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_format(SharedString::new());
            ui.set_status_notice(SharedString::new());
        }
    }

    if let Some(tab) = shell.active_sheet() {
        let cells = tab.view.sheet().map_or(0, |sheet| sheet.cell_count());
        ui.set_status_words(i18n::fill(strings.cell_count, "count", &group_digits(cells)).into());
        ui.set_status_chars(SharedString::new());
        ui.set_status_ime(SharedString::new());
        ui.set_status_composing(false);
    } else if let Some(tab) = shell.active_slides() {
        let hidden = tab
            .view
            .presentation()
            .slides
            .get(tab.view.current())
            .is_some_and(|slide| slide.hidden);
        let mut position = if tab.view.count() == 0 {
            String::new()
        } else {
            i18n::fill(
                &i18n::fill(
                    strings.slide_position,
                    "index",
                    &(tab.view.current() + 1).to_string(),
                ),
                "count",
                &tab.view.count().to_string(),
            )
        };
        if hidden {
            position = format!("{position} · {}", strings.slide_hidden);
        }
        ui.set_status_words(position.into());
        ui.set_status_chars(SharedString::new());
        ui.set_status_ime(SharedString::new());
        ui.set_status_composing(false);
    } else {
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
    first_preview_at_once(shell, width, height, scale);
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

/// The first time the preview of a small document is shown, lay it out here
/// and now: the worker would leave the pane empty for a moment.
fn first_preview_at_once(shell: &mut Shell, width: u16, height: u16, scale: f32) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let style = content::preview_style(&shell.tokens, shell.language.strings());
    let Some(mut builder) = shell.preview_builder.take() else {
        return;
    };
    if let Some(editor) = shell.markdown(id)
        && editor.preview.layout().is_none()
        && editor.document.len() <= PREVIEW_AT_ONCE
    {
        editor.preview.resize(width, height, scale);
        if let Some(job) = editor.preview_job(u64::from(id.0), true, style) {
            builder.set_base(job.base.clone());
            let layout = builder.build(&job.source, job.width, job.scale, &job.style);
            editor.show_preview(layout);
        }
    }
    shell.preview_builder = Some(builder);
}

/// Hand the preview of the tab in front to the worker if it is out of date.
/// Changes to the text count only when `text_changes` is set.
fn request_preview(shell: &mut Shell, text_changes: bool) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let style = content::preview_style(&shell.tokens, shell.language.strings());
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
    refresh_sidebar(ui, shell);
    present(ui, shell);
    if shell.active_surface().is_some() && shell.pending.is_none() {
        ui.invoke_focus_document();
    }
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
        let (can_undo, can_redo) = undo_state(shell);
        if (can_undo, can_redo) != (ui.get_can_undo(), ui.get_can_redo()) {
            refresh_toolbar(ui, shell);
        }
        schedule_preview(ui, shell);
        schedule_outline(ui, shell);
        schedule_count(ui, shell);
        schedule_search(ui, shell);
        schedule_highlight(ui, shell);
    }
    if shell.active_sheet().is_some() {
        refresh_viewer(ui, shell);
    }
    follow_selection(ui, shell);
    refresh_status(ui, shell);
    present(ui, shell);
}

/// Whether the document in front can undo and redo.
fn undo_state(shell: &mut Shell) -> (bool, bool) {
    match shell.active_content() {
        Some(Content::Markdown(editor)) => (editor.document.can_undo(), editor.document.can_redo()),
        Some(Content::Sheet(tab)) => (tab.history.can_undo(), tab.history.can_redo()),
        _ => (false, false),
    }
}

/// Save a picture from the clipboard beside the document in front, on a
/// worker thread, then put a reference to it at the caret.
fn paste_picture(
    ui: &AppWindow,
    shell: &mut Shell,
    picture: aster_platform::clipboard::ClipboardPicture,
) {
    let strings = shell.language.strings();
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(document) = shell.markdown(id).and_then(|editor| editor.path.clone()) else {
        show_dialog(
            ui,
            shell,
            2,
            strings.paste_picture_title.to_owned(),
            strings.paste_picture_body.to_owned(),
        );
        return;
    };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let saved =
            files::save_pasted_picture(&document, &stamp, picture.extension, &picture.bytes);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| match saved {
                    Ok(path) => {
                        let Some(editor) = shell.markdown(id) else {
                            return;
                        };
                        let reference = files::picture_reference(Some(&document), &path);
                        let at = editor.document.selection().range();
                        let end = at.start + reference.len();
                        if editor
                            .apply_external(at, &reference, aster_markdown::Selection::caret(end))
                            .is_ok()
                        {
                            after_edit(ui, shell, true);
                        }
                    }
                    Err(err) => {
                        let strings = shell.language.strings();
                        show_dialog(
                            ui,
                            shell,
                            2,
                            strings.paste_picture_failed.to_owned(),
                            err.to_string(),
                        );
                    }
                }),
            );
        });
    });
}

/// Body text size of an exported PDF, in points.
const PDF_FONT_SIZE: f32 = 10.5;

/// Documents up to this size get their outline worked out at once, as they
/// are typed; larger ones once typing pauses.
const OUTLINE_AT_ONCE: usize = 256 * 1024;
/// Larger documents have no outline: reading all of their headings on the
/// interface thread would hold it up.
const OUTLINE_LIMIT: usize = 16 * 1024 * 1024;
const OUTLINE_DELAY: Duration = Duration::from_millis(400);

/// Bring the sidebar's outline up to date with the document in front: at
/// once for small documents, after a pause in typing for large ones.
fn schedule_outline(ui: &AppWindow, shell: &mut Shell) {
    let active = shell.tabs.active_id();
    let state = active.and_then(|id| {
        shell
            .markdown(id)
            .map(|editor| (id, editor.text_version(), editor.document.len()))
    });
    let Some((id, version, len)) = state else {
        shell.outline.clear();
        shell.outline_of = None;
        ui.set_has_outline(false);
        ui.set_outline(model(Vec::new()));
        return;
    };
    ui.set_has_outline(true);
    if shell.outline_of == Some((id, version)) {
        mark_outline(ui, shell);
        return;
    }
    if len <= OUTLINE_AT_ONCE || shell.outline_of.is_none_or(|(shown, _)| shown != id) {
        build_outline(ui, shell);
        return;
    }
    let weak = ui.as_weak();
    shell
        .outline_timer
        .start(TimerMode::SingleShot, OUTLINE_DELAY, move || {
            if let Some(ui) = weak.upgrade() {
                deliver(ui, Box::new(build_outline));
            }
        });
}

/// One step of scrolling the preview while a selection is dragged past its
/// edge: faster the farther the pointer is past it.
fn drag_scroll_preview(ui: &AppWindow, shell: &mut Shell) {
    let Some((x, y)) = shell.preview_drag_at else {
        shell.drag_timer.stop();
        return;
    };
    let Some(editor) = shell.active_markdown() else {
        shell.preview_drag_at = None;
        shell.drag_timer.stop();
        return;
    };
    let height = f64::from(editor.preview.size().1);
    let past = if y < 0.0 { y } else { y - height };
    let step = past.signum() * past.abs().clamp(8.0, 120.0);
    if editor.scroll_preview(step) {
        editor.preview_drag(x, y);
        present(ui, shell);
    }
}

fn build_outline(ui: &AppWindow, shell: &mut Shell) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    let version = editor.text_version();
    let headings = if editor.document.len() <= OUTLINE_LIMIT {
        aster_markdown::structure::headings(&editor.document.text().to_string())
    } else {
        Vec::new()
    };
    shell.outline = headings;
    shell.outline_of = Some((id, version));
    mark_outline(ui, shell);
}

/// Show the outline with the heading the caret is under marked.
fn mark_outline(ui: &AppWindow, shell: &mut Shell) {
    let caret = shell
        .active_markdown()
        .map_or(0, |editor| editor.document.selection().focus);
    let current = shell
        .outline
        .partition_point(|heading| heading.offset <= caret)
        .checked_sub(1);
    let items: Vec<OutlineItem> = shell
        .outline
        .iter()
        .enumerate()
        .map(|(index, heading)| OutlineItem {
            level: i32::from(heading.level),
            text: heading.text.as_str().into(),
            current: Some(index) == current,
        })
        .collect();
    ui.set_outline(model(items));
}

/// Rebuild the toolbar if the paragraph style field or the pressed format
/// buttons no longer match the selection.
fn follow_selection(ui: &AppWindow, shell: &mut Shell) {
    let shown = shell
        .active_markdown()
        .map_or((0, Styles::default()), |editor| {
            (editor.heading_level(), editor.styles())
        });
    if shown != (shell.shown_heading, shell.shown_styles) {
        refresh_toolbar(ui, shell);
    }
    schedule_outline(ui, shell);
}

/// The formatting a toolbar command stands for in a Markdown tab.
fn format_of(command: Command) -> Option<Format> {
    use aster_markdown::format::{Block, Inline};
    Some(match command {
        Command::Bold => Format::Inline(Inline::Bold),
        Command::Italic => Format::Inline(Inline::Italic),
        Command::Strikethrough => Format::Inline(Inline::Strike),
        Command::InlineCode => Format::Inline(Inline::Code),
        Command::BulletList => Format::Block(Block::Bullet),
        Command::NumberedList => Format::Block(Block::Numbered),
        Command::Checklist => Format::Block(Block::Task),
        Command::Quote => Format::Block(Block::Quote),
        Command::InsertLink => Format::Link,
        Command::InsertImage => Format::Image,
        Command::InsertTable => Format::Table,
        _ => return None,
    })
}

/// The formatting a Format menu code stands for; see `format` in
/// `ui/app.slint`.
fn format_of_menu(code: i32) -> Option<Format> {
    let command = match code {
        0 => Command::Bold,
        1 => Command::Italic,
        2 => Command::Strikethrough,
        3 => Command::InlineCode,
        4 => Command::BulletList,
        5 => Command::NumberedList,
        6 => Command::Checklist,
        7 => Command::Quote,
        8 => Command::InsertLink,
        9 => Command::InsertImage,
        10 => Command::InsertTable,
        11 => return Some(Format::CodeBlock),
        12 => return Some(Format::Rule),
        13 => return Some(Format::Toc),
        14 => return Some(Format::Footnote),
        20..=26 => return Some(Format::Heading((code - 20) as u8)),
        30..=38 => {
            use aster_markdown::table::TableEdit;
            return Some(Format::TableEdit(match code {
                30 => TableEdit::RowAbove,
                31 => TableEdit::RowBelow,
                32 => TableEdit::ColumnLeft,
                33 => TableEdit::ColumnRight,
                34 => TableEdit::DeleteRow,
                35 => TableEdit::DeleteColumn,
                36 => TableEdit::AlignLeft,
                37 => TableEdit::AlignCenter,
                _ => TableEdit::AlignRight,
            }));
        }
        _ => return None,
    };
    format_of(command)
}

/// Carry out a formatting command on the Markdown tab in front.
fn apply_format(ui: &AppWindow, shell: &mut Shell, format: Format) {
    if matches!(format, Format::Link | Format::Image) {
        open_link_dialog(ui, shell, format == Format::Image);
        return;
    }
    let changed = shell
        .active_markdown()
        .is_some_and(|editor| editor.format(format));
    if changed {
        after_edit(ui, shell, true);
        refresh_toolbar(ui, shell);
    }
}

/// Open the link dialog: on the link or image around the selection to
/// change it, or else for a new one, with the selected text as its text (or
/// as its address, if it looks like one).
fn open_link_dialog(ui: &AppWindow, shell: &mut Shell, image: bool) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    if !editor.view_mode.shows_source() || editor.locked() {
        return;
    }
    let text = editor.document.text();
    let selection = editor.document.selection();
    let (range, label, address, image) = match aster_markdown::format::link_around(text, selection)
    {
        Some(link) => (link.range, link.label, link.address, link.image),
        None => {
            let range = selection.range();
            let selected = if range.len() <= 4096 {
                aster_markdown::document::slice(text, range.clone())
            } else {
                String::new()
            };
            let selected = if selected.contains('\n') {
                String::new()
            } else {
                selected
            };
            if aster_markdown::format::is_address(&selected) {
                (range, String::new(), selected.trim().to_owned(), image)
            } else {
                (range, selected, String::new(), image)
            }
        }
    };
    let version = editor.text_version();
    shell.link_edit = Some((id, range, version, image));
    ui.set_link_image(image);
    ui.set_link_label(label.into());
    ui.set_link_address(address.into());
    ui.set_link_open(true);
}

/// OK in the link dialog: write the link, one undo step. Nothing is written
/// if the document changed while the dialog was open.
fn accept_link(ui: &AppWindow, shell: &mut Shell) {
    ui.set_link_open(false);
    let Some((id, range, version, image)) = shell.link_edit.take() else {
        return;
    };
    let label = ui.get_link_label().to_string();
    let address = ui.get_link_address().to_string();
    if let Some(editor) = shell.markdown(id)
        && editor.text_version() == version
        && !(label.trim().is_empty() && address.trim().is_empty())
    {
        let edit = aster_markdown::format::write_link(range, &label, &address, image);
        if editor
            .apply_external(edit.range, &edit.text, edit.selection)
            .is_ok()
        {
            after_edit(ui, shell, true);
            refresh_toolbar(ui, shell);
        }
    }
    ui.invoke_focus_document();
}

/// Shortcuts the shell handles before the document sees the key.
/// A key while the preview has the keyboard: arrows, Page Up and Down,
/// space and Home and End scroll it, Cmd+A selects all of it, Cmd+C copies
/// (also through the menu), Esc drops the selection. Returns whether the key
/// was used. Typing does nothing: the preview is not editable.
fn preview_key(editor: &mut MarkdownEditor, text: &str, shift: bool, primary: bool) -> bool {
    use slint::platform::Key;

    let mut chars = text.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return false;
    };
    let is = |k: Key| char::from(k) == key;
    let view = editor.preview.view_height();
    let scale = f64::from(editor.preview.size().2);
    let line = 40.0 * scale.max(1.0);
    let page = (view - line).max(line);
    if primary {
        if is(Key::UpArrow) {
            editor.scroll_preview_to(0.0);
            return true;
        }
        if is(Key::DownArrow) {
            editor.scroll_preview_to(f64::MAX);
            return true;
        }
        return match key.to_ascii_lowercase() {
            'a' => {
                editor.preview.select_all();
                true
            }
            _ => false,
        };
    }
    if is(Key::UpArrow) {
        editor.scroll_preview(-line);
    } else if is(Key::DownArrow) {
        editor.scroll_preview(line);
    } else if is(Key::PageUp) || (key == ' ' && shift) {
        editor.scroll_preview(-page);
    } else if is(Key::PageDown) || key == ' ' {
        editor.scroll_preview(page);
    } else if is(Key::Home) {
        editor.scroll_preview_to(0.0);
    } else if is(Key::End) {
        editor.scroll_preview_to(f64::MAX);
    } else if is(Key::Escape) {
        editor.preview.clear_selection();
    } else {
        // Typing into a preview does nothing; let it fall through quietly.
        return true;
    }
    true
}

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

/// What the open dialog offers: everything at once, then each kind.
fn open_file_types(strings: &'static i18n::Strings) -> [FileType; 4] {
    [
        FileType {
            name: strings.file_type_supported,
            extensions: SUPPORTED_EXTENSIONS,
        },
        FileType {
            name: strings.file_type_text,
            extensions: TEXT_EXTENSIONS,
        },
        FileType {
            name: strings.file_type_spreadsheet,
            extensions: office::SHEET_EXTENSIONS,
        },
        FileType {
            name: strings.file_type_presentation,
            extensions: office::SLIDE_EXTENSIONS,
        },
    ]
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

// --- Files and folders: the recent list and the sidebar's tree -------------

fn recent_item(row: &sidebar::RecentRow) -> RecentItem {
    RecentItem {
        title: row.title.as_str().into(),
        kind: row.kind.index() as i32,
        location: row.location.as_str().into(),
        opened: row.opened.as_str().into(),
        missing: row.missing,
    }
}

/// Bring the recent lists and the tree on screen up to date.
fn refresh_sidebar(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let now = Local::now();
    let home = sidebar::home_folder();
    let all = sidebar::recent_rows(strings, &shell.recent, None, now, home.as_deref());
    let short: Vec<_> = match &shell.filter {
        Some(filter) => {
            sidebar::recent_rows(strings, &shell.recent, Some(filter), now, home.as_deref())
        }
        None => all.iter().take(SIDEBAR_RECENT).cloned().collect(),
    };
    ui.set_recent(model(all.iter().map(recent_item).collect()));
    ui.set_sidebar_recent(model(short.iter().map(recent_item).collect()));
    shell.start_recent = all.into_iter().map(|row| row.path).collect();
    shell.sidebar_recent = short.into_iter().map(|row| row.path).collect();
    ui.set_sidebar_filtering(shell.filter.is_some());

    // The file in front is marked in the tree.
    let current = shell
        .active_markdown()
        .and_then(|editor| editor.path.clone())
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    ui.set_current_has_file(current.is_some());
    let Some(tree) = &shell.tree else {
        ui.set_folder_name(SharedString::new());
        ui.set_tree(model(Vec::new()));
        shell.tree_rows.clear();
        return;
    };
    ui.set_folder_name(files::display_name(tree.root()).into());
    let rows = tree.rows(shell.filter.as_ref());
    let items = sidebar::tree_lines(strings, &rows)
        .into_iter()
        .map(|line| {
            let mut item = TreeItem {
                name: line.label.into(),
                depth: i32::try_from(line.depth).unwrap_or(i32::MAX),
                ..TreeItem::default()
            };
            match line.look {
                Look::Folder { expanded } => {
                    item.kind = -1;
                    item.expanded = expanded;
                }
                Look::Loop => item.kind = -2,
                Look::File { kind, opens } => {
                    item.kind = kind.index() as i32;
                    item.opens = opens;
                    item.current = current.as_deref() == Some(line.path.as_path());
                }
                Look::Note { problem } => {
                    item.kind = -3;
                    item.problem = problem;
                }
            }
            item
        })
        .collect();
    ui.set_tree(model(items));
    shell.tree_rows = rows;
}

/// Read the recent list and the remembered folder on a worker thread, and
/// show them when they arrive.
fn spawn_workspace_load(ui: &AppWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let recent = aster_platform::paths::recent_files()
            .map(|file| RecentFiles::load(&file))
            .unwrap_or_else(|| Ok(RecentFiles::default()));
        let state = aster_platform::paths::workspace_state()
            .map(|file| WorkspaceState::load(&file))
            .unwrap_or_else(|| Ok(WorkspaceState::default()));
        let found = recent
            .as_ref()
            .map(|recent| aster_workspace::recent::present(recent.paths()))
            .unwrap_or_default();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    finish_workspace_load(ui, shell, recent, state, &found);
                }),
            );
        });
    });
}

fn finish_workspace_load(
    ui: &AppWindow,
    shell: &mut Shell,
    recent: std::io::Result<RecentFiles>,
    state: std::io::Result<WorkspaceState>,
    found: &[(PathBuf, bool)],
) {
    match recent {
        Ok(mut loaded) => {
            loaded.set_missing(found);
            // Files opened since start-up go on top of what was stored.
            let opened_since = std::mem::take(&mut shell.recent);
            for file in opened_since.files().iter().rev() {
                loaded.add(&file.path, file.opened);
            }
            shell.recent = loaded;
            shell.recent_loaded = true;
            if !opened_since.is_empty() {
                shell.save_recent();
            }
        }
        // Not overwritten: it may be readable next time.
        Err(err) => eprintln!("aster-shell: cannot read the recent-files list: {err}"),
    }
    match state {
        Ok(state) => {
            shell.workspace_loaded = true;
            // A folder opened since start-up wins.
            if shell.tree.is_none()
                && let Some(folder) = state.folder
            {
                let wanted = shell.open_folder(&folder, state.expanded);
                read_folders(ui, shell, wanted);
            }
        }
        Err(err) => eprintln!("aster-shell: cannot read the sidebar state: {err}"),
    }
    refresh_sidebar(ui, shell);
}

/// Read folders of the tree on a worker thread, one after another, and show
/// each as it arrives.
fn read_folders(ui: &AppWindow, shell: &Shell, dirs: Vec<PathBuf>) {
    let Some(tree) = &shell.tree else {
        return;
    };
    if dirs.is_empty() {
        return;
    }
    let root = tree.root().to_owned();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        for dir in dirs {
            let result = read_folder(&dir);
            let root = root.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        let Some(tree) = shell.tree.as_mut().filter(|tree| tree.root() == root)
                        else {
                            return;
                        };
                        tree.finished(dir, result);
                        // Expanded folders inside it are now on screen.
                        let wanted = tree.wanted();
                        read_folders(ui, shell, wanted);
                        refresh_sidebar(ui, shell);
                    }),
                );
            });
        }
    });
}

/// Look at the disk again: the folders on screen and whether the recent
/// files are still there. The folder is not watched; this runs when the
/// window comes back to the front and from the refresh button.
/// Look at the files behind open Markdown documents, on another thread, for
/// changes another program made. Done when the window comes to the front.
fn check_disk(ui: &AppWindow, shell: &mut Shell) {
    if shell.checking_disk || shell.pending.is_some() {
        return;
    }
    let jobs: Vec<(TabId, PathBuf, Fingerprint)> = shell
        .contents
        .iter()
        .filter_map(|(id, content)| match content {
            // A document already warned about is left alone: saving asks
            // where to anyway.
            Content::Markdown(editor) if !editor.saving && editor.disk_warning.is_none() => {
                Some((*id, editor.path.clone()?, editor.disk?))
            }
            _ => None,
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    shell.checking_disk = true;
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found: Vec<(TabId, Fingerprint, files::OnDisk)> = jobs
            .into_iter()
            .map(|(id, path, disk)| (id, disk, files::look_again(&path, &disk)))
            .collect();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| finish_check_disk(ui, shell, found)),
            );
        });
    });
}

/// Documents larger than this are not replaced by what is on disk without
/// asking: the undo step would hold the whole old text.
const RELOAD_QUIETLY: usize = 64 * 1024 * 1024;

fn finish_check_disk(
    ui: &AppWindow,
    shell: &mut Shell,
    found: Vec<(TabId, Fingerprint, files::OnDisk)>,
) {
    shell.checking_disk = false;
    let mut ask = None;
    for (id, looked_at, outcome) in found {
        let Some(editor) = shell.markdown(id) else {
            continue;
        };
        // Saved or warned about while the files were being looked at.
        if editor.saving || editor.disk != Some(looked_at) || editor.disk_warning.is_some() {
            continue;
        }
        match outcome {
            files::OnDisk::Unchanged => {}
            files::OnDisk::Missing => editor.disk_warning = Some(DiskWarning::MissingWhileOpen),
            files::OnDisk::Changed(Ok(loaded))
                if !editor.is_modified()
                    && editor.document.len() <= RELOAD_QUIETLY
                    && loaded.decoded.text.len() <= RELOAD_QUIETLY =>
            {
                if editor.reload(loaded).is_err() {
                    // Busy with an input method or a replacement: next time.
                    continue;
                }
                if shell.tabs.active_id() == Some(id) {
                    after_edit(ui, shell, true);
                } else {
                    shell.sync_modified();
                    shell.sync_records();
                }
            }
            files::OnDisk::Changed(Ok(loaded)) if ask.is_none() => ask = Some((id, loaded)),
            // Not readable as text any more, or a second document to ask
            // about: saving it asks where to.
            files::OnDisk::Changed(_) => {
                editor.disk_warning = Some(DiskWarning::ChangedWhileOpen);
            }
        }
    }
    refresh_tabs(ui, shell);
    refresh_status(ui, shell);
    if let Some((id, loaded)) = ask {
        if ui.get_dialog_kind() != 0 || shell.pending.is_some() {
            if let Some(editor) = shell.markdown(id) {
                editor.disk_warning = Some(DiskWarning::ChangedWhileOpen);
            }
            refresh_status(ui, shell);
            return;
        }
        let name = shell
            .tabs
            .get(id)
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        if let Some(index) = shell.tabs.index_of(id) {
            shell.activate(index);
            after_tab_change(ui, shell);
        }
        shell.pending = Some(Pending::DiskChanged(id));
        shell.disk_loaded = Some((id, loaded));
        let strings = shell.language.strings();
        show_dialog(
            ui,
            shell,
            5,
            i18n::fill(strings.disk_changed_title, "name", &name),
            strings.disk_changed_body.to_owned(),
        );
    }
}

/// Replace a document's text with what its file holds now, after the user
/// chose to, or read again in another encoding (`reopened`).
fn reload_document(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    loaded: files::Loaded,
    reopened: bool,
) {
    let Some(editor) = shell.markdown(id) else {
        return;
    };
    if editor.reload(loaded).is_err() {
        editor.disk_warning = Some(DiskWarning::ChangedWhileOpen);
    } else if reopened {
        editor.reloaded = false;
        editor.reopened = true;
    }
    after_edit(ui, shell, true);
    refresh_tabs(ui, shell);
}

fn refresh_workspace(ui: &AppWindow, shell: &mut Shell) {
    if let Some(tree) = &mut shell.tree {
        let again = tree.reread();
        read_folders(ui, shell, again);
    }
    if shell.recent_loaded && !shell.recent.is_empty() {
        let paths = shell.recent.paths();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let found = aster_workspace::recent::present(paths);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        if shell.recent.set_missing(&found) {
                            refresh_sidebar(ui, shell);
                        }
                    }),
                );
            });
        });
    }
    // "Today" may have become "yesterday".
    refresh_sidebar(ui, shell);
}

/// Show a file or folder in Finder or the file manager, on a worker thread;
/// say so if that fails.
fn reveal(ui: &AppWindow, path: PathBuf) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let Err(err) = aster_platform::reveal::show_in_file_manager(&path) else {
            return;
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    let strings = shell.language.strings();
                    let title =
                        i18n::fill(strings.reveal_failed, "name", &files::display_name(&path));
                    show_dialog(ui, shell, 2, title, err.to_string());
                }),
            );
        });
    });
}

/// A recent file was clicked.
fn open_recent(ui: &AppWindow, shell: &mut Shell, path: PathBuf) {
    if let Some(id) = shell.tab_of_file(&path) {
        shell.tabs.activate(id);
        after_tab_change(ui, shell);
        return;
    }
    // Looked at again now: the list may be minutes old.
    let present = path.is_file();
    if shell.recent.set_missing(&[(path.clone(), present)]) {
        refresh_sidebar(ui, shell);
    }
    if present {
        start_load(ui, shell, path);
        return;
    }
    let strings = shell.language.strings();
    let title = i18n::fill(
        strings.recent_missing_title,
        "name",
        &files::display_name(&path),
    );
    let folder = path
        .parent()
        .map(|folder| folder.to_string_lossy().into_owned())
        .unwrap_or_default();
    let body = i18n::fill(strings.recent_missing_body, "path", &folder);
    show_dialog(ui, shell, 4, title, body);
    shell.pending = Some(Pending::MissingRecent(path));
}

/// A file or folder was dropped on the window: a folder goes in the
/// sidebar, a file opens in a tab (or its tab comes to the front). Ignored
/// while a question is on screen, which the new tab would hide.
fn open_dropped(ui: &AppWindow, shell: &mut Shell, path: PathBuf) {
    if ui.get_dialog_kind() != 0 || !shell.candidates.is_empty() {
        return;
    }
    // A picture dropped on a Markdown document whose source is on screen
    // goes into the document, at the caret, on a line of its own.
    if files::is_picture(&path)
        && let Some(editor) = shell.active_markdown()
        && editor.view_mode.shows_source()
    {
        let reference = files::picture_reference(editor.path.as_deref(), &path);
        let caret = editor.document.selection().focus;
        let text = editor.document.text();
        let line_start = text.line_to_byte(text.byte_to_line(caret));
        let line_end = if text.byte_to_line(caret) + 1 < text.len_lines() {
            text.line_to_byte(text.byte_to_line(caret) + 1) - 1
        } else {
            text.len_bytes()
        };
        let inserted = if caret == line_start && caret == line_end {
            reference
        } else {
            format!("\n{reference}\n")
        };
        let at = if caret == line_start && caret == line_end {
            caret
        } else {
            line_end
        };
        let end = at + inserted.len();
        if editor
            .apply_external(at..at, &inserted, aster_markdown::Selection::caret(end))
            .is_ok()
        {
            after_edit(ui, shell, true);
            ui.invoke_focus_document();
        }
        return;
    }
    match files::dropped(&path) {
        Dropped::Folder => {
            let wanted = shell.open_folder(&path, Vec::new());
            read_folders(ui, shell, wanted);
            ui.set_sidebar_open(true);
            refresh_sidebar(ui, shell);
        }
        Dropped::File => match shell.tab_of_file(&path) {
            Some(id) => {
                shell.tabs.activate(id);
                after_tab_change(ui, shell);
            }
            None => start_load(ui, shell, path),
        },
        Dropped::Unsupported => {
            let strings = shell.language.strings();
            let name = files::display_name(&path);
            let title = i18n::fill(strings.unsupported_title, "name", &name);
            show_dialog(ui, shell, 2, title, strings.unsupported_body.to_owned());
        }
    }
}

/// A line of the tree was clicked.
fn open_tree_row(ui: &AppWindow, shell: &mut Shell, row: Row) {
    let strings = shell.language.strings();
    match row.kind {
        RowKind::Folder { .. } => {
            let Some(tree) = &mut shell.tree else {
                return;
            };
            // The second click of a double click.
            if shell
                .last_toggle
                .as_ref()
                .is_some_and(|(path, at)| *path == row.path && at.elapsed() < DOUBLE_CLICK)
            {
                return;
            }
            shell.last_toggle = Some((row.path.clone(), Instant::now()));
            tree.toggle(&row.path);
            let wanted = tree.wanted();
            shell.save_workspace();
            read_folders(ui, shell, wanted);
            refresh_sidebar(ui, shell);
        }
        RowKind::Loop => {
            let title = i18n::fill(strings.loop_title, "name", &row.name);
            show_dialog(ui, shell, 2, title, strings.loop_body.to_owned());
        }
        RowKind::File(kind) if kind.opens() => match shell.tab_of_file(&row.path) {
            Some(id) => {
                shell.tabs.activate(id);
                after_tab_change(ui, shell);
            }
            None => start_load(ui, shell, row.path),
        },
        RowKind::File(_) => {
            let title = i18n::fill(strings.unsupported_title, "name", &row.name);
            show_dialog(ui, shell, 2, title, strings.unsupported_body.to_owned());
        }
        RowKind::Note(_) => {}
    }
}

/// Ask for a folder and show it in the sidebar.
fn choose_folder(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    // The shell is not borrowed while the system dialog is open.
    let Ok((strings, start)) = shell.try_borrow().map(|shell| {
        (
            shell.language.strings(),
            shell.tree.as_ref().map(|tree| tree.root().to_owned()),
        )
    }) else {
        return;
    };
    let weak = ui.as_weak();
    // Never block on a dialog inside an event handler: see `dialogs`.
    let _ = slint::spawn_local(async move {
        let Some(folder) = dialogs::pick_folder(strings.open_folder_dialog, start.as_deref()).await
        else {
            return;
        };
        let Some(ui) = weak.upgrade() else {
            return;
        };
        deliver(
            ui.clone_strong(),
            Box::new(move |ui, shell| {
                let wanted = shell.open_folder(&folder, Vec::new());
                read_folders(ui, shell, wanted);
                ui.set_sidebar_open(true);
                refresh_sidebar(ui, shell);
            }),
        );
    });
}

// --- Find and replace --------------------------------------------------------

/// What the find bar says about the matches.
fn find_position(strings: &i18n::Strings, editor: &MarkdownEditor) -> String {
    let find = &editor.find;
    if find.error.is_some() {
        return strings.find_bad_pattern.to_owned();
    }
    if find.searching {
        return strings.find_counting.to_owned();
    }
    if find.query.pattern.is_empty() {
        return String::new();
    }
    if find.found.is_empty() {
        return strings.find_none.to_owned();
    }
    let count = group_digits(find.found.len());
    let count = if find.found.truncated {
        i18n::fill(strings.find_too_many, "count", &count)
    } else {
        count
    };
    let index = find
        .current
        .map_or_else(|| "–".to_owned(), |index| group_digits(index + 1));
    i18n::fill(
        &i18n::fill(strings.find_position, "index", &index),
        "count",
        &count,
    )
}

/// Push the find bar's state into the window.
fn refresh_find(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    if let Some(tab) = shell.active_sheet() {
        let find = &tab.find;
        ui.set_find_open(find.open);
        ui.set_find_replace(find.replace_open);
        ui.set_find_case(find.query.case_sensitive);
        ui.set_find_whole(find.query.whole_word);
        ui.set_find_regex(find.query.regex);
        ui.set_find_error(find.error.is_some());
        ui.set_find_busy(false);
        ui.set_find_position(sheet_find_position(strings, find).into());
        if ui.get_find_query().as_str() != find.query.pattern {
            ui.set_find_query(find.query.pattern.as_str().into());
        }
        if ui.get_find_replacement().as_str() != find.replacement {
            ui.set_find_replacement(find.replacement.as_str().into());
        }
        return;
    }
    let Some(editor) = shell.active_markdown() else {
        ui.set_find_open(false);
        return;
    };
    let find = &editor.find;
    ui.set_find_open(find.open);
    ui.set_find_replace(find.replace_open);
    ui.set_find_case(find.query.case_sensitive);
    ui.set_find_whole(find.query.whole_word);
    ui.set_find_regex(find.query.regex);
    ui.set_find_error(find.error.is_some());
    ui.set_find_busy(find.replacing);
    ui.set_find_position(find_position(strings, editor).into());
    if ui.get_find_query().as_str() != find.query.pattern {
        ui.set_find_query(find.query.pattern.as_str().into());
    }
    if ui.get_find_replacement().as_str() != find.replacement {
        ui.set_find_replacement(find.replacement.as_str().into());
    }
}

/// Where the find bar is on a workbook: "3 / 12", none, or a bad pattern.
fn sheet_find_position(strings: &i18n::Strings, find: &office::SheetFind) -> String {
    if find.error.is_some() {
        return strings.find_bad_pattern.to_owned();
    }
    if find.query.pattern.is_empty() {
        return String::new();
    }
    if find.matches.is_empty() {
        return strings.find_none.to_owned();
    }
    let count = group_digits(find.matches.len());
    let count = if find.truncated {
        i18n::fill(strings.find_too_many, "count", &count)
    } else {
        count
    };
    let index = find
        .current
        .map_or_else(|| "–".to_owned(), |index| group_digits(index + 1));
    i18n::fill(
        &i18n::fill(strings.find_position, "index", &index),
        "count",
        &count,
    )
}

/// After the find bar acted on a workbook: the cells it changed are
/// recalculated, the selection and the bar are shown.
fn after_sheet_find(ui: &AppWindow, shell: &mut Shell, changes: Vec<aster_sheets::Change>) {
    if !changes.is_empty() && shell.sync_modified() {
        refresh_tabs(ui, shell);
    }
    refresh_find(ui, shell);
    after_sheet_change(ui, shell, changes);
}

/// After the find bar's state changed: redraw and say so.
fn after_find(ui: &AppWindow, shell: &mut Shell, job: Option<SearchJob>) {
    if let Some(job) = job {
        spawn_search(ui, shell, job);
    }
    refresh_find(ui, shell);
    refresh_status(ui, shell);
    present(ui, shell);
}

/// Search a large document on a worker thread.
fn spawn_search(ui: &AppWindow, shell: &mut Shell, job: SearchJob) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let (generation, found) = job.run();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    if let Some(editor) = shell.markdown(id) {
                        editor.search_finished(generation, found);
                    }
                    if shell.tabs.active_id() == Some(id) {
                        refresh_find(ui, shell);
                        present(ui, shell);
                    }
                }),
            );
        });
    });
}

/// Replace throughout a large document on a worker thread.
fn spawn_replace(ui: &AppWindow, shell: &mut Shell, job: ReplaceJob) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let replaced = job.run();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    let changed = shell
                        .markdown(id)
                        .is_some_and(|editor| editor.replace_all_finished(replaced));
                    if shell.tabs.active_id() == Some(id) {
                        after_edit(ui, shell, changed);
                        refresh_find(ui, shell);
                    } else if changed {
                        shell.sync_records();
                    }
                }),
            );
        });
    });
}

/// The text changed with the find bar open: search again once typing pauses.
fn schedule_search(ui: &AppWindow, shell: &mut Shell) {
    if !shell
        .active_markdown()
        .is_some_and(|editor| editor.find.open && editor.find.stale && !editor.find.replacing)
    {
        return;
    }
    let weak = ui.as_weak();
    shell.find_timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(200),
        move || {
            if let Some(ui) = weak.upgrade() {
                deliver(
                    ui,
                    Box::new(|ui, shell| {
                        let job = shell.active_markdown().and_then(|editor| {
                            (editor.find.open && editor.find.stale)
                                .then(|| editor.refresh_search())?
                        });
                        after_find(ui, shell, job);
                    }),
                );
            }
        },
    );
}

/// Rebuild the highlighting of the tab in front on another thread after a
/// change too large to highlight incrementally.
fn schedule_highlight(ui: &AppWindow, shell: &mut Shell) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some((version, text)) = shell.markdown(id).and_then(MarkdownEditor::highlight_job) else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let highlighter = aster_markdown::highlight::Highlighter::new(&text);
        drop(text);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    if let Some(editor) = shell.markdown(id) {
                        editor.highlighted(version, highlighter);
                    }
                    // The text may have changed again meanwhile.
                    schedule_highlight(ui, shell);
                    present(ui, shell);
                }),
            );
        });
    });
}

/// Read a file on a worker thread and open it in a tab when it arrives.
fn spawn_load(ui: &AppWindow, path: PathBuf) {
    deliver(
        ui.clone_strong(),
        Box::new(move |ui, shell| start_load(ui, shell, path)),
    );
}

/// [`spawn_load`] with the shell at hand. A file that is being read already
/// is not read twice.
fn start_load(ui: &AppWindow, shell: &mut Shell, path: PathBuf) {
    start_load_as(ui, shell, path, None);
}

/// Open a file, in `encoding` if the user chose one.
fn start_load_as(ui: &AppWindow, shell: &mut Shell, path: PathBuf, encoding: Option<Encoding>) {
    if shell.opening.contains(&path) {
        return;
    }
    // Say so in the status bar while it takes: a large file takes seconds.
    // Noted before the reading starts, so that it is never noted after the
    // file has arrived.
    shell.opening.push(path.clone());
    refresh_status(ui, shell);
    let weak = ui.as_weak();
    if office::kind_of(&path).is_some() {
        std::thread::spawn(move || {
            let result = office::load(&path);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| finish_office_load(ui, shell, path, result)),
                );
            });
        });
        return;
    }
    std::thread::spawn(move || {
        // Reading, decoding and preparing a large file all happen here.
        let result = match encoding {
            Some(encoding) => files::load_as(&path, encoding),
            None => files::load(&path),
        }
        .map(content::open);
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
                if candidate.workbook.is_some() {
                    continue;
                }
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
            shell.remember_file(&path);
            shell.open_file(path, opened);
            after_tab_change(ui, shell);
        }
        Err(err) => {
            let strings = shell.language.strings();
            let reason = match err {
                LoadError::OtherEncoding(readings) => {
                    let title = i18n::fill(
                        strings.encoding_open_title,
                        "name",
                        &files::display_name(&path),
                    );
                    show_encoding_dialog(
                        ui,
                        shell,
                        EncodingFor::Open(path),
                        readings,
                        title,
                        strings.encoding_open_body.to_owned(),
                    );
                    return;
                }
                err => load_problem(strings, &err),
            };
            let title = i18n::fill(strings.open_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, reason);
        }
    }
}

/// Why a file could not be read as text, in the interface language.
fn load_problem(strings: &i18n::Strings, err: &LoadError) -> String {
    match err {
        LoadError::Decode(DecodeError::NotUtf8 { .. }) | LoadError::OtherEncoding(_) => {
            strings.not_utf8.to_owned()
        }
        LoadError::Decode(DecodeError::Malformed { encoding, offset }) => i18n::fill(
            &i18n::fill(strings.malformed_in, "encoding", encoding.label()),
            "offset",
            &group_digits(*offset),
        ),
        LoadError::Io(err) => err.to_string(),
    }
}

/// What the one-line question is about.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Prompt {
    /// Go to a line of this document.
    GoTo(TabId),
    /// A new name for this file or folder.
    Rename(PathBuf),
    /// A new name for a sheet of this workbook.
    RenameSheet(TabId, usize),
}

/// What stops rows, columns or a sheet from changing, as a user would say
/// it: the file names it by its part or element (`vmlDrawing`, `extLst`).
fn blocker_label(strings: &i18n::Strings, what: &str) -> String {
    match what {
        "table" | "tableParts" | "queryTable" => strings.blocker_table.to_owned(),
        "comments" | "vmlDrawing" | "legacyDrawing" | "threadedComment" | "person" => {
            strings.blocker_comments.to_owned()
        }
        "pivotTable" | "pivotCacheDefinition" => strings.blocker_pivot.to_owned(),
        "chart" => strings.blocker_chart.to_owned(),
        "autoFilter" | "sortState" => strings.blocker_filter.to_owned(),
        "extLst" => strings.blocker_newer.to_owned(),
        "oleObjects" | "oleObject" | "controls" | "ctrlProp" | "package" | "control" => {
            strings.blocker_objects.to_owned()
        }
        other => i18n::fill(strings.blocker_other, "name", other),
    }
}

/// Why a sheet cannot have the name typed.
fn sheet_name_problem(strings: &i18n::Strings, problem: &aster_sheets::NameProblem) -> String {
    use aster_sheets::NameProblem;
    match problem {
        NameProblem::Empty => strings.sheet_name_empty.to_owned(),
        NameProblem::TooLong => strings.sheet_name_long.to_owned(),
        NameProblem::Character(c) => {
            i18n::fill(strings.sheet_name_character, "char", &c.to_string())
        }
        NameProblem::Taken => strings.sheet_name_taken.to_owned(),
        NameProblem::Reserved => strings.sheet_name_reserved.to_owned(),
    }
}

/// Ask for one line of text.
fn show_prompt(
    ui: &AppWindow,
    shell: &mut Shell,
    prompt: Prompt,
    title: String,
    label: String,
    text: String,
) {
    shell.prompt = Some(prompt);
    shell.leave_active_document();
    stop_platform_composition(ui);
    ui.set_prompt_title(title.into());
    ui.set_prompt_label(label.into());
    ui.set_prompt_problem(SharedString::new());
    ui.set_prompt_text(text.into());
    ui.set_prompt_open(true);
    present(ui, shell);
}

fn hide_prompt(ui: &AppWindow, shell: &mut Shell) {
    shell.prompt = None;
    ui.set_prompt_open(false);
    if shell.active_surface().is_some() {
        ui.invoke_focus_document();
    }
}

/// The folder a new file or folder goes in from a row of the tree: the
/// folder itself, or the one the file is in.
fn folder_of_row(row: &Row) -> Option<PathBuf> {
    match row.kind {
        RowKind::Folder { .. } => Some(row.path.clone()),
        _ => row.path.parent().map(Path::to_owned),
    }
}

/// New Markdown File, New Folder, Rename and Move to Trash from the tree.
fn tree_file_action(ui: &AppWindow, shell: &mut Shell, action: i32, row: Row) {
    let strings = shell.language.strings();
    match action {
        2 | 3 => {
            let Some(dir) = folder_of_row(&row) else {
                return;
            };
            let created = if action == 2 {
                aster_platform::files::create_untitled_file(&dir, strings.untitled_file_base, "md")
            } else {
                aster_platform::files::create_untitled_folder(&dir, strings.untitled_folder_base)
            };
            match created {
                Ok(path) => {
                    if let Some(tree) = &mut shell.tree
                        && dir != tree.root()
                    {
                        tree.expand(&dir);
                        shell.save_workspace();
                    }
                    refresh_workspace(ui, shell);
                    if action == 2 {
                        start_load(ui, shell, path);
                    } else {
                        let name = files::display_name(&path);
                        show_prompt(
                            ui,
                            shell,
                            Prompt::Rename(path),
                            i18n::fill(strings.rename_title, "name", &name),
                            strings.rename_label.to_owned(),
                            name,
                        );
                    }
                }
                Err(err) => {
                    let title = i18n::fill(
                        strings.file_op_failed_create,
                        "name",
                        &files::display_name(&dir),
                    );
                    show_dialog(ui, shell, 2, title, sidebar::create_problem(strings, &err));
                }
            }
        }
        4 => {
            let name = files::display_name(&row.path);
            show_prompt(
                ui,
                shell,
                Prompt::Rename(row.path),
                i18n::fill(strings.rename_title, "name", &name),
                strings.rename_label.to_owned(),
                name,
            );
        }
        5 => {
            let name = files::display_name(&row.path);
            let folder = matches!(row.kind, RowKind::Folder { .. });
            let title = i18n::fill(
                &i18n::fill(strings.trash_question, "name", &name),
                "action",
                sidebar::trash_label(strings),
            );
            let body = if folder {
                strings.trash_body_folder
            } else {
                strings.trash_body_file
            };
            shell.pending = Some(Pending::Trash(row.path));
            show_dialog(ui, shell, 7, title, body.to_owned());
        }
        _ => {}
    }
}

/// Whether a document kept at `path`, or in the folder `path`, is being
/// saved. Renaming then would have the save land under the old name.
fn saving_under(shell: &Shell, path: &Path) -> bool {
    shell.contents.values().any(|content| match content {
        Content::Markdown(editor) => {
            editor.saving
                && editor
                    .path
                    .as_deref()
                    .is_some_and(|kept| kept.starts_with(path))
        }
        Content::Sheet(tab) => {
            tab.saving
                && tab
                    .path
                    .as_deref()
                    .is_some_and(|kept| kept.starts_with(path))
        }
        _ => false,
    })
}

/// Give `from` the name the user typed. Open documents in it follow it.
fn rename_entry(ui: &AppWindow, shell: &mut Shell, from: PathBuf) {
    let strings = shell.language.strings();
    if saving_under(shell, &from) {
        ui.set_prompt_problem(strings.rename_while_saving.into());
        return;
    }
    let typed = ui.get_prompt_text().to_string();
    match aster_platform::files::rename(&from, typed.trim()) {
        Ok(to) => {
            hide_prompt(ui, shell);
            let ids: Vec<TabId> = shell.contents.keys().copied().collect();
            for id in ids {
                // Workbooks follow too.
                if let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id)
                    && let Some(rest) = tab.path.as_deref().and_then(|p| p.strip_prefix(&from).ok())
                {
                    let moved = if rest.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(rest)
                    };
                    tab.path = Some(moved.clone());
                    shell.tabs.rename(id, files::display_name(&moved));
                    continue;
                }
                let Some(editor) = shell.markdown(id) else {
                    continue;
                };
                let Some(rest) = editor
                    .path
                    .as_deref()
                    .and_then(|p| p.strip_prefix(&from).ok())
                else {
                    continue;
                };
                let moved = if rest.as_os_str().is_empty() {
                    to.clone()
                } else {
                    to.join(rest)
                };
                editor.path = Some(moved.clone());
                shell.tabs.rename(id, files::display_name(&moved));
            }
            let from_real = std::fs::canonicalize(from.parent().unwrap_or(&from))
                .map(|dir| dir.join(from.file_name().unwrap_or_default()))
                .unwrap_or_else(|_| from.clone());
            if shell.recent.remove(&from_real) || shell.recent.remove(&from) {
                shell.remember_file(&to);
            }
            refresh_workspace(ui, shell);
            refresh_tabs(ui, shell);
            refresh_status(ui, shell);
        }
        Err(err) => ui.set_prompt_problem(sidebar::rename_problem(strings, &err).into()),
    }
}

/// Move a file or folder to the system's trash, on another thread.
fn trash_entry(ui: &AppWindow, path: PathBuf) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = aster_platform::files::move_to_trash(&path);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| {
                    refresh_workspace(ui, shell);
                    match result {
                        // Open documents of it are marked gone.
                        Ok(()) => check_disk(ui, shell),
                        Err(err) => {
                            let strings = shell.language.strings();
                            let title = i18n::fill(
                                strings.file_op_failed_trash,
                                "name",
                                &files::display_name(&path),
                            );
                            show_dialog(ui, shell, 2, title, sidebar::trash_problem(strings, &err));
                        }
                    }
                }),
            );
        });
    });
}

/// What the encoding question is about.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EncodingFor {
    /// A file being opened that is not UTF-8.
    Open(PathBuf),
    /// The file behind an open document, to read again.
    Reopen(TabId, PathBuf),
}

fn show_encoding_dialog(
    ui: &AppWindow,
    shell: &mut Shell,
    target: EncodingFor,
    readings: Vec<files::Reading>,
    title: String,
    body: String,
) {
    let current = match &target {
        EncodingFor::Reopen(id, _) => shell.markdown(*id).map(|editor| editor.format.encoding),
        EncodingFor::Open(_) => None,
    };
    let selected = current
        .and_then(|current| readings.iter().position(|r| r.encoding == current))
        .unwrap_or(0);
    let choices: Vec<EncodingChoice> = readings
        .iter()
        .map(|reading| EncodingChoice {
            label: reading.encoding.label().into(),
            preview: reading.preview.clone().into(),
        })
        .collect();
    shell.encoding_for = Some((target, readings.iter().map(|r| r.encoding).collect()));
    shell.leave_active_document();
    stop_platform_composition(ui);
    ui.set_encoding_title(title.into());
    ui.set_encoding_body(body.into());
    ui.set_encoding_choices(model(choices));
    ui.set_encoding_selected(selected as i32);
    ui.set_encoding_open(true);
    present(ui, shell);
}

/// The user picked an encoding.
fn accept_encoding(ui: &AppWindow, shell: &mut Shell) {
    ui.set_encoding_open(false);
    let Some((target, encodings)) = shell.encoding_for.take() else {
        return;
    };
    let Some(encoding) = usize::try_from(ui.get_encoding_selected())
        .ok()
        .and_then(|at| encodings.get(at).copied())
    else {
        return;
    };
    match target {
        EncodingFor::Open(path) => start_load_as(ui, shell, path, Some(encoding)),
        EncodingFor::Reopen(id, path) => {
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let result = files::load_as(&path, encoding);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    deliver(
                        ui,
                        Box::new(move |ui, shell| match result {
                            Ok(loaded) => reload_document(ui, shell, id, loaded, true),
                            Err(err) => {
                                let strings = shell.language.strings();
                                let title = i18n::fill(
                                    strings.open_failed,
                                    "name",
                                    &files::display_name(&path),
                                );
                                show_dialog(ui, shell, 2, title, load_problem(strings, &err));
                            }
                        }),
                    );
                });
            });
        }
    }
}

fn finish_office_load(
    ui: &AppWindow,
    shell: &mut Shell,
    path: PathBuf,
    result: Result<office::Loaded, office::LoadError>,
) {
    if let Some(at) = shell.opening.iter().position(|opening| opening == &path) {
        shell.opening.remove(at);
    }
    match result {
        Ok(loaded) => {
            shell.remember_file(&path);
            let id = shell.open_office(path, loaded);
            start_calc(ui, shell, id);
            after_tab_change(ui, shell);
        }
        Err(err) => {
            refresh_status(ui, shell);
            let strings = shell.language.strings();
            let title = i18n::fill(strings.open_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, err.message(strings));
        }
    }
}

/// Start a workbook's calculation thread. Until the first results arrive
/// the grid shows the values saved in the file.
fn start_calc(ui: &AppWindow, shell: &mut Shell, id: TabId) {
    let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
        return;
    };
    let weak = ui.as_weak();
    tab.start_calc(move |message| {
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| finish_calc(ui, shell, id, message)),
            );
        });
    });
}

fn finish_calc(ui: &AppWindow, shell: &mut Shell, id: TabId, message: office::CalcMessage) {
    // The tab may have been closed meanwhile.
    let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
        return;
    };
    tab.take_calc(message);
    if shell.tabs.active_id() == Some(id) {
        refresh_viewer(ui, shell);
        refresh_status(ui, shell);
        present(ui, shell);
    }
}

/// What the chrome around a viewer shows: the formula bar and sheet tabs of
/// a workbook, the note over an empty presentation.
fn refresh_viewer(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    if let Some(tab) = shell.active_sheet() {
        let view = &tab.view;
        let visible = view.visible_sheets();
        let names: Vec<SharedString> = visible
            .iter()
            .map(|&i| view.workbook().sheets[i].name.as_str().into())
            .collect();
        let current = visible.iter().position(|&i| i == view.sheet_index());
        let (address, content) = view.selection_text();
        let note = match view.sheet().map(|sheet| sheet.kind) {
            Some(aster_sheets::SheetKind::Chart) => strings.chart_sheet,
            _ => "",
        };
        ui.set_viewer(1);
        // A new model rebuilds the tabs, and a double-click whose first
        // click switched sheets would land on a new tab: only when the
        // names change.
        let shown = ui.get_sheet_names();
        if shown.row_count() != names.len() || shown.iter().zip(&names).any(|(a, b)| a != *b) {
            ui.set_sheet_names(model(names));
        }
        ui.set_sheet_index(current.map_or(-1, |i| i as i32));
        ui.set_cell_address(address.into());
        ui.set_cell_content(content.into());
        ui.set_viewer_note(note.into());
        return;
    }
    if let Some(tab) = shell.active_slides() {
        let note = if tab.view.count() == 0 {
            strings.no_slides
        } else {
            ""
        };
        let notes = tab
            .view
            .presentation()
            .slides
            .get(tab.view.current())
            .and_then(|slide| slide.notes.clone())
            .unwrap_or_default();
        ui.set_viewer(2);
        ui.set_viewer_note(note.into());
        ui.set_slide_notes(notes.into());
        return;
    }
    ui.set_viewer(0);
    ui.set_viewer_note(SharedString::new());
    ui.set_slide_notes(SharedString::new());
}

/// After cells changed (or only the selection moved, with no changes):
/// bring the formula bar, the status bar and the frame up to date.
fn after_sheet_change(ui: &AppWindow, shell: &mut Shell, changes: Vec<aster_sheets::Change>) {
    if let Some(tab) = shell.active_sheet() {
        tab.recalc(&changes);
    }
    shell.sync_sheet_records();
    if !changes.is_empty() && shell.sync_modified() {
        refresh_tabs(ui, shell);
    }
    let look = shell.active_sheet().map(|tab| tab.cell_look());
    if undo_state(shell) != (ui.get_can_undo(), ui.get_can_redo())
        || look.is_some_and(|look| look != shell.shown_cell)
    {
        refresh_toolbar(ui, shell);
    }
    refresh_viewer(ui, shell);
    refresh_status(ui, shell);
    present(ui, shell);
}

/// The toolbar commands that act on a workbook's cells.
fn sheet_formats(command: Command) -> bool {
    command != Command::Undo
        && command != Command::Redo
        && command != Command::NumberFormat
        && command.is_available(DocKind::Sheets)
}

/// Whether a key's text is something to type rather than a control key.
fn is_typed(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c))
}

/// A key on a workbook: typing into a cell, moving and stretching the
/// selection, clearing it. Returns whether the key was used.
fn sheet_key_pressed(
    ui: &AppWindow,
    shell: &mut Shell,
    text: &str,
    shift: bool,
    primary: bool,
    alt: bool,
) -> bool {
    use slint::platform::Key;

    let language = shell.language.tag();
    let Some(tab) = shell.active_sheet() else {
        return false;
    };
    let is = |key: Key| text.chars().eq(std::iter::once(char::from(key)));
    let backtab = is(Key::Backtab);
    let tab_key = is(Key::Tab) || backtab;
    if let Some(mode) = tab.view.editor().map(|editor| editor.mode) {
        let edit_mode = mode == EditMode::Edit;
        let changed = if is(Key::Escape) {
            tab.view.cancel_edit()
        } else if is(Key::Return) && alt {
            tab.view.edit(|editor| editor.insert("\n")).is_some()
        } else if is(Key::Return) || tab_key {
            let changes = tab.commit(language);
            if let Some(tab) = shell.active_sheet() {
                tab.view.step(tab_key, shift || backtab);
            }
            after_sheet_change(ui, shell, changes);
            return true;
        } else if (is(Key::LeftArrow) || is(Key::RightArrow)) && edit_mode {
            let left = is(Key::LeftArrow);
            tab.view
                .edit(|editor| if left { editor.left() } else { editor.right() })
                .unwrap_or(false)
        } else if is(Key::UpArrow)
            || is(Key::DownArrow)
            || is(Key::LeftArrow)
            || is(Key::RightArrow)
        {
            if edit_mode {
                return true;
            }
            // Typing a new entry: the arrows keep it and move on.
            let changes = tab.commit(language);
            if let Some(tab) = shell.active_sheet()
                && let Some(key) = viewer_key(text).and_then(|k| office::sheet_key(k, false, false))
            {
                tab.view.key(key);
            }
            after_sheet_change(ui, shell, changes);
            return true;
        } else if is(Key::Home) {
            tab.view.edit(|editor| editor.home()).unwrap_or(false)
        } else if is(Key::End) {
            tab.view.edit(|editor| editor.end()).unwrap_or(false)
        } else if is(Key::Backspace) {
            tab.view.edit(|editor| editor.backspace()).unwrap_or(false)
        } else if is(Key::Delete) {
            tab.view.edit(|editor| editor.delete()).unwrap_or(false)
        } else if is_typed(text) && !primary {
            tab.view.edit(|editor| editor.insert(text)).is_some()
        } else {
            return false;
        };
        if changed {
            refresh_viewer(ui, shell);
            present(ui, shell);
        }
        return true;
    }
    if primary && !tab.view.is_editing() {
        let command = match text {
            "b" | "B" => Some(Command::Bold),
            "i" | "I" => Some(Command::Italic),
            "u" | "U" => Some(Command::Underline),
            _ => None,
        };
        if let Some(command) = command {
            if shell.run(command) {
                // The pressed buttons follow, as after a toolbar click.
                refresh_toolbar(ui, shell);
                after_edit(ui, shell, true);
            }
            return true;
        }
    }
    let Some(tab) = shell.active_sheet() else {
        return false;
    };
    if is(Key::F2) {
        tab.view.begin_edit(EditMode::Edit, None);
    } else if is(Key::Return) || tab_key {
        tab.view.step(tab_key, shift || backtab);
    } else if is(Key::Backspace) || is(Key::Delete) {
        let changes = tab.clear_selection();
        after_sheet_change(ui, shell, changes);
        return true;
    } else if let Some(key) = viewer_key(text).and_then(|k| office::sheet_key(k, shift, primary)) {
        tab.view.key_extending(key, shift);
    } else if is_typed(text) && !primary {
        tab.view.begin_edit(EditMode::Enter, Some(text.to_owned()));
    } else {
        return false;
    }
    after_sheet_change(ui, shell, Vec::new());
    true
}

/// A key on a viewer, decoded from what Slint reports.
fn viewer_key(text: &str) -> Option<office::Key> {
    use slint::platform::Key;

    let mut chars = text.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let is = |k: Key| char::from(k) == key;
    Some(if is(Key::UpArrow) {
        office::Key::Up
    } else if is(Key::DownArrow) {
        office::Key::Down
    } else if is(Key::LeftArrow) {
        office::Key::Left
    } else if is(Key::RightArrow) {
        office::Key::Right
    } else if is(Key::PageUp) {
        office::Key::PageUp
    } else if is(Key::PageDown) {
        office::Key::PageDown
    } else if is(Key::Home) {
        office::Key::Home
    } else if is(Key::End) {
        office::Key::End
    } else if is(Key::Tab) || key == '\u{19}' {
        office::Key::Tab
    } else if is(Key::Return) {
        office::Key::Enter
    } else if key == ' ' {
        office::Key::Space
    } else {
        return None;
    })
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
    result: Result<Fingerprint, files::SaveError>,
    after: AfterSave,
) {
    shell.finish_save(id, &path, point, result.as_ref().ok().copied());
    if result.is_ok() {
        shell.remember_file(&path);
        refresh_sidebar(ui, shell);
    }
    match result {
        Ok(_) if after == AfterSave::CloseTab => {
            shell.force_close(id);
            after_tab_change(ui, shell);
        }
        Ok(_) => {
            refresh_chrome(ui, shell);
            present(ui, shell);
        }
        Err(files::SaveError::Encode { error, line }) => {
            let strings = shell.language.strings();
            let encoding = shell
                .markdown(id)
                .map_or("", |editor| editor.format.encoding.label());
            let name = files::display_name(&path);
            let title = i18n::fill(
                &i18n::fill(strings.save_unencodable_title, "name", &name),
                "encoding",
                encoding,
            );
            let body = i18n::fill(
                &i18n::fill(
                    &i18n::fill(strings.save_unencodable_body, "char", &error.ch.to_string()),
                    "line",
                    &group_digits(line + 1),
                ),
                "encoding",
                encoding,
            );
            shell.pending = Some(Pending::SaveInUtf8 { id, path, after });
            show_dialog(ui, shell, 6, title, body);
        }
        Err(err) => {
            let strings = shell.language.strings();
            let title = i18n::fill(strings.save_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, err.to_string());
        }
    }
}

fn finish_sheet_save(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    job: office::SheetSave,
    path: PathBuf,
    result: Result<Arc<Vec<u8>>, String>,
    after: AfterSave,
) {
    if let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) {
        tab.finish_save(&job, &path, result.as_ref().ok().cloned());
    }
    // Saved: the record goes, or says where the workbook is now.
    shell.sync_sheet_records();
    match result {
        Ok(_) => {
            shell.tabs.rename(id, files::display_name(&path));
            shell.sync_modified();
            shell.remember_file(&path);
            refresh_sidebar(ui, shell);
            if after == AfterSave::CloseTab {
                shell.force_close(id);
                after_tab_change(ui, shell);
            } else {
                refresh_chrome(ui, shell);
                present(ui, shell);
            }
        }
        Err(reason) => {
            let strings = shell.language.strings();
            let title = i18n::fill(strings.save_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, reason);
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
            let is_sheet = matches!(
                shell
                    .try_borrow()
                    .ok()
                    .and_then(|s| s.contents.get(&id).map(|c| matches!(c, Content::Sheet(_)))),
                Some(true)
            );
            let (weak, shell) = (ui.as_weak(), shell.clone());
            let _ = slint::spawn_local(async move {
                let sheet_types = [FileType {
                    name: strings.file_type_spreadsheet,
                    extensions: &["xlsx"],
                }];
                let text_types = text_file_types(strings);
                let types: &[FileType] = if is_sheet { &sheet_types } else { &text_types };
                let chosen =
                    dialogs::pick_file_to_save(strings.save_dialog, &suggested, types).await;
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
    if matches!(shell.contents.get(&id), Some(Content::Sheet(_))) {
        start_sheet_save(ui, &mut shell, id, path, after);
        return;
    }
    if let Some(job) = shell.begin_save(id, path) {
        spawn_save(ui, job, after);
    }
}

/// Write a workbook to `path` on a worker thread.
fn start_sheet_save(ui: &AppWindow, shell: &mut Shell, id: TabId, path: PathBuf, after: AfterSave) {
    {
        let language = shell.language.tag();
        let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
            return;
        };
        // What is being typed goes in first, as Excel does.
        if tab.view.is_editing() {
            let changes = tab.commit(language);
            tab.recalc(&changes);
        }
        if let Some(job) = tab.begin_save() {
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let result = job.write(&path);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    deliver(
                        ui,
                        Box::new(move |ui, shell| {
                            finish_sheet_save(ui, shell, id, job, path, result, after)
                        }),
                    );
                });
            });
        }
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
    // Workbook records are written at most every few seconds; this writes
    // the last change of a burst once it is old enough.
    let record_tick = Timer::default();
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

    // The recent list and the sidebar's folder: written on a thread of their
    // own, read once at start-up.
    shell.borrow_mut().store = Some(Store::spawn(|file, err| {
        eprintln!("aster-shell: cannot write {}: {err}", file.display());
    }));
    spawn_workspace_load(&ui);

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
    // And one for first previews on this thread, made where it does not
    // hold up the window: loading the system's font list takes a while.
    {
        let weak = ui.as_weak();
        let style = {
            let shell = shell.borrow();
            content::preview_style(&shell.tokens, shell.language.strings())
        };
        std::thread::spawn(move || {
            let mut builder = PreviewBuilder::new();
            builder.build("# 预热\n\n中文、日本語、English。", 600, 2.0, &style);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |_, shell| shell.preview_builder = Some(builder)),
                );
            });
        });
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
            match event {
                // Only where a drop would be taken: not over a question.
                winit::event::WindowEvent::HoveredFile(_) => {
                    if let Some(ui) = weak.upgrade() {
                        let asking =
                            ui.get_dialog_kind() != 0 || ui.get_recovery_items().row_count() > 0;
                        ui.set_drop_hover(!asking);
                    }
                    return EventResult::Propagate;
                }
                winit::event::WindowEvent::HoveredFileCancelled => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_drop_hover(false);
                    }
                    return EventResult::Propagate;
                }
                // One event per file. Handled once the shell is free.
                winit::event::WindowEvent::DroppedFile(path) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_drop_hover(false);
                        let path = path.clone();
                        deliver(ui, Box::new(move |ui, shell| open_dropped(ui, shell, path)));
                    }
                    return EventResult::Propagate;
                }
                _ => {}
            }
            if let winit::event::WindowEvent::Focused(true) = event {
                if let (Some(ui), Ok(mut shell)) = (weak.upgrade(), shell.try_borrow_mut()) {
                    refresh_workspace(&ui, &mut shell);
                    check_disk(&ui, &mut shell);
                }
                return EventResult::Propagate;
            }
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
                Some(Content::Sheet(tab)) => {
                    if tab.view.ime(event) {
                        refresh_viewer(&ui, shell);
                        present(&ui, shell);
                    }
                    return EventResult::PreventDefault;
                }
                _ => return EventResult::Propagate,
            };
            after_edit(&ui, shell, text_changed);
            EventResult::PreventDefault
        }
    });

    ui.on_key_pressed(handler!(|ui, shell, text, shift, primary, alt| {
        let zooms = shell.active_writer().is_some() || shell.active_markdown().is_some();
        if zooms && let Some(command) = shell_shortcut(text.as_str(), primary) {
            if shell.run(command) {
                refresh_toolbar(&ui, shell);
                schedule_preview(&ui, shell);
                after_edit(&ui, shell, false);
            }
            return true;
        }
        // Esc in the document closes the find bar, as it does in the bar.
        if text.as_str() == "\u{1b}"
            && let Some(editor) = shell.active_markdown()
            && editor.find.open
        {
            editor.cancel_replace_all();
            editor.close_find();
            after_find(&ui, shell, None);
            return true;
        }
        if let Some(editor) = shell.active_markdown()
            && editor.preview_has_focus()
        {
            let handled = preview_key(editor, text.as_str(), shift, primary);
            if handled {
                present(&ui, shell);
            }
            return handled;
        }
        if shell.active_sheet().is_some() {
            return sheet_key_pressed(&ui, shell, text.as_str(), shift, primary, alt);
        }
        if shell.active_slides().is_some() {
            let Some(key) = viewer_key(text.as_str()) else {
                return false;
            };
            let moved = if let Some(tab) = shell.active_slides() {
                office::slide_key(&mut tab.view, key, shift)
            } else {
                false
            };
            if moved {
                refresh_viewer(&ui, shell);
                refresh_status(&ui, shell);
                present(&ui, shell);
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
            _ => return false,
        };
        after_edit(&ui, shell, text_changed);
        true
    }));

    ui.on_pointer_down(handler!(|ui, shell, x, y, shift| {
        let language = shell.language.tag();
        let clicks = shell
            .clicks
            .press(f64::from(x), f64::from(y), Instant::now());
        let scale = f64::from(ui.window().scale_factor());
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        match shell.active_content() {
            Some(Content::Writer(view)) => view.surface.pointer_down(x, y, shift),
            Some(Content::Markdown(editor)) => editor.pointer_press(x, y, shift, clicks),
            Some(Content::Sheet(tab)) => {
                if tab.begin_resize(x, y) || tab.begin_fill(x, y) {
                    present(&ui, shell);
                    return;
                }
                let Some(tab) = shell.active_sheet() else {
                    return;
                };
                // A click elsewhere keeps what was being typed.
                let changes = if tab.view.is_editing() {
                    tab.commit(language)
                } else {
                    Vec::new()
                };
                let Some(tab) = shell.active_sheet() else {
                    return;
                };
                tab.view.press(x, y, shift);
                after_sheet_change(&ui, shell, changes);
                return;
            }
            Some(Content::Slides(tab)) => {
                if tab.view.press(x, y) {
                    refresh_viewer(&ui, shell);
                    refresh_status(&ui, shell);
                    present(&ui, shell);
                }
                return;
            }
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
            Some(Content::Sheet(tab)) if tab.is_resizing() => {
                if tab.drag_resize(x, y) {
                    present(&ui, shell);
                }
                return;
            }
            Some(Content::Sheet(tab)) if tab.is_filling() => {
                if tab.drag_fill(x, y) {
                    present(&ui, shell);
                }
                return;
            }
            Some(Content::Sheet(tab)) => {
                if !tab.view.is_editing() && tab.view.drag(x, y) {
                    refresh_viewer(&ui, shell);
                    present(&ui, shell);
                }
                return;
            }
            _ => return,
        }
        follow_selection(&ui, shell);
        present(&ui, shell);
    }));

    ui.on_pointer_up(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet()
            && tab.is_filling()
        {
            let changes = tab.end_fill();
            if shell.sync_modified() {
                refresh_tabs(&ui, shell);
            }
            after_sheet_change(&ui, shell, changes);
            return;
        }
        if let Some(tab) = shell.active_sheet()
            && tab.end_resize()
        {
            after_sheet_change(&ui, shell, Vec::new());
            if shell.sync_modified() {
                refresh_tabs(&ui, shell);
            }
            refresh_toolbar(&ui, shell);
        }
    }));

    ui.on_pointer_hover(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        let cursor = shell
            .active_sheet()
            .map_or(0, |tab| match tab.view.resize_hit(x, y) {
                Some(aster_surface::sheet_view::Resize::Column(_)) => 1,
                Some(aster_surface::sheet_view::Resize::Row(_)) => 2,
                None if tab.view.fill_handle_hit(x, y) => 3,
                None => 0,
            });
        if cursor != ui.get_resize_cursor() {
            ui.set_resize_cursor(cursor);
        }
    }));

    ui.on_pointer_double(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        if let Some(tab) = shell.active_sheet()
            && !tab.view.is_editing()
            && let Some(aster_surface::sheet_view::Hit::Cell(_)) = tab.view.hit(x, y)
        {
            tab.view.begin_edit(EditMode::Edit, None);
            refresh_viewer(&ui, shell);
            present(&ui, shell);
        }
    }));

    ui.on_formula_bar_clicked(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet()
            && !tab.view.is_editing()
        {
            tab.view.begin_edit(EditMode::Edit, None);
            refresh_viewer(&ui, shell);
            present(&ui, shell);
        }
    }));

    ui.on_scrolled(handler!(|ui, shell, delta_x, delta, pointer_x| {
        let scale = f64::from(ui.window().scale_factor());
        let (dx, dy) = (-f64::from(delta_x) * scale, -f64::from(delta) * scale);
        if let Some(tab) = shell.active_sheet() {
            if tab.view.scroll_by(dx, dy) {
                present(&ui, shell);
            }
            return;
        }
        if let Some(tab) = shell.active_slides() {
            if tab.view.wheel(f64::from(pointer_x) * scale, dy) {
                refresh_viewer(&ui, shell);
                refresh_status(&ui, shell);
                present(&ui, shell);
            }
            return;
        }
        if let Some(surface) = shell.active_surface()
            && surface.scroll_by(-f64::from(delta) * scale)
        {
            present(&ui, shell);
        }
    }));

    ui.on_scroll_to(handler!(|ui, shell, offset| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(surface) = shell.active_surface() {
            let (current, _) = surface.scroll_range();
            if surface.scroll_by(f64::from(offset) * scale - current) {
                present(&ui, shell);
            }
        }
    }));

    ui.on_preview_scroll_to(handler!(|ui, shell, offset| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(editor) = shell.active_markdown()
            && editor.scroll_preview_to(f64::from(offset) * scale)
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

    ui.on_preview_pressed(handler!(|ui, shell, x, y, shift| {
        let clicks = shell
            .clicks
            .press(f64::from(x), f64::from(y), Instant::now());
        let scale = f64::from(ui.window().scale_factor());
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        match clicks {
            _ if shift => editor.preview_press(x, y, true),
            2 => editor.preview_select(x, y, false),
            3 => editor.preview_select(x, y, true),
            _ => editor.preview_press(x, y, false),
        }
        present(&ui, shell);
    }));

    ui.on_preview_dragged(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let (x, y) = (f64::from(x) * scale, f64::from(y) * scale);
        editor.preview_drag(x, y);
        let height = f64::from(editor.preview.size().1);
        // Past the top or the bottom: keep scrolling while the button is
        // held there, as text views do.
        if y < 0.0 || y > height {
            shell.preview_drag_at = Some((x, y));
            if !shell.drag_timer.running() {
                let weak = ui.as_weak();
                shell
                    .drag_timer
                    .start(TimerMode::Repeated, Duration::from_millis(40), move || {
                        if let Some(ui) = weak.upgrade() {
                            deliver(ui, Box::new(drag_scroll_preview));
                        }
                    });
            }
        } else {
            shell.preview_drag_at = None;
            shell.drag_timer.stop();
        }
        present(&ui, shell);
    }));
    ui.on_preview_released(handler!(|_ui, shell| {
        shell.preview_drag_at = None;
        shell.drag_timer.stop();
    }));

    ui.on_preview_clicked(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        // A drag that selected text is not a click on a link.
        if editor
            .preview
            .selection()
            .is_some_and(|selection| !selection.is_empty())
        {
            return;
        }
        let Some(url) = editor
            .preview
            .link_at(f64::from(x) * scale, f64::from(y) * scale)
        else {
            return;
        };
        if url.starts_with('#') {
            if editor.follow_preview_link(&url) {
                present(&ui, shell);
            }
            return;
        }
        // A task box: tick it in the source.
        if let Some(item) = url
            .strip_prefix(aster_surface::preview::TASK_LINK)
            .and_then(|offset| offset.parse::<usize>().ok())
        {
            if editor.toggle_task(item) {
                after_edit(&ui, shell, true);
            }
            return;
        }
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
            let id = shell.open_untitled(kind);
            if kind == DocKind::Sheets {
                start_calc(&ui, shell, id);
            }
            after_tab_change(&ui, shell);
        }
    }));

    ui.on_recent_action(handler!(|ui, shell, action, from_sidebar, index| {
        let list = if from_sidebar {
            &shell.sidebar_recent
        } else {
            &shell.start_recent
        };
        let Some(path) = usize::try_from(index)
            .ok()
            .and_then(|i| list.get(i))
            .cloned()
        else {
            return;
        };
        match action {
            0 => open_recent(&ui, shell, path),
            1 => reveal(&ui, path),
            2 => {
                shell.recent.remove(&path);
                shell.save_recent();
                refresh_sidebar(&ui, shell);
            }
            _ => {}
        }
    }));

    ui.on_clear_recent(handler!(|ui, shell| {
        shell.recent.clear();
        shell.save_recent();
        refresh_sidebar(&ui, shell);
    }));

    ui.on_tree_action(handler!(|ui, shell, action, index| {
        // -1: the open folder itself, from the button beside its name.
        let row = if index == -1 {
            shell.tree.as_ref().map(|tree| Row {
                depth: 0,
                name: String::new(),
                path: tree.root().to_owned(),
                kind: RowKind::Folder { expanded: true },
            })
        } else {
            usize::try_from(index)
                .ok()
                .and_then(|i| shell.tree_rows.get(i))
                .cloned()
        };
        let Some(row) = row else {
            return;
        };
        match action {
            0 => open_tree_row(&ui, shell, row),
            1 => reveal(&ui, row.path),
            // Once the menu the command came from has closed: closing it
            // gives the keyboard back to where it was, which would take it
            // from the question this opens.
            _ => {
                let ui = ui.clone_strong();
                Timer::single_shot(Duration::ZERO, move || {
                    deliver(
                        ui,
                        Box::new(move |ui, shell| tree_file_action(ui, shell, action, row)),
                    );
                });
            }
        }
    }));

    ui.on_open_folder({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                choose_folder(&ui, &shell);
            }
        }
    });

    ui.on_refresh_folder(handler!(|ui, shell| {
        refresh_workspace(&ui, shell);
        check_disk(&ui, shell);
    }));

    ui.on_reveal_current(handler!(|ui, shell| {
        if let Some(path) = shell
            .active_markdown()
            .and_then(|editor| editor.path.clone())
        {
            reveal(&ui, path);
        }
    }));

    ui.on_sidebar_filter_edited(handler!(|ui, shell, text| {
        shell.filter = Filter::new(text.as_str());
        refresh_sidebar(&ui, shell);
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

    ui.on_sheet_selected(handler!(|ui, shell, index| {
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let visible = tab.view.visible_sheets();
        if let Some(&sheet) = usize::try_from(index).ok().and_then(|i| visible.get(i))
            && tab.view.set_sheet(sheet)
        {
            refresh_viewer(&ui, shell);
            refresh_status(&ui, shell);
            present(&ui, shell);
        }
    }));

    ui.on_set_language(handler!(|ui, shell, index| {
        if let Some(language) = usize::try_from(index).ok().and_then(Language::from_index) {
            // Titles and text of documents that are already open stay as they are.
            shell.language = language;
            let mode = shell.tokens.mode;
            shell.set_theme(mode);
            apply_language(&ui, language);
            refresh_chrome(&ui, shell);
            refresh_sidebar(&ui, shell);
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
                // After `present`, which tells the window the new view: the
                // keyboard goes to the preview when it is alone on screen.
                present(&ui, shell);
                ui.invoke_focus_document();
                return;
            }
            refresh_toolbar(&ui, shell);
            after_edit(&ui, shell, true);
        }
    }));

    ui.on_toolbar_option(handler!(|ui, shell, id, index| {
        // A colour from a colour button's palette.
        if let Some(command @ (Command::FillColor | Command::TextColor)) = Command::from_id(id)
            && let Some(color) = usize::try_from(index).ok().and_then(toolbar::palette_color)
        {
            let language = shell.language.tag();
            if let Some(tab) = shell.active_sheet() {
                let mut changes = if tab.view.is_editing() {
                    tab.commit(language)
                } else {
                    Vec::new()
                };
                let edit = if command == Command::FillColor {
                    aster_sheets::StyleEdit::Fill(color)
                } else {
                    aster_sheets::StyleEdit::FontColor(color)
                };
                changes.extend(tab.format(&edit));
                after_sheet_change(&ui, shell, changes);
            }
            return;
        }
        if Command::from_id(id) == Some(Command::NumberFormat)
            && let Ok(index) = usize::try_from(index)
        {
            let language = shell.language.tag();
            if let Some(tab) = shell.active_sheet() {
                let mut changes = if tab.view.is_editing() {
                    tab.commit(language)
                } else {
                    Vec::new()
                };
                changes.extend(tab.format(&office::number_format(index, language)));
                after_sheet_change(&ui, shell, changes);
            }
            return;
        }
        if Command::from_id(id) == Some(Command::Heading)
            && let Ok(level) = u8::try_from(index)
        {
            apply_format(&ui, shell, Format::Heading(level));
        }
    }));

    agent::Assistant::start(&ui, &mut shell.borrow_mut());
    ui.on_link_accept(handler!(|ui, shell| {
        accept_link(&ui, shell);
    }));
    ui.on_encoding_accept(handler!(|ui, shell| {
        accept_encoding(&ui, shell);
    }));
    ui.on_encoding_cancel(handler!(|ui, shell| {
        shell.encoding_for = None;
        ui.set_encoding_open(false);
        if shell.active_surface().is_some() {
            ui.invoke_focus_document();
        }
    }));
    ui.on_menu_reopen_encoding(handler!(|ui, shell| {
        let Some(id) = shell.tabs.active_id() else {
            return;
        };
        let Some(path) = shell.markdown(id).and_then(|editor| editor.path.clone()) else {
            return;
        };
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let readings = files::readings_of(&path);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| {
                        let strings = shell.language.strings();
                        let name = files::display_name(&path);
                        match readings {
                            Ok(readings) if !readings.is_empty() => show_encoding_dialog(
                                ui,
                                shell,
                                EncodingFor::Reopen(id, path),
                                readings,
                                strings.encoding_reopen_title.to_owned(),
                                i18n::fill(strings.encoding_reopen_body, "name", &name),
                            ),
                            Ok(_) => {
                                let title = i18n::fill(strings.open_failed, "name", &name);
                                show_dialog(ui, shell, 2, title, strings.not_utf8.to_owned());
                            }
                            Err(err) => {
                                let title = i18n::fill(strings.open_failed, "name", &name);
                                show_dialog(ui, shell, 2, title, err.to_string());
                            }
                        }
                    }),
                );
            });
        });
    }));
    ui.on_menu_convert_utf8(handler!(|ui, shell| {
        if let Some(editor) = shell.active_markdown() {
            editor.set_format(TextFormat {
                encoding: Encoding::Utf8,
                bom: false,
                ..editor.format
            });
        }
        after_edit(&ui, shell, true);
    }));
    ui.on_menu_line_endings(handler!(|ui, shell, crlf| {
        if let Some(editor) = shell.active_markdown() {
            let line_ending = if crlf {
                aster_markdown::LineEnding::CrLf
            } else {
                aster_markdown::LineEnding::Lf
            };
            editor.set_format(TextFormat {
                line_ending,
                mixed_line_endings: false,
                ..editor.format
            });
        }
        after_edit(&ui, shell, true);
    }));
    ui.on_menu_go_to_line(handler!(|ui, shell| {
        let Some(id) = shell.tabs.active_id() else {
            return;
        };
        let Some(editor) = shell.markdown(id) else {
            return;
        };
        if !editor.view_mode.shows_source() {
            editor.set_view_mode(ViewMode::Split);
        }
        let count = editor.line_count();
        let strings = shell.language.strings();
        show_prompt(
            &ui,
            shell,
            Prompt::GoTo(id),
            strings.goto_title.to_owned(),
            i18n::fill(strings.goto_label, "count", &group_digits(count)),
            String::new(),
        );
        refresh_toolbar(&ui, shell);
    }));
    ui.on_prompt_accept(handler!(|ui, shell| {
        match shell.prompt.clone() {
            Some(Prompt::GoTo(id)) => {
                let strings = shell.language.strings();
                let Some(editor) = shell.markdown(id) else {
                    hide_prompt(&ui, shell);
                    return;
                };
                let count = editor.line_count();
                match content::parse_line_target(&ui.get_prompt_text(), count) {
                    Some((line, column)) => {
                        editor.go_to_line(line, column);
                        hide_prompt(&ui, shell);
                        after_edit(&ui, shell, false);
                    }
                    None => ui.set_prompt_problem(
                        i18n::fill(strings.goto_invalid, "count", &group_digits(count)).into(),
                    ),
                }
            }
            Some(Prompt::Rename(from)) => rename_entry(&ui, shell, from),
            Some(Prompt::RenameSheet(id, index)) => {
                let strings = shell.language.strings();
                let typed = ui.get_prompt_text().trim().to_owned();
                let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
                    hide_prompt(&ui, shell);
                    return;
                };
                match tab.rename_sheet(index, &typed) {
                    Ok(()) => {
                        hide_prompt(&ui, shell);
                        if shell.sync_modified() {
                            refresh_tabs(&ui, shell);
                        }
                        after_sheet_change(&ui, shell, Vec::new());
                    }
                    Err(problem) => {
                        ui.set_prompt_problem(sheet_name_problem(strings, &problem).into());
                    }
                }
            }
            None => hide_prompt(&ui, shell),
        }
    }));
    ui.on_prompt_cancel(handler!(|ui, shell| {
        hide_prompt(&ui, shell);
    }));
    ui.on_link_cancel(handler!(|ui, shell| {
        ui.set_link_open(false);
        shell.link_edit = None;
        ui.invoke_focus_document();
    }));
    ui.on_outline_clicked(handler!(|ui, shell, index| {
        let Some(offset) = usize::try_from(index)
            .ok()
            .and_then(|index| shell.outline.get(index))
            .map(|heading| heading.offset)
        else {
            return;
        };
        if let Some(editor) = shell.active_markdown() {
            editor.place_caret(offset);
            editor.scroll_caret_to_top();
        }
        after_edit(&ui, shell, false);
        present(&ui, shell);
    }));
    ui.on_ai_send(handler!(|ui, shell, text| {
        agent::send(&ui, shell, text.as_str());
    }));
    ui.on_ai_quick(handler!(|ui, shell, action| {
        agent::quick(&ui, shell, action);
    }));
    ui.on_ai_stop(handler!(|ui, shell| {
        agent::stop(&ui, shell);
    }));
    ui.on_mcp_disconnect_client(handler!(|ui, shell, index| {
        agent::disconnect_client(&ui, shell, index);
    }));
    ui.on_ai_switch_chat(handler!(|ui, shell, index| {
        agent::switch_chat(&ui, shell, index);
    }));
    ui.on_ai_reply_action(handler!(|ui, shell, row, action| {
        agent::reply_action(&ui, shell, row, action);
    }));
    ui.on_ai_new_chat(handler!(|ui, shell| {
        agent::new_chat(&ui, shell);
    }));
    ui.on_ai_set_access(handler!(|ui, shell, level| {
        agent::set_document_access(&ui, shell, level);
    }));
    ui.on_ai_answer(handler!(|ui, shell, id, choice| {
        agent::answer(&ui, shell, id, choice);
    }));
    ui.on_ai_set_provider(handler!(|ui, shell, index| {
        agent::set_provider(&ui, shell, index);
    }));
    ui.on_ai_set_base_url(handler!(|ui, shell, url| {
        agent::set_base_url(&ui, shell, url.as_str());
    }));
    ui.on_ai_set_model(handler!(|ui, shell, model| {
        agent::set_model(&ui, shell, model.as_str());
    }));
    ui.on_ai_save_key(handler!(|ui, shell, key| {
        agent::save_key(&ui, shell, key.as_str());
    }));
    ui.on_ai_delete_key(handler!(|ui, shell| {
        agent::delete_key(&ui, shell);
    }));
    ui.on_ai_set_redact(handler!(|ui, shell, on| {
        agent::set_redact(&ui, shell, on);
    }));
    ui.on_ai_set_untitled_level(handler!(|ui, shell, level| {
        agent::set_untitled_access(&ui, shell, level);
    }));
    ui.on_ai_set_folder_level(handler!(|ui, shell, level| {
        agent::set_folder_access(&ui, shell, level);
    }));
    ui.on_ai_set_allow_insecure(handler!(|ui, shell, on| {
        agent::set_allow_insecure(&ui, shell, on);
    }));
    ui.on_ai_pick_model(handler!(|ui, shell, id| {
        agent::set_model(&ui, shell, id.as_str());
    }));
    ui.on_ai_refresh_models(handler!(|ui, shell| {
        agent::list_models(&ui, shell);
    }));
    ui.on_mcp_set_enabled(handler!(|ui, shell, on| {
        agent::set_mcp(&ui, shell, on);
    }));

    ui.on_format(handler!(|ui, shell, code| {
        if let Some(format) = format_of_menu(code) {
            apply_format(&ui, shell, format);
        }
    }));
    // The same menu items on a workbook: bold, italic, underline and
    // strikethrough of the selected cells, as the toolbar does.
    ui.on_sheet_format(handler!(|ui, shell, code| {
        let command = match code {
            0 => Command::Bold,
            1 => Command::Italic,
            2 => Command::Underline,
            _ => Command::Strikethrough,
        };
        if shell.run(command) {
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
                let types = open_file_types(strings);
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

    {
        let shell = shell.clone();
        let weak = ui.as_weak();
        ui.on_file_export_html(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(mut borrowed) = shell.try_borrow_mut() else {
                return;
            };
            let strings = borrowed.language.strings();
            let Some(id) = borrowed.tabs.active_id() else {
                return;
            };
            let Some((existing, suggested)) = borrowed.save_target(id) else {
                return;
            };
            let title = borrowed
                .tabs
                .get(id)
                .map(|tab| tab.title.clone())
                .unwrap_or_default();
            let Some(editor) = borrowed.markdown(id) else {
                return;
            };
            let source = editor.document.text().to_string();
            drop(borrowed);
            let suggested = existing.unwrap_or(suggested).with_extension("html");
            let (weak, shell) = (ui.as_weak(), shell.clone());
            let _ = slint::spawn_local(async move {
                let types = [FileType {
                    name: strings.file_type_html,
                    extensions: &["html", "htm"],
                }];
                let Some(path) =
                    dialogs::pick_file_to_save(strings.menu_export_html, &suggested, &types).await
                else {
                    return;
                };
                let page = aster_markdown::export::page(&source, title.trim_end_matches(".md"));
                if let Err(err) = aster_storage::write_atomically(&path, page.as_bytes())
                    && let Some(ui) = weak.upgrade()
                    && let Ok(mut shell) = shell.try_borrow_mut()
                {
                    let title =
                        i18n::fill(strings.export_failed, "name", &files::display_name(&path));
                    show_dialog(&ui, &mut shell, 2, title, err.to_string());
                }
            });
        });
    }

    {
        let shell = shell.clone();
        let weak = ui.as_weak();
        ui.on_file_export_pdf(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(mut borrowed) = shell.try_borrow_mut() else {
                return;
            };
            let strings = borrowed.language.strings();
            let Some(id) = borrowed.tabs.active_id() else {
                return;
            };
            let Some((existing, suggested)) = borrowed.save_target(id) else {
                return;
            };
            let Some(editor) = borrowed.markdown(id) else {
                return;
            };
            let source = editor.document.text().to_string();
            let base = editor
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_owned);
            // On paper: the light colours, whatever the window shows, and
            // body text at a size for print.
            let style = aster_surface::preview::PreviewStyle {
                font_size: PDF_FONT_SIZE,
                ..content::preview_style(&theme::Tokens::of(ThemeMode::Light), strings)
            };
            drop(borrowed);
            // Letter where people print on it, A4 elsewhere.
            let page = match aster_platform::locale::region() {
                Some(region) if aster_platform::locale::uses_letter_paper(&region) => {
                    aster_surface::pdf::PdfPage::LETTER
                }
                _ => aster_surface::pdf::PdfPage::A4,
            };
            let suggested = existing.unwrap_or(suggested).with_extension("pdf");
            let weak = ui.as_weak();
            let _ = slint::spawn_local(async move {
                let types = [FileType {
                    name: strings.file_type_pdf,
                    extensions: &["pdf"],
                }];
                let Some(path) =
                    dialogs::pick_file_to_save(strings.menu_export_pdf, &suggested, &types).await
                else {
                    return;
                };
                // Laying out and writing a long document takes a moment.
                std::thread::spawn(move || {
                    let written =
                        aster_surface::pdf::export_pdf(&source, base.as_deref(), &style, page)
                            .and_then(|bytes| {
                                aster_storage::write_atomically(&path, &bytes)
                                    .map_err(|err| err.to_string())
                            });
                    if let Err(err) = written {
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            deliver(
                                ui,
                                Box::new(move |ui, shell| {
                                    let strings = shell.language.strings();
                                    let title = i18n::fill(
                                        strings.export_failed,
                                        "name",
                                        &files::display_name(&path),
                                    );
                                    show_dialog(ui, shell, 2, title, err);
                                }),
                            );
                        });
                    }
                });
            });
        });
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
        if let Some(tab) = shell.active_sheet() {
            if let Some(text) = tab.copy(true) {
                ui.invoke_copy_to_clipboard(text.into());
            }
            return;
        }
        // The preview cannot be edited: cutting from it copies.
        if let Some(editor) = shell.active_markdown()
            && editor.preview_has_focus()
        {
            if let Some(text) = editor.preview.selected_text() {
                ui.invoke_copy_to_clipboard(text.into());
            }
            return;
        }
        if let Some(text) = shell.active_markdown().and_then(MarkdownEditor::cut) {
            ui.invoke_copy_to_clipboard(text.into());
            after_edit(&ui, shell, true);
        }
    }));

    ui.on_edit_copy(handler!(|ui, shell| {
        // A workbook copies the selected cells as tab-separated text.
        if let Some(tab) = shell.active_sheet() {
            if let Some(text) = tab.copy(false) {
                ui.invoke_copy_to_clipboard(text.into());
            }
            return;
        }
        let text = shell.active_markdown().and_then(|editor| {
            if editor.preview_has_focus() {
                editor.preview.selected_text()
            } else {
                editor.copy()
            }
        });
        if let Some(text) = text {
            ui.invoke_copy_to_clipboard(text.into());
        }
    }));

    ui.on_edit_paste(handler!(|ui, shell| {
        if shell.active_sheet().is_some() {
            let text = ui.invoke_clipboard_text();
            let language = shell.language.tag();
            if let Some(tab) = shell.active_sheet() {
                let changes = tab.paste(text.as_str(), language);
                after_sheet_change(&ui, shell, changes);
            }
            return;
        }
        if shell
            .active_markdown()
            .is_none_or(|editor| editor.preview_has_focus())
        {
            return;
        }
        let text = ui.invoke_clipboard_text();
        // No text: a picture, such as a screenshot, becomes a file beside
        // the document and a reference to it.
        if text.is_empty()
            && let Some(picture) = aster_platform::clipboard::picture()
        {
            paste_picture(&ui, shell, picture);
            return;
        }
        if let Some(editor) = shell.active_markdown()
            && editor.paste(text.as_str())
        {
            after_edit(&ui, shell, true);
        }
    }));

    ui.on_edit_select_all(handler!(|ui, shell| {
        if let Some(editor) = shell.active_markdown()
            && editor.preview_has_focus()
        {
            editor.preview.select_all();
            present(&ui, shell);
            return;
        }
        match shell.active_content() {
            Some(Content::Writer(view)) => view.surface.apply(Action::SelectAll),
            Some(Content::Markdown(editor)) => editor.select_all(),
            _ => return,
        }
        after_edit(&ui, shell, false);
    }));

    ui.on_sheet_add(handler!(|ui, shell| {
        let base = office::sheet_base(shell.language.tag());
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        if tab.view.is_editing() {
            let language = shell.language.tag();
            if let Some(tab) = shell.active_sheet() {
                let changes = tab.commit(language);
                tab.recalc(&changes);
            }
        }
        if let Some(tab) = shell.active_sheet() {
            tab.add_sheet(base);
        }
        if shell.sync_modified() {
            refresh_tabs(&ui, shell);
        }
        after_sheet_change(&ui, shell, Vec::new());
    }));
    ui.on_sheet_rename(handler!(|ui, shell, shown| {
        let Some(id) = shell.tabs.active_id() else {
            return;
        };
        let strings = shell.language.strings();
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let visible = tab.view.visible_sheets();
        let Some(&index) = usize::try_from(shown).ok().and_then(|i| visible.get(i)) else {
            return;
        };
        let name = tab.view.workbook().sheets[index].name.clone();
        show_prompt(
            &ui,
            shell,
            Prompt::RenameSheet(id, index),
            strings.sheet_rename_title.to_owned(),
            strings.sheet_name_label.to_owned(),
            name,
        );
    }));
    ui.on_sheet_move(handler!(|ui, shell, from, to| {
        let language = shell.language.tag();
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let changes = if tab.view.is_editing() {
            tab.commit(language)
        } else {
            Vec::new()
        };
        tab.recalc(&changes);
        // The tabs count the shown sheets; hidden ones keep their places.
        let visible = tab.view.visible_sheets();
        let index = |shown: i32| {
            usize::try_from(shown)
                .ok()
                .and_then(|i| visible.get(i).copied())
        };
        if let (Some(from), Some(to)) = (index(from), index(to)) {
            tab.move_sheet(from, to);
        }
        if shell.sync_modified() {
            refresh_tabs(&ui, shell);
        }
        after_sheet_change(&ui, shell, changes);
    }));
    ui.on_sheet_delete(handler!(|ui, shell, shown| {
        let language = shell.language.tag();
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let changes = if tab.view.is_editing() {
            tab.commit(language)
        } else {
            Vec::new()
        };
        tab.recalc(&changes);
        let visible = tab.view.visible_sheets();
        let Some(&index) = usize::try_from(shown).ok().and_then(|i| visible.get(i)) else {
            return;
        };
        tab.refusal = tab.delete_sheet(index).err();
        if shell.sync_modified() {
            refresh_tabs(&ui, shell);
        }
        after_sheet_change(&ui, shell, changes);
    }));
    record_tick.start(
        TimerMode::Repeated,
        std::time::Duration::from_secs(2),
        handler!(|_ui, shell| {
            shell.sync_sheet_records();
        }),
    );
    ui.on_sheet_hide(handler!(|ui, shell, columns, hide| {
        let language = shell.language.tag();
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let changes = if tab.view.is_editing() {
            tab.commit(language)
        } else {
            Vec::new()
        };
        tab.recalc(&changes);
        tab.hide_lines(columns, hide);
        if shell.sync_modified() {
            refresh_tabs(&ui, shell);
        }
        after_sheet_change(&ui, shell, changes);
    }));
    ui.on_sheet_lines(handler!(|ui, shell, columns, insert| {
        let language = shell.language.tag();
        let Some(tab) = shell.active_sheet() else {
            return;
        };
        let changes = if tab.view.is_editing() {
            tab.commit(language)
        } else {
            Vec::new()
        };
        tab.recalc(&changes);
        tab.refusal = tab.change_lines(columns, insert).err();
        if shell.sync_modified() {
            refresh_tabs(&ui, shell);
        }
        after_sheet_change(&ui, shell, changes);
    }));

    // --- Find and replace ---

    ui.on_menu_find(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            tab.open_find(false);
            let query = tab.find.query.clone();
            tab.set_query(query);
            after_sheet_find(&ui, shell, Vec::new());
            ui.invoke_focus_find_query();
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        editor.open_find(false);
        let job = (!editor.find.query.pattern.is_empty())
            .then(|| editor.search())
            .flatten();
        after_find(&ui, shell, job);
        ui.invoke_focus_find_query();
    }));

    ui.on_menu_replace(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            tab.open_find(true);
            let query = tab.find.query.clone();
            tab.set_query(query);
            after_sheet_find(&ui, shell, Vec::new());
            ui.invoke_focus_find_query();
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        editor.open_find(true);
        let job = (!editor.find.query.pattern.is_empty())
            .then(|| editor.search())
            .flatten();
        after_find(&ui, shell, job);
        ui.invoke_focus_find_query();
    }));

    ui.on_menu_find_selection(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            // What the active cell holds, as the formula bar shows it.
            let text = tab
                .view
                .sheet()
                .and_then(|sheet| sheet.cell(tab.view.selected()))
                .map(|cell| tab.view.workbook().input_text(cell))
                .unwrap_or_default();
            if !text.is_empty() {
                let replace = tab.find.replace_open;
                tab.open_find(replace);
                let mut query = tab.find.query.clone();
                query.pattern = text;
                query.regex = false;
                tab.set_query(query);
            }
            after_sheet_find(&ui, shell, Vec::new());
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let job = editor.find_selection();
        editor.open_find(editor.find.replace_open);
        after_find(&ui, shell, job);
    }));

    ui.on_find_query_edited(handler!(|ui, shell, text| {
        if let Some(tab) = shell.active_sheet() {
            let mut query = tab.find.query.clone();
            query.pattern = text.to_string();
            tab.set_query(query);
            after_sheet_find(&ui, shell, Vec::new());
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let mut query = editor.find.query.clone();
        query.pattern = text.to_string();
        let job = editor.set_query(query);
        after_find(&ui, shell, job);
    }));

    ui.on_find_replacement_edited(handler!(|_ui, shell, text| {
        if let Some(tab) = shell.active_sheet() {
            tab.find.replacement = text.to_string();
            return;
        }
        if let Some(editor) = shell.active_markdown() {
            editor.find.replacement = text.to_string();
        }
    }));

    ui.on_find_toggle(handler!(|ui, shell, option| {
        if let Some(tab) = shell.active_sheet() {
            let mut query = tab.find.query.clone();
            match option {
                0 => query.case_sensitive = !query.case_sensitive,
                1 => query.whole_word = !query.whole_word,
                _ => query.regex = !query.regex,
            }
            tab.set_query(query);
            after_sheet_find(&ui, shell, Vec::new());
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let mut query = editor.find.query.clone();
        match option {
            0 => query.case_sensitive = !query.case_sensitive,
            1 => query.whole_word = !query.whole_word,
            _ => query.regex = !query.regex,
        }
        let job = editor.set_query(query);
        after_find(&ui, shell, job);
    }));

    ui.on_find_next(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            tab.find_step(true);
            after_sheet_find(&ui, shell, Vec::new());
            return;
        }
        let job = shell
            .active_markdown()
            .and_then(|editor| editor.find_step(true));
        after_find(&ui, shell, job);
    }));

    ui.on_find_previous(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            tab.find_step(false);
            after_sheet_find(&ui, shell, Vec::new());
            return;
        }
        let job = shell
            .active_markdown()
            .and_then(|editor| editor.find_step(false));
        after_find(&ui, shell, job);
    }));

    ui.on_find_close(handler!(|ui, shell| {
        if let Some(tab) = shell.active_sheet() {
            tab.close_find();
        }
        if let Some(editor) = shell.active_markdown() {
            editor.cancel_replace_all();
            editor.close_find();
        }
        after_find(&ui, shell, None);
        ui.invoke_focus_document();
    }));

    ui.on_replace_one(handler!(|ui, shell| {
        let language = shell.language.tag();
        if let Some(tab) = shell.active_sheet() {
            let changes = tab.replace_one(language);
            after_sheet_find(&ui, shell, changes);
            return;
        }
        let changed = shell
            .active_markdown()
            .is_some_and(MarkdownEditor::replace_one);
        after_edit(&ui, shell, changed);
        refresh_find(&ui, shell);
    }));

    ui.on_replace_all(handler!(|ui, shell| {
        let language = shell.language.tag();
        if let Some(tab) = shell.active_sheet() {
            let changes = tab.replace_all(language);
            after_sheet_find(&ui, shell, changes);
            return;
        }
        let Some(editor) = shell.active_markdown() else {
            return;
        };
        let before = editor.text_version();
        match editor.replace_all() {
            Some(job) => {
                spawn_replace(&ui, shell, job);
                refresh_find(&ui, shell);
                refresh_status(&ui, shell);
            }
            None => {
                let changed = shell
                    .active_markdown()
                    .is_some_and(|editor| editor.text_version() != before);
                after_edit(&ui, shell, changed);
                refresh_find(&ui, shell);
            }
        }
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
                let pending = shell.pending.clone();
                hide_dialog(&ui, &mut shell);
                pending
            };
            if let Some(Pending::CloseTab(id)) = pending {
                save_document(&ui, &shell, id, false, AfterSave::CloseTab);
            }
        }
    });

    ui.on_dialog_discard(handler!(|ui, shell| {
        let pending = shell.pending.clone();
        hide_dialog(&ui, shell);
        match pending {
            Some(Pending::MissingRecent(path)) => {
                shell.recent.remove(&path);
                shell.save_recent();
                refresh_sidebar(&ui, shell);
            }
            Some(Pending::CloseTab(id)) => {
                shell.force_close(id);
                after_tab_change(&ui, shell);
            }
            Some(Pending::Trash(path)) => trash_entry(&ui, path),
            Some(Pending::SaveInUtf8 { id, path, after }) => {
                if let Some(editor) = shell.markdown(id) {
                    editor.set_format(TextFormat {
                        encoding: Encoding::Utf8,
                        bom: false,
                        ..editor.format
                    });
                }
                if let Some(job) = shell.begin_save(id, path) {
                    spawn_save(&ui, job, after);
                }
            }
            Some(Pending::DiskChanged(id)) => {
                if let Some((loaded_id, loaded)) = shell.disk_loaded.take()
                    && loaded_id == id
                {
                    reload_document(&ui, shell, id, loaded, false);
                }
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
        let pending = shell.pending.clone();
        hide_dialog(&ui, shell);
        if let Some(Pending::DiskChanged(id)) = pending {
            // Kept: never written over without asking where.
            shell.disk_loaded = None;
            if let Some(editor) = shell.markdown(id) {
                editor.disk_warning = Some(DiskWarning::ChangedWhileOpen);
            }
            refresh_status(&ui, shell);
            check_disk(&ui, shell);
        }
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
        let workbook = candidate.workbook.is_some();
        if let Some(id) = shell.restore(candidate)
            && workbook
        {
            start_calc(&ui, shell, id);
        }
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
            let workbook = candidate.workbook.is_some();
            if let Some(id) = shell.restore(candidate)
                && workbook
            {
                start_calc(&ui, shell, id);
            }
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
    if let Ok(mut shell) = shell.try_borrow_mut() {
        agent::shut_down(&mut shell);
    }
    // Carry out what the record writer still has queued (deleting the records
    // of documents that were saved or discarded) before the process ends.
    if let Ok(mut shell) = shell.try_borrow_mut() {
        drop(shell.writer.take());
        shell.candidates.clear();
        // The same for the recent list and the sidebar state.
        if let Some(store) = shell.store.take() {
            store.finish();
        }
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

    #[test]
    fn renaming_waits_for_saves_under_the_name() {
        let mut shell = shell();
        let id = shell.open_untitled(DocKind::Markdown);
        let editor = shell.markdown(id).unwrap();
        editor.path = Some(PathBuf::from("/notes/sub/a.md"));
        assert!(!saving_under(&shell, Path::new("/notes/sub/a.md")));
        shell.markdown(id).unwrap().saving = true;
        assert!(saving_under(&shell, Path::new("/notes/sub/a.md")));
        // Its folder too, and not a folder beside it.
        assert!(saving_under(&shell, Path::new("/notes/sub")));
        assert!(!saving_under(&shell, Path::new("/notes/su")));
        assert!(!saving_under(&shell, Path::new("/other")));
    }

    fn shell() -> Shell {
        Shell::new(ThemeMode::Light, Language::ZhHans)
    }

    #[test]
    fn a_workbook_with_unsaved_changes_comes_back_from_its_record() {
        use aster_sheets::CellRef;
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("帳簿.xlsx");
        std::fs::write(&file, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let root = RecoveryRoot::new(dir.path().join("records"));
        let mut shell = shell();
        shell.writer = Some(RecordWriter::spawn(root.clone(), |_| {}));
        let Ok(office::Loaded::Workbook(workbook, source)) = office::load(&file) else {
            panic!("the workbook loads");
        };
        let id = shell.open_office(file.clone(), office::Loaded::Workbook(workbook, source));
        let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
            panic!("a sheet tab");
        };
        tab.view.select(CellRef::parse("B2").unwrap());
        tab.view.begin_edit(EditMode::Enter, Some("未保存".into()));
        tab.commit("ja");
        shell.sync_sheet_records();
        // A burst of edits writes once; the next waits for the timer.
        let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
            panic!("a sheet tab");
        };
        assert!(tab.record.is_some());
        tab.view.select(CellRef::parse("C3").unwrap());
        tab.view.begin_edit(EditMode::Enter, Some("42".into()));
        tab.commit("ja");
        let version = tab.history.version();
        shell.sync_sheet_records();
        let Some(Content::Sheet(tab)) = shell.contents.get_mut(&id) else {
            panic!("a sheet tab");
        };
        assert_ne!(tab.recorded.map(|(v, _)| v), Some(version));
        // Later the change is written.
        tab.recorded = tab.recorded.map(|(v, _)| {
            (
                v,
                std::time::Instant::now() - std::time::Duration::from_secs(3),
            )
        });
        shell.sync_sheet_records();
        if let Some(writer) = &shell.writer {
            writer.flush();
        }

        // As after a crash: a new run finds the record and restores it.
        drop(shell);
        let mut candidates = recovery::find(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        let candidate = candidates.remove(0);
        assert!(candidate.is_readable());
        assert_eq!(
            candidate.info.as_ref().map(|i| i.path.clone()),
            Some(Some(file.clone()))
        );
        assert_eq!(candidate.disk, Some(FileState::Unchanged));
        let mut again = self::shell();
        again.writer = Some(RecordWriter::spawn(root.clone(), |_| {}));
        let id = again.restore(candidate).expect("restored");
        let Some(Content::Sheet(tab)) = again.contents.get(&id) else {
            panic!("a sheet tab");
        };
        assert!(tab.is_modified());
        assert_eq!(tab.path.as_deref(), Some(file.as_path()));
        let cell = |at: &str| {
            tab.view
                .sheet()
                .and_then(|s| s.cell(CellRef::parse(at).unwrap()))
                .map(|c| c.value.clone())
        };
        assert_eq!(cell("B2"), Some(aster_sheets::Value::Text("未保存".into())));
        assert_eq!(cell("C3"), Some(aster_sheets::Value::Number(42.0)));
        // Closing it is the user's choice: the record goes.
        again.force_close(id);
        if let Some(writer) = &again.writer {
            writer.flush();
        }
        assert!(recovery::find(&root).unwrap().is_empty());
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
    fn placeholder_tabs_have_no_content() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Slides);
        assert!(shell.active_content().is_none());
        // A new workbook is a real one: one empty sheet, ready to edit.
        shell.open_untitled(DocKind::Sheets);
        let tab = shell.active_sheet().unwrap();
        assert_eq!(tab.view.workbook().sheets[0].name, "Sheet1");
        assert!(tab.path.is_none() && !tab.is_modified());
        let (existing, suggested) = shell.save_target(shell.tabs.active_id().unwrap()).unwrap();
        assert!(existing.is_none());
        assert_eq!(suggested.extension().and_then(|e| e.to_str()), Some("xlsx"));
        shell.open_untitled(DocKind::Writer);
        assert!(shell.active_writer().is_some());
        assert!(shell.active_markdown().is_none());
        shell.open_untitled(DocKind::Markdown);
        assert!(shell.active_markdown().is_some());
        assert!(shell.active_writer().is_none());
        shell.show_start_page();
        assert!(shell.active_surface().is_none());
        assert_eq!(shell.contents.len(), 3);
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
    fn a_changed_workbook_asks_before_closing_and_undoes_through_the_shell() {
        let mut shell = shell();
        let mut sheet =
            aster_sheets::SheetBuilder::new("Sheet1", aster_sheets::SheetKind::Worksheet);
        sheet.push(
            0,
            aster_sheets::Cell::new(0, aster_sheets::Value::Number(1.0)),
        );
        let workbook = aster_sheets::Workbook {
            sheets: vec![sheet.finish()],
            ..aster_sheets::Workbook::default()
        };
        let id = shell.open_office(
            PathBuf::from("/tmp/表.xlsx"),
            office::Loaded::Workbook(workbook, Arc::new(Vec::new())),
        );
        assert!(Command::Undo.is_available(DocKind::Sheets));
        assert!(!shell.run(Command::Undo));
        let tab = shell.active_sheet().unwrap();
        tab.view.select(aster_sheets::CellRef::new(0, 1));
        tab.view.begin_edit(EditMode::Enter, Some("2".into()));
        tab.commit("en");
        assert!(shell.sync_modified());
        assert_eq!(shell.close(0), Closing::NeedsAnswer(id));
        assert!(shell.run(Command::Undo));
        assert!(shell.sync_modified());
        assert_eq!(shell.close(0), Closing::Closed);
    }

    #[test]
    fn a_file_is_remembered_once_however_it_was_reached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("議事録.md");
        std::fs::write(&path, "x").unwrap();
        let other = dir.path().join("b.txt");
        std::fs::write(&other, "y").unwrap();

        let mut shell = shell();
        shell.remember_file(&path);
        shell.remember_file(&other);
        shell.remember_file(&dir.path().join(".").join("議事録.md"));
        let names: Vec<String> = shell
            .recent
            .files()
            .iter()
            .map(|file| files::display_name(&file.path))
            .collect();
        assert_eq!(names, ["議事録.md", "b.txt"]);
    }

    #[test]
    fn opening_a_folder_reads_it_and_its_expanded_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("第1章")).unwrap();
        std::fs::write(dir.path().join("第1章").join("草稿.md"), "").unwrap();

        let mut shell = shell();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let wanted = shell.open_folder(dir.path(), vec![root.join("第1章")]);
        assert_eq!(wanted, std::slice::from_ref(&root));
        let tree = shell.tree.as_mut().unwrap();
        assert_eq!(tree.root(), root);
        tree.finished(root.clone(), read_folder(&root));
        assert_eq!(tree.wanted(), [root.join("第1章")]);
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
                Content::Sheet(_) | Content::Slides(_) => unreachable!("not a text tab"),
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
    fn zoom_commands_act_on_writer_and_markdown_tabs() {
        let mut shell = shell();
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Sheets);
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Markdown);
        assert!(shell.run(Command::ZoomIn));
        assert_eq!(shell.active_markdown().expect("markdown").zoom_percent, 110);
        assert!(shell.run(Command::ZoomReset));
        assert!(!shell.run(Command::ZoomReset));
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
            encoding: Encoding::Utf8,
            bom: true,
            mixed_line_endings: false,
        };
        assert_eq!(format_label(&windows), "UTF-8 BOM · CRLF");
        let japanese = TextFormat {
            encoding: Encoding::ShiftJis,
            // Ignored: Shift_JIS has no byte-order mark.
            bom: true,
            ..TextFormat::default()
        };
        assert_eq!(format_label(&japanese), "Shift_JIS · LF");
    }

    #[test]
    fn what_stops_a_change_is_said_in_plain_words() {
        let strings = i18n::Language::ZhHans.strings();
        assert_eq!(blocker_label(strings, "vmlDrawing"), "批注");
        assert_eq!(blocker_label(strings, "comments"), "批注");
        assert_eq!(blocker_label(strings, "table"), "Excel 表");
        assert_eq!(blocker_label(strings, "extLst"), strings.blocker_newer);
        assert!(blocker_label(strings, "scenarios").contains("scenarios"));
    }

    #[test]
    fn every_control_maps_to_a_ui_item() {
        for kind in DocKind::ALL {
            for item in toolbar::toolbar(kind, ViewState::default(), &i18n::ZH_HANS) {
                let ui_item = tool_item(&item);
                assert!((0..=6).contains(&ui_item.control));
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
