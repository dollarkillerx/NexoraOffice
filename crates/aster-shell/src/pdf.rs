//! PDF tabs: a file opened by an [`aster_pdf::Worker`] and shown by a
//! [`PdfView`]. The worker renders the pages the view asks for; nothing about
//! the file is read on the window's thread.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aster_pdf::{
    Event, Exceeded, FindHit, Info, Limits, OpenError, OutlineItem, PageText, Program, Target,
    Worker,
};
use aster_surface::pdf_view::{FindMark, PdfStyle, PdfView, TextLayer, Zoom};

use crate::i18n::{self, Strings};
use crate::office::Key;
use crate::stats::group_digits;
use crate::theme;

/// Whether the shell opens this file as a PDF.
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub const EXTENSIONS: &[&str] = &["pdf"];

pub struct PdfTab {
    pub view: PdfView,
    pub path: PathBuf,
    /// Which worker's events are this tab's.
    pub token: u64,
    pub info: Info,
    worker: Worker,
    /// What was last asked of the worker, so the same list is not sent on
    /// every frame.
    asked: Asked,
    /// The pages whose text was last asked for.
    text_asked: Vec<usize>,
    /// The document's bookmarks, once the worker has sent them.
    pub outline: Option<Outline>,
    /// The find bar's state for this tab; the hits are in the view.
    pub find: PdfFind,
    /// The id of the last search started (ids only grow).
    searches: u64,
    /// The tab is in the background: no pixels are held and none are taken.
    suspended: bool,
    /// The worker gave up (or failed after opening): undrawn pages stay
    /// blank.
    stopped: bool,
}

/// The requests the worker has been sent and not answered yet.
#[derive(Default)]
struct Asked(Vec<(usize, f32)>);

impl Asked {
    /// Take note of `wanted`; false if it is what is already asked.
    fn send(&mut self, wanted: &[(usize, f32)]) -> bool {
        if self.0 == wanted {
            return false;
        }
        self.0 = wanted.to_vec();
        true
    }

    /// A rendering of `page` at `scale` arrived: that request is answered. A
    /// page wanted again later (another zoom) is a new request.
    fn delivered(&mut self, page: usize, scale: f32) {
        self.0
            .retain(|(p, s)| !(*p == page && (s / scale - 1.0).abs() < 1e-4));
    }

    /// Nothing more will come for `page`.
    fn forget(&mut self, page: usize) {
        self.0.retain(|(p, _)| *p != page);
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// The bookmarks of a document, with which of them are open. The list the
/// sidebar shows is the flat list without the children of closed items;
/// only that is ever put into the window's model.
#[derive(Debug, Default)]
pub struct Outline {
    items: Vec<OutlineItem>,
    open: Vec<bool>,
    /// Indices into `items` of the rows now visible, in order.
    rows: Vec<usize>,
}

/// One visible row of the outline.
#[derive(Debug, PartialEq)]
pub struct Row<'a> {
    pub index: usize,
    pub depth: u16,
    pub title: &'a str,
    pub expandable: bool,
    pub expanded: bool,
}

impl Outline {
    pub fn new(items: Vec<OutlineItem>) -> Self {
        let open = items.iter().map(|item| item.open).collect();
        let mut outline = Self {
            items,
            open,
            rows: Vec::new(),
        };
        outline.rebuild();
        outline
    }

    /// Whether item `index` has items below it.
    fn has_children(&self, index: usize) -> bool {
        self.items
            .get(index + 1)
            .is_some_and(|next| next.depth > self.items[index].depth)
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let mut i = 0;
        while i < self.items.len() {
            self.rows.push(i);
            let depth = self.items[i].depth;
            let closed = self.has_children(i) && !self.open[i];
            i += 1;
            if closed {
                while i < self.items.len() && self.items[i].depth > depth {
                    i += 1;
                }
            }
        }
    }

    /// The worker sent the outline again (it does after a child restart):
    /// the same bookmarks keep the open and closed state the user left;
    /// only different ones start from the file's flags.
    pub fn replace(&mut self, items: Vec<OutlineItem>) {
        let same = items.len() == self.items.len()
            && items
                .iter()
                .zip(&self.items)
                .all(|(a, b)| a.depth == b.depth && a.title == b.title && a.target == b.target);
        if !same {
            *self = Self::new(items);
        }
    }

    /// Whether the document has bookmarks at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The rows to show: items whose parents are all open.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(|&index| {
            let item = &self.items[index];
            let expandable = self.has_children(index);
            Row {
                index,
                depth: item.depth,
                title: &item.title,
                expandable,
                expanded: expandable && self.open[index],
            }
        })
    }

    /// The item shown in row `row` and where it goes.
    pub fn target_of_row(&self, row: usize) -> Option<Target> {
        self.items[*self.rows.get(row)?].target
    }

    /// Open or close the item in row `row`. False if it has no children.
    pub fn toggle_row(&mut self, row: usize) -> bool {
        let Some(&index) = self.rows.get(row) else {
            return false;
        };
        if !self.has_children(index) {
            return false;
        }
        self.open[index] = !self.open[index];
        self.rebuild();
        true
    }
}

/// The find bar of a PDF tab.
#[derive(Debug, Default)]
pub struct PdfFind {
    pub open: bool,
    pub query: String,
    pub match_case: bool,
    /// The search whose results are wanted; 0 when none runs or is shown.
    search: u64,
    /// The worker is still looking.
    pub busy: bool,
    /// The search stopped at the most hits it keeps.
    pub truncated: bool,
}

/// The place after (or before) `current` among `count` hits, wrapping
/// around; with no current hit, the first (or last).
pub fn step_hit(current: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(i), true) => (i.min(count - 1) + 1) % count,
        (Some(i), false) => (i.min(count - 1) + count - 1) % count,
    })
}

/// What the find bar says: nothing for no query, "No results", or "3 / 12"
/// (with "…" while more are coming, and "more than" when the search
/// stopped at its limit).
pub fn find_position(
    strings: &Strings,
    find: &PdfFind,
    hits: usize,
    current: Option<usize>,
) -> String {
    if find.query.is_empty() {
        return String::new();
    }
    if hits == 0 {
        return if find.busy {
            strings.find_counting.to_owned()
        } else {
            strings.find_none.to_owned()
        };
    }
    let mut count = group_digits(hits);
    if find.truncated {
        count = i18n::fill(strings.find_too_many, "count", &count);
    } else if find.busy {
        count.push('…');
    }
    let index = current.map_or_else(|| "–".to_owned(), |index| group_digits(index + 1));
    i18n::fill(
        &i18n::fill(strings.find_position, "index", &index),
        "count",
        &count,
    )
}

/// The notice that a search stopped at its limit.
pub fn truncated_notice(strings: &Strings, hits: usize) -> String {
    i18n::fill(strings.pdf_find_truncated, "count", &group_digits(hits))
}

/// Most rendered frames waiting for the window's thread, and their bytes. A
/// worker that is keeping up never has more than a few; beyond this the
/// oldest are dropped (the view asks for what it still needs).
const MAILBOX_FRAMES: usize = 64;
const MAILBOX_BYTES: usize = 256 << 20;
/// Most page failures kept (also never more than the page count).
const MAILBOX_FAILED: usize = 100_000;
/// Most page texts waiting, and their bytes (the newest is always kept).
const MAILBOX_TEXTS: usize = 32;
const MAILBOX_TEXT_BYTES: usize = 16 << 20;
/// Most outline items and search hits kept (what the worker's limits give).
const MAILBOX_OUTLINE: usize = 10_000;
const MAILBOX_HITS: usize = 10_000;
/// Most boxes kept of one hit (the view keeps no more).
const MAILBOX_HIT_BOXES: usize = 256;

/// Bytes a page's text takes while it waits.
fn text_bytes(text: &PageText) -> usize {
    text.text.len() + text.boxes.len() * std::mem::size_of::<Option<[f32; 4]>>()
}

/// Where a worker's events wait for the window's thread. They are coalesced:
/// one frame per page and scale (the newest), each failed page once, the
/// latest of each kind of control event. However much the worker (or a child
/// process behind it) sends, this holds a bounded amount, and only one
/// wake-up of the event loop is pending at a time.
#[derive(Default)]
pub struct Mailbox {
    state: Mutex<Inbox>,
}

#[derive(Default)]
struct Inbox {
    /// The page count, known once the file opened; pages before that, or
    /// beyond it, are not kept.
    pages: Option<usize>,
    next: u64,
    frames: BTreeMap<u64, aster_pdf::Rendered>,
    by_page: HashMap<(usize, u32), u64>,
    bytes: usize,
    failed: BTreeSet<usize>,
    texts: BTreeMap<u64, PageText>,
    text_by_page: HashMap<usize, u64>,
    text_size: usize,
    outline: Option<Vec<OutlineItem>>,
    /// The newest search heard of; the results of older ones are dropped.
    search: u64,
    hits: Vec<FindHit>,
    find_done: Option<(usize, bool)>,
    opened: Option<Event>,
    ended: Option<Event>,
    /// A wake-up has been posted and not yet answered.
    waking: bool,
}

impl Mailbox {
    /// Take an event in. True if the caller must now post a wake-up.
    pub fn push(&self, event: Event) -> bool {
        let mut inbox = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            Event::Opened(info) => {
                inbox.pages = Some(info.pages.len());
                inbox.opened = Some(Event::Opened(info));
            }
            Event::Failed(_) | Event::Stopped => inbox.ended = Some(event),
            Event::Text(text) => {
                if inbox.pages.is_none_or(|count| text.page >= count) {
                    return false;
                }
                if let Some(old) = inbox.text_by_page.remove(&text.page)
                    && let Some(old) = inbox.texts.remove(&old)
                {
                    inbox.text_size -= text_bytes(&old);
                }
                inbox.text_size += text_bytes(&text);
                let seq = inbox.next;
                inbox.next += 1;
                inbox.text_by_page.insert(text.page, seq);
                inbox.texts.insert(seq, text);
                while inbox.texts.len() > 1
                    && (inbox.texts.len() > MAILBOX_TEXTS || inbox.text_size > MAILBOX_TEXT_BYTES)
                {
                    let Some((_, old)) = inbox.texts.pop_first() else {
                        break;
                    };
                    inbox.text_size -= text_bytes(&old);
                    inbox.text_by_page.remove(&old.page);
                }
            }
            Event::Outline(mut items) => {
                if inbox.pages.is_none() {
                    return false;
                }
                items.truncate(MAILBOX_OUTLINE);
                inbox.outline = Some(items);
            }
            Event::FindHits { search, hits } => {
                if search < inbox.search {
                    return false;
                }
                if search > inbox.search {
                    inbox.search = search;
                    inbox.hits.clear();
                    inbox.find_done = None;
                }
                let room = MAILBOX_HITS.saturating_sub(inbox.hits.len());
                inbox
                    .hits
                    .extend(hits.into_iter().take(room).map(|mut hit| {
                        hit.boxes.truncate(MAILBOX_HIT_BOXES);
                        hit
                    }));
            }
            Event::FindDone {
                search,
                total,
                truncated,
            } => {
                if search < inbox.search {
                    return false;
                }
                if search > inbox.search {
                    inbox.search = search;
                    inbox.hits.clear();
                }
                inbox.find_done = Some((total, truncated));
            }
            Event::Page(frame) => {
                if inbox.pages.is_none_or(|count| frame.page >= count) {
                    return false;
                }
                let key = (frame.page, frame.scale.to_bits());
                if let Some(old) = inbox.by_page.remove(&key)
                    && let Some(old) = inbox.frames.remove(&old)
                {
                    inbox.bytes -= old.pixels.len();
                }
                inbox.bytes += frame.pixels.len();
                let seq = inbox.next;
                inbox.next += 1;
                inbox.by_page.insert(key, seq);
                inbox.frames.insert(seq, frame);
                while inbox.frames.len() > 1
                    && (inbox.frames.len() > MAILBOX_FRAMES || inbox.bytes > MAILBOX_BYTES)
                {
                    let Some((_, old)) = inbox.frames.pop_first() else {
                        break;
                    };
                    inbox.bytes -= old.pixels.len();
                    inbox.by_page.remove(&(old.page, old.scale.to_bits()));
                }
            }
            Event::PageFailed(page) => {
                let Some(count) = inbox.pages else {
                    return false;
                };
                if page < count && inbox.failed.len() < MAILBOX_FAILED {
                    inbox.failed.insert(page);
                }
            }
        }
        !std::mem::replace(&mut inbox.waking, true)
    }

    /// Everything waiting, in the order to handle it: the opening, frames
    /// oldest first, failed pages, the end. Answers the wake-up.
    pub fn drain(&self) -> Vec<Event> {
        let mut inbox = self.state.lock().unwrap_or_else(|e| e.into_inner());
        inbox.waking = false;
        let mut events = Vec::new();
        events.extend(inbox.opened.take());
        events.extend(inbox.outline.take().map(Event::Outline));
        let texts = std::mem::take(&mut inbox.texts);
        inbox.text_by_page.clear();
        inbox.text_size = 0;
        events.extend(texts.into_values().map(Event::Text));
        let frames = std::mem::take(&mut inbox.frames);
        inbox.by_page.clear();
        inbox.bytes = 0;
        events.extend(frames.into_values().map(Event::Page));
        events.extend(
            std::mem::take(&mut inbox.failed)
                .into_iter()
                .map(Event::PageFailed),
        );
        let search = inbox.search;
        let hits = std::mem::take(&mut inbox.hits);
        if !hits.is_empty() {
            events.push(Event::FindHits { search, hits });
        }
        if let Some((total, truncated)) = inbox.find_done.take() {
            events.push(Event::FindDone {
                search,
                total,
                truncated,
            });
        }
        events.extend(inbox.ended.take());
        events
    }

    /// Entries held: page texts, bytes of them, hits, whether an outline and
    /// a find's end wait, for tests.
    #[cfg(test)]
    fn held_text(&self) -> (usize, usize, usize, bool, bool) {
        let inbox = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            inbox.texts.len(),
            inbox.text_size,
            inbox.hits.len(),
            inbox.outline.is_some(),
            inbox.find_done.is_some(),
        )
    }

    /// Entries held (frames, failed pages, control events), for tests.
    #[cfg(test)]
    fn held(&self) -> (usize, usize, usize) {
        let inbox = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            inbox.frames.len(),
            inbox.failed.len(),
            usize::from(inbox.opened.is_some()) + usize::from(inbox.ended.is_some()),
        )
    }
}

impl PdfTab {
    pub fn new(
        path: PathBuf,
        token: u64,
        worker: Worker,
        info: Info,
        tokens: &theme::Tokens,
    ) -> Self {
        let mut view = PdfView::new(info.pages.iter().map(|p| (p.width, p.height)).collect());
        view.set_style(pdf_style(tokens));
        worker.want_outline();
        Self {
            view,
            path,
            token,
            info,
            worker,
            asked: Asked::default(),
            text_asked: Vec::new(),
            outline: None,
            find: PdfFind::default(),
            searches: 0,
            suspended: false,
            stopped: false,
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(pdf_style(tokens));
    }

    /// Ask the worker for the pages the view needs now, if that changed.
    pub fn request(&mut self) {
        if self.stopped || self.suspended {
            return;
        }
        let wanted = self.view.wanted();
        if self.asked.send(&wanted) {
            self.worker.want(wanted);
        }
        let text = self.view.wanted_text();
        if text != self.text_asked {
            self.text_asked.clone_from(&text);
            self.worker.want_text(text);
        }
    }

    /// The tab goes to the background: free the pixels, stop rendering.
    pub fn suspend(&mut self) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        // A search is not kept for a tab in the background.
        self.close_find();
        self.view.drop_pixels();
        self.worker.suspend();
        self.asked.clear();
        self.text_asked.clear();
    }

    /// The tab is in front again: the next `request` asks for the pages.
    pub fn resume(&mut self) {
        if !self.suspended {
            return;
        }
        self.suspended = false;
        self.worker.resume();
        self.asked.clear();
        self.text_asked.clear();
    }

    /// Whether the worker gave up on this file.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// A page from the worker. Returns whether the view changed.
    pub fn take(&mut self, event: Event) -> bool {
        match event {
            Event::Page(page) => {
                self.asked.delivered(page.page, page.scale);
                // Pages still on their way when the tab went away are dropped.
                !self.suspended
                    && self
                        .view
                        .insert(page.page, page.scale, page.width, page.height, page.pixels)
            }
            Event::PageFailed(page) => {
                self.asked.forget(page);
                self.view.mark_failed(page);
                true
            }
            // The worker has given up, or the file failed after it opened.
            Event::Failed(_) | Event::Stopped => {
                self.stopped = true;
                self.asked.clear();
                true
            }
            Event::Opened(_) => false,
            Event::Text(PageText { page, text, boxes }) => {
                self.view.set_text(page, TextLayer { text, boxes })
            }
            Event::Outline(items) => {
                match &mut self.outline {
                    Some(outline) => outline.replace(items),
                    None => self.outline = Some(Outline::new(items)),
                }
                false
            }
            Event::FindHits { search, hits } => {
                // Results of a search that was replaced or closed.
                if search != self.find.search {
                    return false;
                }
                let first = self.view.hit_count() == 0;
                self.view.add_find_hits(
                    hits.into_iter()
                        .map(|hit| FindMark {
                            page: hit.page,
                            boxes: hit.boxes,
                        })
                        .collect(),
                );
                if first && self.view.hit_count() > 0 {
                    self.view.set_current_hit(Some(0));
                    self.view.go_to_hit(0);
                }
                true
            }
            Event::FindDone {
                search, truncated, ..
            } => {
                if search == self.find.search {
                    self.find.busy = false;
                    self.find.truncated = truncated;
                }
                false
            }
        }
    }

    /// Open the find bar; a query left in it is searched again.
    pub fn open_find(&mut self) {
        self.find.open = true;
        if !self.find.query.is_empty() && self.find.search == 0 {
            self.restart_find();
        }
    }

    /// Close the find bar: the search stops and the marks go.
    pub fn close_find(&mut self) {
        let was = self.find.search != 0 || self.view.hit_count() > 0;
        self.find.open = false;
        self.find.search = 0;
        self.find.busy = false;
        self.find.truncated = false;
        if was {
            self.worker.cancel_find();
            self.view.clear_find();
        }
    }

    pub fn set_query(&mut self, query: String) {
        self.find.query = query;
        self.restart_find();
    }

    pub fn toggle_match_case(&mut self) {
        self.find.match_case = !self.find.match_case;
        self.restart_find();
    }

    /// Start a search for the query now in the bar, replacing any running.
    fn restart_find(&mut self) {
        self.view.clear_find();
        self.find.truncated = false;
        if self.find.query.is_empty() {
            self.find.search = 0;
            self.find.busy = false;
            self.worker.cancel_find();
            return;
        }
        self.searches += 1;
        self.find.search = self.searches;
        self.find.busy = true;
        let query = self
            .find
            .query
            .chars()
            .take(Limits::default().max_query_chars)
            .collect();
        self.worker.find(self.searches, query, self.find.match_case);
    }

    /// Go to the next or previous hit, wrapping around. False if none.
    pub fn find_step(&mut self, forward: bool) -> bool {
        let Some(next) = step_hit(self.view.current_hit(), self.view.hit_count(), forward) else {
            return false;
        };
        self.view.set_current_hit(Some(next));
        self.view.go_to_hit(next);
        true
    }

    /// The pointer went down on the pages (physical pixels): a thumbnail is
    /// gone to, or a selection starts (`clicks` 1 character, 2 word, 3 line).
    /// Returns whether the picture changed.
    pub fn press(&mut self, x: f64, y: f64, clicks: u8) -> bool {
        self.view.press(x, y) || self.view.press_text(x, y, clicks)
    }

    /// Go to the target of outline row `row`.
    pub fn go_to_row(&mut self, row: usize) -> bool {
        let Some(target) = self.outline.as_ref().and_then(|o| o.target_of_row(row)) else {
            return false;
        };
        self.view
            .go_to_point(target.page, target.top.unwrap_or(0.0));
        true
    }

    /// The zoom commands: steps up and down, 100 %, and fit width.
    pub fn zoom(&mut self, zoom_in: Option<bool>) -> bool {
        match zoom_in {
            Some(true) => self.view.zoom_in(),
            Some(false) => self.view.zoom_out(),
            None => self.view.set_zoom(Zoom::Percent(100.0)),
        }
    }

    /// "Fit width", or "fit page" when the width is fitted already.
    pub fn fit(&mut self) -> bool {
        let next = if self.view.zoom() == Zoom::FitWidth {
            Zoom::FitPage
        } else {
            Zoom::FitWidth
        };
        self.view.set_zoom(next)
    }
}

/// Start opening `path` in a child process; events arrive through
/// `events`, on the supervising thread. If this program's own path cannot be
/// found, the failure is reported through `events` and there is no worker.
pub fn open(
    path: PathBuf,
    password: String,
    mut events: impl FnMut(Event) + Send + 'static,
) -> Option<Worker> {
    match Program::current() {
        Ok(program) => Some(Worker::open(
            program,
            path,
            password,
            Limits::default(),
            events,
        )),
        Err(_) => {
            std::thread::spawn(move || events(Event::Failed(OpenError::Crashed)));
            None
        }
    }
}

pub fn pdf_style(tokens: &theme::Tokens) -> PdfStyle {
    let c = &tokens.colors;
    let opaque = |color: theme::Rgba, under: theme::Rgba| color.over(under).to_array();
    PdfStyle {
        background: opaque(c.desk, c.surface),
        strip: opaque(c.panel, c.surface),
        hairline: opaque(c.hairline, c.surface),
        secondary: opaque(c.text_secondary, c.panel),
        accent: c.accent.to_array(),
        // Pages are white in both themes, so the marks are the accent (the
        // lighter one on the dark theme), yellow and orange, translucent.
        selection: match tokens.mode {
            theme::ThemeMode::Light => c.accent.with_alpha(0.35),
            theme::ThemeMode::Dark => c.caret.with_alpha(0.42),
        }
        .to_array(),
        hit: theme::Rgba::rgb(0xffe135).with_alpha(0.55).to_array(),
        current_hit: theme::Rgba::rgb(0xff8c00).with_alpha(0.65).to_array(),
    }
}

/// Keys on a PDF: arrows and the page keys scroll, Home and End go to the
/// first and last page, the space bar pages down (up with Shift).
pub fn pdf_key(view: &mut PdfView, key: Key, shift: bool, scale: f64) -> bool {
    let line = aster_surface::pdf_view::LINE_STEP * scale;
    match key {
        Key::Up => view.wheel(f64::MAX, 0.0, -line),
        Key::Down => view.wheel(f64::MAX, 0.0, line),
        Key::Left => view.wheel(f64::MAX, -line, 0.0),
        Key::Right => view.wheel(f64::MAX, line, 0.0),
        Key::PageUp => view.page_down(false),
        Key::PageDown | Key::Enter | Key::Tab => view.page_down(true),
        Key::Space => view.page_down(!shift),
        Key::Home => view.first_page(),
        Key::End => view.last_page(),
    }
}

/// What to tell the user when a file does not open (not for a password
/// that is needed or wrong: those are asked for).
pub fn error_message(error: OpenError, strings: &Strings) -> String {
    match error {
        OpenError::Damaged | OpenError::NeedsPassword | OpenError::WrongPassword => {
            strings.pdf_error_damaged.to_owned()
        }
        OpenError::UnsupportedEncryption => strings.pdf_error_encryption.to_owned(),
        OpenError::Crashed => strings.pdf_error_crashed.to_owned(),
        OpenError::Unchecked => strings.pdf_error_unchecked.to_owned(),
        OpenError::Unreadable(kind) => i18n::fill(
            strings.pdf_error_unreadable,
            "reason",
            &std::io::Error::from(kind).to_string(),
        ),
        OpenError::TooLarge(what) => {
            let what = match what {
                Exceeded::File => strings.pdf_limit_file,
                Exceeded::Objects => strings.pdf_limit_objects,
                Exceeded::Pages => strings.pdf_limit_pages,
                Exceeded::Stream => strings.pdf_limit_stream,
                Exceeded::Image => strings.pdf_limit_image,
                Exceeded::Memory => strings.pdf_limit_memory,
                Exceeded::Time => strings.pdf_limit_time,
            };
            i18n::fill(strings.pdf_error_too_large, "what", what)
        }
    }
}

/// A page number typed into "Go to page": 1-based, within the document.
/// Returns the 0-based index.
pub fn parse_page(text: &str, count: usize) -> Option<usize> {
    let number: usize = text.trim().parse().ok()?;
    (1..=count).contains(&number).then(|| number - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_files_by_extension() {
        assert!(is_pdf(Path::new("报告.PDF")));
        assert!(is_pdf(Path::new("/a/b.pdf")));
        assert!(!is_pdf(Path::new("pdf")));
        assert!(!is_pdf(Path::new("a.pdfx")));
    }

    #[test]
    fn requests_are_sent_once_and_again_when_wanted_again() {
        let mut asked = Asked::default();
        let wanted = vec![(0, 1.0), (1, 1.0), (0, 0.1)];
        assert!(asked.send(&wanted));
        assert!(!asked.send(&wanted));
        // Page 0 arrives at 1.0: the list still wanted minus it is not new.
        asked.delivered(0, 1.0);
        let rest = vec![(1, 1.0), (0, 0.1)];
        assert!(!asked.send(&rest));
        asked.delivered(1, 1.0);
        asked.delivered(0, 0.1);
        assert!(!asked.send(&[]));
        // Page 0 wanted again at another zoom: a new request.
        assert!(asked.send(&[(0, 2.0)]));
        // A page that failed is not waited for.
        asked.forget(0);
        assert!(!asked.send(&[]));
        // After a pause everything is sent again.
        asked.send(&[(3, 1.0)]);
        asked.clear();
        assert!(asked.send(&[(3, 1.0)]));
    }

    fn opened(pages: usize) -> Event {
        Event::Opened(Info {
            pages: vec![
                aster_pdf::PageSize {
                    width: 612.0,
                    height: 792.0
                };
                pages
            ],
            encrypted: false,
        })
    }

    fn frame(page: usize, scale: f32, fill: u8) -> Event {
        Event::Page(aster_pdf::Rendered {
            page,
            scale,
            width: 2,
            height: 2,
            pixels: vec![fill; 16],
        })
    }

    #[test]
    fn the_mailbox_stays_bounded_under_a_flood() {
        let mailbox = Mailbox::default();
        // Before the opening nothing but control events is kept.
        assert!(!mailbox.push(Event::PageFailed(0)));
        assert!(!mailbox.push(frame(0, 1.0, 0)));
        assert_eq!(mailbox.held(), (0, 0, 0));
        assert!(mailbox.push(opened(50)));
        let mut wakes = 0;
        for i in 0..1_000_000usize {
            // Half of them for pages that do not exist.
            wakes += usize::from(mailbox.push(Event::PageFailed(i % 100)));
        }
        for i in 0..10_000usize {
            wakes += usize::from(mailbox.push(frame(3, 1.0, (i % 251) as u8)));
        }
        // One wake-up was pending since the opening; no more were asked for.
        assert_eq!(wakes, 0);
        let (frames, failed, control) = mailbox.held();
        assert_eq!((frames, failed, control), (1, 50, 1));
        let events = mailbox.drain();
        assert!(matches!(events[0], Event::Opened(_)));
        let Event::Page(newest) = &events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(newest.pixels[0], (9_999 % 251) as u8);
        assert_eq!(events.len(), 2 + 50);
        assert!(
            events[2..]
                .iter()
                .all(|e| matches!(e, Event::PageFailed(p) if *p < 50))
        );
        // After the drain the next event asks for a wake-up again.
        assert_eq!(mailbox.held(), (0, 0, 0));
        assert!(mailbox.push(Event::Stopped));
    }

    #[test]
    fn many_pages_and_scales_are_capped_and_the_newest_stay() {
        let mailbox = Mailbox::default();
        mailbox.push(opened(10_000));
        for i in 0..5_000usize {
            mailbox.push(frame(i, 1.0, 1));
            mailbox.push(frame(i, 2.0 + i as f32, 2));
        }
        let (frames, ..) = mailbox.held();
        assert!(frames <= MAILBOX_FRAMES, "{frames}");
        let events = mailbox.drain();
        let last = events.iter().rev().find_map(|e| match e {
            Event::Page(f) => Some(f.page),
            _ => None,
        });
        assert_eq!(last, Some(4_999));
        // The same page at two scales (main and thumbnail) both arrive.
        mailbox.push(frame(1, 1.0, 1));
        mailbox.push(frame(1, 0.1, 2));
        assert_eq!(mailbox.drain().len(), 2);
        // A big frame is dropped for the bytes, the newest is always kept.
        let big = |page| {
            Event::Page(aster_pdf::Rendered {
                page,
                scale: 1.0,
                width: 4096,
                height: 4096,
                pixels: vec![0; 4096 * 4096 * 4],
            })
        };
        for page in 0..8 {
            mailbox.push(big(page));
        }
        let (frames, ..) = mailbox.held();
        assert!(frames <= MAILBOX_BYTES / (4096 * 4096 * 4), "{frames}");
        assert!(matches!(mailbox.drain().last(), Some(Event::Page(f)) if f.page == 7));
    }

    #[test]
    fn stopped_text_exists_in_every_language() {
        for language in crate::i18n::Language::ALL {
            let text = language.strings().pdf_stopped;
            assert!(!text.is_empty() && !text.contains('{'), "{text}");
        }
    }

    fn page_text(page: usize, chars: usize) -> Event {
        Event::Text(PageText {
            page,
            text: "x".repeat(chars),
            boxes: vec![Some([0.0; 4]); chars],
        })
    }

    fn hit(page: usize, start: u32) -> FindHit {
        FindHit {
            page,
            start,
            len: 1,
            boxes: vec![[0.0; 4]; 1000],
        }
    }

    fn item(depth: u16, title: &str, open: bool, page: Option<usize>) -> OutlineItem {
        OutlineItem {
            depth,
            title: title.to_owned(),
            target: page.map(|page| Target { page, top: None }),
            open,
        }
    }

    #[test]
    fn page_texts_are_coalesced_and_bounded() {
        let mailbox = Mailbox::default();
        assert!(!mailbox.push(page_text(0, 10)), "before the opening");
        mailbox.push(opened(100_000));
        // The newest text of a page replaces the older.
        for _ in 0..1000 {
            mailbox.push(page_text(5, 100));
        }
        assert_eq!(mailbox.held_text().0, 1);
        // Many pages: only the newest few stay.
        for page in 0..50_000 {
            mailbox.push(page_text(page, 10));
        }
        let (texts, ..) = mailbox.held_text();
        assert!(texts <= MAILBOX_TEXTS, "{texts}");
        // Big pages are dropped for their bytes; the newest stays.
        for page in 0..100 {
            mailbox.push(page_text(page, 1_000_000));
        }
        let (texts, bytes, ..) = mailbox.held_text();
        assert!(
            texts >= 1 && bytes <= 2 * MAILBOX_TEXT_BYTES,
            "{texts} {bytes}"
        );
        // A page that does not exist is not kept.
        mailbox.drain();
        mailbox.push(page_text(100_000, 1));
        assert_eq!(mailbox.held_text().0, 0);
        mailbox.push(page_text(7, 3));
        mailbox.push(page_text(8, 3));
        let events = mailbox.drain();
        assert!(
            matches!(&events[..], [Event::Text(a), Event::Text(b)] if a.page == 7 && b.page == 8)
        );
        assert_eq!(mailbox.held_text(), (0, 0, 0, false, false));
    }

    #[test]
    fn the_latest_outline_is_kept_and_capped() {
        let mailbox = Mailbox::default();
        assert!(!mailbox.push(Event::Outline(vec![item(0, "a", false, None)])));
        mailbox.push(opened(3));
        for n in 0..100 {
            mailbox.push(Event::Outline(vec![item(0, &n.to_string(), false, None)]));
        }
        let big = (0..50_000)
            .map(|n| item(0, &n.to_string(), false, None))
            .collect();
        mailbox.push(Event::Outline(big));
        assert!(mailbox.held_text().3);
        let events = mailbox.drain();
        let outlines: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Outline(items) => Some(items.len()),
                _ => None,
            })
            .collect();
        assert_eq!(outlines, vec![MAILBOX_OUTLINE]);
    }

    #[test]
    fn find_hits_of_old_searches_are_dropped_and_the_rest_bounded() {
        let mailbox = Mailbox::default();
        mailbox.push(opened(10));
        for _ in 0..100 {
            mailbox.push(Event::FindHits {
                search: 1,
                hits: (0..1000).map(|n| hit(0, n)).collect(),
            });
        }
        let (.., hits, _, _) = mailbox.held_text();
        assert_eq!(hits, MAILBOX_HITS);
        mailbox.push(Event::FindDone {
            search: 1,
            total: 5,
            truncated: true,
        });
        // A newer search drops what the older one left.
        mailbox.push(Event::FindHits {
            search: 2,
            hits: vec![hit(1, 0), hit(1, 4)],
        });
        assert_eq!(mailbox.held_text().2, 2);
        assert!(!mailbox.held_text().4);
        // Late results of the older one are not kept.
        mailbox.push(Event::FindHits {
            search: 1,
            hits: vec![hit(0, 0)],
        });
        mailbox.push(Event::FindDone {
            search: 1,
            total: 1,
            truncated: false,
        });
        assert_eq!(mailbox.held_text().2, 2);
        assert!(!mailbox.held_text().4);
        mailbox.push(Event::FindDone {
            search: 2,
            total: 2,
            truncated: false,
        });
        let events = mailbox.drain();
        match &events[1..] {
            [
                Event::FindHits { search: 2, hits },
                Event::FindDone {
                    search: 2,
                    total: 2,
                    truncated: false,
                },
            ] => {
                assert_eq!(hits.len(), 2);
                // Boxes are capped.
                assert!(hits.iter().all(|h| h.boxes.len() == MAILBOX_HIT_BOXES));
            }
            other => panic!("{} events", other.len()),
        }
        // Still dropped after the drain.
        mailbox.push(Event::FindHits {
            search: 1,
            hits: vec![hit(0, 0)],
        });
        assert_eq!(mailbox.held_text().2, 0);
    }

    #[test]
    fn a_flood_of_every_kind_keeps_the_mailbox_small() {
        let mailbox = Mailbox::default();
        mailbox.push(opened(20));
        for n in 0..200_000u64 {
            mailbox.push(page_text((n % 20) as usize, 5));
            mailbox.push(Event::FindHits {
                search: n / 1000,
                hits: vec![hit(0, 0)],
            });
            mailbox.push(Event::FindDone {
                search: n / 1000,
                total: 1,
                truncated: false,
            });
        }
        let (texts, _, hits, _, _) = mailbox.held_text();
        assert!(texts <= 20 && hits <= 1000, "{texts} {hits}");
    }

    #[test]
    fn the_outline_shows_only_what_its_parents_open() {
        let mut outline = Outline::new(vec![
            item(0, "One", true, Some(0)),
            item(1, "One.1", false, Some(1)),
            item(1, "One.2", true, None),
            item(2, "One.2.1", false, Some(3)),
            item(0, "Two", false, Some(4)),
            item(1, "Two.1", false, Some(5)),
            item(0, "Three", false, None),
        ]);
        let titles = |o: &Outline| o.rows().map(|r| r.title.to_owned()).collect::<Vec<_>>();
        // "Two" is closed (the file says so), its child hidden.
        assert_eq!(
            titles(&outline),
            ["One", "One.1", "One.2", "One.2.1", "Two", "Three"]
        );
        let rows: Vec<_> = outline.rows().collect();
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].expandable && rows[0].expanded);
        assert!(!rows[1].expandable);
        assert_eq!(rows[3].depth, 2);
        assert!(rows[4].expandable && !rows[4].expanded);
        assert!(!rows[5].expandable);
        // Closing "One" hides its whole subtree.
        assert!(outline.toggle_row(0));
        assert_eq!(titles(&outline), ["One", "Two", "Three"]);
        // Opening "Two" (now row 1) shows its child.
        assert!(outline.toggle_row(1));
        assert_eq!(titles(&outline), ["One", "Two", "Two.1", "Three"]);
        // A leaf does not toggle; a row that is not there neither.
        assert!(!outline.toggle_row(2));
        assert!(!outline.toggle_row(40));
        // Rows go where their items go; items without a target nowhere.
        assert_eq!(outline.target_of_row(1).map(|t| t.page), Some(4));
        assert_eq!(outline.target_of_row(3), None);
        assert_eq!(outline.target_of_row(9), None);
        assert!(!Outline::new(Vec::new()).rows().any(|_| true));
        assert!(Outline::new(Vec::new()).is_empty());
    }

    #[test]
    fn toggles_survive_refreshes_and_rows_map_to_what_is_shown() {
        let items = || {
            vec![
                item(0, "Chapter One", true, Some(0)),
                item(1, "Lower part", false, Some(0)),
                item(0, "Chapter Two", false, Some(1)),
                item(1, "Section 2.1", false, Some(1)),
                item(1, "Website", false, None),
                item(0, "Third", false, Some(2)),
            ]
        };
        let mut outline = Outline::new(items());
        let shown = |o: &Outline| o.rows().map(|r| r.title.to_owned()).collect::<Vec<_>>();
        assert_eq!(
            shown(&outline),
            ["Chapter One", "Lower part", "Chapter Two", "Third"]
        );
        assert!(outline.toggle_row(2));
        // The worker sends the same outline again, and the view refreshes.
        outline.replace(items());
        outline.replace(items());
        assert_eq!(
            shown(&outline),
            [
                "Chapter One",
                "Lower part",
                "Chapter Two",
                "Section 2.1",
                "Website",
                "Third"
            ]
        );
        // The row shown as "Section 2.1" goes to its page, and its parent
        // is still open afterwards.
        assert_eq!(outline.target_of_row(3).map(|t| t.page), Some(1));
        assert!(outline.rows().nth(2).unwrap().expanded);
        // A bookmark with no target (a web link) goes nowhere.
        assert_eq!(outline.target_of_row(4), None);
        // A different outline starts from the file's flags.
        outline.replace(vec![item(0, "Other", false, None)]);
        assert_eq!(shown(&outline), ["Other"]);
    }

    #[test]
    fn an_outline_of_ten_thousand_items_is_quick() {
        let start = std::time::Instant::now();
        let mut items = Vec::new();
        for chapter in 0..100u32 {
            items.push(item(0, &format!("Chapter {chapter}"), false, Some(0)));
            for section in 0..99u32 {
                items.push(item(
                    1,
                    &format!("Section {chapter}.{section}"),
                    false,
                    None,
                ));
            }
        }
        assert_eq!(items.len(), 10_000);
        let mut outline = Outline::new(items);
        // Collapsed: only the chapters are rows.
        assert_eq!(outline.rows().count(), 100);
        for _ in 0..100 {
            outline.toggle_row(0);
            outline.toggle_row(0);
        }
        // Everything open.
        for chapter in (0..100).rev() {
            let row = outline
                .rows()
                .position(|r| r.index == chapter * 100)
                .unwrap();
            outline.toggle_row(row);
        }
        assert_eq!(outline.rows().count(), 10_000);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn stepping_through_hits_wraps_around() {
        assert_eq!(step_hit(None, 0, true), None);
        assert_eq!(step_hit(Some(3), 0, false), None);
        assert_eq!(step_hit(None, 5, true), Some(0));
        assert_eq!(step_hit(None, 5, false), Some(4));
        assert_eq!(step_hit(Some(0), 5, true), Some(1));
        assert_eq!(step_hit(Some(4), 5, true), Some(0));
        assert_eq!(step_hit(Some(0), 5, false), Some(4));
        assert_eq!(step_hit(Some(3), 5, false), Some(2));
        assert_eq!(step_hit(Some(0), 1, true), Some(0));
        assert_eq!(step_hit(Some(0), 1, false), Some(0));
        // A current hit past the end (the list shrank) does not panic.
        assert_eq!(step_hit(Some(9), 5, true), Some(0));
    }

    #[test]
    fn the_find_bar_says_where_the_hits_are_in_every_language() {
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            let mut find = PdfFind::default();
            assert_eq!(find_position(strings, &find, 0, None), "");
            find.query = "a".to_owned();
            find.busy = true;
            assert_eq!(
                find_position(strings, &find, 0, None),
                strings.find_counting
            );
            find.busy = false;
            assert_eq!(find_position(strings, &find, 0, None), strings.find_none);
            let text = find_position(strings, &find, 12, Some(2));
            assert!(text.contains('3') && text.contains("12"), "{text}");
            find.busy = true;
            assert!(find_position(strings, &find, 12, Some(2)).contains("12…"));
            find.busy = false;
            find.truncated = true;
            let text = find_position(strings, &find, 10_000, Some(0));
            assert!(text.contains("10,000") && !text.contains('{'), "{text}");
            let notice = truncated_notice(strings, 10_000);
            assert_eq!(notice.matches("10,000").count(), 2, "{notice}");
            assert!(!notice.contains('{'));
        }
    }

    #[test]
    fn the_new_pdf_texts_exist_in_every_language() {
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            for text in [strings.pdf_outline_none, strings.pdf_no_text_page] {
                assert!(text.chars().count() > 8 && !text.contains('{'), "{text}");
            }
            assert_eq!(strings.pdf_find_truncated.matches("{count}").count(), 2);
        }
        assert_eq!(
            crate::i18n::EN.pdf_outline_none,
            "This PDF has no outline (bookmarks)."
        );
    }

    #[test]
    fn selection_and_hit_colours_are_translucent_in_both_themes() {
        for mode in [theme::ThemeMode::Light, theme::ThemeMode::Dark] {
            let style = pdf_style(&theme::Tokens::of(mode));
            for colour in [style.selection, style.hit, style.current_hit] {
                assert!(colour[3] > 40 && colour[3] < 220, "{mode:?} {colour:?}");
            }
            assert_ne!(style.hit, style.current_hit);
        }
    }

    #[test]
    fn page_numbers() {
        assert_eq!(parse_page(" 3 ", 10), Some(2));
        assert_eq!(parse_page("1", 1), Some(0));
        assert_eq!(parse_page("0", 10), None);
        assert_eq!(parse_page("11", 10), None);
        assert_eq!(parse_page("二", 10), None);
    }

    #[test]
    fn every_error_has_words_in_every_language() {
        let errors = [
            OpenError::Damaged,
            OpenError::Crashed,
            OpenError::Unchecked,
            OpenError::UnsupportedEncryption,
            OpenError::Unreadable(std::io::ErrorKind::PermissionDenied),
            OpenError::TooLarge(Exceeded::File),
            OpenError::TooLarge(Exceeded::Objects),
            OpenError::TooLarge(Exceeded::Pages),
            OpenError::TooLarge(Exceeded::Stream),
            OpenError::TooLarge(Exceeded::Image),
            OpenError::TooLarge(Exceeded::Memory),
            OpenError::TooLarge(Exceeded::Time),
        ];
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            for error in errors {
                let message = error_message(error, strings);
                assert!(!message.is_empty() && !message.contains('{'), "{message}");
            }
        }
    }

    #[test]
    fn keys_move_the_view() {
        let mut view = PdfView::new(vec![(612.0, 792.0); 10]);
        view.resize(1000, 800, 1.0);
        assert!(pdf_key(&mut view, Key::Down, false, 1.0));
        assert!(pdf_key(&mut view, Key::End, false, 1.0));
        assert_eq!(view.current(), 9);
        assert!(pdf_key(&mut view, Key::Home, false, 1.0));
        assert_eq!(view.current(), 0);
        assert!(pdf_key(&mut view, Key::Space, false, 1.0));
        assert!(pdf_key(&mut view, Key::Space, true, 1.0));
    }
}
