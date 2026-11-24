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
use aster_sheets::structure::Lines;
use aster_sheets::{HAlign, MAX_COLUMNS, MAX_ROWS, StyleEdit};
use aster_surface::cell_editor::EditMode;

use crate::toolbar::{CellLook, Command, font_size_step};
use aster_sheets::{
    CalcEngine, CalcError, Cancel, CellRange, CellRef, Change, Computed, DateOrder, Edit, Workbook,
};
use aster_slides::Presentation;
use aster_surface::sheet_view::{Direction, MIN_SIZE, Resize, SheetKey, SheetStyle, SheetView};
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
/// A batch for the calculation thread: changed cells, or the whole
/// workbook again after its cells moved.
enum Job {
    Edits(Vec<Edit>),
    Reload(Arc<Workbook>),
}

pub struct CalcWorker {
    jobs: Option<Sender<Job>>,
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
        self.send(Job::Edits(edits))
    }

    /// Rows or columns moved: the engine starts again from `workbook`.
    pub fn reload(&mut self, workbook: Arc<Workbook>) -> u64 {
        self.send(Job::Reload(workbook))
    }

    fn send(&mut self, job: Job) -> u64 {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_ok()
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
    jobs: Receiver<Job>,
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
    let mut reload: Option<Arc<Workbook>> = None;
    fn take(job: Job, edits: &mut Vec<Edit>, reload: &mut Option<Arc<Workbook>>) {
        match job {
            Job::Edits(batch) => edits.extend(batch),
            // The workbook holds every edit before it.
            Job::Reload(workbook) => {
                edits.clear();
                *reload = Some(workbook);
            }
        }
    }
    loop {
        if !first && !again {
            match jobs.recv() {
                Ok(job) => {
                    take(job, &mut edits, &mut reload);
                    generation += 1;
                }
                Err(_) => return,
            }
        }
        loop {
            match jobs.try_recv() {
                Ok(job) => {
                    take(job, &mut edits, &mut reload);
                    generation += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(workbook) = reload.take() {
            match aster_calc::Formualizer::load(&workbook) {
                Ok(fresh) => engine = fresh,
                Err(error) => return send(CalcMessage::Failed(error.to_string())),
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
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// How deep the undo stack was when the file was last saved; `None`
    /// once that state can no longer be reached.
    saved: Option<usize>,
}

/// One entry, paste or clear: its cell changes, and the cell formats it
/// added (a date format for a typed date), which go with it on undo.
#[derive(Debug, Clone, Default)]
pub struct Step {
    pub changes: Vec<Change>,
    /// Merged areas changed: sheet, before, after.
    merges: Vec<(usize, Vec<CellRange>, Vec<CellRange>)>,
    /// A column or row resized: sheet, which, size before and after.
    sizes: Vec<(usize, Resize, f64, f64)>,
    /// How many styles the workbook had before the step.
    styles_before: usize,
    /// The styles undo took away, put back by redo.
    added: Vec<aster_sheets::CellStyle>,
    /// Rows or columns inserted or deleted.
    lines: Option<Box<Restructure>>,
    /// A sheet added, renamed or deleted.
    sheets: Option<Box<SheetStep>>,
}

/// What happened to a sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SheetChange {
    Added(usize),
    Renamed(usize),
    Deleted(usize),
}

/// A sheet added, renamed or deleted: the sheets before and after, the
/// cells whose formulas changed with it (counted as after it), and for a
/// deletion what saving had to write on the sheet then, and which sheet of
/// the file it was.
#[derive(Debug, Clone)]
struct SheetStep {
    change: SheetChange,
    before: Vec<aster_sheets::Sheet>,
    after: Vec<aster_sheets::Sheet>,
    formulas: Vec<(usize, CellRef)>,
    marks: Marks,
    origin: Option<usize>,
    /// Which file the origin is of: saving makes a new one.
    file: u64,
}

/// What saving must write on one sheet, as kept by a deletion.
#[derive(Debug, Clone, Default)]
struct Marks {
    dirty: Vec<CellRef>,
    edited: Vec<CellRef>,
    merges: bool,
    columns: Vec<u32>,
    rows: Vec<u32>,
}

/// Rows or columns inserted or deleted: the sheets before and after, and
/// the cells of other sheets whose formulas changed with them.
#[derive(Debug, Clone)]
struct Restructure {
    sheet: usize,
    lines: Lines,
    before: Vec<aster_sheets::Sheet>,
    after: Vec<aster_sheets::Sheet>,
    others: Vec<(usize, CellRef)>,
    /// What inserted lines took from the line before them.
    inherited: aster_sheets::structure::Inherited,
}

/// Why rows or columns cannot be inserted or deleted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The sheet holds something that would not follow (a table, a pivot
    /// table, a chart's data, comments ...); its name in the file.
    Holds(String),
    /// Inserting would push filled cells off the sheet.
    Full,
    /// The only visible worksheet cannot be deleted.
    LastSheet,
    /// The sheet holds something deleting it would leave broken (an Excel
    /// table, a pivot table): its name in the file.
    SheetHolds(String),
}

impl History {
    fn push(&mut self, step: Step) {
        if step.changes.is_empty()
            && step.merges.is_empty()
            && step.sizes.is_empty()
            && step.lines.is_none()
            && step.sheets.is_none()
        {
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
    /// Sheets whose merged areas changed since then.
    pub merges_changed: BTreeSet<usize>,
    /// Columns and rows resized since then: sheet and index.
    pub columns_changed: BTreeSet<(usize, u32)>,
    pub rows_changed: BTreeSet<(usize, u32)>,
    /// Rows and columns inserted and deleted since then, in order, by the
    /// file's sheet numbers.
    pub structure: Vec<(usize, Lines)>,
    /// For each sheet, its number in the file as last read or saved;
    /// `None` for a sheet added since.
    pub origins: Vec<Option<usize>>,
    /// How many times the file was saved here: sheet numbers of an older
    /// one mean nothing in the new one.
    file: u64,
    /// Why the last insert or delete of rows or columns was refused, shown
    /// until the next change.
    pub refusal: Option<Refusal>,
    /// A column or row being dragged, its size before, and how far the
    /// pointer was from its edge when the drag started.
    resizing: Option<(Resize, f64, f64)>,
    /// The file as last read or saved: saving starts from it.
    pub source: Arc<Vec<u8>>,
    /// How many cell formats that file has. Styles past these were added by
    /// typing and are not in the file yet; undo may take them away, never
    /// the file's own.
    file_styles: usize,
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
        let view_styles = workbook.styles.len();
        let sheet_count = workbook.sheets.len();
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
            merges_changed: BTreeSet::new(),
            columns_changed: BTreeSet::new(),
            rows_changed: BTreeSet::new(),
            structure: Vec::new(),
            origins: (0..sheet_count).map(Some).collect(),
            file: 0,
            refusal: None,
            resizing: None,
            file_styles: view_styles,
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
        let styles_before = self.view.workbook().styles.len();
        let (entry, look) = input::read(&text, &reading);
        let cell = self.view.workbook_mut().entry_cell(sheet, at, entry, look);
        let changes = self.view.workbook_mut().set_cells(sheet, vec![(at, cell)]);
        self.record(changes, styles_before)
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
        let styles_before = self.view.workbook().styles.len();
        let changes = self.view.workbook_mut().set_cells(sheet_index, cleared);
        self.record(changes, styles_before)
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
        let styles_before = self.view.workbook().styles.len();
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
        self.record(changes, styles_before)
    }

    /// Type `rows` of entries into the sheet in front, starting at `start`,
    /// as if each were typed into its cell (numbers, dates, `=formulas`,
    /// text; an empty entry empties the cell). One undo step. Entries that
    /// would fall outside the sheet are left out.
    pub fn type_cells(
        &mut self,
        start: CellRef,
        rows: &[Vec<String>],
        language: &str,
    ) -> Vec<Change> {
        let sheet = self.view.sheet_index();
        let styles_before = self.view.workbook().styles.len();
        let reading = self.reading(language);
        let mut cells = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, field) in row.iter().enumerate() {
                let (Ok(r), Ok(c)) = (u32::try_from(r), u32::try_from(c)) else {
                    continue;
                };
                let at = CellRef::new(start.row.saturating_add(r), start.col.saturating_add(c));
                if at.row >= aster_sheets::MAX_ROWS || at.col >= aster_sheets::MAX_COLUMNS {
                    continue;
                }
                let (entry, look) = input::read(field, &reading);
                let cell = self.view.workbook_mut().entry_cell(sheet, at, entry, look);
                cells.push((at, cell));
            }
        }
        let changes = self.view.workbook_mut().set_cells(sheet, cells);
        self.record(changes, styles_before)
    }

    /// Put cells in place on the current sheet as one undo step.
    pub fn put(&mut self, cells: Vec<(CellRef, Option<aster_sheets::Cell>)>) -> Vec<Change> {
        let sheet = self.view.sheet_index();
        let styles_before = self.view.workbook().styles.len();
        let changes = self.view.workbook_mut().set_cells(sheet, cells);
        self.record(changes, styles_before)
    }

    fn record(&mut self, changes: Vec<Change>, styles_before: usize) -> Vec<Change> {
        if changes.is_empty() {
            // Nothing changed after all: a style made for it is not needed.
            self.drop_styles(styles_before);
            return changes;
        }
        self.note(&changes);
        self.history.push(Step {
            changes: changes.clone(),
            merges: Vec::new(),
            sizes: Vec::new(),
            styles_before,
            added: Vec::new(),
            lines: None,
            sheets: None,
        });
        changes
    }

    /// Take away the styles past `count`, but never the file's own; returns
    /// them.
    fn drop_styles(&mut self, count: usize) -> Vec<aster_sheets::CellStyle> {
        let keep = count.max(self.file_styles);
        if self.view.workbook().styles.len() <= keep {
            return Vec::new();
        }
        self.view.workbook_mut().styles.split_off(keep)
    }

    /// Remember which cells saving must write.
    fn note(&mut self, changes: &[Change]) {
        if !changes.is_empty() {
            self.refusal = None;
        }
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
                merges: self.merges_changed.clone(),
                columns: self.columns_changed.clone(),
                rows: self.rows_changed.clone(),
                structure: self.structure.clone(),
                origins: self.origins.clone(),
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
        // The written file holds every style the job had, even one undone
        // meanwhile: those are the file's own now.
        let written_styles = &job.workbook.styles;
        let now = self.view.workbook().styles.len();
        if now < written_styles.len() {
            let missing = written_styles[now..].to_vec();
            self.view.workbook_mut().styles.extend(missing);
        }
        self.file_styles = self.file_styles.max(written_styles.len());
        let unchanged_since = self.history.depth() == job.depth;
        // What moved since the save started was not written; cells written
        // were where they are now only if nothing moved since.
        let moved_since = self.structure.len() > job.changes.structure.len();
        self.structure
            .drain(..job.changes.structure.len().min(self.structure.len()));
        // The written file has the job's sheets in the job's order. Sheets
        // cannot change while a save runs, so they are the ones here now.
        let renumber = |file: usize| job.changes.origins.iter().position(|o| *o == Some(file));
        self.structure = std::mem::take(&mut self.structure)
            .into_iter()
            .filter_map(|(file, lines)| Some((renumber(file)?, lines)))
            .collect();
        self.origins = (0..self.view.workbook().sheets.len()).map(Some).collect();
        self.file += 1;
        for cell in job.changes.cells.iter().filter(|_| !moved_since) {
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
            self.merges_changed.clear();
            self.columns_changed.clear();
            self.rows_changed.clear();
        }
        self.history.mark_saved_at(job.depth);
    }

    /// Undo the last step. Returns the changes made, to recalculate.
    pub fn undo(&mut self) -> Vec<Change> {
        self.view.cancel_edit();
        let Some(mut step) = self.history.undo.pop() else {
            return Vec::new();
        };
        let inverse: Vec<Change> = step.changes.iter().rev().map(Change::inverse).collect();
        self.view.workbook_mut().apply(&inverse);
        for (sheet, before, _) in &step.merges {
            self.set_merges(*sheet, before.clone());
        }
        for &(sheet, resize, before, _) in &step.sizes {
            self.set_size(sheet, resize, before);
        }
        if let Some(change) = &step.lines {
            self.view.workbook_mut().sheets = change.before.clone();
            let back = Lines {
                insert: !change.lines.insert,
                ..change.lines
            };
            self.moved(change.sheet, back, &change.others);
            if back.insert {
                self.restored(change.sheet, change.lines);
            }
            self.reload_calc();
        }
        if let Some(step) = &step.sheets {
            self.undo_sheets(step);
        }
        // The formats the step added go with it: no cell uses them now,
        // since every later step was undone first.
        step.added = self.drop_styles(step.styles_before);
        self.note(&inverse);
        self.show(&inverse);
        self.history.redo.push(step);
        inverse
    }

    pub fn redo(&mut self) -> Vec<Change> {
        self.view.cancel_edit();
        let Some(mut step) = self.history.redo.pop() else {
            return Vec::new();
        };
        // Its formats come back at the places its cells refer to.
        let added = std::mem::take(&mut step.added);
        if !added.is_empty() {
            let workbook = self.view.workbook_mut();
            if workbook.styles.len() == step.styles_before.max(self.file_styles) {
                workbook.styles.extend(added);
            }
        }
        self.view.workbook_mut().apply(&step.changes);
        for (sheet, _, after) in &step.merges {
            self.set_merges(*sheet, after.clone());
        }
        for &(sheet, resize, _, after) in &step.sizes {
            self.set_size(sheet, resize, after);
        }
        if let Some(change) = &step.lines {
            self.view.workbook_mut().sheets = change.after.clone();
            self.moved(change.sheet, change.lines, &change.others);
            self.mark_inherited(change.sheet, change.lines, &change.inherited);
            if change.lines.insert {
                self.restored(change.sheet, change.lines);
            }
            self.reload_calc();
        }
        if let Some(step) = &step.sheets {
            self.redo_sheets(step);
        }
        self.note(&step.changes);
        self.show(&step.changes);
        let applied = step.changes.clone();
        self.history.undo.push(step);
        applied
    }

    /// Insert or delete rows or columns of the active sheet: those the
    /// selection spans. Formulas everywhere follow the cells. One undo step.
    pub fn change_lines(&mut self, columns: bool, insert: bool) -> Result<(), Refusal> {
        let sheet = self.view.sheet_index();
        let range = self.view.selection();
        let Some(current) = self.view.sheet() else {
            return Ok(());
        };
        if let Some(holds) = &current.structure_blocker {
            return Err(Refusal::Holds(holds.clone()));
        }
        let lines = if columns {
            Lines {
                columns,
                at: range.first.col,
                count: range.cols(),
                insert,
            }
        } else {
            Lines {
                columns,
                at: range.first.row,
                count: range.rows(),
                insert,
            }
        };
        if insert {
            let limit = if columns { MAX_COLUMNS } else { MAX_ROWS };
            let pushed_off = current.rows().iter().any(|row| {
                if columns {
                    row.cells
                        .iter()
                        .any(|c| c.col >= limit - lines.count && !c.is_blank())
                } else {
                    row.index >= limit - lines.count && row.cells.iter().any(|c| !c.is_blank())
                }
            });
            if pushed_off {
                return Err(Refusal::Full);
            }
        }
        self.view.cancel_edit();
        let before = self.view.workbook().sheets.clone();
        let others = self.view.workbook_mut().restructure(sheet, &lines);
        let inherited = self.view.workbook_mut().inherit_format(sheet, &lines);
        let after = self.view.workbook().sheets.clone();
        self.moved(sheet, lines, &others);
        self.mark_inherited(sheet, lines, &inherited);
        self.history.push(Step {
            styles_before: self.view.workbook().styles.len(),
            lines: Some(Box::new(Restructure {
                sheet,
                lines,
                before,
                after,
                others,
                inherited,
            })),
            ..Step::default()
        });
        self.reload_calc();
        Ok(())
    }

    /// Lines moved on `sheet`: what saving must write moves with them.
    fn moved(&mut self, sheet: usize, lines: Lines, others: &[(usize, CellRef)]) {
        let shift = |at: CellRef| -> Option<CellRef> {
            if lines.columns {
                Some(CellRef::new(at.row, lines.moved(at.col)?))
            } else {
                Some(CellRef::new(lines.moved(at.row)?, at.col))
            }
        };
        let remap = |set: &mut BTreeSet<(usize, CellRef)>| {
            *set = std::mem::take(set)
                .into_iter()
                .filter_map(|(s, at)| {
                    if s == sheet {
                        Some((s, shift(at)?))
                    } else {
                        Some((s, at))
                    }
                })
                .collect();
        };
        remap(&mut self.dirty);
        remap(&mut self.edited);
        let resized = if lines.columns {
            &mut self.columns_changed
        } else {
            &mut self.rows_changed
        };
        *resized = std::mem::take(resized)
            .into_iter()
            .filter_map(|(s, i)| {
                if s == sheet {
                    Some((s, lines.moved(i)?))
                } else {
                    Some((s, i))
                }
            })
            .collect();
        for &cell in others {
            self.dirty.insert(cell);
            self.edited.insert(cell);
        }
        self.formulas_changed = true;
        self.merges_changed.insert(sheet);
        if let Some(Some(file)) = self.origins.get(sheet) {
            // Undoing the last move still to be saved takes it out rather
            // than adding its opposite: the file's rows keep their bytes
            // (styles, hidden, heights) and pictures their anchors. Not
            // while a save is writing what is listed.
            let undone = (
                *file,
                Lines {
                    insert: !lines.insert,
                    ..lines
                },
            );
            if !self.saving && self.structure.last() == Some(&undone) {
                self.structure.pop();
            } else {
                self.structure.push((*file, lines));
            }
        }
    }

    /// Cells and sizes inserted lines took from their neighbour are for
    /// saving.
    fn mark_inherited(
        &mut self,
        sheet: usize,
        lines: Lines,
        inherited: &aster_sheets::structure::Inherited,
    ) {
        self.dirty
            .extend(inherited.cells.iter().map(|&at| (sheet, at)));
        self.edited
            .extend(inherited.cells.iter().map(|&at| (sheet, at)));
        if inherited.sized {
            let resized = if lines.columns {
                &mut self.columns_changed
            } else {
                &mut self.rows_changed
            };
            resized.extend((lines.at..lines.at + lines.count).map(|i| (sheet, i)));
        }
    }

    /// Deleted lines came back (undo of a delete, redo of an insert has
    /// none): their cells and sizes are written again, the file lost them.
    fn restored(&mut self, sheet: usize, lines: Lines) {
        let Some(target) = self.view.workbook().sheets.get(sheet) else {
            return;
        };
        let span = lines.at..lines.at + lines.count;
        let mut cells = Vec::new();
        for row in target.rows() {
            for cell in &row.cells {
                let line = if lines.columns { cell.col } else { row.index };
                if span.contains(&line) {
                    cells.push((sheet, CellRef::new(row.index, cell.col)));
                }
            }
        }
        let axis = if lines.columns {
            &target.columns
        } else {
            &target.row_sizes
        };
        let custom: Vec<u32> = span
            .clone()
            .filter(|&i| axis.size(i) != axis.default_size())
            .collect();
        self.dirty.extend(cells.iter().copied());
        self.edited.extend(cells);
        let resized = if lines.columns {
            &mut self.columns_changed
        } else {
            &mut self.rows_changed
        };
        resized.extend(custom.into_iter().map(|i| (sheet, i)));
    }

    /// Add an empty worksheet after the others and show it. One undo step.
    pub fn add_sheet(&mut self, base: &str) -> Option<usize> {
        if self.saving {
            return None;
        }
        self.view.cancel_edit();
        let before = self.view.workbook().sheets.clone();
        let name = self.view.workbook().new_sheet_name(base);
        let index = self.view.workbook_mut().add_sheet(&name);
        self.view.sheet_inserted(index);
        self.origins.insert(index, None);
        self.shift_marks(index, true);
        self.sheet_step(
            SheetChange::Added(index),
            before,
            Vec::new(),
            Marks::default(),
            None,
        );
        self.view.set_sheet(index);
        Some(index)
    }

    /// Whether sheet `index` could be called `name`.
    pub fn check_sheet_name(
        &self,
        index: usize,
        name: &str,
    ) -> Result<(), aster_sheets::NameProblem> {
        self.view.workbook().check_sheet_name(name, Some(index))
    }

    /// Rename sheet `index`; formulas that name it follow. One undo step.
    pub fn rename_sheet(
        &mut self,
        index: usize,
        name: &str,
    ) -> Result<(), aster_sheets::NameProblem> {
        self.check_sheet_name(index, name)?;
        if self.saving
            || self
                .view
                .workbook()
                .sheets
                .get(index)
                .is_none_or(|s| s.name == name)
        {
            return Ok(());
        }
        self.view.cancel_edit();
        let before = self.view.workbook().sheets.clone();
        let formulas = self.view.workbook_mut().rename_sheet(index, name);
        self.mark_formulas(&formulas);
        self.sheet_step(
            SheetChange::Renamed(index),
            before,
            formulas,
            Marks::default(),
            None,
        );
        Ok(())
    }

    /// Delete sheet `index`; references to it become `#REF!`. One undo
    /// step: undo brings it back as it was.
    pub fn delete_sheet(&mut self, index: usize) -> Result<(), Refusal> {
        let workbook = self.view.workbook();
        let Some(sheet) = workbook.sheets.get(index) else {
            return Ok(());
        };
        if let Some(holds) = &sheet.delete_blocker {
            return Err(Refusal::SheetHolds(holds.clone()));
        }
        let visible_worksheets = workbook
            .sheets
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                *i != index && !s.hidden && s.kind == aster_sheets::SheetKind::Worksheet
            })
            .count();
        if visible_worksheets == 0 {
            return Err(Refusal::LastSheet);
        }
        if self.saving {
            return Ok(());
        }
        self.view.cancel_edit();
        let before = self.view.workbook().sheets.clone();
        let marks = self.take_marks(index);
        let origin = self.origins.remove(index);
        self.shift_marks(index, false);
        let formulas = self.view.workbook_mut().delete_sheet(index);
        self.view.sheet_removed(index);
        self.mark_formulas(&formulas);
        self.sheet_step(SheetChange::Deleted(index), before, formulas, marks, origin);
        let count = self.view.workbook().sheets.len();
        self.show_sheet(index.min(count - 1));
        Ok(())
    }

    fn sheet_step(
        &mut self,
        change: SheetChange,
        before: Vec<aster_sheets::Sheet>,
        formulas: Vec<(usize, CellRef)>,
        marks: Marks,
        origin: Option<usize>,
    ) {
        self.formulas_changed = true;
        self.refusal = None;
        let after = self.view.workbook().sheets.clone();
        self.history.push(Step {
            styles_before: self.view.workbook().styles.len(),
            sheets: Some(Box::new(SheetStep {
                change,
                before,
                after,
                formulas,
                marks,
                origin,
                file: self.file,
            })),
            ..Step::default()
        });
        self.reload_calc();
    }

    fn undo_sheets(&mut self, step: &SheetStep) {
        self.view.workbook_mut().sheets = step.before.clone();
        match step.change {
            SheetChange::Added(index) => {
                self.view.sheet_removed(index);
                self.take_marks(index);
                self.origins.remove(index);
                self.shift_marks(index, false);
                self.show_sheet(index.saturating_sub(1));
            }
            SheetChange::Renamed(index) => {
                self.mark_formulas(&step.formulas);
                self.show_sheet(index);
            }
            SheetChange::Deleted(index) => {
                // The formulas that lost the sheet, where they are again.
                let formulas: Vec<(usize, CellRef)> = step
                    .formulas
                    .iter()
                    .map(|&(s, at)| (if s >= index { s + 1 } else { s }, at))
                    .collect();
                self.view.sheet_inserted(index);
                self.shift_marks(index, true);
                // The file still has it, unless it was saved without it:
                // then it is written out whole as a new sheet.
                let origin = step.origin.filter(|_| step.file == self.file);
                self.origins.insert(index, origin);
                if origin.is_some() {
                    self.put_marks(index, &step.marks);
                }
                self.mark_formulas(&formulas);
                self.show_sheet(index);
            }
        }
        self.formulas_changed = true;
        self.reload_calc();
    }

    fn redo_sheets(&mut self, step: &SheetStep) {
        self.view.workbook_mut().sheets = step.after.clone();
        match step.change {
            SheetChange::Added(index) => {
                self.view.sheet_inserted(index);
                self.origins.insert(index, None);
                self.shift_marks(index, true);
                self.show_sheet(index);
            }
            SheetChange::Renamed(index) => {
                self.mark_formulas(&step.formulas);
                self.show_sheet(index);
            }
            SheetChange::Deleted(index) => {
                self.view.sheet_removed(index);
                self.take_marks(index);
                self.origins.remove(index);
                self.shift_marks(index, false);
                self.mark_formulas(&step.formulas);
                let count = self.view.workbook().sheets.len();
                self.show_sheet(index.min(count - 1));
            }
        }
        self.formulas_changed = true;
        self.reload_calc();
    }

    fn show_sheet(&mut self, index: usize) {
        if !self.view.set_sheet(index) {
            // The same number, another sheet: its first cell.
            self.view.select(CellRef::new(0, 0));
        }
    }

    fn mark_formulas(&mut self, cells: &[(usize, CellRef)]) {
        self.dirty.extend(cells.iter().copied());
        self.edited.extend(cells.iter().copied());
    }

    /// Sheets from `index` on move one up (a sheet came in there) or down
    /// (the sheet there went; take its marks first).
    fn shift_marks(&mut self, index: usize, inserted: bool) {
        let shift = |s: usize| -> usize {
            if s < index {
                s
            } else if inserted {
                s + 1
            } else {
                s - 1
            }
        };
        let cells = |set: &mut BTreeSet<(usize, CellRef)>| {
            *set = std::mem::take(set)
                .into_iter()
                .map(|(s, at)| (shift(s), at))
                .collect();
        };
        cells(&mut self.dirty);
        cells(&mut self.edited);
        let lines = |set: &mut BTreeSet<(usize, u32)>| {
            *set = std::mem::take(set)
                .into_iter()
                .map(|(s, i)| (shift(s), i))
                .collect();
        };
        lines(&mut self.columns_changed);
        lines(&mut self.rows_changed);
        self.merges_changed = std::mem::take(&mut self.merges_changed)
            .into_iter()
            .map(shift)
            .collect();
    }

    /// Take away what saving had to write on sheet `index`.
    fn take_marks(&mut self, index: usize) -> Marks {
        let mut marks = Marks::default();
        let cells = |set: &mut BTreeSet<(usize, CellRef)>| -> Vec<CellRef> {
            let taken: Vec<CellRef> = set
                .iter()
                .filter(|(s, _)| *s == index)
                .map(|&(_, at)| at)
                .collect();
            set.retain(|(s, _)| *s != index);
            taken
        };
        marks.dirty = cells(&mut self.dirty);
        marks.edited = cells(&mut self.edited);
        let lines = |set: &mut BTreeSet<(usize, u32)>| -> Vec<u32> {
            let taken: Vec<u32> = set
                .iter()
                .filter(|(s, _)| *s == index)
                .map(|&(_, i)| i)
                .collect();
            set.retain(|(s, _)| *s != index);
            taken
        };
        marks.columns = lines(&mut self.columns_changed);
        marks.rows = lines(&mut self.rows_changed);
        marks.merges = self.merges_changed.remove(&index);
        marks
    }

    fn put_marks(&mut self, index: usize, marks: &Marks) {
        self.dirty.extend(marks.dirty.iter().map(|&at| (index, at)));
        self.edited
            .extend(marks.edited.iter().map(|&at| (index, at)));
        self.columns_changed
            .extend(marks.columns.iter().map(|&i| (index, i)));
        self.rows_changed
            .extend(marks.rows.iter().map(|&i| (index, i)));
        if marks.merges {
            self.merges_changed.insert(index);
        }
    }

    /// The calculation thread starts again from the workbook as it is.
    fn reload_calc(&mut self) {
        if let Some(worker) = &mut self.worker {
            worker.reload(self.view.workbook().clone());
            self.calc = Calc::Running;
        }
    }

    fn set_size(&mut self, sheet: usize, resize: Resize, size: f64) {
        if let Some(target) = self.view.workbook_mut().sheets.get_mut(sheet) {
            match resize {
                Resize::Column(col) => {
                    target.columns.set(col, col + 1, size);
                    self.columns_changed.insert((sheet, col));
                }
                Resize::Row(row) => {
                    target.row_sizes.set(row, row + 1, size);
                    self.rows_changed.insert((sheet, row));
                }
            }
        }
    }

    fn size_of(&self, resize: Resize) -> f64 {
        self.view.sheet().map_or(0.0, |sheet| match resize {
            Resize::Column(col) => sheet.columns.size(col),
            Resize::Row(row) => sheet.row_sizes.size(row),
        })
    }

    /// Start dragging a column's or row's far edge, if one is under the
    /// pointer (physical pixels). Returns whether a drag started.
    pub fn begin_resize(&mut self, x: f64, y: f64) -> bool {
        let Some(resize) = self.view.resize_hit(x, y) else {
            return false;
        };
        self.view.cancel_edit();
        let before = self.size_of(resize);
        let grab = before - self.view.resized_size(resize, x, y);
        self.resizing = Some((resize, before, grab));
        true
    }

    pub fn is_resizing(&self) -> bool {
        self.resizing.is_some()
    }

    /// The pointer moved during a drag: the size follows it.
    pub fn drag_resize(&mut self, x: f64, y: f64) -> bool {
        let Some((resize, _, grab)) = self.resizing else {
            return false;
        };
        let size = (self.view.resized_size(resize, x, y) + grab).max(MIN_SIZE);
        if size == self.size_of(resize) {
            return false;
        }
        let sheet = self.view.sheet_index();
        self.set_size(sheet, resize, size);
        true
    }

    /// The drag ended: one undo step, if the size changed.
    pub fn end_resize(&mut self) -> bool {
        let Some((resize, before, _)) = self.resizing.take() else {
            return false;
        };
        let after = self.size_of(resize);
        if after == before {
            return false;
        }
        let sheet = self.view.sheet_index();
        let styles_before = self.view.workbook().styles.len();
        self.history.push(Step {
            changes: Vec::new(),
            merges: Vec::new(),
            sizes: vec![(sheet, resize, before, after)],
            styles_before,
            added: Vec::new(),
            lines: None,
            sheets: None,
        });
        true
    }

    fn set_merges(&mut self, sheet: usize, merges: Vec<CellRange>) {
        if let Some(target) = self.view.workbook_mut().sheets.get_mut(sheet) {
            target.merges = merges;
            target.refresh();
        }
        self.merges_changed.insert(sheet);
    }

    /// Apply a formatting change to the selection. One undo step.
    pub fn format(&mut self, edit: &StyleEdit) -> Vec<Change> {
        let sheet = self.view.sheet_index();
        let range = self.view.selection();
        let styles_before = self.view.workbook().styles.len();
        let cells = self.view.workbook_mut().restyle(sheet, range, edit);
        let changes = self.view.workbook_mut().set_cells(sheet, cells);
        self.record(changes, styles_before)
    }

    /// The style of the active cell.
    pub fn active_style(&self) -> aster_sheets::CellStyle {
        let workbook = self.view.workbook();
        self.view
            .sheet()
            .map(|sheet| workbook.style(sheet.style_at(self.view.selected())))
            .unwrap_or_default()
    }

    /// What a formatting command does here: turn bold on or off as the
    /// active cell has it, step the size, and so on.
    pub fn edit_for(&self, command: Command, language: &str) -> Option<StyleEdit> {
        let style = self.active_style();
        let font = &style.font;
        let toggle_align =
            |align: HAlign| StyleEdit::Align((style.align != Some(align)).then_some(align));
        Some(match command {
            Command::Bold => StyleEdit::Bold(!font.bold),
            Command::Italic => StyleEdit::Italic(!font.italic),
            Command::Underline => StyleEdit::Underline(!font.underline),
            Command::Strikethrough => StyleEdit::Strike(!font.strike),
            Command::FontSizeUp => StyleEdit::Size(font_size_step(font.size, true)),
            Command::FontSizeDown => StyleEdit::Size(font_size_step(font.size, false)),
            Command::FontSize => StyleEdit::Size(11.0),
            Command::AlignLeft => toggle_align(HAlign::Left),
            Command::AlignCenter => toggle_align(HAlign::Center),
            Command::AlignRight => toggle_align(HAlign::Right),
            Command::WrapText => StyleEdit::Wrap(!style.wrap),
            Command::FillColor => StyleEdit::Fill(match style.fill {
                Some(_) => None,
                None => Some(aster_sheets::restyle::YELLOW),
            }),
            Command::Borders => StyleEdit::Borders(style.borders.is_empty()),
            Command::Currency => number_format(2, language),
            Command::Percent => StyleEdit::Format(9, Arc::new(aster_sheets::Format::builtin(9))),
            _ => return None,
        })
    }

    /// Merge the selected cells and centre what the first holds, as Excel's
    /// "merge and centre" does; on a merged area, split it again. The
    /// other cells' contents go. One undo step.
    pub fn merge(&mut self) -> Vec<Change> {
        let sheet = self.view.sheet_index();
        let range = self.view.selection();
        let Some(current) = self.view.sheet() else {
            return Vec::new();
        };
        let before = current.merges.clone();
        let styles_before = self.view.workbook().styles.len();
        let mut changes = Vec::new();
        let after: Vec<CellRange> = if before.contains(&range) {
            before.iter().filter(|m| **m != range).copied().collect()
        } else {
            if range.first == range.last || aster_sheets::restyle::is_whole(&range) {
                return Vec::new();
            }
            let emptied: Vec<(CellRef, Option<aster_sheets::Cell>)> = current
                .cells_in(range)
                .filter(|(at, cell)| *at != range.first && !cell.is_blank())
                .map(|(at, cell)| {
                    let kept = (cell.style != 0).then(|| aster_sheets::Cell {
                        style: cell.style,
                        ..aster_sheets::Cell::new(at.col, aster_sheets::Value::Empty)
                    });
                    (at, kept)
                })
                .collect();
            changes.extend(self.view.workbook_mut().set_cells(sheet, emptied));
            // Centre the first cell, which shows for the whole area.
            let centred = self.view.workbook_mut().restyle(
                sheet,
                CellRange::single(range.first),
                &StyleEdit::Align(Some(HAlign::Center)),
            );
            changes.extend(self.view.workbook_mut().set_cells(sheet, centred));
            let mut list: Vec<CellRange> = before
                .iter()
                .filter(|m| !m.intersects(&range))
                .copied()
                .collect();
            list.push(range);
            list
        };
        self.set_merges(sheet, after.clone());
        self.note(&changes);
        self.history.push(Step {
            changes: changes.clone(),
            merges: vec![(sheet, before, after)],
            sizes: Vec::new(),
            styles_before,
            added: Vec::new(),
            lines: None,
            sheets: None,
        });
        self.view.select(range.first);
        self.view.extend_to(range.last);
        changes
    }

    /// Sum the numbers right above the active cell (or, if none, to its
    /// left) into it, as Excel's AutoSum does. With no numbers next to it,
    /// start typing `=SUM(` there.
    pub fn auto_sum(&mut self, language: &str) -> Vec<Change> {
        let at = self.view.selected();
        let Some(sheet) = self.view.sheet() else {
            return Vec::new();
        };
        let is_number = |cell: CellRef| {
            sheet
                .cell(cell)
                .is_some_and(|c| matches!(c.value, aster_sheets::Value::Number(_)))
        };
        let run = |step: &dyn Fn(CellRef) -> Option<CellRef>| {
            let mut first = None;
            let mut cell = step(at);
            while let Some(here) = cell.filter(|c| is_number(*c)) {
                first = Some(here);
                cell = step(here);
            }
            first
        };
        let up = |c: CellRef| c.row.checked_sub(1).map(|r| CellRef::new(r, c.col));
        let left = |c: CellRef| c.col.checked_sub(1).map(|col| CellRef::new(c.row, col));
        let range = run(&up)
            .map(|first| CellRange::new(first, up(at).unwrap_or(first)))
            .or_else(|| run(&left).map(|first| CellRange::new(first, left(at).unwrap_or(first))));
        match range {
            Some(range) => {
                self.view
                    .begin_edit(EditMode::Enter, Some(format!("=SUM({range})")));
                self.commit(language)
            }
            None => {
                self.view
                    .begin_edit(EditMode::Enter, Some("=SUM(".to_owned()));
                Vec::new()
            }
        }
    }

    /// How the active cell looks, for the toolbar.
    pub fn cell_look(&self) -> CellLook {
        let style = self.active_style();
        let code = style.format.code();
        let format = if code == "General" || code.is_empty() {
            Some(0)
        } else if code.contains(['$', '¥', '￥', '€', '£', '₩']) {
            Some(2)
        } else if code.contains('%') {
            Some(3)
        } else if style.format.is_date() && style.format.date_order().is_some() {
            Some(4)
        } else if style.format.is_date() {
            Some(5)
        } else if code == "@" {
            Some(6)
        } else if code.contains('0') {
            Some(1)
        } else {
            None
        };
        CellLook {
            bold: style.font.bold,
            italic: style.font.italic,
            underline: style.font.underline,
            strike: style.font.strike,
            align: match style.align {
                None => 0,
                Some(HAlign::Left) => 1,
                Some(HAlign::Center) => 2,
                Some(HAlign::Right) => 3,
            },
            wrap: style.wrap,
            merged: self
                .view
                .sheet()
                .is_some_and(|sheet| sheet.merge_at(self.view.selected()).is_some()),
            filled: style.fill.is_some(),
            bordered: !style.borders.is_empty(),
            size: style.font.size,
            format,
        }
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

/// The number format at `index` of the toolbar's list (General, Number,
/// Currency, Percent, Date, Time, Text). The currency is the one of the
/// interface language.
pub fn number_format(index: usize, language: &str) -> StyleEdit {
    use aster_sheets::Format;
    let builtin = |id: u32| StyleEdit::Format(id, Arc::new(Format::builtin(id)));
    match index {
        1 => builtin(4),
        2 => {
            let code = match language {
                "zh-Hans" | "zh-Hant" => "\"¥\"#,##0.00",
                "ja" => "\"¥\"#,##0",
                "ko" => "\"₩\"#,##0",
                "fr" | "de" | "es" => "#,##0.00\" €\"",
                _ => "\"$\"#,##0.00",
            };
            StyleEdit::Format(
                aster_sheets::style::UNNUMBERED_FORMAT,
                Arc::new(Format::parse(code)),
            )
        }
        3 => builtin(10),
        4 => builtin(14),
        5 => builtin(21),
        6 => builtin(49),
        _ => builtin(0),
    }
}

/// A new workbook with one sheet, its default font the one Excel uses for
/// the interface language (it sets the column width unit).
/// The name new sheets are numbered after, as Excel names them in each
/// language.
pub fn sheet_base(language: &str) -> &'static str {
    match language {
        "zh-Hant" => "工作表",
        "fr" => "Feuil",
        "de" => "Tabelle",
        "es" => "Hoja",
        _ => "Sheet",
    }
}

pub fn blank_workbook(language: &str) -> Vec<u8> {
    let font = match language {
        "zh-Hans" => "等线",
        "zh-Hant" => "新細明體",
        "ja" => "游ゴシック",
        "ko" => "맑은 고딕",
        _ => "Calibri",
    };
    aster_ooxml::xlsx_save::blank(&format!("{}1", sheet_base(language)), font)
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
        // Rows inserted: the engine starts again from the moved cells, and
        // an edit after that reaches the formula where it is now.
        tab.view.select(at("A1"));
        tab.change_lines(false, true).unwrap();
        while tab.calc == Calc::Running {
            wait(&mut tab);
        }
        enter(&mut tab, "A2", "7");
        while tab.calc == Calc::Running {
            wait(&mut tab);
        }
        assert_eq!(value(&tab, "B2"), Some(aster_sheets::Value::Number(27.0)));
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
        let edits = edits(&tab.history.undo[1].changes);
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
    fn sheets_are_added_renamed_and_deleted_with_undo_and_save() {
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("工作表.xlsx");
        std::fs::write(&file, blank_workbook("fr")).unwrap();
        let Ok(Loaded::Workbook(workbook, source)) = load(&file) else {
            panic!("a blank workbook loads");
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        let mut tab = SheetTab::new(workbook, source, Some(file.clone()), &tokens);
        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            tab.commit("fr");
        };
        let names = |tab: &SheetTab| -> Vec<String> {
            tab.view
                .workbook()
                .sheets
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        let save = |tab: &mut SheetTab| {
            let job = tab.begin_save().unwrap();
            let written = job.write(&file).unwrap();
            tab.finish_save(&job, &file, Some(written));
        };
        enter(&mut tab, "A1", "3");
        // A new sheet, shown, with a formula on the first.
        assert_eq!(tab.add_sheet(sheet_base("fr")), Some(1));
        assert_eq!(names(&tab), ["Feuil1", "Feuil2"]);
        assert_eq!(tab.view.sheet_index(), 1);
        enter(&mut tab, "B2", "=Feuil1!A1*2");
        assert_eq!(
            tab.rename_sheet(0, "Feuil2"),
            Err(aster_sheets::NameProblem::Taken)
        );
        tab.rename_sheet(0, "Données 2026").unwrap();
        let formula = |tab: &SheetTab, sheet: usize, cell: &str| {
            tab.view.workbook().sheets[sheet]
                .cell(at(cell))
                .and_then(|c| c.formula.clone())
        };
        assert_eq!(
            formula(&tab, 1, "B2").as_deref(),
            Some("'Données 2026'!A1*2")
        );
        tab.undo();
        assert_eq!(names(&tab), ["Feuil1", "Feuil2"]);
        assert_eq!(formula(&tab, 1, "B2").as_deref(), Some("Feuil1!A1*2"));
        tab.redo();
        save(&mut tab);
        assert!(!tab.is_modified());
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        assert_eq!(
            again
                .sheets
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Données 2026", "Feuil2"]
        );
        let cell = again.sheets[1].cell(at("B2")).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("'Données 2026'!A1*2"));

        // Deleting the first: the formula loses it; the last one stays.
        tab.delete_sheet(0).unwrap();
        assert_eq!(names(&tab), ["Feuil2"]);
        assert_eq!(formula(&tab, 0, "B2").as_deref(), Some("#REF!*2"));
        assert_eq!(tab.delete_sheet(0), Err(Refusal::LastSheet));
        save(&mut tab);
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        assert_eq!(again.sheets.len(), 1);
        // Undone after the save, the sheet comes back whole, as a new one.
        tab.undo();
        assert_eq!(names(&tab), ["Données 2026", "Feuil2"]);
        assert_eq!(tab.origins, [None, Some(0)]);
        save(&mut tab);
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        assert_eq!(
            again
                .sheets
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Données 2026", "Feuil2"]
        );
        assert_eq!(
            again.sheets[0].cell(at("A1")).map(|c| c.value.clone()),
            Some(aster_sheets::Value::Number(3.0))
        );
        assert_eq!(
            again.sheets[1]
                .cell(at("B2"))
                .and_then(|c| c.formula.clone())
                .as_deref(),
            Some("'Données 2026'!A1*2")
        );
    }

    #[test]
    fn undone_deletes_leave_the_file_alone_and_inserts_take_formats() {
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("格式.xlsx");
        std::fs::write(&file, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let Ok(Loaded::Workbook(workbook, source)) = load(&file) else {
            panic!("a blank workbook loads");
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        let mut tab = SheetTab::new(workbook, source, Some(file.clone()), &tokens);
        for (cell, text) in [("A1", "1"), ("A2", "2"), ("B2", "x"), ("A3", "3")] {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            tab.commit("en");
        }
        tab.view.select(at("A2"));
        tab.view.extend_to(at("B2"));
        tab.format(&StyleEdit::Bold(true));
        tab.view.workbook_mut().sheets[0].row_sizes.set(1, 2, 40.0);
        tab.rows_changed.insert((0, 1));
        let save = |tab: &mut SheetTab| {
            let job = tab.begin_save().unwrap();
            let written = job.write(&file).unwrap();
            tab.finish_save(&job, &file, Some(written));
        };
        let sheet_xml = || {
            let bytes = std::fs::read(&file).unwrap();
            let mut package = aster_ooxml::Package::open(bytes).unwrap();
            package.read("xl/worksheets/sheet1.xml").unwrap()
        };
        save(&mut tab);
        let saved = sheet_xml();

        // Row 2 deleted and brought back: nothing is left to move.
        tab.view.select(at("A2"));
        tab.change_lines(false, false).unwrap();
        tab.undo();
        assert!(tab.structure.is_empty());
        save(&mut tab);
        assert_eq!(sheet_xml(), saved);

        // A row inserted below the bold one is bold and as tall.
        tab.view.select(at("A3"));
        tab.change_lines(false, true).unwrap();
        let bold = |tab: &SheetTab, cell: &str| {
            let sheet = tab.view.sheet().unwrap();
            let style = sheet.cell(at(cell)).map_or(0, |c| c.style);
            tab.view.workbook().style(style).font.bold
        };
        assert!(bold(&tab, "A3") && bold(&tab, "B3"));
        save(&mut tab);
        // Empty cells with a format are written; reading drops those that
        // draw nothing, so look at the file.
        let xml = String::from_utf8(sheet_xml()).unwrap();
        assert!(
            xml.contains(r#"<row r="3" ht="30" customHeight="1"><c r="A3" s="#),
            "{xml}"
        );
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        let sheet = &again.sheets[0];
        assert_eq!(sheet.row_sizes.size(2), 40.0);
        assert_eq!(
            sheet.cell(at("A4")).map(|c| c.value.clone()),
            Some(aster_sheets::Value::Number(3.0))
        );
    }

    #[test]
    fn rows_and_columns_go_in_and_out_with_undo_and_save() {
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("行列.xlsx");
        std::fs::write(&file, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let Ok(Loaded::Workbook(workbook, source)) = load(&file) else {
            panic!("a blank workbook loads");
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        let mut tab = SheetTab::new(workbook, source, Some(file.clone()), &tokens);
        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            tab.commit("en");
        };
        enter(&mut tab, "A1", "1");
        enter(&mut tab, "A2", "2");
        enter(&mut tab, "A3", "=SUM(A1:A2)");
        tab.view.workbook_mut().sheets[0].row_sizes.set(1, 2, 40.0);
        tab.rows_changed.insert((0, 1));
        let formula = |tab: &SheetTab, cell: &str| {
            tab.view
                .sheet()
                .unwrap()
                .cell(at(cell))
                .and_then(|c| c.formula.clone())
        };

        // Two rows above row 2: the sum follows its cells.
        tab.view.select(at("B2"));
        tab.view.extend_to(at("C3"));
        tab.change_lines(false, true).unwrap();
        assert_eq!(formula(&tab, "A5").as_deref(), Some("SUM(A1:A4)"));
        assert_eq!(value(&tab, "A4"), Some(aster_sheets::Value::Number(2.0)));
        assert!(tab.dirty.contains(&(0, at("A4"))) && !tab.dirty.contains(&(0, at("A2"))));
        assert!(tab.rows_changed.contains(&(0, 3)));
        assert_eq!(tab.view.sheet().unwrap().row_sizes.size(3), 40.0);

        // Undo puts it back; redo does it again.
        tab.undo();
        assert_eq!(formula(&tab, "A3").as_deref(), Some("SUM(A1:A2)"));
        assert!(tab.view.sheet().unwrap().cell(at("A5")).is_none());
        tab.redo();
        assert_eq!(formula(&tab, "A5").as_deref(), Some("SUM(A1:A4)"));

        // Then the first column goes: the sum's column with it.
        tab.view.select(at("A1"));
        tab.change_lines(true, false).unwrap();
        assert!(tab.view.sheet().unwrap().cell(at("A5")).is_none());
        tab.undo();
        assert_eq!(formula(&tab, "A5").as_deref(), Some("SUM(A1:A4)"));
        // Each undo took its move out again: only the redone insert is
        // left to replay on the file.
        assert_eq!(tab.structure.len(), 1);

        let job = tab.begin_save().unwrap();
        let written = job.write(&file).unwrap();
        tab.finish_save(&job, &file, Some(written));
        assert!(tab.structure.is_empty() && !tab.is_modified());
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        let cell = |c: &str| again.sheets[0].cell(at(c)).cloned();
        assert_eq!(
            cell("A5").and_then(|c| c.formula).as_deref(),
            Some("SUM(A1:A4)")
        );
        assert_eq!(
            cell("A4").map(|c| c.value),
            Some(aster_sheets::Value::Number(2.0))
        );
        assert!(cell("A2").is_none());
        assert_eq!(again.sheets[0].row_sizes.size(3), 40.0);

        // A sheet holding what would not follow says so and stays as it is.
        tab.view.workbook_mut().sheets[0].structure_blocker = Some("table".into());
        assert_eq!(
            tab.change_lines(false, false),
            Err(Refusal::Holds("table".into()))
        );
        assert_eq!(formula(&tab, "A5").as_deref(), Some("SUM(A1:A4)"));
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

    #[test]
    fn undo_takes_away_the_formats_an_entry_added() {
        use aster_surface::cell_editor::EditMode;

        let enter = |tab: &mut SheetTab, cell: &str, text: &str| {
            tab.view.select(at(cell));
            tab.view.begin_edit(EditMode::Enter, Some(text.into()));
            tab.commit("en")
        };
        let styles = |tab: &SheetTab| tab.view.workbook().styles.len();
        let mut tab = tab();
        assert_eq!(styles(&tab), 2);
        enter(&mut tab, "B1", "50%");
        assert_eq!(styles(&tab), 3);
        // The second percentage uses the same new format.
        enter(&mut tab, "B2", "60%");
        assert_eq!(styles(&tab), 3);
        tab.undo();
        assert_eq!(styles(&tab), 3, "B1 still uses it");
        tab.undo();
        assert_eq!(styles(&tab), 2);
        assert_eq!(value(&tab, "B1"), None);
        // Redo brings the format back where B1 refers to it.
        tab.redo();
        assert_eq!(styles(&tab), 3);
        let b1 = tab.view.sheet().unwrap().cell(at("B1")).unwrap().clone();
        assert_eq!(tab.view.workbook().display(&b1).unwrap().text, "50%");
        tab.redo();
        assert_eq!(styles(&tab), 3);
        // A new step after undoing: the format is made again, at the same
        // place.
        tab.undo();
        tab.undo();
        enter(&mut tab, "C1", "$7");
        assert_eq!(styles(&tab), 3);
        assert!(!tab.history.can_redo());
        tab.undo();
        assert_eq!(styles(&tab), 2);
    }

    #[test]
    fn formats_already_saved_stay_on_undo() {
        use aster_surface::cell_editor::EditMode;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("formats.xlsx");
        std::fs::write(&file, aster_ooxml::xlsx_save::blank("Sheet1", "Calibri")).unwrap();
        let Ok(Loaded::Workbook(workbook, source)) = load(&file) else {
            panic!("a blank workbook loads");
        };
        let tokens = theme::Tokens::of(theme::ThemeMode::Light);
        let mut tab = SheetTab::new(workbook, source, Some(file.clone()), &tokens);
        let before = tab.view.workbook().styles.len();
        tab.view.select(at("A1"));
        tab.view.begin_edit(EditMode::Enter, Some("¥1,200".into()));
        tab.commit("ja");
        assert_eq!(tab.view.workbook().styles.len(), before + 1);
        let job = tab.begin_save().unwrap();
        let written = job.write(&file).unwrap();
        tab.finish_save(&job, &file, Some(written));
        // The file has the format now: undoing the entry keeps it, so the
        // next new format does not take its place.
        tab.undo();
        assert_eq!(tab.view.workbook().styles.len(), before + 1);
        tab.view.begin_edit(EditMode::Enter, Some("$2.50".into()));
        tab.commit("en");
        assert_eq!(tab.view.workbook().styles.len(), before + 2);
        let job = tab.begin_save().unwrap();
        let written = job.write(&file).unwrap();
        tab.finish_save(&job, &file, Some(written));
        let Ok(Loaded::Workbook(again, _)) = load(&file) else {
            panic!("the saved file loads");
        };
        let a1 = again.sheets[0].cell(at("A1")).unwrap();
        assert_eq!(again.display(a1).unwrap().text, "$2.50");
        assert_eq!(again.styles.len(), before + 2);
    }

    #[test]
    fn toolbar_formatting_merging_and_autosum() {
        let mut tab = tab();
        tab.view.select(at("A1"));
        tab.view.extend_to(at("B2"));
        // Bold on: the active cell is not bold, so the command turns it on.
        let bold = tab.edit_for(Command::Bold, "en").unwrap();
        assert_eq!(bold, StyleEdit::Bold(true));
        assert_eq!(tab.format(&bold).len(), 4);
        assert!(tab.cell_look().bold);
        // Again: off.
        let again = tab.edit_for(Command::Bold, "en").unwrap();
        assert_eq!(again, StyleEdit::Bold(false));
        tab.format(&tab.edit_for(Command::FontSizeUp, "en").unwrap());
        assert_eq!(tab.cell_look().size, 12.0);
        tab.format(&tab.edit_for(Command::AlignCenter, "en").unwrap());
        assert_eq!(tab.cell_look().align, 2);
        // Centred again: back to by value.
        tab.format(&tab.edit_for(Command::AlignCenter, "en").unwrap());
        assert_eq!(tab.cell_look().align, 0);
        tab.format(&tab.edit_for(Command::FillColor, "en").unwrap());
        assert!(tab.cell_look().filled);
        tab.format(&number_format(3, "en"));
        assert_eq!(tab.cell_look().format, Some(3));
        assert_eq!(value(&tab, "A1"), Some(aster_sheets::Value::Number(1.0)));
        let a1 = tab.view.sheet().unwrap().cell(at("A1")).unwrap().clone();
        assert_eq!(tab.view.workbook().display(&a1).unwrap().text, "100.00%");
        tab.format(&number_format(2, "ja"));
        assert_eq!(tab.cell_look().format, Some(2));

        // Merge and centre: B2's... A2's value goes, A1 is centred.
        tab.view.select(at("A1"));
        tab.view.extend_to(at("A2"));
        let changes = tab.merge();
        assert!(!changes.is_empty());
        assert_eq!(tab.view.sheet().unwrap().merges.len(), 1);
        assert!(tab.cell_look().merged);
        assert_eq!(tab.cell_look().align, 2);
        assert_eq!(value(&tab, "A2"), Some(aster_sheets::Value::Empty));
        assert!(tab.merges_changed.contains(&0));
        // On the merged area: split again.
        tab.merge();
        assert!(tab.view.sheet().unwrap().merges.is_empty());
        // Undo the split and the merge.
        tab.undo();
        assert_eq!(tab.view.sheet().unwrap().merges.len(), 1);
        tab.undo();
        assert!(tab.view.sheet().unwrap().merges.is_empty());
        assert_eq!(value(&tab, "A2"), Some(aster_sheets::Value::Number(2.0)));

        // AutoSum under the numbers in column A.
        tab.view.select(at("A3"));
        tab.auto_sum("en");
        let sum = tab
            .view
            .sheet()
            .unwrap()
            .cell(at("A3"))
            .unwrap()
            .formula
            .clone();
        assert_eq!(sum.as_deref(), Some("SUM(A1:A2)"));
        // With nothing to sum: an entry is started.
        tab.view.select(at("F9"));
        assert!(tab.auto_sum("en").is_empty());
        assert_eq!(tab.view.editor().unwrap().text(), "=SUM(");
    }

    #[test]
    fn dragging_an_edge_resizes_one_step_at_a_time() {
        let mut tab = tab();
        tab.view.resize(800, 600, 1.0);
        let width = |tab: &SheetTab| tab.view.sheet().unwrap().columns.size(0);
        let height = |tab: &SheetTab| tab.view.sheet().unwrap().row_sizes.size(1);
        let (w0, h0) = (width(&tab), height(&tab));
        // Find the right edge of column A on the column letters.
        let edge = (0..800)
            .map(f64::from)
            .find(|x| tab.view.resize_hit(*x, 10.0) == Some(Resize::Column(0)))
            .unwrap();
        assert!(
            tab.view.resize_hit(edge, 300.0).is_none(),
            "not over the cells"
        );
        assert!(tab.begin_resize(edge, 10.0));
        assert!(tab.drag_resize(edge + 36.0, 10.0));
        assert!(tab.drag_resize(edge + 40.0, 10.0));
        assert!(tab.end_resize());
        assert_eq!(width(&tab), (w0 + 40.0).round());
        assert!(tab.is_modified());
        assert!(tab.columns_changed.contains(&(0, 0)));
        // Dragging far to the left stops at a few pixels.
        assert!(tab.begin_resize(edge + 40.0, 10.0));
        tab.drag_resize(0.0, 10.0);
        tab.end_resize();
        assert_eq!(width(&tab), 4.0);
        tab.undo();
        assert_eq!(width(&tab), (w0 + 40.0).round());
        tab.undo();
        assert_eq!(width(&tab), w0);
        assert!(!tab.is_modified());
        tab.redo();
        assert_eq!(width(&tab), (w0 + 40.0).round());
        // A row: the bottom edge of row 2 on the row numbers.
        let edge = (0..600)
            .map(f64::from)
            .find(|y| tab.view.resize_hit(5.0, *y) == Some(Resize::Row(1)))
            .unwrap();
        assert!(tab.begin_resize(5.0, edge));
        tab.drag_resize(5.0, edge + 10.0);
        assert!(tab.end_resize());
        assert_eq!(height(&tab), h0 + 10.0);
        assert!(tab.rows_changed.contains(&(0, 1)));
        // A press that does not move changes nothing.
        assert!(tab.begin_resize(5.0, edge + 10.0));
        assert!(!tab.end_resize());
    }
}
