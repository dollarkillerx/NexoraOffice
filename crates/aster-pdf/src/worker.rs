//! The parent's side of a PDF child process: starting it, passing on what
//! is wanted, reporting what comes back, and enforcing the time limits.
//!
//! One supervising thread per document owns the child. A page that takes
//! longer than `page_time` gets the child killed; a child that ends while a
//! page renders (killed, out of memory, or after a panic) is started again,
//! and that page is reported as failed and never asked for again. Dropping
//! the [`Worker`] kills the child at once.
//!
//! The parent does not trust the child: a page that is larger than what was
//! asked for, or that was not asked for, ends the child like a crash, before
//! its pixels are read. So does any answer that does not answer an open
//! request (see [`Ledger`]): a page that does not exist, one answered twice,
//! a start nobody asked for. Requests go to the child through a thread of
//! their own with room for only the newest, so the supervisor never waits on
//! a full pipe. Answers reach the supervisor through a [`Gate`] that lets a
//! couple of them wait at most, so a child that is faster than the
//! application is slowed down by its pipe instead of filling the memory; and
//! closing the tab or sending the child away is a flag that the supervisor
//! and the reading thread look at first, never a message behind answers.
//!
//! All the children of a process live in one [`Pool`]: at most
//! `max_alive` at a time, each with an equal share of the memory budget.
//! A tab in the background ([`Worker::suspend`]) has its child ended after a
//! while, or at once when another needs the place, and started again by
//! [`Worker::resume`] or the next request.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufReader, ErrorKind};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::child::{self, FromChild, ToChild};
use crate::{Event, Exceeded, Info, Limits, OpenError, PageSize, Rendered};

/// How to start a child: an executable and the arguments that come before
/// [`child::SUBCOMMAND`]'s own (usually none).
#[derive(Debug, Clone)]
pub struct Program {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
}

impl Program {
    /// The running executable, which must call [`child::run_if_child`] at
    /// the start of its `main`.
    pub fn current() -> std::io::Result<Program> {
        Ok(Program {
            executable: std::env::current_exe()?,
            arguments: Vec::new(),
        })
    }
}

/// Times a child is started again after it ended with pages to render. The
/// count starts over after a page has been drawn.
const MAX_RESTARTS: u32 = 8;
/// How often the supervisor looks at the clock.
const TICK: Duration = Duration::from_millis(100);
/// Children alive at once, and how long one stays in the background.
const POOL_CHILDREN: usize = 2;
const POOL_BACKGROUND: Duration = Duration::from_secs(60);
/// A request names at most this many pages (the child refuses more).
const MAX_WANTED: usize = 4096;
/// Answers that wait for the supervisor at most (plus the one being read).
const MAX_IN_FLIGHT: usize = 2;
/// How long a request can be answered after it was last sent: the child may
/// be a long way into an older request while newer ones replace it.
const ANSWER_WINDOW: Duration = Duration::from_secs(60);
/// Requests that may be open at once, whatever their age.
const MAX_OPEN: usize = 65_536;

enum Message {
    Answer(u64, FromChild, Permit),
    /// The child's output ended (it exited or was killed), or it sent
    /// something that cannot be right (the flag).
    Ended(u64, bool),
    Want(Vec<(usize, f32)>),
    Suspend,
    Resume,
    /// The pool asks for this worker's child (see [`Pool::try_acquire`]).
    Evict,
    Close,
}

/// The children of a process: how many may live at once, and who goes when
/// there is no room. The application has one ([`Pool::global`]); tests make
/// their own.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    max_alive: usize,
    background_time: Duration,
    state: Mutex<PoolState>,
}

#[derive(Default)]
struct PoolState {
    next_id: u64,
    clock: u64,
    entries: HashMap<u64, Entry>,
}

struct Entry {
    background: bool,
    last_active: u64,
    /// Has a child, or is starting one.
    alive: bool,
    /// The child has opened the file; only then can it be sent away.
    evictable: bool,
    /// Asked to give its child up.
    evicting: bool,
    /// The same, for threads that must not take the lock.
    evict_flag: Arc<AtomicBool>,
    notify: Sender<Message>,
}

impl Pool {
    /// A pool of at most `max_alive` children, in which a child in the
    /// background ends after `background_time`. Each child gets
    /// `max_memory_bytes / max_alive` of its worker's limit.
    pub fn new(max_alive: usize, background_time: Duration) -> Pool {
        Pool {
            inner: Arc::new(PoolInner {
                max_alive: max_alive.max(1),
                background_time,
                state: Mutex::new(PoolState::default()),
            }),
        }
    }

    /// The pool of this process: two children, a minute in the background.
    pub fn global() -> Pool {
        static GLOBAL: OnceLock<Pool> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Pool::new(POOL_CHILDREN, POOL_BACKGROUND))
            .clone()
    }

    fn state(&self) -> MutexGuard<'_, PoolState> {
        // Nothing here panics while the lock is held.
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn register(&self, notify: Sender<Message>) -> (u64, Arc<AtomicBool>) {
        let mut state = self.state();
        state.next_id += 1;
        state.clock += 1;
        let id = state.next_id;
        let last_active = state.clock;
        let evict_flag = Arc::new(AtomicBool::new(false));
        state.entries.insert(
            id,
            Entry {
                background: false,
                last_active,
                alive: false,
                evictable: false,
                evicting: false,
                evict_flag: evict_flag.clone(),
                notify,
            },
        );
        (id, evict_flag)
    }

    fn remove(&self, id: u64) {
        self.state().entries.remove(&id);
    }

    fn touch(&self, id: u64) {
        let mut state = self.state();
        state.clock += 1;
        let now = state.clock;
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.last_active = now;
        }
    }

    fn set_background(&self, id: u64, background: bool) {
        let mut state = self.state();
        state.clock += 1;
        let now = state.clock;
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.background = background;
            if !background {
                entry.last_active = now;
            }
        }
    }

    /// Take a place for a child. When none is free, ask the least recently
    /// active worker in the background (else the least recently active one)
    /// to give up its child, and say no; the caller asks again later.
    fn try_acquire(&self, id: u64) -> bool {
        let mut state = self.state();
        let alive = state.entries.values().filter(|e| e.alive).count();
        if alive < self.inner.max_alive {
            if let Some(entry) = state.entries.get_mut(&id) {
                entry.alive = true;
                entry.evictable = false;
                entry.evicting = false;
                entry.evict_flag.store(false, Ordering::SeqCst);
            }
            return true;
        }
        let victim = state
            .entries
            .iter()
            .filter(|(other, e)| **other != id && e.alive && e.evictable && !e.evicting)
            .min_by_key(|(_, e)| (!e.background, e.last_active))
            .map(|(other, _)| *other);
        if let Some(entry) = victim.and_then(|v| state.entries.get_mut(&v)) {
            entry.evicting = true;
            entry.evict_flag.store(true, Ordering::SeqCst);
            let _ = entry.notify.send(Message::Evict);
        }
        false
    }

    /// The child opened the file: it may be sent away now.
    fn opened(&self, id: u64) {
        if let Some(entry) = self.state().entries.get_mut(&id) {
            entry.evictable = true;
        }
    }

    /// Was this worker asked to give up its child?
    fn is_evicting(&self, id: u64) -> bool {
        self.state().entries.get(&id).is_some_and(|e| e.evicting)
    }

    fn release(&self, id: u64) {
        if let Some(entry) = self.state().entries.get_mut(&id) {
            entry.alive = false;
            entry.evictable = false;
            entry.evicting = false;
            entry.evict_flag.store(false, Ordering::SeqCst);
        }
    }
}

/// A PDF file opened in a child process. Dropping it ends the process.
pub struct Worker {
    messages: Sender<Message>,
    pid: Arc<AtomicU32>,
    /// Set before the close message, which may wait behind other messages.
    closing: Arc<AtomicBool>,
    pool: Pool,
    id: u64,
}

impl Worker {
    /// Open the file at `path` in a child process started from `program`,
    /// in the process-wide [`Pool`]. `events` is called on the supervising
    /// thread.
    pub fn open(
        program: Program,
        path: PathBuf,
        password: String,
        limits: Limits,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        Worker::open_in(&Pool::global(), program, path, password, limits, events)
    }

    /// Like [`Worker::open`], in a pool of its own choosing.
    pub fn open_in(
        pool: &Pool,
        program: Program,
        path: PathBuf,
        password: String,
        limits: Limits,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        let (messages, inbox) = mpsc::channel();
        let pid = Arc::new(AtomicU32::new(0));
        let (id, evict) = pool.register(messages.clone());
        let closing = Arc::new(AtomicBool::new(false));
        let supervisor = Supervisor {
            program,
            path,
            password,
            limits,
            messages: messages.clone(),
            pid: pid.clone(),
            events: Box::new(events),
            pool: pool.clone(),
            id,
            flags: Flags {
                closing: closing.clone(),
                evict,
            },
        };
        let spawned = std::thread::Builder::new()
            .name("pdf-supervisor".into())
            .spawn(move || supervisor.run(inbox));
        if spawned.is_err() {
            // Without a thread nothing is reported; the tab stays as it is.
            pool.remove(id);
        }
        Worker {
            messages,
            pid,
            closing,
            pool: pool.clone(),
            id,
        }
    }

    /// Render these pages, in this order, replacing what was asked before.
    pub fn want(&self, pages: Vec<(usize, f32)>) {
        self.pool.touch(self.id);
        let _ = self.messages.send(Message::Want(pages));
    }

    /// The tab went to the background: its child process is ended after a
    /// while (or sooner, if another needs the place) to free its memory.
    /// Nothing is reported while it is ended.
    pub fn suspend(&self) {
        self.pool.set_background(self.id, true);
        let _ = self.messages.send(Message::Suspend);
    }

    /// The tab is in front again: start the child process again if it was
    /// ended, and render what is wanted.
    pub fn resume(&self) {
        self.pool.set_background(self.id, false);
        let _ = self.messages.send(Message::Resume);
    }

    /// The child process running now, 0 if none (for tests).
    pub fn child_id(&self) -> u32 {
        self.pid.load(Ordering::Relaxed)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.messages.send(Message::Close);
    }
}

struct Supervisor {
    program: Program,
    path: PathBuf,
    password: String,
    limits: Limits,
    messages: Sender<Message>,
    pid: Arc<AtomicU32>,
    events: Box<dyn FnMut(Event) + Send>,
    pool: Pool,
    id: u64,
    flags: Flags,
}

/// What ends a child at once, whatever is queued: the tab closed, or the
/// pool wants the place.
#[derive(Clone)]
struct Flags {
    closing: Arc<AtomicBool>,
    evict: Arc<AtomicBool>,
}

impl Flags {
    fn raised(&self) -> bool {
        self.closing.load(Ordering::SeqCst) || self.evict.load(Ordering::SeqCst)
    }
}

/// Room for [`MAX_IN_FLIGHT`] answers between the reading thread and the
/// supervisor. The reading thread waits for a [`Permit`] before it passes an
/// answer on, and so stops reading: the child waits on its full pipe.
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

struct GateState {
    used: usize,
    closed: bool,
}

/// A place in the [`Gate`], given back when dropped (wherever the answer
/// ends up).
struct Permit(Arc<Gate>);

impl Gate {
    fn new() -> Arc<Gate> {
        Arc::new(Gate {
            state: Mutex::new(GateState {
                used: 0,
                closed: false,
            }),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait for a place; `None` when the gate is closed or `flags` are up.
    fn acquire(self: &Arc<Gate>, flags: &Flags) -> Option<Permit> {
        let mut state = self.lock();
        loop {
            if state.closed || flags.raised() {
                return None;
            }
            if state.used < MAX_IN_FLIGHT {
                state.used += 1;
                return Some(Permit(self.clone()));
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.lock().used -= 1;
        self.0.changed.notify_all();
    }
}

type Key = (usize, u32);

/// What a child may answer: the requests sent to it that are not answered
/// yet. A request stays answerable for [`ANSWER_WINDOW`] after it was last
/// sent (a child may be a long while in a page while many newer requests
/// replace its queue), at most twice at once for each page and scale, and at
/// most [`MAX_OPEN`] in all (the oldest go first), so the whole is bounded.
/// Every request is answered once: the answer takes its entry. A start must
/// name a page with an entry, and the answer that follows (a page, or its
/// failure) must be for that page.
#[derive(Default)]
struct Ledger {
    /// The sequence numbers of the requests in which each (page, scale) is
    /// open, oldest first.
    open: HashMap<Key, VecDeque<u64>>,
    log: VecDeque<(u64, Instant, Key)>,
    next: u64,
    /// The page started and the scales it may be answered at, as they were
    /// when it started: a request that replaces them while it renders does
    /// not take them away.
    started: Option<(usize, Vec<Key>)>,
}

impl Ledger {
    /// A new request (without doubles).
    fn ask(&mut self, keys: Vec<Key>) {
        let now = Instant::now();
        for key in keys {
            self.next += 1;
            let entries = self.open.entry(key).or_default();
            if entries.len() >= 2 {
                entries.pop_front();
            }
            entries.push_back(self.next);
            self.log.push_back((self.next, now, key));
        }
        self.prune(now);
    }

    /// Forget what is too old, and the oldest beyond [`MAX_OPEN`].
    fn prune(&mut self, now: Instant) {
        while let Some(&(seq, sent, key)) = self.log.front() {
            if self.log.len() <= MAX_OPEN && now.duration_since(sent) <= ANSWER_WINDOW {
                break;
            }
            self.log.pop_front();
            if let Some(entries) = self.open.get_mut(&key) {
                if entries.front() == Some(&seq) {
                    entries.pop_front();
                }
                if entries.is_empty() {
                    self.open.remove(&key);
                }
            }
        }
    }

    fn take(&mut self, key: &Key) {
        if let Some(entries) = self.open.get_mut(key) {
            entries.pop_front();
            if entries.is_empty() {
                self.open.remove(key);
            }
        }
    }

    /// The child says it renders `page`: is it one that was asked for, and
    /// is the child through with the last?
    fn start(&mut self, page: usize) -> bool {
        if self.started.is_some() {
            return false;
        }
        self.prune(Instant::now());
        let keys: Vec<Key> = self
            .open
            .keys()
            .filter(|(p, _)| *p == page)
            .copied()
            .collect();
        if keys.is_empty() {
            return false;
        }
        self.started = Some((page, keys));
        true
    }

    /// A page comes back, at this scale: is it the one started, and asked
    /// for? Takes its entry.
    fn answer(&mut self, page: usize, scale: f32) -> bool {
        let key = (page, scale.to_bits());
        let Some((started, keys)) = &mut self.started else {
            return false;
        };
        if *started != page {
            return false;
        }
        let Some(at) = keys.iter().position(|k| *k == key) else {
            return false;
        };
        keys.swap_remove(at);
        self.started = None;
        self.take(&key);
        true
    }

    /// The page started failed. It is never asked for again: all its
    /// entries go.
    fn fail(&mut self, page: usize) -> bool {
        if self.started.as_ref().is_none_or(|(p, _)| *p != page) {
            return false;
        }
        self.started = None;
        self.open.retain(|(p, _), _| *p != page);
        true
    }
}

/// The newest request for a child's writer thread, and whether to stop.
#[derive(Default)]
struct Outbox {
    slot: Mutex<OutSlot>,
    ready: Condvar,
}

#[derive(Default)]
struct OutSlot {
    want: Option<Vec<(usize, f32)>>,
    closed: bool,
}

impl Outbox {
    fn slot(&self) -> MutexGuard<'_, OutSlot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replace what is waiting to be sent.
    fn put(&self, pages: Vec<(usize, f32)>) {
        self.slot().want = Some(pages);
        self.ready.notify_all();
    }

    fn close(&self) {
        self.slot().closed = true;
        self.ready.notify_all();
    }
}

/// One child process and its input.
struct Running {
    child: Child,
    outbox: Arc<Outbox>,
    generation: u64,
    began: Instant,
    /// It has said it opened the file.
    opened: bool,
    rendering: Option<(usize, Instant)>,
    /// Killed for taking too long.
    timed_out: bool,
    ledger: Arc<Mutex<Ledger>>,
    gate: Arc<Gate>,
}

impl Running {
    fn kill(&mut self) {
        self.outbox.close();
        self.gate.close();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// How it ended, once its output has closed.
    fn status(&mut self) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.outbox.close();
                    return status.code();
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    self.kill();
                    return None;
                }
            }
        }
    }

    /// Kill it for taking too long; its end arrives as a message.
    fn give_up(&mut self) {
        if !self.timed_out {
            self.timed_out = true;
            let _ = self.child.kill();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The most a page of `size` may measure, in pixels, at `scale`: what
/// rendering gives, rounded up, and one more.
fn bound(side: f32, scale: f32) -> u64 {
    let pixels = (f64::from(side.max(0.0)) * f64::from(scale)).ceil();
    // Beyond what a frame can say, whatever the page.
    pixels.min(f64::from(u32::MAX)) as u64 + 1
}

impl Supervisor {
    /// The child's share of the memory.
    fn budget(&self) -> u64 {
        self.limits.max_memory_bytes / self.pool.inner.max_alive as u64
    }

    fn start(&mut self, generation: u64, known: Option<Arc<Vec<PageSize>>>) -> Option<Running> {
        let mut command = Command::new(&self.program.executable);
        command
            .args(&self.program.arguments)
            .arg(child::SUBCOMMAND)
            .arg(self.budget().to_string())
            .arg(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = command.spawn().ok()?;
        let (Some(input), Some(output)) = (process.stdin.take(), process.stdout.take()) else {
            let _ = process.kill();
            let _ = process.wait();
            return None;
        };
        let outbox = Arc::new(Outbox::default());
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let gate = Gate::new();
        let running = Running {
            child: process,
            outbox: outbox.clone(),
            generation,
            began: Instant::now(),
            opened: false,
            rendering: None,
            timed_out: false,
            ledger: ledger.clone(),
            gate: gate.clone(),
        };
        let start = ToChild::Start {
            password: self.password.clone(),
            limits: child::WireLimits::of(&self.limits),
        };
        let writer = std::thread::Builder::new()
            .name("pdf-requests".into())
            .spawn(move || write_requests(input, start, &outbox));
        if writer.is_err() {
            return None;
        }
        self.pid.store(running.child.id(), Ordering::Relaxed);
        let messages = self.messages.clone();
        let max_pixels = self.limits.max_render_pixels;
        let flags = self.flags.clone();
        let (ledger, gate) = (ledger.clone(), gate.clone());
        let reader = std::thread::Builder::new()
            .name("pdf-answers".into())
            .spawn(move || {
                let mut output = BufReader::with_capacity(1 << 16, output);
                // Page sizes: known from the first time the file was opened,
                // else from this child's own answer.
                let mut sizes = known;
                let mut seen_opened = false;
                let misbehaved = loop {
                    if flags.raised() {
                        break true;
                    }
                    let answer = child::read_answer_with(
                        &mut output,
                        max_pixels,
                        |page, scale, width, height| {
                            let Some(size) = seen_opened
                                .then_some(sizes.as_ref())
                                .flatten()
                                .and_then(|s| s.get(page))
                            else {
                                return false;
                            };
                            u64::from(width) <= bound(size.width, scale)
                                && u64::from(height) <= bound(size.height, scale)
                                && ledger
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .answer(page, scale)
                        },
                    );
                    let answer = match answer {
                        Ok(answer) => answer,
                        Err(error) => {
                            // The end of the output is a child that ended;
                            // anything else is one that sent what it
                            // should not.
                            break error.kind() != ErrorKind::UnexpectedEof;
                        }
                    };
                    let count = sizes.as_ref().map_or(0, |s| s.len());
                    let fine = match &answer {
                        FromChild::Opened(info) => {
                            let once = !seen_opened;
                            seen_opened = true;
                            match &sizes {
                                None => {
                                    sizes = Some(Arc::new(info.pages.clone()));
                                    once
                                }
                                // The file must be the one that was opened.
                                Some(known) => once && known.as_slice() == info.pages.as_slice(),
                            }
                        }
                        FromChild::Failed(_) => true,
                        // (Pages were checked as they were read.)
                        FromChild::Page(..) => true,
                        FromChild::Started(page) => {
                            seen_opened
                                && *page < count
                                && ledger
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .start(*page)
                        }
                        FromChild::PageFailed(page) => {
                            seen_opened
                                && *page < count
                                && ledger.lock().unwrap_or_else(|e| e.into_inner()).fail(*page)
                        }
                    };
                    if !fine {
                        break true;
                    }
                    // Wait for room: a child that is faster than the
                    // supervisor waits on its pipe.
                    let Some(permit) = gate.acquire(&flags) else {
                        break true;
                    };
                    if messages
                        .send(Message::Answer(generation, answer, permit))
                        .is_err()
                    {
                        return;
                    }
                };
                let _ = messages.send(Message::Ended(generation, misbehaved));
            });
        if reader.is_err() {
            return None;
        }
        Some(running)
    }

    /// The child is over: free its place.
    fn forget(&self) {
        self.pid.store(0, Ordering::Relaxed);
        self.pool.release(self.id);
    }

    fn run(mut self, inbox: mpsc::Receiver<Message>) {
        let id = self.id;
        let pool = self.pool.clone();
        self.supervise(inbox);
        pool.remove(id);
    }

    fn supervise(&mut self, inbox: mpsc::Receiver<Message>) {
        let mut generation = 0u64;
        let mut restarts = 0u32;
        // Children ended for answering what was not asked: never forgiven.
        let mut misbehaved_count = 0u32;
        let mut opened: Option<Info> = None;
        let mut known: Option<Arc<Vec<PageSize>>> = None;
        let mut wanted: Vec<(usize, f32)> = Vec::new();
        let mut broken: HashSet<usize> = HashSet::new();
        let mut running: Option<Running> = None;
        // A child is to be started (again) as soon as there is room.
        let mut pending = true;
        let mut suspended: Option<Instant> = None;
        loop {
            // First of all, before anything that was queued: the tab is
            // closed, or the pool wants the place.
            if self.flags.closing.load(Ordering::SeqCst) {
                if let Some(mut r) = running.take() {
                    r.kill();
                }
                self.forget();
                return;
            }
            if self.flags.evict.load(Ordering::SeqCst)
                && let Some(mut r) = running.take()
            {
                if r.opened && self.pool.is_evicting(self.id) {
                    r.kill();
                    self.forget();
                } else {
                    running = Some(r);
                }
            }
            if running.is_none() && pending && self.pool.try_acquire(self.id) {
                pending = false;
                generation += 1;
                match self.start(generation, known.clone()) {
                    Some(started) => {
                        running = Some(started);
                    }
                    None => {
                        self.forget();
                        (self.events)(match opened {
                            None => Event::Failed(OpenError::Crashed),
                            Some(_) => Event::Stopped,
                        });
                        return;
                    }
                }
            }
            match inbox.recv_timeout(TICK) {
                Ok(Message::Want(pages)) => {
                    wanted = pages;
                    match &mut running {
                        Some(r) if r.opened => self.send_wanted(r, &wanted, &broken),
                        Some(_) => {}
                        None => pending = pending || (opened.is_some() && suspended.is_none()),
                    }
                }
                Ok(Message::Suspend) => suspended = Some(Instant::now()),
                Ok(Message::Resume) => {
                    suspended = None;
                    match &mut running {
                        Some(r) if r.opened => self.send_wanted(r, &wanted, &broken),
                        Some(_) => {}
                        None => pending = true,
                    }
                }
                Ok(Message::Evict) => {
                    // Only if the pool still means it: the child may have
                    // ended and been replaced since.
                    if let Some(mut r) = running.take() {
                        if r.opened && self.pool.is_evicting(self.id) {
                            r.kill();
                            self.forget();
                        } else {
                            running = Some(r);
                        }
                    }
                }
                Ok(Message::Close) | Err(RecvTimeoutError::Disconnected) => {
                    if let Some(mut r) = running.take() {
                        r.kill();
                    }
                    self.forget();
                    return;
                }
                Ok(Message::Answer(from, answer, _permit)) => {
                    let Some(r) = running.as_mut().filter(|r| r.generation == from) else {
                        continue;
                    };
                    match answer {
                        FromChild::Opened(info) => {
                            if r.opened {
                                continue;
                            }
                            r.opened = true;
                            self.pool.opened(self.id);
                            if opened.is_none() {
                                known = Some(Arc::new(info.pages.clone()));
                                (self.events)(Event::Opened(info.clone()));
                                opened = Some(info);
                            }
                            self.send_wanted(r, &wanted, &broken);
                        }
                        FromChild::Failed(error) => {
                            if let Some(mut r) = running.take() {
                                r.kill();
                            }
                            self.forget();
                            // A file that opened once and now does not:
                            // it changed, or the child went wrong.
                            (self.events)(match opened {
                                None => Event::Failed(error),
                                Some(_) => Event::Stopped,
                            });
                            return;
                        }
                        FromChild::Started(page) => r.rendering = Some((page, Instant::now())),
                        FromChild::Page(page, scale, width, height, pixels) => {
                            r.rendering = None;
                            restarts = 0;
                            (self.events)(Event::Page(Rendered {
                                page,
                                scale,
                                width,
                                height,
                                pixels,
                            }));
                        }
                        FromChild::PageFailed(page) => {
                            r.rendering = None;
                            broken.insert(page);
                            (self.events)(Event::PageFailed(page));
                        }
                    }
                }
                Ok(Message::Ended(from, misbehaved)) => {
                    if running.as_ref().is_none_or(|r| r.generation != from) {
                        // From a child that has been replaced.
                        continue;
                    }
                    let Some(mut r) = running.take() else {
                        continue;
                    };
                    if misbehaved {
                        r.kill();
                        misbehaved_count += 1;
                    }
                    let status = r.status();
                    self.forget();
                    if !r.opened {
                        if opened.is_none() {
                            let error = match status {
                                _ if r.timed_out => OpenError::TooLarge(Exceeded::Time),
                                Some(child::MEMORY_EXIT) => OpenError::TooLarge(Exceeded::Memory),
                                _ => OpenError::Crashed,
                            };
                            (self.events)(Event::Failed(error));
                            return;
                        }
                        if r.timed_out {
                            (self.events)(Event::Stopped);
                            return;
                        }
                    }
                    // A child that lied is not the page's fault: the page is
                    // asked again. A crash, memory or time on it is.
                    if !misbehaved
                        && let Some((page, _)) = r.rendering
                        && broken.insert(page)
                    {
                        (self.events)(Event::PageFailed(page));
                    }
                    restarts += 1;
                    if restarts > MAX_RESTARTS || misbehaved_count > MAX_RESTARTS {
                        (self.events)(Event::Stopped);
                        return;
                    }
                    pending = suspended.is_none();
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Some(r) = &mut running {
                if !r.opened && r.began.elapsed() > self.limits.open_time {
                    r.give_up();
                }
                if let Some((_, since)) = r.rendering
                    && since.elapsed() > self.limits.page_time
                {
                    r.give_up();
                }
                // A tab in the background gives up its child after a while;
                // the next request starts one again.
                if r.opened
                    && suspended
                        .is_some_and(|since| since.elapsed() > self.pool.inner.background_time)
                {
                    r.kill();
                    running = None;
                    self.forget();
                }
            }
        }
    }

    /// Ask the child for these pages, minus those that failed. Never
    /// waits: the writer thread takes the newest request when the pipe has
    /// room.
    fn send_wanted(&self, running: &Running, wanted: &[(usize, f32)], broken: &HashSet<usize>) {
        let mut seen = HashSet::new();
        let pages: Vec<(usize, f32)> = wanted
            .iter()
            .copied()
            .filter(|(page, scale)| !broken.contains(page) && scale.is_finite() && *scale > 0.0)
            // Once each: a page asked for twice is answered once.
            .filter(|(page, scale)| seen.insert((*page, scale.to_bits())))
            .take(MAX_WANTED)
            .collect();
        // Before the child can hear of it.
        running
            .ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ask(pages.iter().map(|(p, s)| (*p, s.to_bits())).collect());
        running.outbox.put(pages);
    }
}

/// The child's input: the start frame, then the newest request whenever the
/// previous one is written. Ends when the outbox is closed or a write fails
/// (the child has ended; its end arrives as a message).
fn write_requests(mut input: ChildStdin, start: ToChild, outbox: &Outbox) {
    if child::write_request(&mut input, &start).is_err() {
        return;
    }
    loop {
        let pages = {
            let mut slot = outbox.slot();
            loop {
                if slot.closed {
                    return;
                }
                if let Some(pages) = slot.want.take() {
                    break pages;
                }
                slot = outbox.ready.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        if child::write_request(&mut input, &ToChild::Want(pages)).is_err() {
            return;
        }
    }
}
