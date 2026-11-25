//! The child process that opens a PDF and renders its pages, and what goes
//! over the pipes between it and the [`crate::Worker`] that started it.
//!
//! The child is the application's own executable started with a sub-command
//! (`--pdf-worker <memory budget> <file>`). Its standard input brings the
//! password and the pages wanted; its standard output carries the answers.
//! Nothing else is written to standard output.
//!
//! Memory: the executable installs [`Budgeted`] as its global allocator. In
//! the application it only passes allocations on; in the child it also
//! counts them, and the moment they pass the budget the process exits with
//! [`MEMORY_EXIT`]. No allocation fails and nothing unwinds: the process is
//! simply gone, and its parent says why.
//!
//! # Protocol
//!
//! Integers are little-endian; every count and length is checked against
//! the [`Limits`] before anything is allocated for it.
//!
//! Requests (parent to child), after the start frame `K`:
//!
//! - `W count:u32 (page:u32 scale:f32)*`: pages to render, newest wins.
//! - `T count:u32 (page:u32)*`: pages whose text is wanted (at most 4096),
//!   newest wins.
//! - `L`: the outline, once.
//! - `Q search:u64 match_case:u8 start_page:u32 found:u32 len:u32 query`:
//!   search from `start_page` (a restart continues where the last child
//!   left off, with `found` matches already reported). Replaces the
//!   running search.
//! - `C`: stop the running search.
//!
//! Answers (child to parent):
//!
//! - `O`, `F`, `S`, `P`, `X`: open, failed, started and rendered pages.
//! - `w kind:u8 search:u64 page:u32`: a step begins (for the time limit):
//!   kind 0 the text of `page`, 1 the outline, 2 page `page` of search
//!   `search`. Every step ends with exactly one of the frames below.
//! - `T page:u32 bytes:u32 chars:u32 text (flag:u8 [l t r b:f32])*chars`:
//!   the text of a page; one box entry per char.
//! - `L count:u32 (depth:u16 flags:u8 len:u32 title [page:u32] [top:f32])*`:
//!   the outline; flags bit 0 open, bit 1 has a target page, bit 2 has a
//!   height (only with bit 1).
//! - `H search:u64 page:u32 count:u32 (start:u32 len:u32 boxes:u32
//!   (l t r b:f32)*)*`: matches of one page (at least one).
//! - `N search:u64 page:u32`: a search step that found nothing.
//! - `D search:u64 total:u32 truncated:u8`: the search is over.
//!
//! The child never answers one request twice or out of order; the parent
//! kills a child that does (see `worker::Ledger`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use hayro::RenderCache;

use crate::{
    Document, Exceeded, FindHit, Info, Limits, OpenError, OutlineItem, PageSize, PageText,
    STACK_BYTES, Target, TextBox,
};

/// The sub-command that makes an executable a PDF child process.
pub const SUBCOMMAND: &str = "--pdf-worker";
/// Exit status of a child that passed its memory budget.
pub const MEMORY_EXIT: i32 = 77;
/// Exit status of a child whose parser panicked: its state may be poisoned,
/// so it ends and is started again.
pub const PANIC_EXIT: i32 = 70;

static BUDGET: AtomicUsize = AtomicUsize::new(0);
static USED: AtomicIsize = AtomicIsize::new(0);

/// The global allocator of executables that can be PDF child processes.
pub struct Budgeted;

impl Budgeted {
    #[inline]
    fn take(size: usize) {
        let budget = BUDGET.load(Ordering::Relaxed);
        if budget == 0 {
            return;
        }
        let used = USED.fetch_add(size as isize, Ordering::Relaxed) + size as isize;
        if used > budget as isize {
            over_budget();
        }
    }

    #[inline]
    fn give(size: usize) {
        if BUDGET.load(Ordering::Relaxed) != 0 {
            USED.fetch_sub(size as isize, Ordering::Relaxed);
        }
    }
}

#[cold]
fn over_budget() -> ! {
    // Nothing that allocates, and no unwinding out of an allocator.
    // SAFETY: `_exit` ends the process at once; it takes no pointers.
    unsafe { libc::_exit(MEMORY_EXIT) }
}

// SAFETY: every call is passed on to `System` unchanged; the counting around
// it touches only atomics.
unsafe impl GlobalAlloc for Budgeted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::take(layout.size());
        // SAFETY: the caller's contract is passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::take(layout.size());
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::give(layout.size());
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::take(new_size - layout.size());
        } else {
            Self::give(layout.size() - new_size);
        }
        // SAFETY: as above.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Start counting allocations against `bytes`. Only the child calls this.
pub fn enforce_budget(bytes: u64) {
    USED.store(0, Ordering::Relaxed);
    BUDGET.store(
        usize::try_from(bytes).unwrap_or(usize::MAX),
        Ordering::Relaxed,
    );
}

/// If the arguments (after the program name) start with [`SUBCOMMAND`], be
/// the child: returns its exit status. `None` for any other arguments.
pub fn run_if_child(args: &[OsString]) -> Option<i32> {
    if args.first().is_none_or(|a| a != SUBCOMMAND) {
        return None;
    }
    // Before anything opens a file or starts a thread: nothing the parent
    // left open may be used from here.
    crate::sandbox::close_inherited_descriptors();
    Some(main(&args[1..]))
}

/// The child's main: `<memory budget in bytes> <file>`.
pub fn main(args: &[OsString]) -> i32 {
    let (Some(budget), Some(path)) = (args.first(), args.get(1)) else {
        return 2;
    };
    let Some(budget) = budget.to_str().and_then(|b| b.parse::<u64>().ok()) else {
        return 2;
    };
    enforce_budget(budget);
    watch_parent();
    let path = PathBuf::from(path);
    let worker = std::thread::Builder::new()
        .name("pdf".into())
        .stack_size(STACK_BYTES)
        .spawn(move || serve(&path, budget));
    match worker.map(|thread| thread.join()) {
        Ok(Ok(code)) => code,
        _ => PANIC_EXIT,
    }
}

/// End at once when the parent is gone: nobody is left to read the answers
/// or to kill this process. Standard input closing is the main sign (see
/// `serve`); this also catches a parent that died while its pipes stayed
/// open in another process.
#[cfg(unix)]
fn watch_parent() {
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let parent = unsafe { libc::getppid() };
    let _ = std::thread::Builder::new()
        .name("pdf-parent".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // SAFETY: as above.
                if unsafe { libc::getppid() } != parent {
                    gone();
                }
            }
        });
}

/// On other systems only the closing of standard input tells.
#[cfg(not(unix))]
fn watch_parent() {}

/// The parent is gone: end now, even in the middle of a page.
fn gone() -> ! {
    // SAFETY: `_exit` ends the process at once; it takes no pointers.
    unsafe { libc::_exit(0) }
}

fn serve(path: &Path, budget: u64) -> i32 {
    let mut input = io::stdin();
    let mut output = io::stdout().lock();
    let (password, limits) = match read_frame(&mut input) {
        Ok(ToChild::Start { password, limits }) => (password, limits.into_limits(budget)),
        _ => return 2,
    };
    // Requests are read on a thread of their own, from the start: a new one
    // replaces the old while a page renders, and the end of the input (the
    // parent is gone) ends the process, whatever it is doing.
    let (requests, inbox) = mpsc::channel();
    let max_query_chars = limits.max_query_chars;
    let reader = std::thread::Builder::new()
        .name("pdf-requests".into())
        .spawn(move || {
            while let Ok(request) = read_frame_with(&mut input, max_query_chars) {
                if matches!(request, ToChild::Start { .. }) || requests.send(request).is_err() {
                    break;
                }
            }
            gone();
        });
    if reader.is_err() {
        return 0;
    }
    // The file is read before the sandbox, which then only allows reading
    // the font directories (the fonts module reads them lazily).
    let bytes = match read_file(path, &limits) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = write_frame(&mut output, &FromChild::Failed(error));
            return 0;
        }
    };
    let font_dirs = crate::sandbox::font_dirs();
    let font_dirs: Vec<&Path> = font_dirs.iter().map(PathBuf::as_path).collect();
    if let Err(_reason) = crate::sandbox::restrict(&font_dirs)
        && cfg!(target_os = "macos")
    {
        // Never go on without the sandbox where there is one.
        let _ = write_frame(&mut output, &FromChild::Failed(OpenError::Crashed));
        return 0;
    }
    let opened = Document::open(bytes, &password, limits);
    drop(password);
    let document = match opened {
        Ok(document) => document,
        Err(error) => {
            let _ = write_frame(&mut output, &FromChild::Failed(error));
            return 0;
        }
    };
    if write_frame(&mut output, &FromChild::Opened(document.info().clone())).is_err() {
        return 0;
    }
    let mut work = Job {
        document: &document,
        cache: RenderCache::new(),
        limits,
        render: Vec::new(),
        texts: Vec::new(),
        outline: false,
        search: None,
    };
    work.run(&inbox, &mut output)
}

/// A search that is running.
struct Search {
    id: u64,
    query: String,
    match_case: bool,
    /// The next page to look at.
    next: usize,
    /// Matches reported so far.
    total: usize,
    /// Pages not to look at (the parent knows they kill children).
    skip: HashSet<usize>,
}

/// What the child has been asked for and not yet done.
struct Job<'d> {
    document: &'d Document,
    cache: RenderCache<'d>,
    limits: Limits,
    /// Pages to render, the most needed last.
    render: Vec<(usize, f32)>,
    /// Pages whose text is wanted, the most needed last.
    texts: Vec<usize>,
    outline: bool,
    search: Option<Search>,
}

impl<'d> Job<'d> {
    /// Take in what the parent asks.
    fn apply(&mut self, request: ToChild) {
        let pages = self.document.info().pages.len();
        match request {
            ToChild::Start { .. } => {}
            ToChild::Want(pages) => {
                self.render = pages;
                self.render.reverse();
            }
            ToChild::WantText(wanted) => {
                self.texts = wanted.into_iter().filter(|p| *p < pages).collect();
                self.texts.reverse();
            }
            ToChild::WantOutline => self.outline = true,
            ToChild::Find {
                search,
                query,
                match_case,
                start_page,
                found,
                skip,
            } => {
                self.search = Some(Search {
                    id: search,
                    query,
                    match_case,
                    next: start_page,
                    total: found,
                    skip: skip.into_iter().collect(),
                });
            }
            ToChild::CancelFind => self.search = None,
        }
    }

    /// Do what is asked, most urgent first: pages, text, outline, search.
    /// Between two steps whatever the parent has sent is taken in, so a new
    /// request replaces the rest of the old. Returns the exit status.
    fn run(&mut self, inbox: &mpsc::Receiver<ToChild>, output: &mut impl Write) -> i32 {
        loop {
            let idle = self.render.is_empty()
                && self.texts.is_empty()
                && !self.outline
                && self.search.is_none();
            let next = if idle {
                inbox.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
            } else {
                inbox.try_recv()
            };
            match next {
                Ok(request) => {
                    self.apply(request);
                    continue;
                }
                // The parent is gone or closed the tab.
                Err(mpsc::TryRecvError::Disconnected) => return 0,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let step = if let Some((page, scale)) = self.render.pop() {
                self.render_page(output, page, scale)
            } else if let Some(page) = self.texts.pop() {
                self.text_of(output, page)
            } else if self.outline {
                self.outline = false;
                self.outline_of(output)
            } else {
                self.search_step(output)
            };
            if let Some(code) = step {
                return code;
            }
        }
    }

    /// `Some(exit status)` ends the child.
    fn render_page(&mut self, output: &mut impl Write, page: usize, scale: f32) -> Option<i32> {
        write_frame(output, &FromChild::Started(page)).ok()?;
        let sent = match self.document.render(&self.cache, page, scale) {
            Ok(Some(pixmap)) => write_page(output, page, scale, &pixmap),
            Ok(None) => write_frame(output, &FromChild::PageFailed(page)),
            Err(()) => {
                let _ = write_frame(output, &FromChild::PageFailed(page));
                return Some(PANIC_EXIT);
            }
        };
        sent.is_err().then_some(0)
    }

    fn text_of(&mut self, output: &mut impl Write, page: usize) -> Option<i32> {
        if write_frame(output, &FromChild::Working(Step::Text(page))).is_err() {
            return Some(0);
        }
        // A panic in the parser ends the child, as one in rendering does.
        let text = match self.document.page_text(page) {
            Ok(text) => fit_text(text, page, self.limits.max_text_chars),
            Err(()) => return Some(PANIC_EXIT),
        };
        write_frame(output, &FromChild::Text(text))
            .is_err()
            .then_some(0)
    }

    fn outline_of(&mut self, output: &mut impl Write) -> Option<i32> {
        if write_frame(output, &FromChild::Working(Step::Outline)).is_err() {
            return Some(0);
        }
        let Ok(items) = catch_unwind(AssertUnwindSafe(|| self.document.outline())) else {
            return Some(PANIC_EXIT);
        };
        let items = fit_outline(items, &self.limits, self.document.info().pages.len());
        write_frame(output, &FromChild::Outline(items))
            .is_err()
            .then_some(0)
    }

    /// One page of the running search, or its end.
    fn search_step(&mut self, output: &mut impl Write) -> Option<i32> {
        let pages = self.document.info().pages.len();
        let max_hits = self.limits.max_find_hits;
        let search = self.search.as_mut()?;
        let id = search.id;
        while search.skip.contains(&search.next) {
            search.next += 1;
        }
        if search.next >= pages || search.total >= max_hits {
            let done = FromChild::FindDone {
                search: id,
                total: search.total.min(max_hits),
                // Stopped at the limit (perhaps with nothing more to find).
                truncated: search.total >= max_hits,
            };
            self.search = None;
            return write_frame(output, &done).is_err().then_some(0);
        }
        let page = search.next;
        let work = FromChild::Working(Step::Find { search: id, page });
        if write_frame(output, &work).is_err() {
            return Some(0);
        }
        let allowance = max_hits - search.total;
        let found = catch_unwind(AssertUnwindSafe(|| {
            self.document
                .find_in_page(page, &search.query, search.match_case, allowance)
        }));
        let Ok(hits) = found else {
            return Some(PANIC_EXIT);
        };
        let hits = fit_hits(hits, page, allowance, &self.limits);
        search.next = page + 1;
        search.total += hits.len();
        let frame = if hits.is_empty() {
            FromChild::Searched { search: id, page }
        } else {
            FromChild::FindHits { search: id, hits }
        };
        write_frame(output, &frame).is_err().then_some(0)
    }
}

/// A page's text as the parent will take it: at most `max_chars` chars, one
/// box entry per char, no box that is not a number.
fn fit_text(mut text: PageText, page: usize, max_chars: usize) -> PageText {
    text.page = page;
    if let Some((at, _)) = text.text.char_indices().nth(max_chars) {
        text.text.truncate(at);
    }
    let chars = text.text.chars().count();
    text.boxes.resize(chars, None);
    for entry in &mut text.boxes {
        if entry.is_some_and(|b| !b.iter().all(|v| v.is_finite())) {
            *entry = None;
        }
    }
    text
}

/// An outline within the limits: items, depth, titles and targets.
fn fit_outline(items: Vec<OutlineItem>, limits: &Limits, pages: usize) -> Vec<OutlineItem> {
    let deepest = limits.max_outline_depth.saturating_sub(1);
    let mut bytes = 0usize;
    let mut fitted = Vec::new();
    for mut item in items.into_iter().take(limits.max_outline_items) {
        item.depth = item.depth.min(u16::try_from(deepest).unwrap_or(u16::MAX));
        item.title = clean_title(&item.title)
            .chars()
            .take(limits.max_title_chars)
            .collect();
        bytes += item.title.len();
        if bytes > MAX_OUTLINE_BYTES {
            break;
        }
        item.target = item.target.filter(|t| t.page < pages).map(|t| Target {
            page: t.page,
            top: t.top.filter(|v| v.is_finite()),
        });
        fitted.push(item);
    }
    fitted
}

/// Hits on `page`, at most `allowance`, each with at most
/// [`MAX_BOXES_PER_HIT`] boxes and all together at most
/// [`MAX_BOXES_PER_FRAME`], every box finite; hits that do not fit the page
/// text's size are dropped.
fn fit_hits(
    mut hits: Vec<FindHit>,
    page: usize,
    allowance: usize,
    limits: &Limits,
) -> Vec<FindHit> {
    hits.retain(|h| {
        h.len > 0 && u64::from(h.start) + u64::from(h.len) <= limits.max_text_chars as u64
    });
    hits.truncate(allowance.min(limits.max_find_hits));
    let each = (MAX_BOXES_PER_FRAME / hits.len().max(1)).clamp(1, MAX_BOXES_PER_HIT);
    for hit in &mut hits {
        hit.page = page;
        hit.boxes.retain(|b| b.iter().all(|v| v.is_finite()));
        hit.boxes.truncate(each);
    }
    hits
}

fn read_file(path: &Path, limits: &Limits) -> Result<Vec<u8>, OpenError> {
    let size = std::fs::metadata(path)
        .map_err(|e| OpenError::Unreadable(e.kind()))?
        .len();
    if size > limits.max_file_bytes {
        return Err(OpenError::TooLarge(Exceeded::File));
    }
    std::fs::read(path).map_err(|e| OpenError::Unreadable(e.kind()))
}

/// What the parent sends.
#[derive(Debug, PartialEq)]
pub enum ToChild {
    /// The password to open the file with (empty for none) and the limits to
    /// work within. Sent once, first.
    Start {
        password: String,
        limits: WireLimits,
    },
    /// Pages to render, most needed first, as (page, pixels per point).
    Want(Vec<(usize, f32)>),
    /// Pages whose text is wanted, most needed first (at most
    /// [`MAX_WANTED_TEXT`]); replaces the last such request.
    WantText(Vec<usize>),
    /// The outline.
    WantOutline,
    /// Search for `query` from `start_page` on; replaces a running search.
    /// `found` matches have been reported already (by a child that ended).
    /// The pages in `skip` (at most [`MAX_WANTED_TEXT`], ascending) are not
    /// looked at: they are known to end children.
    Find {
        search: u64,
        query: String,
        match_case: bool,
        start_page: usize,
        found: usize,
        skip: Vec<usize>,
    },
    /// Stop the running search.
    CancelFind,
}

/// The limits that go to the child. The memory budget is a command-line
/// argument, and the time limits are the parent's own business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireLimits {
    pub max_file_bytes: u64,
    pub max_objects: u64,
    pub max_pages: u64,
    pub max_stream_bytes: u64,
    pub max_image_pixels: u64,
    pub max_render_pixels: u64,
    pub max_text_chars: u64,
    pub max_outline_items: u64,
    pub max_outline_depth: u64,
    pub max_title_chars: u64,
    pub max_find_hits: u64,
    pub max_query_chars: u64,
}

/// Numbers in a start frame.
const LIMIT_VALUES: usize = 12;

impl WireLimits {
    /// What the parent's limits say, for the wire.
    pub fn of(limits: &Limits) -> WireLimits {
        WireLimits {
            max_file_bytes: limits.max_file_bytes,
            max_objects: limits.max_objects as u64,
            max_pages: limits.max_pages as u64,
            max_stream_bytes: limits.max_stream_bytes as u64,
            max_image_pixels: limits.max_image_pixels,
            max_render_pixels: limits.max_render_pixels,
            max_text_chars: limits.max_text_chars as u64,
            max_outline_items: limits.max_outline_items as u64,
            max_outline_depth: limits.max_outline_depth as u64,
            max_title_chars: limits.max_title_chars as u64,
            max_find_hits: limits.max_find_hits as u64,
            max_query_chars: limits.max_query_chars as u64,
        }
    }

    fn as_array(&self) -> [u64; LIMIT_VALUES] {
        [
            self.max_file_bytes,
            self.max_objects,
            self.max_pages,
            self.max_stream_bytes,
            self.max_image_pixels,
            self.max_render_pixels,
            self.max_text_chars,
            self.max_outline_items,
            self.max_outline_depth,
            self.max_title_chars,
            self.max_find_hits,
            self.max_query_chars,
        ]
    }

    fn from_array(v: [u64; LIMIT_VALUES]) -> WireLimits {
        WireLimits {
            max_file_bytes: v[0],
            max_objects: v[1],
            max_pages: v[2],
            max_stream_bytes: v[3],
            max_image_pixels: v[4],
            max_render_pixels: v[5],
            max_text_chars: v[6],
            max_outline_items: v[7],
            max_outline_depth: v[8],
            max_title_chars: v[9],
            max_find_hits: v[10],
            max_query_chars: v[11],
        }
    }

    /// The child's limits, with its memory budget.
    fn into_limits(self, budget: u64) -> Limits {
        // `read_frame` has checked that the counts fit.
        let count = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        Limits {
            max_file_bytes: self.max_file_bytes,
            max_objects: count(self.max_objects),
            max_pages: count(self.max_pages),
            max_stream_bytes: count(self.max_stream_bytes),
            max_image_pixels: self.max_image_pixels,
            max_render_pixels: self.max_render_pixels,
            max_memory_bytes: budget,
            max_text_chars: count(self.max_text_chars),
            max_outline_items: count(self.max_outline_items),
            max_outline_depth: count(self.max_outline_depth),
            max_title_chars: count(self.max_title_chars),
            max_find_hits: count(self.max_find_hits),
            max_query_chars: count(self.max_query_chars),
            ..Limits::default()
        }
    }
}

/// A step the child is in, announced before it starts so that the parent
/// can time it. Every step ends with exactly one answer: the page, the text,
/// the outline, or the hits (or none) of that page of the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The text of this page.
    Text(usize),
    Outline,
    /// This page of this search.
    Find {
        search: u64,
        page: usize,
    },
}

/// What the child sends.
#[derive(Debug, PartialEq)]
pub enum FromChild {
    Opened(Info),
    Failed(OpenError),
    /// A page is being rendered (for the time limit).
    Started(usize),
    /// A page: the scale asked for, width, height and the pixels.
    Page(usize, f32, u32, u32, Vec<u8>),
    PageFailed(usize),
    /// A text, outline or search step begins (for the time limit).
    Working(Step),
    /// A page's text; `boxes` has one entry per `char` of `text`.
    Text(PageText),
    Outline(Vec<OutlineItem>),
    /// Matches of one search on one page: at least one, all on the same
    /// page (each [`FindHit::page`]).
    FindHits {
        search: u64,
        hits: Vec<FindHit>,
    },
    /// A search step that found nothing.
    Searched {
        search: u64,
        page: usize,
    },
    /// The search looked at every page, or stopped at `max_find_hits`.
    FindDone {
        search: u64,
        total: usize,
        truncated: bool,
    },
}

/// Pages and pixels are never larger than this, whatever a message says.
const MAX_PAGES: u32 = 1_000_000;
const MAX_SIDE: u32 = 16_384;
/// A limit in a start frame is never larger than this (1 PiB), and a count
/// must fit in `usize`.
const MAX_LIMIT: u64 = 1 << 50;
/// Pages a text request may name.
pub const MAX_WANTED_TEXT: usize = 4096;
/// Boxes in one match (a longer match keeps its first boxes).
pub const MAX_BOXES_PER_HIT: usize = 256;
/// Boxes in all the matches of one frame.
pub const MAX_BOXES_PER_FRAME: usize = 262_144;
/// Bytes of all the titles of one outline.
pub const MAX_OUTLINE_BYTES: usize = 8 << 20;
/// A password is never longer than this.
const MAX_PASSWORD: u32 = 1 << 16;

/// What the parent accepts in an answer: the limits and the number of pages
/// of the document (0 until it is known).
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    pub max_pixels: u64,
    pub pages: usize,
    pub max_text_chars: usize,
    pub max_outline_items: usize,
    pub max_outline_depth: usize,
    pub max_title_chars: usize,
    pub max_find_hits: usize,
}

impl ReadLimits {
    pub fn of(limits: &Limits, pages: usize) -> ReadLimits {
        ReadLimits {
            max_pixels: limits.max_render_pixels,
            pages,
            max_text_chars: limits.max_text_chars,
            max_outline_items: limits.max_outline_items,
            max_outline_depth: limits.max_outline_depth,
            max_title_chars: limits.max_title_chars,
            max_find_hits: limits.max_find_hits,
        }
    }
}

/// What an answer says before its body is read, for [`read_answer_with`]'s
/// `accept`: a `false` ends the reading (the caller ends the child) before
/// anything is allocated for the body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Peek {
    Page {
        page: usize,
        scale: f32,
        width: u32,
        height: u32,
    },
    Text {
        page: usize,
    },
    Outline,
    FindHits {
        search: u64,
        page: usize,
    },
}

fn u32_of(value: usize) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| io::Error::other("too large"))
}

fn unwritable() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "cannot be written")
}

fn put_box(buffer: &mut Vec<u8>, b: &TextBox) {
    for v in b {
        buffer.extend_from_slice(&v.to_le_bytes());
    }
}

pub fn write_frame(out: &mut impl Write, frame: &FromChild) -> io::Result<()> {
    let mut buffer = Vec::new();
    match frame {
        FromChild::Opened(info) => {
            buffer.push(b'O');
            buffer.extend_from_slice(&u32_of(info.pages.len())?.to_le_bytes());
            for page in &info.pages {
                buffer.extend_from_slice(&page.width.to_le_bytes());
                buffer.extend_from_slice(&page.height.to_le_bytes());
            }
            buffer.push(u8::from(info.encrypted));
        }
        FromChild::Failed(error) => {
            buffer.push(b'F');
            buffer.extend_from_slice(&error_code(*error));
        }
        FromChild::Started(page) => {
            buffer.push(b'S');
            buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
        }
        FromChild::Page(page, scale, width, height, pixels) => {
            out.write_all(&page_header(*page, *scale, *width, *height)?)?;
            out.write_all(pixels)?;
            return out.flush();
        }
        FromChild::PageFailed(page) => {
            buffer.push(b'X');
            buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
        }
        FromChild::Working(step) => {
            buffer.push(b'w');
            let (kind, search, page) = match *step {
                Step::Text(page) => (0u8, 0u64, page),
                Step::Outline => (1, 0, 0),
                Step::Find { search, page } => (2, search, page),
            };
            buffer.push(kind);
            buffer.extend_from_slice(&search.to_le_bytes());
            buffer.extend_from_slice(&u32_of(page)?.to_le_bytes());
        }
        FromChild::Text(text) => {
            let chars = text.text.chars().count();
            if text.boxes.len() != chars {
                return Err(unwritable());
            }
            buffer.push(b'T');
            buffer.extend_from_slice(&u32_of(text.page)?.to_le_bytes());
            buffer.extend_from_slice(&u32_of(text.text.len())?.to_le_bytes());
            buffer.extend_from_slice(&u32_of(chars)?.to_le_bytes());
            buffer.extend_from_slice(text.text.as_bytes());
            for entry in &text.boxes {
                match entry {
                    None => buffer.push(0),
                    Some(b) => {
                        buffer.push(1);
                        put_box(&mut buffer, b);
                    }
                }
            }
        }
        FromChild::Outline(items) => {
            buffer.push(b'L');
            buffer.extend_from_slice(&u32_of(items.len())?.to_le_bytes());
            for item in items {
                buffer.extend_from_slice(&item.depth.to_le_bytes());
                let flags = u8::from(item.open)
                    | u8::from(item.target.is_some()) << 1
                    | u8::from(item.target.is_some_and(|t| t.top.is_some())) << 2;
                buffer.push(flags);
                buffer.extend_from_slice(&u32_of(item.title.len())?.to_le_bytes());
                buffer.extend_from_slice(item.title.as_bytes());
                if let Some(target) = item.target {
                    buffer.extend_from_slice(&u32_of(target.page)?.to_le_bytes());
                    if let Some(top) = target.top {
                        buffer.extend_from_slice(&top.to_le_bytes());
                    }
                }
            }
        }
        FromChild::FindHits { search, hits } => {
            let Some(page) = hits.first().map(|h| h.page) else {
                return Err(unwritable());
            };
            if hits.iter().any(|h| h.page != page) {
                return Err(unwritable());
            }
            buffer.push(b'H');
            buffer.extend_from_slice(&search.to_le_bytes());
            buffer.extend_from_slice(&u32_of(page)?.to_le_bytes());
            buffer.extend_from_slice(&u32_of(hits.len())?.to_le_bytes());
            for hit in hits {
                buffer.extend_from_slice(&hit.start.to_le_bytes());
                buffer.extend_from_slice(&hit.len.to_le_bytes());
                buffer.extend_from_slice(&u32_of(hit.boxes.len())?.to_le_bytes());
                for b in &hit.boxes {
                    put_box(&mut buffer, b);
                }
            }
        }
        FromChild::Searched { search, page } => {
            buffer.push(b'N');
            buffer.extend_from_slice(&search.to_le_bytes());
            buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
        }
        FromChild::FindDone {
            search,
            total,
            truncated,
        } => {
            buffer.push(b'D');
            buffer.extend_from_slice(&search.to_le_bytes());
            buffer.extend_from_slice(&u32_of(*total)?.to_le_bytes());
            buffer.push(u8::from(*truncated));
        }
    }
    out.write_all(&buffer)?;
    out.flush()
}

fn page_header(page: usize, scale: f32, width: u32, height: u32) -> io::Result<Vec<u8>> {
    let mut header = vec![b'P'];
    header.extend_from_slice(&u32_of(page)?.to_le_bytes());
    header.extend_from_slice(&scale.to_le_bytes());
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    Ok(header)
}

/// A rendered page straight from the parser's pixmap, without copying it.
fn write_page(
    out: &mut impl Write,
    page: usize,
    scale: f32,
    pixmap: &hayro::vello_cpu::Pixmap,
) -> io::Result<()> {
    let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    out.write_all(&page_header(page, scale, width, height)?)?;
    out.write_all(pixmap.data_as_u8_slice())?;
    out.flush()
}

pub fn write_request(out: &mut impl Write, frame: &ToChild) -> io::Result<()> {
    let mut buffer = Vec::new();
    match frame {
        ToChild::Start { password, limits } => {
            buffer.push(b'K');
            buffer.extend_from_slice(&u32_of(password.len())?.to_le_bytes());
            buffer.extend_from_slice(password.as_bytes());
            for value in limits.as_array() {
                buffer.extend_from_slice(&value.to_le_bytes());
            }
        }
        ToChild::Want(pages) => {
            buffer.push(b'W');
            buffer.extend_from_slice(&u32_of(pages.len())?.to_le_bytes());
            for (page, scale) in pages {
                buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
                buffer.extend_from_slice(&scale.to_le_bytes());
            }
        }
        ToChild::WantText(pages) => {
            buffer.push(b'T');
            buffer.extend_from_slice(&u32_of(pages.len())?.to_le_bytes());
            for page in pages {
                buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
            }
        }
        ToChild::WantOutline => buffer.push(b'L'),
        ToChild::Find {
            search,
            query,
            match_case,
            start_page,
            found,
            skip,
        } => {
            buffer.push(b'Q');
            buffer.extend_from_slice(&search.to_le_bytes());
            buffer.push(u8::from(*match_case));
            buffer.extend_from_slice(&u32_of(*start_page)?.to_le_bytes());
            buffer.extend_from_slice(&u32_of(*found)?.to_le_bytes());
            buffer.extend_from_slice(&u32_of(query.len())?.to_le_bytes());
            buffer.extend_from_slice(query.as_bytes());
            buffer.extend_from_slice(&u32_of(skip.len())?.to_le_bytes());
            for page in skip {
                buffer.extend_from_slice(&u32_of(*page)?.to_le_bytes());
            }
        }
        ToChild::CancelFind => buffer.push(b'C'),
    }
    out.write_all(&buffer)?;
    out.flush()
}

fn read_u8(input: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    input.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16(input: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    input.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    input.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    input.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_f32(input: &mut impl Read) -> io::Result<f32> {
    Ok(f32::from_bits(read_u32(input)?))
}

/// A finite number.
fn read_finite(input: &mut impl Read) -> io::Result<f32> {
    let value = read_f32(input)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(bad())
    }
}

fn read_box(input: &mut impl Read) -> io::Result<TextBox> {
    Ok([
        read_finite(input)?,
        read_finite(input)?,
        read_finite(input)?,
        read_finite(input)?,
    ])
}

/// `bytes` bytes of valid UTF-8; the caller has bounded `bytes`.
fn read_string(input: &mut impl Read, bytes: usize) -> io::Result<String> {
    let mut data = vec![0u8; bytes];
    input.read_exact(&mut data)?;
    String::from_utf8(data).map_err(|_| bad())
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "bad message")
}

/// A request, as the child reads it, with the query as long as the default
/// limit says.
pub fn read_frame(input: &mut impl Read) -> io::Result<ToChild> {
    read_frame_with(input, Limits::default().max_query_chars)
}

/// A request, as the child reads it; a query is at most `max_query_chars`
/// chars (and four times as many bytes, checked before it is read).
pub fn read_frame_with(input: &mut impl Read, max_query_chars: usize) -> io::Result<ToChild> {
    match read_u8(input)? {
        b'K' => {
            let length = read_u32(input)?;
            if length > MAX_PASSWORD {
                return Err(bad());
            }
            let password = read_string(input, length as usize)?;
            let mut values = [0u64; LIMIT_VALUES];
            for value in &mut values {
                *value = read_u64(input)?;
                if *value > MAX_LIMIT || usize::try_from(*value).is_err() {
                    return Err(bad());
                }
            }
            let limits = WireLimits::from_array(values);
            Ok(ToChild::Start { password, limits })
        }
        b'W' => {
            let count = read_u32(input)?;
            if count > 4096 {
                return Err(bad());
            }
            let mut pages = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let page = read_u32(input)? as usize;
                let scale = read_f32(input)?;
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(bad());
                }
                pages.push((page, scale));
            }
            Ok(ToChild::Want(pages))
        }
        b'T' => {
            let count = read_u32(input)? as usize;
            if count > MAX_WANTED_TEXT {
                return Err(bad());
            }
            let mut pages = Vec::with_capacity(count);
            for _ in 0..count {
                pages.push(read_u32(input)? as usize);
            }
            Ok(ToChild::WantText(pages))
        }
        b'L' => Ok(ToChild::WantOutline),
        b'Q' => {
            let search = read_u64(input)?;
            let match_case = match read_u8(input)? {
                0 => false,
                1 => true,
                _ => return Err(bad()),
            };
            let start_page = read_u32(input)? as usize;
            let found = read_u32(input)? as usize;
            let bytes = read_u32(input)? as usize;
            if bytes == 0 || bytes > max_query_chars.saturating_mul(4) {
                return Err(bad());
            }
            let query = read_string(input, bytes)?;
            if query.chars().count() > max_query_chars {
                return Err(bad());
            }
            let count = read_u32(input)? as usize;
            if count > MAX_WANTED_TEXT {
                return Err(bad());
            }
            let mut skip = Vec::with_capacity(count);
            for _ in 0..count {
                skip.push(read_u32(input)? as usize);
            }
            Ok(ToChild::Find {
                search,
                query,
                match_case,
                start_page,
                found,
                skip,
            })
        }
        b'C' => Ok(ToChild::CancelFind),
        _ => Err(bad()),
    }
}

/// An answer, as the parent reads it. `max_pixels` bounds a page; the text
/// limits are the defaults and any number of pages is believed.
pub fn read_answer(input: &mut impl Read, max_pixels: u64) -> io::Result<FromChild> {
    let limits = ReadLimits {
        max_pixels,
        ..ReadLimits::of(&Limits::default(), MAX_PAGES as usize)
    };
    read_answer_with(input, &limits, |_| true)
}

/// Like [`read_answer`], but every count and length is checked against
/// `limits` before anything is allocated for it, and `accept` is asked about
/// a page, a text, an outline or hits before their bodies are read; a
/// `false` is an error, and the caller ends the child. Text and titles come
/// back without control characters.
pub fn read_answer_with(
    input: &mut impl Read,
    limits: &ReadLimits,
    mut accept: impl FnMut(&Peek) -> bool,
) -> io::Result<FromChild> {
    match read_u8(input)? {
        b'O' => {
            let count = read_u32(input)?;
            if count == 0 || count > MAX_PAGES {
                return Err(bad());
            }
            let mut pages = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let width = read_f32(input)?;
                let height = read_f32(input)?;
                if !(width.is_finite() && height.is_finite()) {
                    return Err(bad());
                }
                pages.push(PageSize { width, height });
            }
            let encrypted = read_u8(input)? != 0;
            Ok(FromChild::Opened(Info { pages, encrypted }))
        }
        b'F' => {
            let code = [read_u8(input)?, read_u8(input)?];
            Ok(FromChild::Failed(error_of(code).ok_or_else(bad)?))
        }
        b'S' => Ok(FromChild::Started(read_u32(input)? as usize)),
        b'X' => Ok(FromChild::PageFailed(read_u32(input)? as usize)),
        b'P' => {
            let page = read_u32(input)? as usize;
            let scale = read_f32(input)?;
            let width = read_u32(input)?;
            let height = read_u32(input)?;
            let pixels = u64::from(width) * u64::from(height);
            let max_pixels = limits.max_pixels;
            // A page is never larger than the limit (plus rounding).
            if width > MAX_SIDE
                || height > MAX_SIDE
                || pixels > max_pixels + max_pixels / 50 + 4096
                || !accept(&Peek::Page {
                    page,
                    scale,
                    width,
                    height,
                })
            {
                return Err(bad());
            }
            let mut data = vec![0u8; pixels as usize * 4];
            input.read_exact(&mut data)?;
            Ok(FromChild::Page(page, scale, width, height, data))
        }
        b'w' => {
            let kind = read_u8(input)?;
            let search = read_u64(input)?;
            let page = read_u32(input)? as usize;
            Ok(FromChild::Working(match kind {
                0 if search == 0 && page < limits.pages => Step::Text(page),
                1 if search == 0 && page == 0 => Step::Outline,
                2 if page < limits.pages => Step::Find { search, page },
                _ => return Err(bad()),
            }))
        }
        b'T' => {
            let page = read_u32(input)? as usize;
            let bytes = read_u32(input)? as usize;
            let chars = read_u32(input)? as usize;
            // A char is one to four bytes, and has one box entry.
            if page >= limits.pages
                || chars > limits.max_text_chars
                || bytes > limits.max_text_chars.saturating_mul(4)
                || bytes < chars
                || bytes > chars.saturating_mul(4)
                || !accept(&Peek::Text { page })
            {
                return Err(bad());
            }
            let mut text = read_string(input, bytes)?;
            if text.chars().count() != chars {
                return Err(bad());
            }
            let mut boxes = Vec::with_capacity(chars);
            for _ in 0..chars {
                boxes.push(match read_u8(input)? {
                    0 => None,
                    1 => Some(read_box(input)?),
                    _ => return Err(bad()),
                });
            }
            clean_text(&mut text);
            Ok(FromChild::Text(PageText { page, text, boxes }))
        }
        b'L' => {
            if !accept(&Peek::Outline) {
                return Err(bad());
            }
            let count = read_u32(input)? as usize;
            if count > limits.max_outline_items {
                return Err(bad());
            }
            let mut items = Vec::with_capacity(count);
            let mut total = 0usize;
            for _ in 0..count {
                let depth = read_u16(input)?;
                let flags = read_u8(input)?;
                let bytes = read_u32(input)? as usize;
                total = total.saturating_add(bytes);
                if usize::from(depth) >= limits.max_outline_depth
                    || flags > 0b111
                    || (flags & 0b100 != 0 && flags & 0b010 == 0)
                    || bytes > limits.max_title_chars.saturating_mul(4)
                    || total > MAX_OUTLINE_BYTES
                {
                    return Err(bad());
                }
                let title = read_string(input, bytes)?;
                if title.chars().count() > limits.max_title_chars {
                    return Err(bad());
                }
                let target = if flags & 0b010 != 0 {
                    let page = read_u32(input)? as usize;
                    if page >= limits.pages {
                        return Err(bad());
                    }
                    let top = if flags & 0b100 != 0 {
                        Some(read_finite(input)?)
                    } else {
                        None
                    };
                    Some(Target { page, top })
                } else {
                    None
                };
                items.push(OutlineItem {
                    depth,
                    title: clean_title(&title),
                    target,
                    open: flags & 1 != 0,
                });
            }
            Ok(FromChild::Outline(items))
        }
        b'H' => {
            let search = read_u64(input)?;
            let page = read_u32(input)? as usize;
            let count = read_u32(input)? as usize;
            if page >= limits.pages
                || count == 0
                || count > limits.max_find_hits
                || !accept(&Peek::FindHits { search, page })
            {
                return Err(bad());
            }
            let mut hits = Vec::with_capacity(count);
            let mut all_boxes = 0usize;
            for _ in 0..count {
                let start = read_u32(input)?;
                let len = read_u32(input)?;
                let boxes = read_u32(input)? as usize;
                all_boxes = all_boxes.saturating_add(boxes);
                if len == 0
                    || u64::from(start) + u64::from(len) > limits.max_text_chars as u64
                    || boxes > MAX_BOXES_PER_HIT
                    || all_boxes > MAX_BOXES_PER_FRAME
                {
                    return Err(bad());
                }
                let mut list = Vec::with_capacity(boxes);
                for _ in 0..boxes {
                    list.push(read_box(input)?);
                }
                hits.push(FindHit {
                    page,
                    start,
                    len,
                    boxes: list,
                });
            }
            Ok(FromChild::FindHits { search, hits })
        }
        b'N' => {
            let search = read_u64(input)?;
            let page = read_u32(input)? as usize;
            if page >= limits.pages {
                return Err(bad());
            }
            Ok(FromChild::Searched { search, page })
        }
        b'D' => {
            let search = read_u64(input)?;
            let total = read_u32(input)? as usize;
            let truncated = match read_u8(input)? {
                0 => false,
                1 => true,
                _ => return Err(bad()),
            };
            if total > limits.max_find_hits {
                return Err(bad());
            }
            Ok(FromChild::FindDone {
                search,
                total,
                truncated,
            })
        }
        _ => Err(bad()),
    }
}

/// Page text without unwanted characters (controls, bidirectional and
/// zero-width characters, separators; see [`crate::clean`]): those other than
/// a line break become spaces, so that the boxes stay where they are.
pub fn clean_text(text: &mut String) {
    crate::clean::lines_in_place(text);
}

/// A title without unwanted characters.
pub fn clean_title(title: &str) -> String {
    crate::clean::line(title)
}

fn error_code(error: OpenError) -> [u8; 2] {
    match error {
        OpenError::NeedsPassword => [1, 0],
        OpenError::WrongPassword => [2, 0],
        OpenError::UnsupportedEncryption => [3, 0],
        OpenError::TooLarge(what) => [
            4,
            match what {
                Exceeded::File => 0,
                Exceeded::Objects => 1,
                Exceeded::Pages => 2,
                Exceeded::Stream => 3,
                Exceeded::Image => 4,
                Exceeded::Memory => 5,
                Exceeded::Time => 6,
            },
        ],
        OpenError::Unchecked => [5, 0],
        OpenError::Damaged => [6, 0],
        OpenError::Crashed => [7, 0],
        OpenError::Unreadable(kind) => [
            8,
            match kind {
                io::ErrorKind::NotFound => 0,
                io::ErrorKind::PermissionDenied => 1,
                _ => 2,
            },
        ],
    }
}

fn error_of(code: [u8; 2]) -> Option<OpenError> {
    Some(match code {
        [1, _] => OpenError::NeedsPassword,
        [2, _] => OpenError::WrongPassword,
        [3, _] => OpenError::UnsupportedEncryption,
        [4, what] => OpenError::TooLarge(match what {
            0 => Exceeded::File,
            1 => Exceeded::Objects,
            2 => Exceeded::Pages,
            3 => Exceeded::Stream,
            4 => Exceeded::Image,
            5 => Exceeded::Memory,
            6 => Exceeded::Time,
            _ => return None,
        }),
        [5, _] => OpenError::Unchecked,
        [6, _] => OpenError::Damaged,
        [7, _] => OpenError::Crashed,
        [8, kind] => OpenError::Unreadable(match kind {
            0 => io::ErrorKind::NotFound,
            1 => io::ErrorKind::PermissionDenied,
            _ => io::ErrorKind::Other,
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small limits, so that each can be broken by a few bytes.
    fn small() -> ReadLimits {
        ReadLimits {
            max_pixels: 1000,
            pages: 3,
            max_text_chars: 100,
            max_outline_items: 5,
            max_outline_depth: 3,
            max_title_chars: 10,
            max_find_hits: 4,
        }
    }

    fn read(wire: &[u8]) -> io::Result<FromChild> {
        read_answer_with(&mut &*wire, &small(), |_| true)
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn text_frame(page: u32, text: &str, chars: u32, boxes: &[Option<TextBox>]) -> Vec<u8> {
        let mut wire = vec![b'T'];
        wire.extend(u32s(&[page, text.len() as u32, chars]));
        wire.extend_from_slice(text.as_bytes());
        for entry in boxes {
            match entry {
                None => wire.push(0),
                Some(b) => {
                    wire.push(1);
                    put_box(&mut wire, b);
                }
            }
        }
        wire
    }

    const B: TextBox = [1.0, 2.0, 3.0, 4.0];

    #[test]
    fn messages_go_both_ways() {
        let mut wire = Vec::new();
        let start = ToChild::Start {
            password: "密码".into(),
            limits: WireLimits::of(&Limits {
                max_render_pixels: 64_000_000,
                max_pages: 12,
                max_text_chars: 77,
                max_outline_items: 78,
                max_outline_depth: 7,
                max_title_chars: 79,
                max_find_hits: 80,
                max_query_chars: 81,
                ..Limits::default()
            }),
        };
        let requests = [
            start,
            ToChild::Want(vec![(3, 1.5), (0, 0.25)]),
            ToChild::WantText(vec![5, 1, 0]),
            ToChild::WantOutline,
            ToChild::Find {
                search: u64::MAX - 1,
                query: "Grüße 日本".into(),
                match_case: true,
                start_page: 9,
                found: 4,
                skip: vec![11, 40],
            },
            ToChild::CancelFind,
        ];
        for request in &requests {
            write_request(&mut wire, request).unwrap();
        }
        let mut input = wire.as_slice();
        for request in requests {
            assert_eq!(read_frame(&mut input).unwrap(), request);
        }
        assert!(input.is_empty());

        let answers = [
            FromChild::Opened(Info {
                pages: vec![PageSize {
                    width: 612.0,
                    height: 792.0,
                }],
                encrypted: true,
            }),
            FromChild::Failed(OpenError::TooLarge(Exceeded::Image)),
            FromChild::Failed(OpenError::Unreadable(io::ErrorKind::PermissionDenied)),
            FromChild::Started(7),
            FromChild::Page(7, 2.0, 2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]),
            FromChild::PageFailed(9),
        ];
        let mut wire = Vec::new();
        for answer in &answers {
            write_frame(&mut wire, answer).unwrap();
        }
        let mut input = wire.as_slice();
        for answer in answers {
            assert_eq!(read_answer(&mut input, 1000).unwrap(), answer);
        }
    }

    #[test]
    fn text_outline_and_search_answers_go_through() {
        let hit = |page, start, len| FindHit {
            page,
            start,
            len,
            boxes: vec![B, [5.0, 6.0, 7.0, 8.0]],
        };
        let answers = [
            FromChild::Working(Step::Text(2)),
            FromChild::Working(Step::Outline),
            FromChild::Working(Step::Find { search: 9, page: 1 }),
            FromChild::Text(PageText {
                page: 1,
                text: "añ\n日".into(),
                boxes: vec![Some(B), None, None, Some(B)],
            }),
            FromChild::Text(PageText {
                page: 0,
                ..PageText::default()
            }),
            FromChild::Outline(vec![
                OutlineItem {
                    depth: 0,
                    title: "One".into(),
                    target: Some(Target {
                        page: 2,
                        top: Some(10.5),
                    }),
                    open: true,
                },
                OutlineItem {
                    depth: 2,
                    title: "日本語".into(),
                    target: Some(Target { page: 0, top: None }),
                    open: false,
                },
                OutlineItem {
                    depth: 1,
                    title: String::new(),
                    target: None,
                    open: false,
                },
            ]),
            FromChild::Outline(Vec::new()),
            FromChild::FindHits {
                search: 9,
                hits: vec![hit(1, 0, 3), hit(1, 50, 50)],
            },
            FromChild::Searched { search: 9, page: 2 },
            FromChild::FindDone {
                search: 9,
                total: 4,
                truncated: true,
            },
        ];
        let mut wire = Vec::new();
        for answer in &answers {
            write_frame(&mut wire, answer).unwrap();
        }
        let mut input = wire.as_slice();
        for answer in answers {
            let read = read_answer_with(&mut input, &small(), |_| true).unwrap();
            assert_eq!(read, answer);
        }
        assert!(input.is_empty());
    }

    #[test]
    fn what_cannot_be_written_is_not_written() {
        let mut wire = Vec::new();
        // Boxes that do not match the chars; no hits; hits on two pages.
        assert!(
            write_frame(
                &mut wire,
                &FromChild::Text(PageText {
                    page: 0,
                    text: "ab".into(),
                    boxes: vec![None],
                })
            )
            .is_err()
        );
        let hit = |page| FindHit {
            page,
            start: 0,
            len: 1,
            boxes: vec![],
        };
        for hits in [vec![], vec![hit(0), hit(1)]] {
            assert!(write_frame(&mut wire, &FromChild::FindHits { search: 1, hits }).is_err());
        }
        assert!(wire.is_empty());
    }

    #[test]
    fn oversized_messages_are_refused_before_allocating() {
        let mut wire = page_header(0, 1.0, 16_000, 16_000).unwrap();
        wire.extend_from_slice(&[0; 16]);
        assert!(read_answer(&mut wire.as_slice(), 1_000_000).is_err());
        let mut wire = vec![b'W'];
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut wire.as_slice()).is_err());
        // A start frame with an absurd limit.
        let mut wire = vec![b'K'];
        wire.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..LIMIT_VALUES {
            wire.extend_from_slice(&u64::MAX.to_le_bytes());
        }
        assert!(read_frame(&mut wire.as_slice()).is_err());
        let mut wire = vec![b'K'];
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut wire.as_slice()).is_err());
        let mut wire = vec![b'O'];
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_answer(&mut wire.as_slice(), 1000).is_err());
    }

    /// Frames whose header names more than a limit allows end the reading at
    /// the header: the body is not there to be read (so a refusal can only
    /// have come from the header), and no `Vec` of that size is made.
    #[test]
    fn requests_are_bounded() {
        // Too many pages of text; a query too long (bytes, then chars).
        let mut wire = vec![b'T'];
        wire.extend(u32s(&[MAX_WANTED_TEXT as u32 + 1]));
        assert!(read_frame(&mut wire.as_slice()).is_err());
        let mut wire = vec![b'Q'];
        wire.extend(0u64.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[0, 0, 13]));
        assert!(read_frame_with(&mut wire.as_slice(), 3).is_err());
        let mut wire = vec![b'Q'];
        wire.extend(0u64.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[0, 0, u32::MAX]));
        assert!(read_frame_with(&mut wire.as_slice(), 256).is_err());
        // Four chars in twelve bytes against a limit of three chars.
        let mut wire = vec![b'Q'];
        wire.extend(0u64.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[0, 0, 4]));
        wire.extend_from_slice(b"abcd");
        assert!(read_frame_with(&mut wire.as_slice(), 3).is_err());
        wire.truncate(wire.len() - 1);
        wire.extend_from_slice(b"");
        // Empty, and not UTF-8.
        let mut wire = vec![b'Q'];
        wire.extend(0u64.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[0, 0, 0]));
        assert!(read_frame(&mut wire.as_slice()).is_err());
        let mut wire = vec![b'Q'];
        wire.extend(0u64.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[0, 0, 2]));
        wire.extend_from_slice(&[0xff, 0xfe]);
        assert!(read_frame(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn oversized_text_is_refused_before_allocating() {
        // 101 chars against a limit of 100, and 401 bytes against 400: the
        // frame ends after the header, so only the header can be the reason.
        for (bytes, chars) in [(101u32, 101u32), (401, 100), (u32::MAX, 50), (50, u32::MAX)] {
            let mut wire = vec![b'T'];
            wire.extend(u32s(&[0, bytes, chars]));
            let error = read(&wire).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{bytes} {chars}");
        }
        // Fewer bytes than chars, or more than four bytes a char.
        for (bytes, chars) in [(2u32, 3u32), (9, 2)] {
            let mut wire = vec![b'T'];
            wire.extend(u32s(&[0, bytes, chars]));
            assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        // At the limit it is read (and ends for want of bytes).
        let mut wire = vec![b'T'];
        wire.extend(u32s(&[0, 100, 100]));
        assert_eq!(
            read(&wire).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn text_with_boxes_and_chars_that_disagree_is_refused() {
        // The header says 2 chars; the bytes hold one ("é").
        let wire = text_frame(0, "é", 2, &[None, None]);
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Three chars, header says two, and the bytes are within 4 per char.
        let wire = text_frame(0, "ñññ", 2, &[None, None]);
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Fewer boxes than chars: the frame ends early.
        let wire = text_frame(0, "ab", 2, &[None]);
        assert_eq!(
            read(&wire).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        // A box entry that is neither none nor some.
        let mut wire = text_frame(0, "a", 1, &[]);
        wire.push(7);
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Right.
        let wire = text_frame(0, "é", 1, &[Some(B)]);
        assert!(read(&wire).is_ok());
    }

    #[test]
    fn text_that_is_not_utf8_is_refused() {
        let mut wire = vec![b'T'];
        wire.extend(u32s(&[0, 2, 2]));
        wire.extend_from_slice(&[0xc3, 0x28]);
        wire.extend_from_slice(&[0, 0]);
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // A surrogate encoded as UTF-8 is not UTF-8 either.
        let mut wire = vec![b'T'];
        wire.extend(u32s(&[0, 3, 1]));
        wire.extend_from_slice(&[0xed, 0xa0, 0x80]);
        wire.push(0);
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_box_that_is_not_a_number_is_refused() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let wire = text_frame(0, "a", 1, &[Some([1.0, bad, 3.0, 4.0])]);
            assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
            let mut wire = vec![b'H'];
            wire.extend(7u64.to_le_bytes());
            wire.extend(u32s(&[0, 1, 0, 1, 1]));
            put_box(&mut wire, &[bad, 0.0, 0.0, 0.0]);
            assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
            let mut wire = vec![b'L'];
            wire.extend(u32s(&[1]));
            wire.extend(0u16.to_le_bytes());
            wire.push(0b110);
            wire.extend(u32s(&[0, 0]));
            wire.extend(bad.to_le_bytes());
            assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn text_from_the_child_comes_back_without_control_characters() {
        let text = "a\u{0}b\tc\nd\u{85}e\u{7f}f\u{1b}[31m";
        let chars = text.chars().count() as u32;
        let wire = text_frame(1, text, chars, &vec![None; chars as usize]);
        let FromChild::Text(got) = read(&wire).unwrap() else {
            panic!("not a text");
        };
        // Replaced, not removed: the boxes stay where they are.
        assert_eq!(got.text.chars().count(), chars as usize);
        assert_eq!(got.boxes.len(), chars as usize);
        assert!(!got.text.chars().any(|c| c.is_control() && c != '\n'));
        assert_eq!(got.text, "a b c\nd e f [31m");
    }

    #[test]
    fn text_without_bidirectional_zero_width_and_separator_characters() {
        let text = "a\u{202E}b\u{200B}c\u{2028}d\u{FEFF}e\u{2066}f\ng";
        let chars = text.chars().count() as u32;
        let wire = text_frame(1, text, chars, &vec![None; chars as usize]);
        let FromChild::Text(got) = read(&wire).unwrap() else {
            panic!("not a text");
        };
        assert_eq!(got.text, "a b c d e f\ng");
        assert_eq!(got.text.chars().count(), chars as usize);
        assert_eq!(got.boxes.len(), chars as usize);
    }

    #[test]
    fn titles_lose_bidirectional_zero_width_and_separator_characters() {
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[1]));
        let title = "g\u{202E}p\u{200B}e\u{2066}\u{2028}!\u{FEFF}";
        wire.extend(outline_item(0, 0, title.as_bytes(), &[]));
        let FromChild::Outline(items) = read(&wire).unwrap() else {
            panic!("not an outline");
        };
        assert_eq!(items[0].title, "gpe!");
        assert_eq!(clean_title("x\u{202D}y"), "xy");
    }

    fn outline_item(depth: u16, flags: u8, title: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut wire = depth.to_le_bytes().to_vec();
        wire.push(flags);
        wire.extend(u32s(&[title.len() as u32]));
        wire.extend_from_slice(title);
        wire.extend_from_slice(rest);
        wire
    }

    #[test]
    fn an_outline_beyond_its_limits_is_refused() {
        // Too many items: only the count is there.
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[6]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[u32::MAX]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // At the limit: reads on (and runs out).
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[5]));
        assert_eq!(
            read(&wire).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        // Depth: 3 levels are allowed (0, 1, 2), the fourth is not.
        for (depth, fine) in [(2u16, true), (3, false), (u16::MAX, false)] {
            let mut wire = vec![b'L'];
            wire.extend(u32s(&[1]));
            wire.extend(outline_item(depth, 0, b"x", &[]));
            assert_eq!(read(&wire).is_ok(), fine, "depth {depth}");
        }
        // A title of 41 bytes against 10 chars (40 bytes); the body is
        // absent, so the length alone ended it.
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[1]));
        wire.extend(0u16.to_le_bytes());
        wire.push(0);
        wire.extend(u32s(&[41]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // 11 chars in 11 bytes: within the byte bound, over the char one.
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[1]));
        wire.extend(outline_item(0, 0, b"abcdefghijk", &[]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Not UTF-8.
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[1]));
        wire.extend(outline_item(0, 0, &[0xff, 0xff], &[]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Flags that mean nothing: a height without a page, unknown bits.
        for flags in [0b100u8, 0b1000, 0xff] {
            let mut wire = vec![b'L'];
            wire.extend(u32s(&[1]));
            wire.extend(outline_item(0, flags, b"x", &[0; 8]));
            assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn titles_come_back_without_control_characters() {
        let mut wire = vec![b'L'];
        wire.extend(u32s(&[1]));
        wire.extend(outline_item(0, 0, "A\u{0}\tB\u{202}\n".as_bytes(), &[]));
        let FromChild::Outline(items) = read(&wire).unwrap() else {
            panic!("not an outline");
        };
        assert_eq!(items[0].title, "AB\u{202}");
    }

    #[test]
    fn pages_and_targets_out_of_range_are_refused() {
        // The document has three pages.
        let text = |page| text_frame(page, "a", 1, &[None]);
        assert!(read(&text(2)).is_ok());
        assert!(read(&text(3)).is_err());
        assert!(read(&text(u32::MAX)).is_err());
        for (page, fine) in [(2u32, true), (3, false)] {
            let mut wire = vec![b'L'];
            wire.extend(u32s(&[1]));
            wire.extend(outline_item(0, 0b10, b"x", &page.to_le_bytes()));
            assert_eq!(read(&wire).is_ok(), fine, "target {page}");
            let mut wire = vec![b'N'];
            wire.extend(5u64.to_le_bytes());
            wire.extend(u32s(&[page]));
            assert_eq!(read(&wire).is_ok(), fine, "searched {page}");
            let mut wire = vec![b'H'];
            wire.extend(5u64.to_le_bytes());
            wire.extend(u32s(&[page, 1, 0, 1, 0]));
            assert_eq!(read(&wire).is_ok(), fine, "hits {page}");
            let mut wire = vec![b'w', 0];
            wire.extend(0u64.to_le_bytes());
            wire.extend(u32s(&[page]));
            assert_eq!(read(&wire).is_ok(), fine, "working text {page}");
            let mut wire = vec![b'w', 2];
            wire.extend(5u64.to_le_bytes());
            wire.extend(u32s(&[page]));
            assert_eq!(read(&wire).is_ok(), fine, "working find {page}");
        }
        // A step of another kind that names things it has no use for.
        let mut wire = vec![b'w', 1];
        wire.extend(1u64.to_le_bytes());
        wire.extend(u32s(&[0]));
        assert!(read(&wire).is_err());
        let mut wire = vec![b'w', 9];
        wire.extend(0u64.to_le_bytes());
        wire.extend(u32s(&[0]));
        assert!(read(&wire).is_err());
    }

    fn hits_header(count: u32) -> Vec<u8> {
        let mut wire = vec![b'H'];
        wire.extend(7u64.to_le_bytes());
        wire.extend(u32s(&[1, count]));
        wire
    }

    #[test]
    fn too_many_hits_or_boxes_are_refused() {
        // Five hits against a limit of four, and none at all: header only.
        for count in [5, 0, u32::MAX] {
            assert_eq!(
                read(&hits_header(count)).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{count}"
            );
        }
        // One hit with 257 boxes (the limit is 256): its header only.
        let mut wire = hits_header(1);
        wire.extend(u32s(&[0, 1, MAX_BOXES_PER_HIT as u32 + 1]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut wire = hits_header(1);
        wire.extend(u32s(&[0, 1, u32::MAX]));
        assert_eq!(read(&wire).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // A hit of no length, or one past the end of the text.
        let mut wire = hits_header(1);
        wire.extend(u32s(&[0, 0, 0]));
        assert!(read(&wire).is_err());
        let mut wire = hits_header(1);
        wire.extend(u32s(&[99, 2, 0]));
        assert!(read(&wire).is_err());
        let mut wire = hits_header(1);
        wire.extend(u32s(&[u32::MAX, u32::MAX, 0]));
        assert!(read(&wire).is_err());
        // A total over the frame's budget: with a limit this small the sum
        // of 256 boxes a hit stays under it, so check the sum directly.
        let big = ReadLimits {
            max_find_hits: 10_000,
            max_text_chars: 1 << 20,
            ..small()
        };
        let mut wire = hits_header(0);
        wire.truncate(1 + 8 + 4);
        wire.extend(u32s(&[2000]));
        for _ in 0..2000 {
            wire.extend(u32s(&[0, 1, MAX_BOXES_PER_HIT as u32]));
            // (The boxes are not there: the sum is over before they are
            // needed once the budget runs out, but they are read first.)
            wire.extend(vec![0u8; MAX_BOXES_PER_HIT * 16]);
        }
        let error = read_answer_with(&mut wire.as_slice(), &big, |_| true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // And a search that ended with more matches than are allowed.
        let mut wire = vec![b'D'];
        wire.extend(7u64.to_le_bytes());
        wire.extend(u32s(&[5]));
        wire.push(0);
        assert!(read(&wire).is_err());
        let mut wire = vec![b'D'];
        wire.extend(7u64.to_le_bytes());
        wire.extend(u32s(&[4]));
        wire.push(2);
        assert!(read(&wire).is_err());
    }

    #[test]
    fn the_parent_is_asked_before_a_body_is_read() {
        // `accept` says no: the bodies are not even there.
        let mut asked = Vec::new();
        for header in [
            {
                let mut w = vec![b'T'];
                w.extend(u32s(&[1, 1, 1]));
                w
            },
            vec![b'L'],
            hits_header(1),
        ] {
            let error = read_answer_with(&mut header.as_slice(), &small(), |peek| {
                asked.push(*peek);
                false
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        assert_eq!(
            asked,
            vec![
                Peek::Text { page: 1 },
                Peek::Outline,
                Peek::FindHits { search: 7, page: 1 }
            ]
        );
    }

    #[test]
    fn the_child_fits_what_the_parser_gives_it() {
        let limits = Limits {
            max_text_chars: 4,
            max_outline_items: 2,
            max_outline_depth: 2,
            max_title_chars: 3,
            max_find_hits: 2,
            ..Limits::default()
        };
        // Text: cut at 4 chars, boxes the same number, bad boxes gone.
        let text = fit_text(
            PageText {
                page: 9,
                text: "añbcdef".into(),
                boxes: vec![Some(B), Some([f32::NAN, 0.0, 0.0, 0.0])],
            },
            3,
            4,
        );
        assert_eq!((text.page, text.text.as_str()), (3, "añbc"));
        assert_eq!(text.boxes, vec![Some(B), None, None, None]);
        // Outline: two items, depth at most 1, titles of 3 chars, a target
        // beyond the document gone.
        let items = (0..5)
            .map(|k| OutlineItem {
                depth: 9,
                title: format!("t\u{0}ítulo{k}"),
                target: Some(Target {
                    page: if k == 0 { 1 } else { 7 },
                    top: Some(f32::NAN),
                }),
                open: false,
            })
            .collect();
        let items = fit_outline(items, &limits, 2);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].depth, items[0].title.as_str()), (1, "tít"));
        assert_eq!(items[0].target, Some(Target { page: 1, top: None }));
        assert_eq!(items[1].target, None);
        // Hits: at most the allowance, on the right page, boxes bounded.
        let hit = |len| FindHit {
            page: 99,
            start: 0,
            len,
            boxes: vec![B; 400],
        };
        let hits = fit_hits(vec![hit(1), hit(0), hit(2), hit(3)], 5, 2, &limits);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.page == 5 && h.boxes.len() == 256));
        // Many hits share the frame's budget.
        let limits = Limits {
            max_find_hits: 10_000,
            max_text_chars: 1 << 20,
            ..Limits::default()
        };
        let hits = fit_hits((0..5000).map(|_| hit(1)).collect(), 0, 10_000, &limits);
        let boxes: usize = hits.iter().map(|h| h.boxes.len()).sum();
        assert!(boxes <= MAX_BOXES_PER_FRAME, "{boxes}");
        assert!(hits.iter().all(|h| !h.boxes.is_empty()));
    }
}
