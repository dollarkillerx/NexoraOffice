//! The child process: what happens to it is what would otherwise happen to
//! the application. Out of memory, too slow, a panic, a closed tab.

#[path = "../src/builder.rs"]
mod builder;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use aster_pdf::{Event, Exceeded, Limits, OpenError, PageSize, Pool, Program, Worker};
use builder::{Builder, exponential, one_page};

fn program() -> Program {
    Program {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_aster-pdf-worker")),
        arguments: Vec::new(),
    }
}

/// A pool of its own, so that tests running side by side do not send each
/// other's children away. One place: the child gets all the memory.
fn alone() -> Pool {
    Pool::new(1, Duration::from_secs(60))
}

/// Write `bytes` to a file of its own and open it in a child process.
fn open(bytes: &[u8], limits: Limits) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    open_in(&alone(), bytes, limits)
}

fn open_in(
    pool: &Pool,
    bytes: &[u8],
    limits: Limits,
) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.pdf");
    std::fs::write(&path, bytes).unwrap();
    let (send, receive) = mpsc::channel();
    let worker = Worker::open_in(pool, program(), path, String::new(), limits, move |event| {
        let _ = send.send(event);
    });
    (worker, receive, dir)
}

/// A document of `n` pages that each measure `media` and draw a square.
fn doc(media: &str, n: usize) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let mut kids = String::new();
    for _ in 0..n {
        let content = b.stream("", b"0 g 0 0 10 10 re f");
        let page = b.add(format!(
            "<< /Type /Page /Parent {pages} 0 R /MediaBox {media} /Contents {content} 0 R >>"
        ));
        kids.push_str(&format!("{page} 0 R "));
    }
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.finish(catalog)
}

/// Limits under which a page at scale 40 is too large for the child (a 128
/// MB pixmap against 96 MB), and the child ends.
fn crashy() -> Limits {
    Limits {
        max_memory_bytes: 96 << 20,
        max_render_pixels: 64_000_000,
        ..Limits::default()
    }
}

/// A stand-in for the child, a shell script that is given the arguments
/// `--pdf-worker <budget> <file>` as `$1 $2 $3`. Unix only, like the rest.
fn fake(script: &str) -> Program {
    Program {
        executable: PathBuf::from("/bin/sh"),
        arguments: vec![
            "-c".into(),
            // Like the real child: nothing inherited stays open.
            format!("for fd in 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do eval \"exec $fd>&-\"; done 2>/dev/null; {script}").into(),
            "fake".into(),
        ],
    }
}

/// The frames the stand-in sends, as `printf` formats them: a one-page
/// document of 200 x 100 points.
const FAKE_OPENED: &str = r"printf 'O\001\000\000\000\000\000\110\103\000\000\310\102\000';";
/// Page 0 started (only after it was asked for: anything else is refused).
const FAKE_STARTED: &str = r"printf 'S\000\000\000\000';";
/// Wait until the start frame (101 bytes), the empty request that follows
/// the opening (5) and the start of the one for a page (13) have arrived.
const FAKE_WAIT_FOR_WANT: &str = "head -c 110 >/dev/null;";

fn open_fake(program: Program, limits: Limits) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.pdf");
    std::fs::write(&path, b"unused").unwrap();
    let (send, receive) = mpsc::channel();
    let worker = Worker::open_in(
        &alone(),
        program,
        path,
        String::new(),
        limits,
        move |event| {
            let _ = send.send(event);
        },
    );
    (worker, receive, dir)
}

fn wait_until(what: &str, seconds: u64, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn quiet(events: &Receiver<Event>) {
    if let Ok(event) = events.recv_timeout(Duration::from_millis(400)) {
        panic!("an event while there should be none: {event:?}");
    }
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

// H1: nothing larger than asked for.

#[test]
fn a_thumbnail_of_a_huge_page_stays_a_thumbnail() {
    // 1e6 x 1e6 points at 132 / 1e6 pixels per point: 132 pixels a side
    // (133 with rounding), not a 4000 x 4000 pixmap.
    let (worker, events, _dir) = open(&doc("[0 0 1000000 1000000]", 3), Limits::default());
    let Event::Opened(info) = next(&events) else {
        panic!("not opened");
    };
    assert_eq!(info.pages[0].width, 1_000_000.0);
    worker.want(vec![(0, 0.000132), (1, 0.000132), (2, 0.000132)]);
    for _ in 0..3 {
        let Event::Page(page) = next(&events) else {
            panic!("no page");
        };
        assert!(page.width <= 133 && page.height <= 133, "{page:?}");
        assert!(page.pixels.len() <= 133 * 133 * 4);
    }
}

#[test]
fn a_scale_is_never_raised() {
    let size = PageSize {
        width: 1_000_000.0,
        height: 1_000_000.0,
    };
    assert_eq!(
        aster_pdf::fitted_scale(size, 0.000132, 16_000_000),
        0.000132
    );
    assert_eq!(aster_pdf::fitted_scale(size, 1e-9, 16_000_000), 1e-9);
    let a4 = PageSize {
        width: 595.0,
        height: 842.0,
    };
    assert!(aster_pdf::fitted_scale(a4, 50.0, 16_000_000) < 50.0);
}

#[test]
fn a_page_too_thin_to_draw_fails_without_ending_the_child() {
    let (worker, events, _dir) = open(&doc("[0 0 1000000 1]", 3), Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    // Too wide for a side: scaled down until it has no height. Then a scale
    // so small that nothing is left.
    worker.want(vec![(0, 0.1), (1, 1e-12)]);
    for _ in 0..2 {
        assert!(matches!(
            next(&events),
            Event::PageFailed(_) | Event::Page(_)
        ));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(worker.child_id(), pid, "the child was started again");
    assert!(alive(pid));
}

/// A child that sends a page larger than it should, or one that was not
/// asked for, is ended like one that crashed.
fn lying_child(page: &str) -> String {
    format!("{FAKE_OPENED} {FAKE_WAIT_FOR_WANT} {FAKE_STARTED} printf '{page}'; exec sleep 30")
}

#[test]
fn a_page_larger_than_asked_for_is_refused_before_its_pixels() {
    // Asked: page 0 (200 x 100 points) at 1.0. The child says 3000 x 3000
    // pixels and sends none: the parent must not wait for them or allocate.
    let page = r"P\000\000\000\000\000\000\200\077\270\013\000\000\270\013\000\000";
    let (worker, events, _dir) = open_fake(fake(&lying_child(page)), Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want(vec![(0, 1.0)]);
    wait_until("the child is still running", 5, || !alive(pid));
    // The page is not blamed: it is asked again, until the tab stops.
    assert!(matches!(next(&events), Event::Stopped));
}

#[test]
fn a_page_at_a_scale_that_was_not_asked_for_is_refused() {
    // 10 x 10 pixels is small enough, but the scale in the frame is 0.5, and
    // only 1.0 was asked for.
    let page = r"P\000\000\000\000\000\000\000\077\012\000\000\000\012\000\000\000";
    let (worker, events, _dir) = open_fake(fake(&lying_child(page)), Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want(vec![(0, 1.0)]);
    wait_until("the child is still running", 5, || !alive(pid));
    // The page is not blamed: it is asked again, until the tab stops.
    assert!(matches!(next(&events), Event::Stopped));
}

// L1: the child does not outlive its parent.

#[test]
fn the_child_ends_when_its_input_closes() {
    use aster_pdf::child::{ToChild, WireLimits, write_request};
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slow.pdf");
    std::fs::write(&path, exponential(30, 2)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aster-pdf-worker"))
        .arg("--pdf-worker")
        .arg((3u64 << 30).to_string())
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let start = ToChild::Start {
        password: String::new(),
        limits: WireLimits::of(&Limits::default()),
    };
    write_request(&mut input, &start).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    write_request(&mut input, &ToChild::Want(vec![(0, 1.0)])).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    // It is in the middle of a page that takes minutes. The parent "dies".
    drop(input);
    drop(output);
    wait_until("the child outlived its parent", 5, || {
        child.try_wait().unwrap().is_some()
    });
    let _ = child.kill();
}

#[test]
fn the_child_ends_when_its_parent_is_gone_though_its_input_is_open() {
    use std::process::{Command, Stdio};
    // The shell starts the child with an input that stays open (`sleep`
    // holds it), prints its number and ends a second later (the child has
    // noted its parent by then): the child is left without a parent.
    let script = format!(
        "sleep 20 </dev/null | {} --pdf-worker 1000000000 /nonexistent >/dev/null 2>&1 & echo $!; sleep 1",
        env!("CARGO_BIN_EXE_aster-pdf-worker")
    );
    let output = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    let pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("the shell did not print a number");
    wait_until("the child outlived its parent", 5, || !alive(pid));
}

// L2: a tab does not die silently.

#[test]
fn restarts_start_over_after_a_page_is_drawn() {
    // Six children end on a page that is too large, then a page draws, and
    // six more: twelve in all, more than the eight allowed in a row.
    let (worker, events, _dir) = open(&doc("[0 0 200 100]", 16), crashy());
    assert!(matches!(next(&events), Event::Opened(_)));
    for round in 0..2 {
        for page in 0..6 {
            let page = round * 7 + page;
            worker.want(vec![(page, 40.0)]);
            assert!(matches!(next(&events), Event::PageFailed(p) if p == page));
        }
        worker.want(vec![(round * 7 + 6, 1.0)]);
        assert!(matches!(next(&events), Event::Page(_)));
    }
}

#[test]
fn a_tab_whose_child_keeps_ending_is_stopped_and_says_so() {
    let (worker, events, _dir) = open(&doc("[0 0 200 100]", 12), crashy());
    assert!(matches!(next(&events), Event::Opened(_)));
    for page in 0..9 {
        worker.want(vec![(page, 40.0)]);
        assert!(matches!(next(&events), Event::PageFailed(p) if p == page));
    }
    assert!(matches!(next(&events), Event::Stopped));
}

#[test]
fn a_child_that_cannot_open_the_file_again_stops_the_tab() {
    let (worker, events, dir) = open(&doc("[0 0 200 100]", 3), crashy());
    assert!(matches!(next(&events), Event::Opened(_)));
    std::fs::remove_file(dir.path().join("test.pdf")).unwrap();
    worker.want(vec![(1, 40.0)]);
    assert!(matches!(next(&events), Event::PageFailed(1)));
    assert!(matches!(next(&events), Event::Stopped));
}

// L3: a reopened child is timed, and nothing waits on a full pipe.

#[test]
fn a_child_that_hangs_when_opened_again_stops_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("second");
    // First time: opens, takes a request, and ends in the middle of a page.
    // The second time it says nothing at all.
    let script = format!(
        "if [ -e '{m}' ]; then exec sleep 30; fi; : > '{m}'; {FAKE_OPENED} {FAKE_WAIT_FOR_WANT} {FAKE_STARTED} exit 1",
        m = marker.display()
    );
    let limits = Limits {
        open_time: Duration::from_secs(1),
        ..Limits::default()
    };
    let (worker, events, _dir) = open_fake(fake(&script), limits);
    assert!(matches!(next(&events), Event::Opened(_)));
    let started = Instant::now();
    worker.want(vec![(0, 1.0)]);
    assert!(matches!(next(&events), Event::PageFailed(0)));
    assert!(matches!(next(&events), Event::Stopped));
    assert!(started.elapsed() < Duration::from_secs(10));
    wait_until("the hanging child is still there", 5, || {
        worker.child_id() == 0
    });
}

#[test]
fn requests_do_not_wait_for_a_child_that_reads_nothing() {
    // The child opens the file and never reads its input. Requests that fill
    // the pipe many times over must not stop the supervisor, which has to
    // end the child when the tab closes.
    let script = format!("{FAKE_OPENED} exec sleep 30");
    let (worker, events, _dir) = open_fake(fake(&script), Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    for round in 0..200 {
        worker.want((0..4096).map(|page| (page, 1.0 + round as f32)).collect());
    }
    std::thread::sleep(Duration::from_millis(300));
    drop(worker);
    wait_until("the child outlived its tab", 5, || !alive(pid));
}

// Suspending, resuming, and the memory shared by the children.

fn pool_of_three_documents(pool: &Pool) -> [(Worker, Receiver<Event>, tempfile::TempDir); 2] {
    let make = || {
        let (worker, events, dir) = open_in(pool, &doc("[0 0 200 100]", 3), Limits::default());
        assert!(matches!(next(&events), Event::Opened(_)));
        (worker, events, dir)
    };
    [make(), make()]
}

#[test]
fn a_third_document_sends_the_older_foreground_one_away() {
    let pool = Pool::new(2, Duration::from_secs(60));
    let [(first, first_events, _a), (second, _second_events, _b)] = pool_of_three_documents(&pool);
    assert_ne!(first.child_id(), 0);
    assert_ne!(second.child_id(), 0);
    let (third, third_events, _c) = open_in(&pool, &doc("[0 0 200 100]", 3), Limits::default());
    assert!(matches!(next(&third_events), Event::Opened(_)));
    assert_eq!(first.child_id(), 0, "the older one keeps its child");
    assert_ne!(second.child_id(), 0);
    assert_ne!(third.child_id(), 0);
    // Nothing is said about it, and it comes back when it is asked for: the
    // least recently active of the others goes.
    quiet(&first_events);
    first.want(vec![(0, 1.0)]);
    let Event::Page(page) = next(&first_events) else {
        panic!("no page after the child came back");
    };
    assert_eq!(page.page, 0);
    assert_eq!(second.child_id(), 0);
    assert_ne!(third.child_id(), 0);
}

#[test]
fn a_document_in_the_background_goes_before_an_older_one_in_front() {
    let pool = Pool::new(2, Duration::from_secs(60));
    let [(first, _first_events, _a), (second, second_events, _b)] = pool_of_three_documents(&pool);
    second.suspend();
    let (third, third_events, _c) = open_in(&pool, &doc("[0 0 200 100]", 3), Limits::default());
    assert!(matches!(next(&third_events), Event::Opened(_)));
    assert_ne!(first.child_id(), 0);
    assert_eq!(second.child_id(), 0);
    assert_ne!(third.child_id(), 0);
    quiet(&second_events);
    // Wanted in the background: nothing starts. In front again: it does,
    // and what was wanted is drawn.
    second.want(vec![(1, 1.0)]);
    quiet(&second_events);
    assert_eq!(second.child_id(), 0);
    second.resume();
    let Event::Page(page) = next(&second_events) else {
        panic!("no page after resume");
    };
    assert_eq!(page.page, 1);
    assert_ne!(second.child_id(), 0);
}

#[test]
fn a_child_in_the_background_ends_after_a_while_and_returns_on_resume() {
    let pool = Pool::new(2, Duration::from_millis(500));
    let (worker, events, _dir) = open_in(&pool, &doc("[0 0 200 100]", 3), Limits::default());
    assert!(matches!(next(&events), Event::Opened(_)));
    worker.want(vec![(0, 1.0)]);
    assert!(matches!(next(&events), Event::Page(_)));
    let pid = worker.child_id();
    worker.suspend();
    wait_until("the child stays", 10, || worker.child_id() == 0);
    wait_until("the process stays", 5, || !alive(pid));
    quiet(&events);
    worker.resume();
    // The latest request is sent again.
    let Event::Page(page) = next(&events) else {
        panic!("no page after resume");
    };
    assert_eq!(page.page, 0);
    assert_ne!(worker.child_id(), 0);
}

#[test]
fn each_child_gets_its_share_of_the_memory() {
    for (children, share) in [(2usize, 500u64), (4, 250), (1, 1000)] {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("budget");
        let script = format!("echo \"$2\" > '{}'; exec sleep 30", record.display());
        let limits = Limits {
            max_memory_bytes: 1000 << 20,
            ..Limits::default()
        };
        let pool = Pool::new(children, Duration::from_secs(60));
        let _worker = Worker::open_in(
            &pool,
            fake(&script),
            dir.path().join("x.pdf"),
            String::new(),
            limits,
            |_| {},
        );
        wait_until("no budget", 10, || record.exists());
        wait_until("empty budget", 10, || {
            std::fs::read_to_string(&record).is_ok_and(|s| s.ends_with('\n'))
        });
        let budget: u64 = std::fs::read_to_string(&record)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(budget, share << 20);
    }
}

#[test]
fn the_default_memory_is_a_quarter_of_the_machine_at_most_3_gib() {
    let memory = Limits::default().max_memory_bytes;
    assert!(memory > 0 && memory <= 3 << 30, "{memory}");
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .unwrap();
        let ram: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        assert_eq!(memory, (3u64 << 30).min(ram / 4));
    }
}

// A compromised child: whatever it sends, the parent's memory stays bounded
// and the child does not outlive its tab.

const PYTHON: &str = "/usr/bin/python3";

/// A child that speaks the protocol (three pages of 1000 x 1000 points) and
/// then misbehaves as `argv[1]` says.
const EVIL_CHILD: &str = r#"
import sys, struct, time, os, signal
os.closerange(3, 4096)
signal.alarm(30)
mode = sys.argv[1]
marker = None
if "=" in mode:
    mode, marker = mode.split("=", 1)
inp = sys.stdin.buffer
out = sys.stdout.buffer
def read(n):
    b = b""
    while len(b) < n:
        c = inp.read(n - len(b))
        if not c: sys.exit(0)
        b += c
    return b
def started(p): out.write(b"S" + struct.pack("<I", p))
def page(p, scale, w, h, body):
    out.write(b"P" + struct.pack("<IfII", p, scale, w, h)); out.write(body)
assert read(1) == b"K"
(n,) = struct.unpack("<I", read(4)); read(n); read(96)
out.write(b"O" + struct.pack("<I", 3) + struct.pack("<ff", 1000.0, 1000.0) * 3 + b"\0"); out.flush()
while True:
    assert read(1) == b"W"
    (count,) = struct.unpack("<I", read(4))
    entries = [struct.unpack("<If", read(8)) for _ in range(count)]
    if entries: break
if mode == "failed":
    i = 0
    while True:
        out.write(b"".join(b"X" + struct.pack("<I", (i + k) & 0xffffffff) for k in range(100000)))
        out.flush(); i += 100000
elif mode == "failed_started":
    started(0)
    while True:
        out.write(b"X" + struct.pack("<I", 0) * 1000); out.flush()
elif mode == "pages":
    p, scale = entries[0]
    w = h = int(1000 * scale)
    frame = b"P" + struct.pack("<IfII", p, scale, w, h) + b"\x7f" * (w * h * 4)
    while True:
        out.write(frame); out.flush()
elif mode == "twice":
    p, scale = entries[0]
    w = h = int(1000 * scale)
    body = b"\x7f" * (w * h * 4)
    while True:
        started(p); page(p, scale, w, h, body); out.flush()
elif mode == "valid":
    body = b"\x7f" * (2000 * 2000 * 4)
    for p, scale in entries:
        started(p); page(p, scale, 2000, 2000, body); out.flush()
elif mode == "old":
    # Honest: answers the first request only after a newer one with page 2.
    p, scale = entries[0]
    open(marker, "w").close()
    while not any(q == 2 for q, _ in entries):
        assert read(1) == b"W"
        (count,) = struct.unpack("<I", read(4))
        entries = [struct.unpack("<If", read(8)) for _ in range(count)]
    started(p); page(p, scale, 10, 10, b"\0" * 400); out.flush()
elif mode == "garbage_once":
    p, scale = entries[0]
    started(p)
    if not os.path.exists(marker):
        open(marker, "w").close()
        out.write(b"X" + struct.pack("<I", 2)); out.flush()
    else:
        page(p, scale, 10, 10, b"\0" * 400); out.flush()
elif mode == "start_range":
    started(99); out.flush()
elif mode == "start_unasked":
    started(1); out.flush()
elif mode == "page_range":
    page(99, 1.0, 10, 10, b"\0" * 400); out.flush()
elif mode == "failed_unasked":
    started(0); out.write(b"X" + struct.pack("<I", 2)); out.flush()
time.sleep(30)
"#;

fn rss_mb() -> u64 {
    // Through a file, not a pipe: a child that another thread starts at the
    // same moment may inherit a pipe's write end (no atomic close-on-exec
    // for pipes on macOS) and keep it open, so that the reading never ends.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rss");
    let file = std::fs::File::create(&path).unwrap();
    let status = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        >> 10
}

/// Only one flood at a time: they measure the memory of the whole process.
static FLOODS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Open a worker whose child is `python3 <script> <mode>`; `None` when there
/// is no python.
fn open_evil(
    script: &std::path::Path,
    mode: &str,
    slow: Duration,
) -> Option<(Worker, Receiver<Event>, tempfile::TempDir)> {
    open_evil_with(script, mode, slow, Limits::default())
}

fn open_evil_with(
    script: &std::path::Path,
    mode: &str,
    slow: Duration,
    limits: Limits,
) -> Option<(Worker, Receiver<Event>, tempfile::TempDir)> {
    if !std::path::Path::new(PYTHON).exists() {
        eprintln!("skipped: no {PYTHON}");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.pdf");
    std::fs::write(&path, b"%PDF-1.7").unwrap();
    let program = Program {
        executable: PathBuf::from(PYTHON),
        arguments: vec!["-I".into(), script.into(), mode.into()],
    };
    let (send, receive) = mpsc::channel();
    let worker = Worker::open_in(
        &alone(),
        program,
        path,
        String::new(),
        limits,
        move |event| {
            if !slow.is_zero() {
                std::thread::sleep(slow);
            }
            let _ = send.send(event);
        },
    );
    Some((worker, receive, dir))
}

fn evil_script(dir: &tempfile::TempDir) -> PathBuf {
    let script = dir.path().join("evil.py");
    std::fs::write(&script, EVIL_CHILD).unwrap();
    script
}

/// Drain events until `Stopped`, counting them; the child of the first
/// start must be gone by then.
fn until_stopped(events: &Receiver<Event>) -> (u64, u64) {
    let (mut all, mut pages) = (0, 0);
    loop {
        match events
            .recv_timeout(Duration::from_secs(60))
            .expect("never stopped")
        {
            Event::Stopped => return (all + 1, pages),
            Event::Page(_) => {
                all += 1;
                pages += 1;
            }
            _ => all += 1,
        }
    }
}

#[test]
fn a_child_flooding_page_failures_is_killed_at_once() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let Some((worker, events, _dir)) =
        open_evil(std::path::Path::new(repo_fake), "failed", Duration::ZERO)
    else {
        return;
    };
    drop(tmp);
    let before = rss_mb();
    assert!(matches!(next(&events), Event::Opened(_)));
    let began = Instant::now();
    let pid = worker.child_id();
    worker.want(vec![(0, 4.0)]);
    wait_until("the flooding child was not killed", 2, || !alive(pid));
    let killed_after = began.elapsed();
    let (count, pages) = until_stopped(&events);
    let growth = rss_mb().saturating_sub(before);
    eprintln!(
        "[failed flood] killed after {killed_after:?}, {count} events up to Stopped, RSS growth {growth} MB"
    );
    assert_eq!(pages, 0);
    assert!(count < 50, "{count} events");
    assert!(growth < 300, "RSS grew by {growth} MB");
    wait_until("a child is left", 5, || worker.child_id() == 0);
}

#[test]
fn a_child_flooding_pages_is_killed_at_once() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    // The repository's fake: answers without having started, and the same
    // request again and again.
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let Some((worker, events, _dir)) = open_evil(
        std::path::Path::new(repo_fake),
        "pages",
        Duration::from_millis(30),
    ) else {
        return;
    };
    let before = rss_mb();
    assert!(matches!(next(&events), Event::Opened(_)));
    let began = Instant::now();
    let pid = worker.child_id();
    worker.want(vec![(0, 4.0)]);
    wait_until("the flooding child was not killed", 2, || !alive(pid));
    let killed_after = began.elapsed();
    let (count, pages) = until_stopped(&events);
    let growth = rss_mb().saturating_sub(before);
    eprintln!(
        "[pages flood] killed after {killed_after:?}, {count} events up to Stopped ({pages} pages), RSS growth {growth} MB"
    );
    assert_eq!(pages, 0);
    assert!(growth < 300, "RSS grew by {growth} MB");
}

#[test]
fn a_request_answered_twice_ends_the_child() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let script = evil_script(&tmp);
    let Some((worker, events, _dir)) = open_evil(&script, "twice", Duration::from_millis(30))
    else {
        return;
    };
    let before = rss_mb();
    assert!(matches!(next(&events), Event::Opened(_)));
    let began = Instant::now();
    let pid = worker.child_id();
    // A page of 1000 x 1000 points at 4: 64 MB, answered legally once.
    worker.want(vec![(0, 4.0)]);
    wait_until("the child was not killed", 5, || !alive(pid));
    let killed_after = began.elapsed();
    let (count, pages) = until_stopped(&events);
    let growth = rss_mb().saturating_sub(before);
    eprintln!(
        "[answered twice] killed after {killed_after:?}, {count} events up to Stopped ({pages} pages), RSS growth {growth} MB"
    );
    // One page per child at most, and the tab stops after the restarts.
    assert!(pages <= 10, "{pages} pages");
    assert!(growth < 300, "RSS grew by {growth} MB");
}

#[test]
fn dropping_the_worker_ends_a_child_that_floods_valid_pages() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let script = evil_script(&tmp);
    // A busy consumer, and a child with 300 legal answers of 16 MB (4.8 GB
    // in all) ready to send.
    let Some((worker, events, _dir)) = open_evil(&script, "valid", Duration::from_millis(100))
    else {
        return;
    };
    let before = rss_mb();
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want((0..300).map(|k| (0, 2.0 + k as f32 * 0.001)).collect());
    let mut taken = 0;
    while taken < 4 {
        if matches!(next(&events), Event::Page(_)) {
            taken += 1;
        }
    }
    std::thread::sleep(Duration::from_secs(1));
    let during = rss_mb().saturating_sub(before);
    let dropped = Instant::now();
    drop(worker);
    wait_until("the child outlived its tab", 2, || !alive(pid));
    eprintln!(
        "[valid flood] RSS growth while flooding {during} MB, child gone {:?} after the drop",
        dropped.elapsed()
    );
    assert!(during < 300, "RSS grew by {during} MB");
}

#[test]
fn an_answer_for_a_page_that_does_not_exist_or_was_not_asked_for_ends_the_child() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let script = evil_script(&tmp);
    for mode in [
        "start_range",
        "start_unasked",
        "page_range",
        "failed_unasked",
    ] {
        let Some((worker, events, _dir)) = open_evil(&script, mode, Duration::ZERO) else {
            return;
        };
        assert!(matches!(next(&events), Event::Opened(_)), "{mode}");
        let pid = worker.child_id();
        // Only page 0 is asked for.
        worker.want(vec![(0, 1.0)]);
        wait_until(&format!("{mode}: the child was not killed"), 2, || {
            !alive(pid)
        });
        if mode == "failed_started" {
            // Its first failure was a legal one: reported, and the next
            // child has nothing to render.
            assert!(matches!(next(&events), Event::PageFailed(0)), "{mode}");
        } else {
            let (_, pages) = until_stopped(&events);
            assert_eq!(pages, 0, "{mode}");
        }
    }
}

#[test]
fn an_honest_answer_to_an_old_request_is_not_refused() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let script = evil_script(&tmp);
    let read = tmp.path().join("read");
    let mode = format!("old={}", read.display());
    let Some((worker, events, _dir)) = open_evil(&script, &mode, Duration::ZERO) else {
        return;
    };
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want(vec![(0, 1.0)]);
    // The child has the first request before the newer ones come.
    wait_until("the child did not read the request", 5, || read.exists());
    // Forty newer requests, then the one that makes the child answer.
    for k in 0..40 {
        worker.want(vec![(1, 1.0 + k as f32 * 0.01)]);
    }
    worker.want(vec![(2, 1.0)]);
    assert!(matches!(next(&events), Event::Page(p) if p.page == 0));
    assert!(alive(pid), "the honest child was killed");
    assert_eq!(worker.child_id(), pid);
}

#[test]
fn a_child_that_lied_does_not_make_its_page_fail() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let script = evil_script(&tmp);
    let marker = tmp.path().join("lied");
    let mode = format!("garbage_once={}", marker.display());
    let Some((worker, events, _dir)) = open_evil(&script, &mode, Duration::ZERO) else {
        return;
    };
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want(vec![(0, 1.0)]);
    // The first child answers with garbage while on page 0 and is killed;
    // the page is asked of the next, which draws it.
    assert!(matches!(next(&events), Event::Page(p) if p.page == 0));
    wait_until("the first child is still there", 2, || !alive(pid));
}

// Text, the outline and search, with the real child.

/// A document of `pages` pages that each say "Page <n> needle" in
/// Helvetica, with an outline: "Chapter one" (page 0, open) holding
/// "Section 1.1" (page 1), then "Chapter two" (page 2, 90 points up).
fn text_document(pages: usize) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let tree = b.reserve();
    let outlines = b.reserve();
    let font = b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let mut kids = Vec::new();
    for n in 0..pages {
        let content = b.stream(
            "",
            format!("BT /F1 24 Tf 20 50 Td (Page {n} needle) Tj ET").as_bytes(),
        );
        kids.push(b.add(format!(
            "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 300 100] \
             /Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>"
        )));
    }
    let (one, section, two) = (b.reserve(), b.reserve(), b.reserve());
    let page = |n: usize| kids[n.min(pages - 1)];
    b.set(
        one,
        format!(
            "<< /Title (Chapter one) /Parent {outlines} 0 R /Next {two} 0 R /First {section} 0 R \
             /Last {section} 0 R /Count 1 /Dest [{} 0 R /XYZ 0 90 0] >>",
            page(0)
        ),
    );
    b.set(
        section,
        format!(
            "<< /Title (Section 1.1) /Parent {one} 0 R /Dest [{} 0 R /Fit] >>",
            page(1)
        ),
    );
    b.set(
        two,
        format!(
            "<< /Title (Chapter two) /Parent {outlines} 0 R /Prev {one} 0 R \
             /Dest [{} 0 R /XYZ 0 90 0] >>",
            page(2)
        ),
    );
    b.set(
        outlines,
        format!("<< /Type /Outlines /First {one} 0 R /Last {two} 0 R /Count 3 >>"),
    );
    let kids: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    b.set(
        tree,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
    );
    b.set(
        catalog,
        format!("<< /Type /Catalog /Pages {tree} 0 R /Outlines {outlines} 0 R >>"),
    );
    b.finish(catalog)
}

/// Events until `done` says so; the others are returned too.
fn collect_until(events: &Receiver<Event>, mut done: impl FnMut(&Event) -> bool) -> Vec<Event> {
    let mut all = Vec::new();
    loop {
        let event = next(events);
        let end = done(&event);
        all.push(event);
        if end {
            return all;
        }
    }
}

fn opened_text_document(
    pages: usize,
    limits: Limits,
) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    let (worker, events, dir) = open(&text_document(pages), limits);
    assert!(matches!(next(&events), Event::Opened(info) if info.pages.len() == pages));
    (worker, events, dir)
}

#[test]
fn the_text_of_pages_comes_back_with_a_box_for_each_char() {
    let (worker, events, _dir) = opened_text_document(3, Limits::default());
    worker.want_text(vec![2, 0]);
    let mut texts = Vec::new();
    for _ in 0..2 {
        let Event::Text(text) = next(&events) else {
            panic!("no text");
        };
        texts.push(text);
    }
    // In the order asked.
    assert_eq!(texts.iter().map(|t| t.page).collect::<Vec<_>>(), [2, 0]);
    for text in &texts {
        assert!(
            text.text.contains(&format!("Page {} needle", text.page)),
            "{:?}",
            text.text
        );
        assert_eq!(text.boxes.len(), text.text.chars().count());
        assert!(
            !text.text.chars().any(|c| c.is_control() && c != '\n'),
            "{:?}",
            text.text
        );
        let drawn: Vec<_> = text.boxes.iter().flatten().collect();
        assert!(!drawn.is_empty());
        for b in drawn {
            assert!(b.iter().all(|v| v.is_finite()));
            // On the page (300 x 100), left before right, top before bottom.
            assert!(b[0] >= -1.0 && b[2] <= 301.0 && b[0] <= b[2], "{b:?}");
            assert!(b[1] >= -1.0 && b[3] <= 101.0 && b[1] <= b[3], "{b:?}");
        }
    }
    // A page that does not exist is left out; asking again gives it again.
    worker.want_text(vec![9, 1, 1]);
    assert!(matches!(next(&events), Event::Text(t) if t.page == 1));
    quiet(&events);
    worker.want_text(vec![1]);
    assert!(matches!(next(&events), Event::Text(t) if t.page == 1));
}

#[test]
fn a_page_and_texts_asked_together_all_come() {
    let (worker, events, _dir) = opened_text_document(3, Limits::default());
    worker.want_text(vec![0, 1, 2]);
    worker.want(vec![(1, 1.0)]);
    let (mut pages, mut texts) = (0, 0);
    let _ = collect_until(&events, |e| {
        match e {
            Event::Page(_) => pages += 1,
            Event::Text(_) => texts += 1,
            _ => {}
        }
        pages == 1 && texts == 3
    });
}

#[test]
fn the_outline_comes_back_flat_with_targets() {
    let (worker, events, _dir) = opened_text_document(3, Limits::default());
    worker.want_outline();
    let Event::Outline(items) = next(&events) else {
        panic!("no outline");
    };
    let shown: Vec<_> = items
        .iter()
        .map(|i| (i.depth, i.title.as_str(), i.open))
        .collect();
    assert_eq!(
        shown,
        [
            (0, "Chapter one", true),
            (1, "Section 1.1", false),
            (0, "Chapter two", false)
        ]
    );
    let targets: Vec<_> = items.iter().map(|i| i.target.map(|t| t.page)).collect();
    assert_eq!(targets, [Some(0), Some(1), Some(2)]);
    // 90 points up on a page of 100 is 10 from the top.
    let top = items[0].target.unwrap().top.unwrap();
    assert!((top - 10.0).abs() < 1.5, "{top}");
    assert_eq!(items[1].target.unwrap().top, None);
    // A document without one has an empty list.
    let (worker2, events2, _dir2) = open(
        &Builder::document(&[b"0 g 0 0 1 1 re f"]),
        Limits::default(),
    );
    assert!(matches!(next(&events2), Event::Opened(_)));
    worker2.want_outline();
    assert!(matches!(next(&events2), Event::Outline(items) if items.is_empty()));
    drop(worker);
}

/// What a search reported: its hits (page, start, len) and its end (total,
/// truncated).
type SearchResults = (Vec<(usize, u32, u32)>, Option<(usize, bool)>);

/// Every FindHits of the events: (page, start, len), and the end.
fn search_results(events: &[Event], search: u64) -> SearchResults {
    let mut hits = Vec::new();
    let mut end = None;
    for event in events {
        match event {
            Event::FindHits { search: s, hits: h } if *s == search => {
                hits.extend(h.iter().map(|h| (h.page, h.start, h.len)));
            }
            Event::FindDone {
                search: s,
                total,
                truncated,
            } if *s == search => end = Some((*total, *truncated)),
            _ => {}
        }
    }
    (hits, end)
}

fn is_done(search: u64) -> impl FnMut(&Event) -> bool {
    move |e| matches!(e, Event::FindDone { search: s, .. } if *s == search)
}

#[test]
fn a_search_reports_its_hits_in_page_order_and_ends() {
    let (worker, events, _dir) = opened_text_document(3, Limits::default());
    worker.want_text(vec![0, 1, 2]);
    let mut texts = std::collections::HashMap::new();
    for _ in 0..3 {
        let Event::Text(t) = next(&events) else {
            panic!("no text");
        };
        texts.insert(t.page, t);
    }
    worker.find(7, "NEEDLE".into(), false);
    let all = collect_until(&events, is_done(7));
    let (hits, end) = search_results(&all, 7);
    assert_eq!(end, Some((3, false)));
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [0, 1, 2]);
    for (page, start, len) in hits {
        let chars: String = texts[&page]
            .text
            .chars()
            .skip(start as usize)
            .take(len as usize)
            .collect();
        assert_eq!(chars, "needle");
    }
    for event in &all {
        if let Event::FindHits { hits, .. } = event {
            assert!(
                hits.iter()
                    .all(|h| !h.boxes.is_empty() && h.boxes.len() <= 256)
            );
        }
    }
    // Case matters when it is asked to.
    worker.find(8, "NEEDLE".into(), true);
    let all = collect_until(&events, is_done(8));
    assert_eq!(search_results(&all, 8), (vec![], Some((0, false))));
    worker.find(9, "Page 1".into(), true);
    let all = collect_until(&events, is_done(9));
    let (hits, end) = search_results(&all, 9);
    assert_eq!((hits.len(), end), (1, Some((1, false))));
    assert_eq!(hits[0].0, 1);
}

#[test]
fn a_search_stops_at_the_hit_limit_and_says_so() {
    let limits = Limits {
        max_find_hits: 2,
        ..Limits::default()
    };
    let (worker, events, _dir) = opened_text_document(5, limits);
    worker.find(1, "needle".into(), false);
    let all = collect_until(&events, is_done(1));
    let (hits, end) = search_results(&all, 1);
    assert_eq!(hits.len(), 2);
    assert_eq!(end, Some((2, true)));
}

#[test]
fn an_empty_or_too_long_query_finds_nothing_at_once() {
    let limits = Limits {
        max_query_chars: 8,
        ..Limits::default()
    };
    let (worker, events, _dir) = opened_text_document(2, limits);
    worker.find(1, String::new(), false);
    assert!(matches!(
        next(&events),
        Event::FindDone {
            search: 1,
            total: 0,
            truncated: false
        }
    ));
    // Nine chars against a limit of eight; the child never hears of it.
    worker.find(2, "needle!!!".into(), false);
    assert!(matches!(
        next(&events),
        Event::FindDone {
            search: 2,
            total: 0,
            truncated: false
        }
    ));
    worker.find(3, "needle".into(), false);
    let all = collect_until(&events, is_done(3));
    assert_eq!(search_results(&all, 3).1, Some((2, false)));
}

#[test]
fn a_search_that_is_cancelled_or_replaced_reports_nothing_more() {
    let (worker, events, _dir) = opened_text_document(300, Limits::default());
    worker.find(1, "needle".into(), false);
    worker.cancel_find();
    // What came before the cancel was read may be there; no end for it.
    std::thread::sleep(Duration::from_millis(1500));
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        seen.iter().all(|e| !matches!(e, Event::FindDone { .. })),
        "{seen:?}"
    );
    worker.find(2, "needle".into(), false);
    worker.find(3, "Page 7 ".into(), true);
    let all = collect_until(&events, is_done(3));
    assert!(
        !all.iter()
            .any(|e| matches!(e, Event::FindDone { search: 2, .. })),
        "the replaced search ended"
    );
    let (hits, end) = search_results(&all, 3);
    assert_eq!(end.map(|e| e.1), Some(false));
    // "Page 7 " is page 7 only, and "Page 70 ".. do not match: the space.
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [7]);
}

#[test]
fn a_child_killed_in_the_middle_is_replaced_and_the_work_goes_on() {
    let (worker, events, _dir) = opened_text_document(300, Limits::default());
    worker.want_text(vec![5]);
    worker.want_outline();
    let pid = worker.child_id();
    // SAFETY: only a signal to the child of this test.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let mut got_text = false;
    let mut got_outline = false;
    let mut found = Vec::new();
    // (Whatever the first child had done is not done twice.)
    let all = collect_until(&events, |e| {
        got_text |= matches!(e, Event::Text(t) if t.page == 5);
        got_outline |= matches!(e, Event::Outline(_));
        got_text && got_outline
    });
    drop(all);
    worker.find(1, "needle".into(), false);
    let mut killed = false;
    let all = collect_until(&events, |e| {
        if matches!(e, Event::FindHits { .. }) && !killed {
            killed = true;
            let pid = worker.child_id();
            // SAFETY: as above.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        matches!(e, Event::FindDone { .. })
    });
    let (hits, end) = search_results(&all, 1);
    found.extend(hits.iter().map(|h| h.0));
    let mut sorted = found.clone();
    sorted.sort_unstable();
    sorted.dedup();
    // No page twice, in order; and what the end says is what was reported.
    assert_eq!(found, sorted, "a page was reported twice or out of order");
    assert_eq!(end.map(|e| e.0), Some(found.len()));
    assert!(found.len() >= 290, "{} of 300 pages", found.len());
}

// A compromised child that sends text, an outline or search results it
// should not.

/// Run `mode` of the repository's fake child: it is asked by `ask` once the
/// document is open; the child must be killed quickly and the application
/// must not be flooded. With `to_the_end` the events are read up to the
/// `Stopped` that the tab ends in after repeated misbehaviour (the fake
/// misbehaves again in each child that is started, which takes a while);
/// otherwise the next moments' events. Returns the events.
fn compromised(
    mode: &str,
    limits: Limits,
    to_the_end: bool,
    ask: impl FnOnce(&Worker),
) -> Vec<Event> {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let Some((worker, events, _dir)) = open_evil_with(
        std::path::Path::new(repo_fake),
        mode,
        Duration::ZERO,
        limits,
    ) else {
        return Vec::new();
    };
    let before = rss_mb();
    assert!(matches!(next(&events), Event::Opened(_)), "{mode}");
    let began = Instant::now();
    let pid = worker.child_id();
    ask(&worker);
    wait_until(&format!("{mode}: the child was not killed"), 5, || {
        !alive(pid)
    });
    let killed_after = began.elapsed();
    let mut all = Vec::new();
    if to_the_end {
        loop {
            let event = events
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("{mode}: never stopped"));
            let stopped = matches!(event, Event::Stopped);
            all.push(event);
            if stopped {
                break;
            }
        }
        wait_until("a child is left", 5, || worker.child_id() == 0);
    } else {
        let until = Instant::now() + Duration::from_millis(300);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if let Ok(event) = events.recv_timeout(left) {
                all.push(event);
            }
        }
    }
    let growth = rss_mb().saturating_sub(before);
    eprintln!(
        "[{mode}] killed after {killed_after:?}, {} events, RSS growth {growth} MB",
        all.len()
    );
    assert!(growth < 300, "{mode}: RSS grew by {growth} MB");
    assert!(all.len() < 50, "{mode}: {} events", all.len());
    all
}

/// Nothing of what the child said came through.
fn nothing_came_through(mode: &str, events: &[Event]) {
    for event in events {
        assert!(
            matches!(event, Event::Stopped),
            "{mode}: an event from a lying child: {event:?}"
        );
    }
}

#[test]
fn text_that_breaks_a_limit_never_reaches_the_application() {
    for mode in [
        "text_oversized",
        "text_chars_over",
        "text_mismatch",
        "text_nan",
        "text_utf8",
        "text_wrong_page",
        "text_out_of_range",
        "text_without_start",
        "text_flood",
    ] {
        let events = compromised(mode, Limits::default(), false, |w| w.want_text(vec![0]));
        nothing_came_through(mode, &events);
    }
    // Text nobody asked for.
    let events = compromised("text_unasked", Limits::default(), false, |_| {});
    nothing_came_through("text_unasked", &events);
}

#[test]
fn a_text_answered_twice_ends_the_child_after_the_first() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let Some((worker, events, _dir)) = open_evil_with(
        std::path::Path::new(repo_fake),
        "text_twice",
        Duration::ZERO,
        Limits::default(),
    ) else {
        return;
    };
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want_text(vec![0]);
    assert!(matches!(next(&events), Event::Text(t) if t.page == 0 && t.text == "a"));
    wait_until("the child was not killed", 5, || !alive(pid));
    // The one answer was the asked one: no more of them.
    std::thread::sleep(Duration::from_millis(500));
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, Event::Text(_)), "{event:?}");
    }
}

#[test]
fn an_outline_that_breaks_a_limit_never_reaches_the_application() {
    for mode in [
        "outline_huge",
        "outline_many",
        "outline_deep",
        "outline_title",
        "outline_target",
    ] {
        let events = compromised(mode, Limits::default(), false, |w| w.want_outline());
        nothing_came_through(mode, &events);
    }
    let events = compromised("outline_unasked", Limits::default(), false, |_| {});
    nothing_came_through("outline_unasked", &events);
}

#[test]
fn search_results_that_do_not_belong_never_reach_the_application() {
    let few = Limits {
        max_find_hits: 5,
        ..Limits::default()
    };
    for (mode, limits) in [
        ("find_wrong_search", Limits::default()),
        ("find_hits_wrong_search", Limits::default()),
        ("find_unknown_search", Limits::default()),
        ("find_hits_in_a_frame", few),
        ("find_boxes_in_a_hit", Limits::default()),
        ("find_nan", Limits::default()),
        ("find_descending", Limits::default()),
        ("find_page_range", Limits::default()),
        ("find_done_early", Limits::default()),
        ("find_done_total", Limits::default()),
        ("find_flood", Limits::default()),
    ] {
        let events = compromised(mode, limits, false, |w| w.find(5, "x".into(), false));
        if mode == "find_done_total" {
            // All three pages were reported as searched (honestly) before
            // the lie about the total: the next child has nothing left to
            // search, and the parent ends the search with what it counted,
            // not with what the child said (7).
            assert!(
                matches!(
                    events.as_slice(),
                    [Event::FindDone {
                        total: 0,
                        truncated: false,
                        ..
                    }]
                ),
                "{events:?}"
            );
            continue;
        }
        nothing_came_through(mode, &events);
    }
}

#[test]
fn more_hits_than_the_limit_ends_the_child_after_the_ones_that_fit() {
    let few = Limits {
        max_find_hits: 5,
        ..Limits::default()
    };
    // Three hits are within the limit and come through; three more are not.
    // The next child goes on after the page that was reported, finds the
    // same lie, and the tab ends.
    let events = compromised("find_hits_over_total", few, true, |w| {
        w.find(5, "x".into(), false)
    });
    let hits: usize = events
        .iter()
        .map(|e| match e {
            Event::FindHits { hits, .. } => hits.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(hits, 3, "{events:?}");
    assert!(matches!(events.last(), Some(Event::Stopped)));
}

#[test]
fn repeated_misbehaviour_ends_the_tab_for_text_outline_and_search() {
    for (mode, ask) in [
        ("text_oversized", 0),
        ("outline_huge", 1),
        ("find_wrong_search", 2),
    ] {
        let events = compromised(mode, Limits::default(), true, |w| match ask {
            0 => w.want_text(vec![0]),
            1 => w.want_outline(),
            _ => w.find(5, "x".into(), false),
        });
        nothing_came_through(mode, &events);
        assert!(matches!(events.last(), Some(Event::Stopped)), "{mode}");
    }
}

#[test]
fn an_outline_or_search_answered_twice_ends_the_child_after_the_first() {
    for (mode, ask) in [("outline_twice", 0), ("find_done_twice", 1)] {
        let events = compromised(mode, Limits::default(), false, |w| match ask {
            0 => w.want_outline(),
            _ => w.find(5, "x".into(), false),
        });
        // The first, the asked-for answer, came through: nothing more.
        assert_eq!(events.len(), 1, "{mode}: {events:?}");
        assert!(
            matches!(&events[0], Event::Outline(items) if items.is_empty())
                || matches!(&events[0], Event::FindDone { total: 0, .. }),
            "{mode}: {events:?}"
        );
    }
}

#[test]
fn a_replaced_search_may_finish_its_page_and_is_not_blamed() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let Some((worker, events, _dir)) = open_evil_with(
        std::path::Path::new(repo_fake),
        "find_replaced",
        Duration::ZERO,
        Limits::default(),
    ) else {
        return;
    };
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.find(1, "x".into(), false);
    std::thread::sleep(Duration::from_millis(500));
    // The child is on page 0 of search 1; the application stops it, and
    // the child's answer comes after.
    worker.cancel_find();
    std::thread::sleep(Duration::from_millis(1000));
    assert!(alive(pid), "an honest answer to a search that was stopped");
    assert_eq!(worker.child_id(), pid);
    quiet(&events);
}

#[test]
fn text_outline_and_search_steps_are_timed_like_pages() {
    let _lock = FLOODS.lock().unwrap_or_else(|e| e.into_inner());
    let repo_fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake/fake_child.py");
    let limits = Limits {
        page_time: Duration::from_millis(700),
        ..Limits::default()
    };
    let open_mode = |mode: &str| {
        open_evil_with(
            std::path::Path::new(repo_fake),
            mode,
            Duration::ZERO,
            limits,
        )
    };
    // A text that never comes: killed, and the text is empty.
    let Some((worker, events, _dir)) = open_mode("text_hang") else {
        return;
    };
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    let began = Instant::now();
    worker.want_text(vec![0]);
    assert!(matches!(
        next(&events),
        Event::Text(t) if t.page == 0 && t.text.is_empty() && t.boxes.is_empty()
    ));
    assert!(began.elapsed() < Duration::from_secs(10));
    wait_until("the hanging child is still there", 5, || !alive(pid));
    // The same for the outline.
    let (worker, events, _dir) = open_mode("outline_hang").unwrap();
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.want_outline();
    assert!(matches!(next(&events), Event::Outline(items) if items.is_empty()));
    wait_until("the hanging child is still there", 5, || !alive(pid));
    // A search: the page it hung on is skipped and the next child goes on
    // at the page after it.
    let (worker, events, _dir) = open_mode("find_hang").unwrap();
    assert!(matches!(next(&events), Event::Opened(_)));
    let pid = worker.child_id();
    worker.find(4, "x".into(), false);
    assert!(matches!(
        next(&events),
        Event::FindDone {
            search: 4,
            total: 0,
            truncated: false
        }
    ));
    wait_until("the hanging child is still there", 5, || !alive(pid));
}

// Pages whose text blows the child's memory, as in a reviewer's file: many
// pages that share one content stream of millions of glyphs.

/// `bad` pages sharing one huge text content stream, then `good` ordinary
/// pages that say "Page <n> needle".
fn heavy_document(bad: usize, good: usize, glyphs: usize) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let tree = b.reserve();
    let font = b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let mut huge = b"BT /F1 6 Tf 0 50 Td (".to_vec();
    huge.extend(std::iter::repeat_n(b'a', glyphs));
    huge.extend_from_slice(b") Tj ET");
    let shared = b.stream("/Filter /FlateDecode", &builder::deflate(&huge));
    let resources = format!("/Resources << /Font << /F1 {font} 0 R >> >>");
    let mut kids = String::new();
    for _ in 0..bad {
        let page = b.add(format!(
            "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 300 100] {resources} \
             /Contents {shared} 0 R >>"
        ));
        kids.push_str(&format!("{page} 0 R "));
    }
    for n in 0..good {
        let content = b.stream(
            "",
            format!("BT /F1 24 Tf 20 50 Td (Page {n} needle) Tj ET").as_bytes(),
        );
        let page = b.add(format!(
            "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 300 100] {resources} \
             /Contents {content} 0 R >>"
        ));
        kids.push_str(&format!("{page} 0 R "));
    }
    b.set(
        tree,
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", bad + good),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    b.finish(catalog)
}

fn small_memory() -> Limits {
    Limits {
        max_memory_bytes: 160 << 20,
        page_time: Duration::from_secs(20),
        ..Limits::default()
    }
}

/// Children seen while `done` is false: the distinct process ids of the
/// worker's child, polled while events are read.
fn children_until(
    worker: &Worker,
    events: &Receiver<Event>,
    pids: &mut std::collections::BTreeSet<u32>,
    mut done: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let mut all = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        // 0 means no child at this instant: between one child and the next.
        let pid = worker.child_id();
        if pid != 0 {
            pids.insert(pid);
        }
        match events.recv_timeout(Duration::from_millis(5)) {
            Ok(event) => {
                let end = done(&event);
                all.push(event);
                if end {
                    return all;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return all,
        }
        assert!(Instant::now() < deadline, "no end within two minutes");
    }
}

fn heavy(bad: usize, good: usize) -> (Worker, Receiver<Event>, tempfile::TempDir) {
    let (worker, events, dir) = open(&heavy_document(bad, good, 6_000_000), small_memory());
    assert!(matches!(next(&events), Event::Opened(info) if info.pages.len() == bad + good));
    (worker, events, dir)
}

/// Run search `id` to its end (or to the worker's end). Returns what it
/// found and how it ended, or `None` for `Stopped`.
fn search_to_the_end(
    worker: &Worker,
    events: &Receiver<Event>,
    pids: &mut std::collections::BTreeSet<u32>,
    id: u64,
) -> Option<SearchResults> {
    worker.find(id, "needle".into(), false);
    let all = children_until(worker, events, pids, |e| {
        matches!(e, Event::Stopped) || is_done(id)(e)
    });
    if all.iter().any(|e| matches!(e, Event::Stopped)) {
        return None;
    }
    Some(search_results(&all, id))
}

#[test]
fn pages_whose_text_kills_the_child_are_skipped_by_later_searches() {
    // Pages 0 and 1 kill the child; 2 to 5 are fine.
    let (worker, events, _dir) = heavy(2, 4);
    let mut pids = std::collections::BTreeSet::new();
    let began = Instant::now();
    let (hits, end) = search_to_the_end(&worker, &events, &mut pids, 1).unwrap();
    assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), [2, 3, 4, 5]);
    assert_eq!(end, Some((4, false)));
    // The first and one new child for each page that killed one.
    assert_eq!(pids.len(), 3, "{pids:?}");
    // Incremental search: five more, none ends a child.
    for id in 2..7 {
        let (hits, end) = search_to_the_end(&worker, &events, &mut pids, id).unwrap();
        assert_eq!(hits.len(), 4, "search {id}");
        assert_eq!(end, Some((4, false)));
    }
    assert_eq!(pids.len(), 3, "a later search killed a child: {pids:?}");
    assert!(began.elapsed() < Duration::from_secs(30));

    // The text of a page known to be broken is empty at once, and nothing
    // is started for it.
    let pid = worker.child_id();
    worker.want_text(vec![1, 0, 3]);
    let mut texts = Vec::new();
    while texts.len() < 3 {
        if let Event::Text(t) = next(&events) {
            texts.push(t);
        }
    }
    for t in &texts {
        assert_eq!(t.text.is_empty(), t.page < 2, "page {}", t.page);
    }
    assert!(
        texts
            .iter()
            .any(|t| t.page == 3 && t.text.contains("needle"))
    );
    assert_eq!(worker.child_id(), pid);
    quiet(&events);
}

#[test]
fn a_search_gives_up_after_a_few_pages_that_end_children() {
    let (worker, events, _dir) = heavy(40, 4);
    let mut pids = std::collections::BTreeSet::new();
    let (hits, end) = search_to_the_end(&worker, &events, &mut pids, 1).unwrap();
    assert!(hits.is_empty());
    // Marked: it did not look at every page.
    assert_eq!(end, Some((0, true)));
    assert!((3..=4).contains(&pids.len()), "{pids:?}");
}

#[test]
fn a_document_whose_text_keeps_killing_children_ends_in_stopped_though_pages_render() {
    let (worker, events, _dir) = heavy(40, 4);
    let mut pids = std::collections::BTreeSet::new();
    let began = Instant::now();
    let mut stopped = false;
    for id in 1..=10 {
        // Pages render in between: it must not start the count over.
        worker.want(vec![(41, 0.5)]);
        let all = children_until(&worker, &events, &mut pids, |e| {
            matches!(e, Event::Page(_) | Event::Stopped)
        });
        if matches!(all.last(), Some(Event::Stopped)) {
            stopped = true;
            break;
        }
        if search_to_the_end(&worker, &events, &mut pids, id).is_none() {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "never stopped; {} children", pids.len());

    assert!(pids.len() <= 10, "{pids:?}");
    assert!(began.elapsed() < Duration::from_secs(60));
    // Nothing starts again.
    worker.find(99, "needle".into(), false);
    worker.want_text(vec![41]);
    worker.want(vec![(42, 1.0)]);
    worker.want_outline();
    quiet(&events);
}
