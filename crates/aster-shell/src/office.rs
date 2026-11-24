//! Excel and PowerPoint files in tabs: reading them on a worker thread,
//! calculating formulas after the workbook is on screen, and the read-only
//! views' colours and keys.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use aster_ooxml::{ErrorKind, FormatCodec, Pptx, Xlsx};
use aster_sheets::input::{self, Reading};
use aster_sheets::{
    CalcEngine, CalcError, Cancel, CellRange, CellRef, Change, Computed, DateOrder, Edit, Workbook,
};
use aster_slides::Presentation;
use aster_surface::sheet_view::{Direction, SheetKey, SheetStyle, SheetView};
use aster_surface::slide_view::{SlideStyle, SlideView};

use crate::i18n::Strings;
use crate::tabs::DocKind;
use crate::theme;

pub const SHEET_EXTENSIONS: &[&str] = &["xlsx", "xlsm"];
pub const SLIDE_EXTENSIONS: &[&str] = &["pptx", "pptm"];
/// Files larger than this are not read: the package limits would refuse
/// them anyway once inflated, and reading them takes the memory first.
pub const MAX_FILE: u64 = 1 << 30;

/// Which office view a file opens in, by its extension.
pub fn kind_of(path: &Path) -> Option<DocKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if SHEET_EXTENSIONS.contains(&extension.as_str()) {
        Some(DocKind::Sheets)
    } else if SLIDE_EXTENSIONS.contains(&extension.as_str()) {
        Some(DocKind::Slides)
    } else {
        None
    }
}

pub enum Loaded {
    /// The workbook and the file's bytes, which saving starts from.
    Workbook(Workbook, Arc<Vec<u8>>),
    Presentation(Presentation),
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    TooLarge,
    Format(aster_ooxml::Error),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(err) => err.fmt(f),
            LoadError::TooLarge => f.write_str("file too large"),
            LoadError::Format(err) => err.fmt(f),
        }
    }
}

impl LoadError {
    /// What the error dialog says.
    pub fn message(&self, strings: &Strings) -> String {
        match self {
            LoadError::Io(err) => err.to_string(),
            LoadError::TooLarge => strings.office_too_large.to_owned(),
            LoadError::Format(err) => match err.kind() {
                ErrorKind::LegacyOrEncrypted => strings.office_legacy.to_owned(),
                ErrorKind::NotZip => strings.office_not_office.to_owned(),
                ErrorKind::WrongKind => strings.office_wrong_kind.to_owned(),
                ErrorKind::TooLarge => strings.office_too_large.to_owned(),
                ErrorKind::Broken => {
                    crate::i18n::fill(strings.office_broken, "reason", &err.to_string())
                }
            },
        }
    }
}

/// Read and decode a workbook or presentation. Runs on a worker thread.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let size = std::fs::metadata(path).map_err(LoadError::Io)?.len();
    if size > MAX_FILE {
        return Err(LoadError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(LoadError::Io)?;
    match kind_of(path) {
        Some(DocKind::Slides) => Pptx
            .read(bytes)
            .map(Loaded::Presentation)
            .map_err(LoadError::Format),
        _ => {
            let source = Arc::new(bytes.clone());
            Xlsx.read(bytes)
                .map(|workbook| Loaded::Workbook(workbook, source))
                .map_err(LoadError::Format)
        }
    }
}

/// Where a workbook's calculation stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Calc {
    /// Running on the tab's worker thread.
    Running,
    /// Done: how many formulas there are.
    Done(usize),
    Failed(String),
}

/// What the calculation thread sends back.
#[derive(Debug)]
pub enum CalcMessage {
    Done(CalcUpdate),
    Failed(String),
}

#[derive(Debug)]
pub struct CalcUpdate {
    /// Every formula's result.
    pub results: Vec<Computed>,
    pub formulas: usize,
    /// Formulas in a circular reference: they show 0, as in Excel.
    pub circular: Vec<(usize, CellRef)>,
    /// How many batches of edits this calculation took in; 0 for the one
    /// when the file was opened.
    pub generation: u64,
}

/// A thread that keeps the formula engine of one workbook. It calculates
/// everything once, then only what each batch of edits affects; a new batch
/// cancels a calculation still running, which then carries on with the new
/// edits too.
pub struct CalcWorker {
    jobs: Option<Sender<Vec<Edit>>>,
    current: Arc<Mutex<Cancel>>,
    sent: u64,
}

impl CalcWorker {
    pub fn spawn(workbook: Arc<Workbook>, send: impl Fn(CalcMessage) + Send + 'static) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let current = Arc::new(Mutex::new(Cancel::new()));
        let shared = current.clone();
        std::thread::spawn(move || calculate_loop(workbook, receiver, shared, send));
        Self {
            jobs: Some(jobs),
            current,
            sent: 0,
        }
    }

    /// Hand the engine changed cells. Returns the generation that includes
    /// them.
    pub fn edit(&mut self, edits: Vec<Edit>) -> u64 {
        if edits.is_empty() {
            return self.sent;
        }
        if let Some(jobs) = &self.jobs
            && jobs.send(edits).is_ok()
        {
            self.sent += 1;
            if let Ok(current) = self.current.lock() {
                current.cancel();
            }
        }
        self.sent
    }

    /// The latest generation handed over.
    pub fn sent(&self) -> u64 {
        self.sent
    }
}

impl Drop for CalcWorker {
    fn drop(&mut self) {
        // The thread ends at its next look at the queue.
        self.jobs = None;
        if let Ok(current) = self.current.lock() {
            current.cancel();
        }
    }
}

fn calculate_loop(
    workbook: Arc<Workbook>,
    jobs: Receiver<Vec<Edit>>,
    current: Arc<Mutex<Cancel>>,
    send: impl Fn(CalcMessage),
) {
    let mut engine = match aster_calc::Formualizer::load(&workbook) {
        Ok(engine) => engine,
        Err(error) => return send(CalcMessage::Failed(error.to_string())),
    };
    // Shared with the view until now: let go, so the view's first edit
    // does not copy the workbook.
    drop(workbook);
    let mut edits: Vec<Edit> = Vec::new();
    let mut generation = 0u64;
    let mut first = true;
    let mut again = false;
    loop {
        if !first && !again {
            match jobs.recv() {
                Ok(batch) => {
                    edits.extend(batch);
                    generation += 1;
                }
                Err(_) => return,
            }
        }
        loop {
            match jobs.try_recv() {
                Ok(batch) => {
                    edits.extend(batch);
                    generation += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let cancel = Cancel::new();
        if let Ok(mut slot) = current.lock() {
            *slot = cancel.clone();
        }
        let outcome = engine.apply(&edits, &cancel);
        // The edits are in the engine whatever happened to the calculation.
        edits.clear();
        first = false;
        again = false;
        match outcome {
            Ok(results) => send(CalcMessage::Done(CalcUpdate {
                formulas: results.len(),
                results,
                circular: engine.circular().to_vec(),
                generation,
            })),
            // New edits came, or the tab closed: look at the queue again.
            Err(CalcError::Cancelled) => again = true,
            Err(CalcError::Engine(error)) => send(CalcMessage::Failed(error)),
        }
    }
}

/// Undo and redo, one step per entry, paste or clear.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    /// How deep the undo stack was when the file was last saved; `None`
    /// once that state can no longer be reached.
    saved: Option<usize>,
}

impl History {
    fn push(&mut self, step: Vec<Change>) {
        if step.is_empty() {
            return;
        }
        if self.saved.is_some_and(|saved| saved > self.undo.len()) {
            self.saved = None;
        }
        self.undo.push(step);
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    /// How many steps there are to undo.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// The state `depth` steps in is what is on disk now.
    pub fn mark_saved_at(&mut self, depth: usize) {
        self.saved = Some(depth);
    }
}

/// A workbook being saved: what is written, taken when saving started.
pub struct SheetSave {
    pub source: Arc<Vec<u8>>,
    pub workbook: Arc<Workbook>,
    pub changes: aster_ooxml::xlsx_save::Changes,
    depth: usize,
}

impl SheetSave {
    /// Make the file and write it in place of `path` in one step. Runs on a
    /// worker thread. Returns the bytes written.
    pub fn write(&self, path: &Path) -> Result<Arc<Vec<u8>>, String> {
        let bytes =
            aster_ooxml::xlsx_save::save((*self.source).clone(), &self.workbook, &self.changes)
                .map_err(|err| err.to_string())?;
        aster_storage::write_atomically(path, &bytes).map_err(|err| err.to_string())?;
        Ok(Arc::new(bytes))
    }
}

pub struct SheetTab {
    pub view: SheetView,
    pub path: Option<PathBuf>,
    pub calc: Calc,
    pub worker: Option<CalcWorker>,
    /// Formulas in a circular reference at the last calculation.
    pub circular: Vec<(usize, CellRef)>,
    pub history: History,
    /// Cells changed since the file was read or saved: what saving
    /// rewrites.
    pub dirty: BTreeSet<(usize, CellRef)>,
    /// Of `dirty`, the cells changed by hand rather than by recalculation.
    pub edited: BTreeSet<(usize, CellRef)>,
    /// A formula appeared or went since the file was read or saved.
    pub formulas_changed: bool,
    /// The file as last read or saved: saving starts from it.
    pub source: Arc<Vec<u8>>,
    pub saving: bool,
    /// What was last copied or cut here.
    pub clip: Option<aster_sheets::Clip>,
}

impl SheetTab {
    pub fn new(
        workbook: Workbook,
        source: Arc<Vec<u8>>,
        path: Option<PathBuf>,
        tokens: &theme::Tokens,
    ) -> Self {
        let mut view = SheetView::new(Arc::new(workbook));
        view.set_style(sheet_style(tokens));
        let mut history = History::default();
        history.mark_saved();
        Self {
            view,
            path,
            calc: Calc::Done(0),
            worker: None,
            circular: Vec::new(),
            history,
            dirty: BTreeSet::new(),
            edited: BTreeSet::new(),
            formulas_changed: false,
            source,
            saving: false,
            clip: None,
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(sheet_style(tokens));
    }

    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Start the calculation thread on the workbook as it is now.
    pub fn start_calc(&mut self, send: impl Fn(CalcMessage) + Send + 'static) {
        let workbook = self.view.workbook().clone();
        self.worker = Some(CalcWorker::spawn(workbook, send));
        self.calc = Calc::Running;
    }

    /// Have changed cells calculated with what depends on them.
    pub fn recalc(&mut self, changes: &[Change]) {
        if changes.is_empty() {
            return;
        }
        if let Some(worker) = &mut self.worker {
            worker.edit(edits(changes));
            self.calc = Calc::Running;
        }
    }

    /// Results from the calculation thread. Values that changed because of
    /// an edit are saved with the file; those of the first calculation only
    /// when something else changes them, so an unchanged formula keeps the
    /// result the file was saved with.
    pub fn take_calc(&mut self, message: CalcMessage) {
        match message {
            CalcMessage::Done(update) => {
                let changed = self.view.workbook_mut().apply_results(&update.results);
                if update.generation > 0 {
                    self.dirty.extend(changed);
                }
                self.circular = update.circular;
                let latest = self.worker.as_ref().map_or(0, CalcWorker::sent);
                self.calc = if update.generation >= latest {
                    Calc::Done(update.formulas)
                } else {
                    Calc::Running
                };
            }
            CalcMessage::Failed(reason) => self.calc = Calc::Failed(reason),
        }
    }

    /// How a date typed into the active cell is read: in the order of its
    /// date format, else as people writing `language` do.
    pub fn reading(&self, language: &str) -> Reading {
        let workbook = self.view.workbook();
        let at = self.view.selected();
        let format_order = self
            .view
            .sheet()
            .map(|sheet| workbook.style(sheet.style_at(at)))
            .and_then(|style| style.format.date_order());
        Reading {
            order: format_order.unwrap_or_else(|| date_order(language)),
            date1904: workbook.date1904,
            this_year: this_year(),
        }
    }

    /// Keep what was typed into the cell being edited. Returns the changes
    /// made, empty when nothing changed.
    pub fn commit(&mut self, language: &str) -> Vec<Change> {
        let reading = self.reading(language);
        let Some((at, text)) = self.view.finish_edit() else {
            return Vec::new();
        };
        let sheet = self.view.sheet_index();
        let (entry, look) = input::read(&text, &reading);
        let cell = self.view.workbook_mut().entry_cell(sheet, at, entry, look);
        let changes = self.view.workbook_mut().set_cells(sheet, vec![(at, cell)]);
        self.record(changes)
    }

    /// Empty the selected cells, keeping their formats. One undo step.
    pub fn clear_selection(&mut self) -> Vec<Change> {
        let sheet_index = self.view.sheet_index();
        let range = self.view.selection();
        let Some(sheet) = self.view.sheet() else {
            return Vec::new();
        };
        let cleared: Vec<(CellRef, Option<aster_sheets::Cell>)> = sheet
            .cells_in(range)
            .filter(|(_, cell)| !cell.is_blank())
            .map(|(at, cell)| {
                let kept = (cell.style != 0).then(|| aster_sheets::Cell {
                    style: cell.style,
                    ..aster_sheets::Cell::new(at.col, aster_sheets::Value::Empty)
                });
                (at, kept)
            })
            .collect();
        if cleared.is_empty() {
            return Vec::new();
        }
        let changes = self.view.workbook_mut().set_cells(sheet_index, cleared);
        self.record(changes)
    }

    /// Copy (or cut) the selection; returns the text for the system
    /// clipboard. Cutting moves the cells when they are pasted.
    pub fn copy(&mut self, cut: bool) -> Option<String> {
        if let Some(editor) = self.view.editor() {
            return Some(editor.text().to_owned());
        }
        let clip =
            self.view
                .workbook()
                .copy(self.view.sheet_index(), self.view.selection(), cut)?;
        let text = clip.text.clone();
        self.clip = Some(clip);
        Some(text)
    }

    /// Paste text from the system clipboard at the selection. Cells copied
    /// here come back as cells, with their formulas following; anything
    /// else is read as tab-separated entries. One undo step.
    pub fn paste(&mut self, text: &str, language: &str) -> Vec<Change> {
        if self.view.is_editing() {
            self.view.edit(|editor| editor.insert(text));
            return Vec::new();
        }
        let sheet = self.view.sheet_index();
        let range = self.view.selection();
        let target = range.first;
        let same = |a: &str, b: &str| a.replace('\r', "") == b.replace('\r', "");
        let clip = self
            .clip
            .clone()
            .filter(|clip| same(&clip.text, text) && !text.is_empty());
        let mut changes = Vec::new();
        let pasted;
        if let Some(clip) = clip {
            let single = clip.range.first == clip.range.last;
            let mut cells = Vec::new();
            if clip.cut {
                // Moving: the source is emptied, then the cells land.
                let cleared: Vec<_> = (0..clip.range.rows())
                    .flat_map(|r| (0..clip.range.cols()).map(move |c| (r, c)))
                    .map(|(r, c)| {
                        (
                            CellRef::new(clip.range.first.row + r, clip.range.first.col + c),
                            None,
                        )
                    })
                    .collect();
                if clip.sheet == sheet {
                    cells.extend(cleared);
                } else {
                    changes.extend(self.view.workbook_mut().set_cells(clip.sheet, cleared));
                }
                self.clip = None;
            }
            if single && range.first != range.last && !clip.cut {
                // One cell pasted over a selection fills all of it.
                let count = u64::from(range.rows()) * u64::from(range.cols());
                if count <= 1_000_000 {
                    for row in range.first.row..=range.last.row {
                        for col in range.first.col..=range.last.col {
                            cells.extend(clip.cells_at(CellRef::new(row, col)));
                        }
                    }
                }
                pasted = range;
            } else {
                cells.extend(clip.cells_at(target));
                pasted = CellRange::new(
                    target,
                    CellRef::new(
                        target.row + clip.range.rows() - 1,
                        target.col + clip.range.cols() - 1,
                    ),
                );
            }
            changes.extend(self.view.workbook_mut().set_cells(sheet, cells));
        } else {
            let rows = aster_sheets::parse_tsv(text);
            if rows.is_empty() {
                return Vec::new();
            }
            let reading = self.reading(language);
            let mut cells = Vec::new();
            let mut width = 0;
            for (r, row) in rows.iter().enumerate() {
                width = width.max(row.len());
                for (c, field) in row.iter().enumerate() {
                    let at = CellRef::new(target.row + r as u32, target.col + c as u32);
                    if at.row >= aster_sheets::MAX_ROWS || at.col >= aster_sheets::MAX_COLUMNS {
                        continue;
                    }
                    let (entry, look) = input::read(field, &reading);
                    let cell = self.view.workbook_mut().entry_cell(sheet, at, entry, look);
                    cells.push((at, cell));
                }
            }
            changes.extend(self.view.workbook_mut().set_cells(sheet, cells));
            pasted = CellRange::new(
                target,
                CellRef::new(
                    target.row + rows.len() as u32 - 1,
                    target.col + width.max(1) as u32 - 1,
                ),
            );
        }
        self.view.select(pasted.first);
        self.view.extend_to(pasted.last);
        self.record(changes)
    }

    /// Put cells in place on the current sheet as one undo step.
    pub fn put(&mut self, cells: Vec<(CellRef, Option<aster_sheets::Cell>)>) -> Vec<Change> {
        let sheet = self.view.sheet_index();
        let changes = self.view.workbook_mut().set_cells(sheet, cells);
        self.record(changes)
    }

    fn record(&mut self, changes: Vec<Change>) -> Vec<Change> {
        self.note(&changes);
        self.history.push(changes.clone());
        changes
    }

    /// Remember which cells saving must write.
    fn note(&mut self, changes: &[Change]) {
        for change in changes {
            self.dirty.insert((change.sheet, change.at));
            self.edited.insert((change.sheet, change.at));
            self.formulas_changed |= change.touches_formula();
        }
    }

    /// What saving now would write. `None` while a save is running.
    pub fn begin_save(&mut self) -> Option<SheetSave> {
        if self.saving {
            return None;
        }
        self.saving = true;
        // What is being typed is not part of the file yet.
        Some(SheetSave {
            source: self.source.clone(),
            workbook: self.view.workbook().clone(),
            changes: aster_ooxml::xlsx_save::Changes {
                cells: self.dirty.clone(),
                edited: self.edited.clone(),
                formulas_changed: self.formulas_changed,
            },
            depth: self.history.depth(),
        })
    }

    /// A save has finished. On success the written file is what the next
    /// save starts from, and what was written no longer counts as changed;
    /// what changed while it ran still does.
    pub fn finish_save(&mut self, job: &SheetSave, path: &Path, written: Option<Arc<Vec<u8>>>) {
        self.saving = false;
        let Some(bytes) = written else {
            return;
        };
        self.source = bytes;
        self.path = Some(path.to_owned());
        let unchanged_since = self.history.depth() == job.depth;
        for cell in &job.changes.cells {
            // A cell is still to be written if it changed again since.
            let now = self
                .view
                .workbook()
                .sheets
                .get(cell.0)
                .and_then(|s| s.cell(cell.1));
            let then = job.workbook.sheets.get(cell.0).and_then(|s| s.cell(cell.1));
            if now == then {
                self.dirty.remove(cell);
                self.edited.remove(cell);
            }
        }
        if unchanged_since {
            self.formulas_changed = false;
        }
        self.history.mark_saved_at(job.depth);
    }

    /// Undo the last step. Returns the changes made, to recalculate.
    pub fn undo(&mut self) -> Vec<Change> {
        self.view.cancel_edit();
        let Some(step) = self.history.undo.pop() else {
            return Vec::new();
        };
        let inverse: Vec<Change> = step.iter().rev().map(Change::inverse).collect();
        self.view.workbook_mut().apply(&inverse);
        self.note(&inverse);
        self.show(&inverse);
        self.history.redo.push(step);
        inverse
    }

    pub fn redo(&mut self) -> Vec<Change> {
        self.view.cancel_edit();
        let Some(step) = self.history.redo.pop() else {
            return Vec::new();
        };
        self.view.workbook_mut().apply(&step);
        self.note(&step);
        self.show(&step);
        let applied = step.clone();
        self.history.undo.push(step);
        applied
    }

    /// Bring the cells a step changed into view, selected.
    fn show(&mut self, changes: &[Change]) {
        let Some(first) = changes.first() else {
            return;
        };
        if first.sheet != self.view.sheet_index() {
            self.view.set_sheet(first.sheet);
        }
        let mut range = CellRange::single(first.at);
        for change in changes.iter().filter(|c| c.sheet == first.sheet) {
            range = range.union(&CellRange::single(change.at));
        }
        self.view.select(range.first);
        self.view.extend_to(range.last);
    }
}

/// A new workbook with one sheet, its default font the one Excel uses for
/// the interface language (it sets the column width unit).
pub fn blank_workbook(language: &str) -> Vec<u8> {
    let font = match language {
        "zh-Hans" => "等线",
        "zh-Hant" => "新細明體",
        "ja" => "游ゴシック",
        "ko" => "맑은 고딕",
        _ => "Calibri",
    };
    aster_ooxml::xlsx_save::blank("Sheet1", font)
}

/// The engine's edits for cell changes.
pub fn edits(changes: &[Change]) -> Vec<Edit> {
    changes
        .iter()
        .map(
            |change| match change.after.as_ref().and_then(|c| c.formula.clone()) {
                Some(formula) => Edit::Formula {
                    sheet: change.sheet,
                    at: change.at,
                    formula: formula.to_string(),
                },
                None => Edit::Value {
                    sheet: change.sheet,
                    at: change.at,
                    value: change
                        .after
                        .as_ref()
                        .map(|c| c.value.clone())
                        .unwrap_or_default(),
                },
            },
        )
        .collect()
}

/// The usual order of a date written with numbers in a language.
pub fn date_order(language: &str) -> DateOrder {
    match language.split('-').next().unwrap_or_default() {
        "en" => DateOrder::Mdy,
        "fr" | "de" | "es" => DateOrder::Dmy,
        _ => DateOrder::Ymd,
    }
}

fn this_year() -> i64 {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    // Days since 1970 to an Excel serial, then to its year.
    aster_sheets::date_parts(days as f64 + 25_569.0, false).map_or(2026, |p| i64::from(p[0]))
}

pub struct SlidesTab {
    pub view: SlideView,
    pub path: PathBuf,
}

impl SlidesTab {
    pub fn new(
        presentation: Presentation,
        path: PathBuf,
        tokens: &theme::Tokens,
        strings: &Strings,
    ) -> Self {
        let mut view = SlideView::new(Arc::new(presentation));
        view.set_style(slide_style(tokens, strings));
        Self { view, path }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens, strings: &Strings) {
        self.view.set_style(slide_style(tokens, strings));
    }
}

fn opaque(color: theme::Rgba, under: theme::Rgba) -> [u8; 4] {
    color.over(under).to_array()
}

pub fn sheet_style(tokens: &theme::Tokens) -> SheetStyle {
    let c = &tokens.colors;
    SheetStyle {
        background: c.surface.to_array(),
        text: c.text.to_array(),
        secondary: opaque(c.text_secondary, c.surface),
        grid: opaque(c.hairline, c.surface),
        header: opaque(c.panel, c.surface),
        header_text: opaque(c.text_secondary, c.panel),
        header_active: opaque(c.accent_soft, c.panel),
        accent: c.accent.to_array(),
    }
}

pub fn slide_style(tokens: &theme::Tokens, strings: &Strings) -> SlideStyle {
    let c = &tokens.colors;
    SlideStyle {
        background: opaque(c.desk, c.surface),
        strip: opaque(c.panel, c.surface),
        hairline: opaque(c.hairline, c.surface),
        text: c.text.to_array(),
        secondary: opaque(c.text_secondary, c.panel),
        accent: c.accent.to_array(),
        picture: strings.shape_picture.to_owned(),
        table: strings.shape_table.to_owned(),
        chart: strings.shape_chart.to_owned(),
        graphic: strings.shape_graphic.to_owned(),
    }
}

/// Keys the views understand, from what the window reports. Kept apart from
/// the toolkit's key codes: `key` is the name the shell decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Enter,
    Space,
}

pub fn sheet_key(key: Key, shift: bool, primary: bool) -> Option<SheetKey> {
    Some(match (key, primary) {
        (Key::Up, true) => SheetKey::Jump(Direction::Up),
        (Key::Down, true) => SheetKey::Jump(Direction::Down),
        (Key::Left, true) => SheetKey::Jump(Direction::Left),
        (Key::Right, true) => SheetKey::Jump(Direction::Right),
        (Key::Home, true) => SheetKey::Start,
        (Key::End, true) => SheetKey::Last,
        (Key::Up, false) => SheetKey::Up,
        (Key::Down, false) => SheetKey::Down,
        (Key::Left, false) => SheetKey::Left,
        (Key::Right, false) => SheetKey::Right,
        (Key::PageUp, _) => SheetKey::PageUp,
        (Key::PageDown, _) => SheetKey::PageDown,
        (Key::Home, false) => SheetKey::Home,
        (Key::End, false) => SheetKey::End,
        (Key::Tab, _) if shift => SheetKey::Left,
        (Key::Tab, _) => SheetKey::Right,
        (Key::Enter, _) if shift => SheetKey::Up,
        (Key::Enter, _) => SheetKey::Down,
        (Key::Space, _) => return None,
    })
}

/// Move through slides. Returns whether the slide changed.
pub fn slide_key(view: &mut SlideView, key: Key, shift: bool) -> bool {
    match key {
        Key::Up | Key::Left | Key::PageUp => view.previous_slide(),
        Key::Space | Key::Enter if shift => view.previous_slide(),
        Key::Down | Key::Right | Key::PageDown | Key::Space | Key::Enter | Key::Tab => {
            view.next_slide()
        }
        Key::Home => view.go_to(0),
        Key::End => view.last_slide(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_choose_the_view() {
        assert_eq!(kind_of(Path::new("预算.XLSX")), Some(DocKind::Sheets));
        assert_eq!(kind_of(Path::new("a/b/マクロ.xlsm")), Some(DocKind::Sheets));
        assert_eq!(kind_of(Path::new("発表.pptx")), Some(DocKind::Slides));
        assert_eq!(kind_of(Path::new("old.xls")), None);
        assert_eq!(kind_of(Path::new("notes.md")), None);
        assert_eq!(kind_of(Path::new("xlsx")), None);
    }

    #[test]
    fn errors_have_a_message_in_every_language() {
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            let legacy = LoadError::Format(aster_ooxml::Error::LegacyOrEncrypted);
            assert_eq!(legacy.message(strings), strings.office_legacy);
            let broken = LoadError::Format(aster_ooxml::Error::Broken("xl/workbook.xml: x".into()));
            assert!(broken.message(strings).contains("xl/workbook.xml"));
        }
    }

    #[test]
    fn a_missing_file_and_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load(&dir.path().join("无.xlsx")),
            Err(LoadError::Io(_))
        ));
        let fake = dir.path().join("假的.xlsx");
        std::fs::write(&fake, "这不是表格").unwrap();
        let Err(LoadError::Format(err)) = load(&fake) else {
            panic!("expected a format error")
        };
        assert_eq!(err.kind(), ErrorKind::NotZip);
    }

    #[test]
    fn keys_map_to_moves() {
        assert_eq!(
            sheet_key(Key::Down, false, true),
            Some(SheetKey::Jump(Direction::Down))
        );
        assert_eq!(sheet_key(Key::Tab, true, false), Some(SheetKey::Left));
        assert_eq!(sheet_key(Key::Enter, false, false), Some(SheetKey::Down));
        assert_eq!(sheet_key(Key::Home, false, true), Some(SheetKey::Start));
        assert_eq!(sheet_key(Key::Space, false, false), None);
    }

    #[test]
    fn the_worker_calculates_edits_and_reports_cycles() {
        use aster_surface::cell_editor::EditMode;

        let mut tab = tab();
        let (send, receive) = mpsc::channel();
        tab.start_calc(move |message| {
            let _ = send.send(message);
        });
        let wait = |tab: &mut SheetTab| {
            let message = receive
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("a result");
            tab.take_calc(message);
        };
        wait(&mut tab);
        assert_eq!(tab.calc, Calc::Done(0));
        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            let changes = tab.commit("en");
            tab.recalc(&changes);
        };
        enter(&mut tab, "B1", "=A1+A2*10");
        assert_eq!(tab.calc, Calc::Running);
        wait(&mut tab);
        assert_eq!(value(&tab, "B1"), Some(aster_sheets::Value::Number(21.0)));
        assert!(tab.dirty.contains(&(0, at("B1"))));
        enter(&mut tab, "A1", "5");
        enter(&mut tab, "C1", "=C2");
        enter(&mut tab, "C2", "=C1");
        // Batches sent while one is being calculated are taken together.
        while tab.calc == Calc::Running {
            wait(&mut tab);
        }
        assert_eq!(value(&tab, "B1"), Some(aster_sheets::Value::Number(25.0)));
        assert_eq!(tab.circular, [(0, at("C1")), (0, at("C2"))]);
        assert_eq!(value(&tab, "C2"), Some(aster_sheets::Value::Number(0.0)));
        // Undo goes through the engine too.
        let changes = tab.undo();
        tab.recalc(&changes);
        while tab.calc == Calc::Running {
            wait(&mut tab);
        }
        assert!(tab.circular.is_empty());
    }

    fn tab() -> SheetTab {
        let mut sheet = aster_sheets::SheetBuilder::new("表", aster_sheets::SheetKind::Worksheet);
        sheet.push(
            0,
            aster_sheets::Cell::new(0, aster_sheets::Value::Number(1.0)),
        );
        sheet.push(
            1,
            aster_sheets::Cell::new(0, aster_sheets::Value::Number(2.0)),
        );
        let workbook = Workbook {
            sheets: vec![sheet.finish()],
            styles: vec![
                aster_sheets::CellStyle::default(),
                aster_sheets::CellStyle {
                    format: Arc::new(aster_sheets::Format::builtin(14)),
                    ..aster_sheets::CellStyle::default()
                },
            ],
            ..Workbook::default()
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        SheetTab::new(workbook, Arc::new(Vec::new()), None, &tokens)
    }

    fn at(text: &str) -> CellRef {
        CellRef::parse(text).unwrap()
    }

    fn value(tab: &SheetTab, cell: &str) -> Option<aster_sheets::Value> {
        tab.view.sheet()?.cell(at(cell)).map(|c| c.value.clone())
    }

    #[test]
    fn typing_commits_one_undoable_step_at_a_time() {
        use aster_surface::cell_editor::EditMode;

        let mut tab = tab();
        assert!(!tab.is_modified());
        tab.view.select(at("B1"));
        tab.view.begin_edit(EditMode::Enter, Some("12%".into()));
        let changes = tab.commit("zh-Hans");
        assert_eq!(changes.len(), 1);
        assert_eq!(value(&tab, "B1"), Some(aster_sheets::Value::Number(0.12)));
        assert!(tab.is_modified());
        assert!(tab.dirty.contains(&(0, at("B1"))));
        assert!(!tab.formulas_changed);

        tab.view.select(at("C1"));
        tab.view.begin_edit(EditMode::Enter, Some("=a1+a2".into()));
        tab.commit("en");
        let formula = tab
            .view
            .sheet()
            .unwrap()
            .cell(at("C1"))
            .unwrap()
            .formula
            .clone();
        assert_eq!(formula.as_deref(), Some("A1+A2"));
        assert!(tab.formulas_changed);
        let edits = edits(&tab.history.undo[1]);
        assert!(matches!(&edits[0], Edit::Formula { formula, .. } if formula == "A1+A2"));

        // A date typed in English is month first; in Chinese year first.
        tab.view.select(at("D1"));
        tab.view
            .begin_edit(EditMode::Enter, Some("3/4/2026".into()));
        tab.commit("en");
        let march = aster_sheets::date_serial(2026, 3, 4, false).unwrap();
        assert_eq!(value(&tab, "D1"), Some(aster_sheets::Value::Number(march)));
        assert_eq!(tab.view.sheet().unwrap().cell(at("D1")).unwrap().style, 1);

        // Escape: nothing kept.
        tab.view.begin_edit(EditMode::Enter, Some("x".into()));
        assert!(tab.view.cancel_edit());
        assert!(tab.commit("en").is_empty());

        // Undo back to the file as read: no longer modified.
        assert_eq!(tab.undo().len(), 1);
        assert_eq!(tab.undo().len(), 1);
        assert!(tab.is_modified());
        assert_eq!(tab.undo().len(), 1);
        assert!(!tab.is_modified());
        assert_eq!(value(&tab, "B1"), None);
        assert!(tab.undo().is_empty());
        // Redo selects what it changed.
        assert_eq!(tab.redo().len(), 1);
        assert_eq!(tab.view.selected(), at("B1"));
        assert!(tab.is_modified());
        tab.history.mark_saved();
        assert!(!tab.is_modified());
        tab.undo();
        assert!(tab.is_modified());
        // A new step after undoing past the save: the saved state is gone.
        tab.view.begin_edit(EditMode::Enter, Some("z".into()));
        tab.commit("en");
        assert!(tab.is_modified());
        assert!(!tab.history.can_redo());
    }

    #[test]
    fn clearing_a_selection_is_one_step() {
        let mut tab = tab();
        tab.view.select(at("A1"));
        tab.view.extend_to(at("C9"));
        let changes = tab.clear_selection();
        assert_eq!(changes.len(), 2);
        assert_eq!(tab.view.sheet().unwrap().cell_count(), 0);
        // Nothing left to clear: no step.
        assert!(tab.clear_selection().is_empty());
        tab.undo();
        assert_eq!(value(&tab, "A2"), Some(aster_sheets::Value::Number(2.0)));
        assert!(!tab.is_modified());
    }

    #[test]
    fn dates_read_in_the_language_order() {
        assert_eq!(date_order("en"), DateOrder::Mdy);
        assert_eq!(date_order("de"), DateOrder::Dmy);
        assert_eq!(date_order("zh-Hant"), DateOrder::Ymd);
        assert_eq!(date_order("ja"), DateOrder::Ymd);
        assert!((2026..2200).contains(&this_year()));
    }

    #[test]
    fn copy_and_paste_within_and_from_outside() {
        let mut tab = tab();
        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(
                aster_surface::cell_editor::EditMode::Enter,
                Some(text.into()),
            );
            tab.commit("en");
        };
        enter(&mut tab, "B1", "=A1*2");
        tab.view.select(at("A1"));
        tab.view.extend_to(at("B1"));
        let text = tab.copy(false).unwrap();
        assert_eq!(text, "1\t=A1*2");
        // Pasting what was copied here moves the formula with it; the
        // system clipboard may have turned line ends into \n.
        tab.view.select(at("A3"));
        let changes = tab.paste(&text, "en");
        assert_eq!(changes.len(), 2);
        let b3 = tab
            .view
            .sheet()
            .unwrap()
            .cell(at("B3"))
            .unwrap()
            .formula
            .clone();
        assert_eq!(b3.as_deref(), Some("A3*2"));
        assert_eq!(tab.view.selection().to_string(), "A3:B3");
        // Text from elsewhere is read cell by cell.
        tab.view.select(at("D1"));
        let changes = tab.paste("12%\tTRUE\r\n2026/1/2\t=d1+1\r\n", "zh-Hans");
        assert_eq!(changes.len(), 4);
        assert_eq!(value(&tab, "D1"), Some(aster_sheets::Value::Number(0.12)));
        assert_eq!(value(&tab, "E1"), Some(aster_sheets::Value::Bool(true)));
        let e2 = tab
            .view
            .sheet()
            .unwrap()
            .cell(at("E2"))
            .unwrap()
            .formula
            .clone();
        assert_eq!(e2.as_deref(), Some("D1+1"));
        // One cell over a selection fills it.
        tab.view.select(at("A1"));
        tab.copy(false);
        tab.view.select(at("G1"));
        tab.view.extend_to(at("H3"));
        let text = tab.clip.as_ref().unwrap().text.clone();
        assert_eq!(tab.paste(&text, "en").len(), 6);
        // Cutting moves: the source empties, the formula stays as it was.
        tab.view.select(at("B3"));
        let text = tab.copy(true).unwrap();
        tab.view.select(at("C9"));
        tab.paste(&text, "en");
        assert!(tab.view.sheet().unwrap().cell(at("B3")).is_none());
        let moved = tab
            .view
            .sheet()
            .unwrap()
            .cell(at("C9"))
            .unwrap()
            .formula
            .clone();
        assert_eq!(moved.as_deref(), Some("A3*2"));
        // One undo step brings both back.
        tab.undo();
        assert!(tab.view.sheet().unwrap().cell(at("B3")).is_some());
        assert!(tab.view.sheet().unwrap().cell(at("C9")).is_none());
    }

    #[test]
    fn open_type_save_and_open_again() {
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("新しい.xlsx");
        std::fs::write(&file, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let Ok(Loaded::Workbook(workbook, source)) = load(&file) else {
            panic!("a blank workbook loads");
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        let mut tab = SheetTab::new(workbook, source, Some(file.clone()), &tokens);
        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            tab.commit("ja");
        };
        enter(&mut tab, "A1", "東京");
        enter(&mut tab, "A2", "2026/11/24");
        enter(&mut tab, "B2", "=A2+1");
        let job = tab.begin_save().unwrap();
        // One save at a time.
        assert!(tab.begin_save().is_none());
        // Typed while the save runs: still to be saved afterwards.
        enter(&mut tab, "C3", "後で");
        let written = job.write(&file).unwrap();
        tab.finish_save(&job, &file, Some(written));
        assert!(tab.is_modified());
        assert_eq!(tab.dirty.iter().collect::<Vec<_>>(), [&(0, at("C3"))]);
        let job = tab.begin_save().unwrap();
        let written = job.write(&file).unwrap();
        tab.finish_save(&job, &file, Some(written));
        assert!(!tab.is_modified());
        assert!(tab.dirty.is_empty() && !tab.formulas_changed);

        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        let cell = |c: &str| again.sheets[0].cell(at(c)).unwrap().clone();
        assert_eq!(cell("A1").value, aster_sheets::Value::Text("東京".into()));
        assert_eq!(again.display(&cell("A2")).unwrap().text, "2026/11/24");
        assert_eq!(cell("B2").formula.as_deref(), Some("A2+1"));
        assert_eq!(cell("C3").value, aster_sheets::Value::Text("後で".into()));
        // A failed save changes nothing.
        enter(&mut tab, "D4", "1");
        let job = tab.begin_save().unwrap();
        let failed = job.write(&dir.path().join("無い/x.xlsx"));
        assert!(failed.is_err());
        tab.finish_save(&job, &file, None);
        assert!(tab.is_modified() && !tab.saving);
    }
}
