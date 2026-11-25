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
        arguments: vec!["-c".into(), script.into(), "fake".into()],
    }
}

/// The frames the stand-in sends, as `printf` formats them: a one-page
/// document of 200 x 100 points, and page 0 started.
const FAKE_OPENED: &str =
    r"printf 'O\001\000\000\000\000\000\110\103\000\000\310\102\000'; printf 'S\000\000\000\000';";
/// Wait until the start frame (53 bytes) and a request for one page (13)
/// have arrived.
const FAKE_WAIT_FOR_WANT: &str = "head -c 60 >/dev/null;";

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
    format!("{FAKE_OPENED} {FAKE_WAIT_FOR_WANT} printf '{page}'; exec sleep 30")
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
    assert!(matches!(next(&events), Event::PageFailed(0)));
    wait_until("the child is still running", 5, || !alive(pid));
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
    assert!(matches!(next(&events), Event::PageFailed(0)));
    wait_until("the child is still running", 5, || !alive(pid));
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
        "if [ -e '{m}' ]; then exec sleep 60; fi; : > '{m}'; {FAKE_OPENED} {FAKE_WAIT_FOR_WANT} exit 1",
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
    let script = format!("{FAKE_OPENED} exec sleep 60");
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
