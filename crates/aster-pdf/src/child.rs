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

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use hayro::RenderCache;

use crate::{Document, Exceeded, Info, Limits, OpenError, PageSize, STACK_BYTES};

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
    let reader = std::thread::Builder::new()
        .name("pdf-requests".into())
        .spawn(move || {
            while let Ok(ToChild::Want(pages)) = read_frame(&mut input) {
                if requests.send(pages).is_err() {
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
    let cache = RenderCache::new();
    let mut queue: Vec<(usize, f32)> = Vec::new();
    loop {
        let next = if queue.is_empty() {
            inbox
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            inbox.recv_timeout(Duration::ZERO)
        };
        match next {
            Ok(pages) => {
                queue = pages;
                queue.reverse();
                continue;
            }
            // The parent is gone or closed the tab.
            Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let Some((page, scale)) = queue.pop() else {
            continue;
        };
        if write_frame(&mut output, &FromChild::Started(page)).is_err() {
            return 0;
        }
        let sent = match document.render(&cache, page, scale) {
            Ok(Some(pixmap)) => write_page(&mut output, page, scale, &pixmap),
            Ok(None) => write_frame(&mut output, &FromChild::PageFailed(page)),
            Err(()) => {
                let _ = write_frame(&mut output, &FromChild::PageFailed(page));
                return PANIC_EXIT;
            }
        };
        if sent.is_err() {
            return 0;
        }
    }
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
}

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
        }
    }

    fn as_array(&self) -> [u64; 6] {
        [
            self.max_file_bytes,
            self.max_objects,
            self.max_pages,
            self.max_stream_bytes,
            self.max_image_pixels,
            self.max_render_pixels,
        ]
    }

    fn from_array(v: [u64; 6]) -> WireLimits {
        WireLimits {
            max_file_bytes: v[0],
            max_objects: v[1],
            max_pages: v[2],
            max_stream_bytes: v[3],
            max_image_pixels: v[4],
            max_render_pixels: v[5],
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
            ..Limits::default()
        }
    }
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
}

/// Pages and pixels are never larger than this, whatever a message says.
const MAX_PAGES: u32 = 1_000_000;
const MAX_SIDE: u32 = 16_384;
/// A limit in a start frame is never larger than this (1 PiB), and a count
/// must fit in `usize`.
const MAX_LIMIT: u64 = 1 << 50;

fn u32_of(value: usize) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| io::Error::other("too large"))
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
    }
    out.write_all(&buffer)?;
    out.flush()
}

fn read_u8(input: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    input.read_exact(&mut b)?;
    Ok(b[0])
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

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "bad message")
}

/// A request, as the child reads it.
pub fn read_frame(input: &mut impl Read) -> io::Result<ToChild> {
    match read_u8(input)? {
        b'K' => {
            let length = read_u32(input)?;
            if length > 1 << 16 {
                return Err(bad());
            }
            let mut bytes = vec![0u8; length as usize];
            input.read_exact(&mut bytes)?;
            let password = String::from_utf8(bytes).map_err(|_| bad())?;
            let mut values = [0u64; 6];
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
        _ => Err(bad()),
    }
}

/// An answer, as the parent reads it. `max_pixels` bounds a page.
pub fn read_answer(input: &mut impl Read, max_pixels: u64) -> io::Result<FromChild> {
    read_answer_with(input, max_pixels, |_, _, _, _| true)
}

/// Like [`read_answer`], but `accept(page, scale, width, height)` is asked
/// about a page before its pixels are read or allocated; a `false` is an
/// error, and the caller ends the child.
pub fn read_answer_with(
    input: &mut impl Read,
    max_pixels: u64,
    mut accept: impl FnMut(usize, f32, u32, u32) -> bool,
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
            // A page is never larger than the limit (plus rounding).
            if width > MAX_SIDE
                || height > MAX_SIDE
                || pixels > max_pixels + max_pixels / 50 + 4096
                || !accept(page, scale, width, height)
            {
                return Err(bad());
            }
            let mut data = vec![0u8; pixels as usize * 4];
            input.read_exact(&mut data)?;
            Ok(FromChild::Page(page, scale, width, height, data))
        }
        _ => Err(bad()),
    }
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

    #[test]
    fn messages_go_both_ways() {
        let mut wire = Vec::new();
        let start = ToChild::Start {
            password: "密码".into(),
            limits: WireLimits::of(&Limits {
                max_render_pixels: 64_000_000,
                max_pages: 12,
                ..Limits::default()
            }),
        };
        write_request(&mut wire, &start).unwrap();
        write_request(&mut wire, &ToChild::Want(vec![(3, 1.5), (0, 0.25)])).unwrap();
        let mut input = wire.as_slice();
        assert_eq!(read_frame(&mut input).unwrap(), start);
        assert_eq!(
            read_frame(&mut input).unwrap(),
            ToChild::Want(vec![(3, 1.5), (0, 0.25)])
        );

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
        for _ in 0..6 {
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
}
