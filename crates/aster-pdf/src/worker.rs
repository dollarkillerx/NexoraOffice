//! The parent's side of a PDF child process: starting it, passing on what
//! is wanted, reporting what comes back, and enforcing the time limits.
//!
//! One supervising thread per document owns the child. A page that takes
//! longer than `page_time` gets the child killed; a child that ends while a
//! page renders (killed, out of memory, or after a panic) is started again,
//! and that page is reported as failed and never asked for again. Dropping
//! the [`Worker`] kills the child at once.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::child::{self, FromChild, ToChild};
use crate::{Event, Exceeded, Info, Limits, OpenError, Rendered};

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

/// Times a child is started again after it ended with pages to render.
const MAX_RESTARTS: u32 = 8;
/// How often the supervisor looks at the clock.
const TICK: Duration = Duration::from_millis(100);

enum Message {
    Answer(u64, FromChild),
    /// The child's output ended: it exited or was killed.
    Ended(u64),
    Want(Vec<(usize, f32)>),
    Close,
}

/// A PDF file opened in a child process. Dropping it ends the process.
pub struct Worker {
    messages: Sender<Message>,
    pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Worker {
    /// Open the file at `path` in a child process started from `program`.
    /// `events` is called on the supervising thread.
    pub fn open(
        program: Program,
        path: PathBuf,
        password: String,
        limits: Limits,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        let (messages, inbox) = mpsc::channel();
        let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let supervisor = Supervisor {
            program,
            path,
            password,
            limits,
            messages: messages.clone(),
            pid: pid.clone(),
            events: Box::new(events),
        };
        let spawned = std::thread::Builder::new()
            .name("pdf-supervisor".into())
            .spawn(move || supervisor.run(inbox));
        if spawned.is_err() {
            // Without a thread nothing is reported; the tab stays as it is.
        }
        Worker { messages, pid }
    }

    /// Render these pages, in this order, replacing what was asked before.
    pub fn want(&self, pages: Vec<(usize, f32)>) {
        let _ = self.messages.send(Message::Want(pages));
    }

    /// The child process running now, 0 if none (for tests).
    pub fn child_id(&self) -> u32 {
        self.pid.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.messages.send(Message::Close);
    }
}

struct Supervisor {
    program: Program,
    path: PathBuf,
    password: String,
    limits: Limits,
    messages: Sender<Message>,
    pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    events: Box<dyn FnMut(Event) + Send>,
}

/// One child process and its input.
struct Running {
    child: Child,
    input: ChildStdin,
    generation: u64,
}

impl Running {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// How it ended, once its output has closed.
    fn status(&mut self) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
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
}

impl Supervisor {
    fn start(&mut self, generation: u64) -> Option<Running> {
        let mut command = Command::new(&self.program.executable);
        command
            .args(&self.program.arguments)
            .arg(child::SUBCOMMAND)
            .arg(self.limits.max_memory_bytes.to_string())
            .arg(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = command.spawn().ok()?;
        let mut input = process.stdin.take()?;
        let output = process.stdout.take()?;
        let start = ToChild::Start {
            password: self.password.clone(),
            limits: child::WireLimits::of(&self.limits),
        };
        if child::write_request(&mut input, &start).is_err() {
            let _ = process.kill();
            let _ = process.wait();
            return None;
        }
        self.pid
            .store(process.id(), std::sync::atomic::Ordering::Relaxed);
        let messages = self.messages.clone();
        let max_pixels = self.limits.max_render_pixels;
        let reader = std::thread::Builder::new()
            .name("pdf-answers".into())
            .spawn(move || {
                let mut output = BufReader::with_capacity(1 << 16, output);
                while let Ok(answer) = child::read_answer(&mut output, max_pixels) {
                    if messages.send(Message::Answer(generation, answer)).is_err() {
                        return;
                    }
                }
                let _ = messages.send(Message::Ended(generation));
            });
        if reader.is_err() {
            let _ = process.kill();
            let _ = process.wait();
            return None;
        }
        Some(Running {
            child: process,
            input,
            generation,
        })
    }

    fn run(mut self, inbox: mpsc::Receiver<Message>) {
        let mut generation = 0u64;
        let mut restarts = 0u32;
        let mut opened: Option<Info> = None;
        let mut wanted: Vec<(usize, f32)> = Vec::new();
        let mut broken: HashSet<usize> = HashSet::new();
        'process: loop {
            generation += 1;
            let Some(mut running) = self.start(generation) else {
                if opened.is_none() {
                    (self.events)(Event::Failed(OpenError::Crashed));
                }
                return;
            };
            let began = Instant::now();
            let mut rendering: Option<(usize, Instant)> = None;
            let mut timed_out = false;
            if opened.is_some() {
                send_wanted(&mut running, &wanted, &broken);
            }
            loop {
                match inbox.recv_timeout(TICK) {
                    Ok(Message::Want(pages)) => {
                        wanted = pages;
                        if opened.is_some() {
                            send_wanted(&mut running, &wanted, &broken);
                        }
                    }
                    Ok(Message::Close) | Err(RecvTimeoutError::Disconnected) => {
                        running.kill();
                        self.pid.store(0, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    Ok(Message::Answer(from, answer)) if from == running.generation => match answer
                    {
                        FromChild::Opened(info) => {
                            if opened.is_none() {
                                (self.events)(Event::Opened(info.clone()));
                                opened = Some(info);
                            }
                            send_wanted(&mut running, &wanted, &broken);
                        }
                        FromChild::Failed(error) => {
                            running.kill();
                            self.pid.store(0, std::sync::atomic::Ordering::Relaxed);
                            (self.events)(Event::Failed(error));
                            return;
                        }
                        FromChild::Started(page) => rendering = Some((page, Instant::now())),
                        FromChild::Page(page, scale, width, height, pixels) => {
                            rendering = None;
                            (self.events)(Event::Page(Rendered {
                                page,
                                scale,
                                width,
                                height,
                                pixels,
                            }));
                        }
                        FromChild::PageFailed(page) => {
                            rendering = None;
                            broken.insert(page);
                            (self.events)(Event::PageFailed(page));
                        }
                    },
                    Ok(Message::Ended(from)) if from == running.generation => {
                        let status = running.status();
                        self.pid.store(0, std::sync::atomic::Ordering::Relaxed);
                        if opened.is_none() {
                            let error = match status {
                                _ if timed_out => OpenError::TooLarge(Exceeded::Time),
                                Some(child::MEMORY_EXIT) => OpenError::TooLarge(Exceeded::Memory),
                                _ => OpenError::Crashed,
                            };
                            (self.events)(Event::Failed(error));
                            return;
                        }
                        if let Some((page, _)) = rendering
                            && broken.insert(page)
                        {
                            (self.events)(Event::PageFailed(page));
                        }
                        restarts += 1;
                        if restarts > MAX_RESTARTS {
                            // Pages not drawn stay blank.
                            return;
                        }
                        continue 'process;
                    }
                    // From a child that has been replaced.
                    Ok(Message::Answer(..) | Message::Ended(_)) => {}
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if opened.is_none() && began.elapsed() > self.limits.open_time && !timed_out {
                    timed_out = true;
                    let _ = running.child.kill();
                }
                if let Some((_, since)) = rendering
                    && since.elapsed() > self.limits.page_time
                    && !timed_out
                {
                    timed_out = true;
                    let _ = running.child.kill();
                }
            }
        }
    }
}

fn send_wanted(running: &mut Running, wanted: &[(usize, f32)], broken: &HashSet<usize>) {
    let pages: Vec<(usize, f32)> = wanted
        .iter()
        .copied()
        .filter(|(page, _)| !broken.contains(page))
        .collect();
    // A child that cannot take it has ended; its end arrives as a message.
    let _ = child::write_request(&mut running.input, &ToChild::Want(pages));
    let _ = running.input.flush();
}
