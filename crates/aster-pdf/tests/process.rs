//! The child process: what happens to it is what would otherwise happen to
//! the application. Out of memory, too slow, a panic, a closed tab.

#[path = "../src/builder.rs"]
mod builder;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use aster_pdf::{Event, Exceeded, Limits, OpenError, Program, Worker};
use builder::{Builder, exponential, one_page};

fn program() -> Program {
    Program {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_aster-pdf-worker")),
        arguments: Vec::new(),
    }
}

/// Write `bytes` to a file of its own and open it in a child process.
fn open(bytes: &[u8], limits: Limits) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.pdf");
    std::fs::write(&path, bytes).unwrap();
    let (send, receive) = mpsc::channel();
    let worker = Worker::open(program(), path, String::new(), limits, move |event| {
        let _ = send.send(event);
    });
    (worker, receive, dir)
}

fn next(events: &Receiver<Event>) -> Event {
    events
        .recv_timeout(Duration::from_secs(60))
        .expect("no answer within a minute")
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

#[test]
fn a_page_comes_back_from_the_child() {
    let bytes = Builder::document(&[b"1 0 0 rg 0 0 200 100 re f", b"0 g 0 0 10 10 re f"]);
    let (worker, events, _dir) = open(&bytes, Limits::default());
    let Event::Opened(info) = next(&events) else {
        panic!("not opened");
    };
    assert_eq!(info.pages.len(), 2);
    worker.want(vec![(0, 1.0)]);
    let Event::Page(page) = next(&events) else {
        panic!("no page");
    };
    assert_eq!((page.page, page.width, page.height), (0, 200, 100));
    assert_eq!(&page.pixels[..4], &[255, 0, 0, 255]);
}

#[test]
fn opening_reports_errors() {
    let (_worker, events, _dir) = open(b"not a pdf", Limits::default());
    assert!(matches!(next(&events), Event::Failed(OpenError::Damaged)));
    let (send, receive) = mpsc::channel();
    let _worker = Worker::open(
        program(),
        PathBuf::from("/nonexistent/a.pdf"),
        String::new(),
        Limits::default(),
        move |event| {
            let _ = send.send(event);
        },
    );
    assert!(matches!(
        next(&receive),
        Event::Failed(OpenError::Unreadable(std::io::ErrorKind::NotFound))
    ));
}

#[test]
fn a_child_over_its_memory_budget_is_gone_and_said_so() {
    // The catalog is in an object stream with 1 GB of padding, which the
    // parser inflates while opening, before any check can run. With a
    // budget of 256 MB the child ends; the parent says why.
    use std::io::Write;
    let mut objects = b"2 0 ".to_vec();
    let first = objects.len();
    objects.extend_from_slice(b"<< /Type /Catalog /Pages 3 0 R >>");
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&objects).unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..1024 {
        encoder.write_all(&chunk).unwrap();
    }
    let packed = encoder.finish().unwrap();
    let mut b = Builder::default();
    b.stream(
        &format!("/Type /ObjStm /N 1 /First {first} /Filter /FlateDecode"),
        &packed,
    );
    let _catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    let limits = Limits {
        max_memory_bytes: 256 << 20,
        ..Limits::default()
    };
    let (_worker, events, _dir) = open(&b.finish(2), limits);
    assert!(matches!(
        next(&events),
        Event::Failed(OpenError::TooLarge(Exceeded::Memory))
    ));
}

#[test]
fn a_page_over_the_memory_budget_fails_and_the_rest_still_render() {
    // Page 0 is fine; page 1 is drawn at a scale that needs a 128 MB pixmap
    // (32 M pixels x 4 bytes), more than the 96 MiB budget.
    let bytes = Builder::document(&[b"0 g 0 0 10 10 re f", b"0 g 0 0 10 10 re f"]);
    let limits = Limits {
        max_memory_bytes: 96 << 20,
        max_render_pixels: 64_000_000,
        ..Limits::default()
    };
    let (worker, events, _dir) = open(&bytes, limits);
    assert!(matches!(next(&events), Event::Opened(_)));
    worker.want(vec![(1, 40.0)]);
    assert!(matches!(next(&events), Event::PageFailed(1)));
    // The child was started again; page 1 is not asked for again.
    worker.want(vec![(1, 40.0), (0, 1.0)]);
    let Event::Page(page) = next(&events) else {
        panic!("no page after the restart");
    };
    assert_eq!(page.page, 0);
}

#[test]
fn a_page_that_takes_too_long_is_given_up() {
    // Thirty levels of forms that each draw the next twice: a billion fills,
    // within every limit on nesting.
    let bytes = exponential(30, 2);
    let limits = Limits {
        page_time: Duration::from_secs(2),
        ..Limits::default()
    };
    let (worker, events, _dir) = open(&bytes, limits);
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    let started = Instant::now();
    worker.want(vec![(0, 1.0)]);
    assert!(matches!(next(&events), Event::PageFailed(0)));
    let took = started.elapsed();
    assert!(took < Duration::from_secs(10), "{took:?}");
    // The slow child is gone.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!alive(pid));
}

#[test]
fn closing_the_tab_ends_the_child() {
    let bytes = exponential(30, 2);
    let (worker, events, _dir) = open(&bytes, Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    worker.want(vec![(0, 1.0)]);
    std::thread::sleep(Duration::from_millis(300));
    let pid = worker.child_id();
    assert!(alive(pid));
    drop(worker);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !alive(pid),
        "the child is still running after the tab closed"
    );
}

#[test]
fn a_stream_bomb_is_refused_by_the_child() {
    let bytes = one_page(
        |_| "<< >>".into(),
        "/F /FlateDecode",
        &builder::zeros_deflated(1 << 30),
    );
    let (_worker, events, _dir) = open(&bytes, Limits::default());
    assert!(matches!(
        next(&events),
        Event::Failed(OpenError::TooLarge(Exceeded::Stream))
    ));
}
