//! How the Markdown editor copes with large files.
//!
//! ```sh
//! cargo run --release -p aster-shell --example large_files              # 1, 10 and 100 MB
//! cargo run --release -p aster-shell --example large_files -- --sizes 1,10
//! cargo run --release -p aster-shell --example large_files -- --timeout 300 --out /tmp/big
//! ```
//!
//! For each size it writes two Markdown files, unless they are already there:
//! one of mixed Chinese, Japanese and English paragraphs with headings, lists,
//! code and links, and one that is a single long line. The files go to
//! `target/large-files` (never into the repository).
//!
//! Every file is measured in a process of its own, so a case that runs out of
//! time or memory is reported and the others still run. The measurements go
//! through the same code the window uses (`MarkdownEditor`, the file loader,
//! the recovery writer), without a window: "keystroke" is the edit, the
//! highlighting, the recovery step and a full redraw of a 1008 × 700 view at
//! 2× scale; "scroll frame" is a scroll step and a redraw.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aster_shell::content::{self, JournalStep, MarkdownEditor, preview_style, rope_bytes};
use aster_shell::files;
use aster_shell::theme::{ThemeMode, Tokens};
use aster_storage::{RecordWriter, RecoveryRoot};
use aster_surface::Action;
use aster_surface::preview::PreviewBuilder;

const MB: usize = 1024 * 1024;
/// The view: a 1008 × 700 logical pane on a 2× display.
const VIEW: (u16, u16, f32) = (2016, 1400, 2.0);
const KEYSTROKES: usize = 40;
const SCROLL_FRAMES: usize = 200;
/// Physical pixels per scroll step: about what one notch of a wheel moves.
const SCROLL_STEP: f64 = 120.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = args.iter().position(|a| a == "--case") {
        let path = PathBuf::from(&args[at + 1]);
        let preview = args.iter().any(|a| a == "--preview");
        run_case(&path, preview);
        return;
    }
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|at| args.get(at + 1).cloned())
    };
    let sizes: Vec<usize> = value("--sizes")
        .unwrap_or_else(|| "1,10,100".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let timeout = Duration::from_secs(
        value("--timeout")
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    );
    let out = value("--out").map_or_else(default_folder, PathBuf::from);
    std::fs::create_dir_all(&out).expect("cannot create the output folder");

    let mut report = String::new();
    for size in sizes {
        for variant in [Variant::Mixed, Variant::OneLine] {
            let path = out.join(format!("{size}mb-{}.md", variant.name()));
            if std::fs::metadata(&path).map(|m| m.len() as usize).ok() != Some(size * MB) {
                eprintln!("writing {}", path.display());
                std::fs::write(&path, generate(size * MB, variant))
                    .expect("cannot write a test file");
            }
            eprintln!("measuring {}", path.display());
            let preview = size <= 10;
            let result = run_child(&path, preview, timeout);
            let _ = writeln!(report, "## {size} MB, {}\n\n{result}", variant.label());
        }
    }
    println!("{report}");
}

fn default_folder() -> PathBuf {
    // target/ next to the workspace's Cargo.lock.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("../../target/large-files")
}

/// Run one case in a child process; return its measurements as a table.
fn run_child(path: &Path, preview: bool, timeout: Duration) -> String {
    let exe = std::env::current_exe().expect("no path to this program");
    let mut command = Command::new(exe);
    command
        .arg("--case")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if preview {
        command.arg("--preview");
    }
    let mut child = command.spawn().expect("cannot start the measuring process");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("cannot wait for the child") {
            break Some(status);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut output);
    }
    let mut table = String::from("| 项目 | 结果 |\n| --- | --- |\n");
    for line in output.lines() {
        if let Some((name, value)) = line.split_once('\t') {
            let _ = writeln!(table, "| {name} | {value} |");
        }
    }
    match status {
        Some(status) if status.success() => {}
        Some(status) => {
            let _ = writeln!(table, "| 结束 | 异常退出（{status}） |");
        }
        None => {
            let _ = writeln!(table, "| 结束 | 超过 {} 秒，已终止 |", timeout.as_secs());
        }
    }
    table
}

/// Print one measurement for the parent. Flushed at once, so a case that is
/// killed later still reports what it got to.
fn report(name: &str, value: String) {
    use std::io::Write;
    println!("{name}\t{value}");
    let _ = std::io::stdout().flush();
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

fn summary(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let n = samples.len();
    if n == 0 {
        return "—".into();
    }
    let median = samples[n / 2];
    let p95 = samples[(n * 95 / 100).min(n - 1)];
    let max = samples[n - 1];
    format!(
        "中位 {:.2} ms，95% {:.2} ms，最大 {:.2} ms（{n} 次）",
        median.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0
    )
}

fn run_case(path: &Path, preview: bool) {
    let tokens = Tokens::of(ThemeMode::Light);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) as usize;

    // Open as the window does: read, decode and prepare on a worker thread,
    // then build the editor and draw the first screen on the interface thread.
    let started = Instant::now();
    let loaded = files::load(path).expect("cannot read the test file");
    let read = started.elapsed();
    let opened = content::open(loaded);
    let prepared = started.elapsed();
    let ui_started = Instant::now();
    let mut editor = MarkdownEditor::from_prepared(
        opened.prepared,
        opened.format,
        Some(path.to_owned()),
        Some(opened.fingerprint),
        &tokens,
    );
    editor.surface.resize(VIEW.0, VIEW.1, VIEW.2);
    editor.decorate();
    editor.surface.render();
    let ui = ui_started.elapsed();
    report("打开：读文件和解码（工作线程）", ms(read));
    report(
        "打开：准备文本、行长、高亮、字数（工作线程）",
        ms(prepared - read),
    );
    report("打开：建编辑器到首屏画完（界面线程）", ms(ui));
    report("打开：总计", ms(started.elapsed()));
    report("常驻内存（打开后）", format!("{} MB", resident_mb()));

    // Typing at the start, in the middle and at the end.
    let len = editor.document.len();
    for (name, at) in [("文首", 0), ("文中", len / 2), ("文末", len)] {
        let at = aster_markdown::document::floor_char_boundary(editor.document.text(), at);
        editor.place_caret(at);
        let mut samples = Vec::with_capacity(KEYSTROKES);
        for _ in 0..KEYSTROKES {
            let started = Instant::now();
            editor.key(Action::Insert("字".into()));
            let _ = editor.journal_step(|| 1);
            editor.decorate();
            editor.surface.render();
            samples.push(started.elapsed());
        }
        report(&format!("按键到画完（{name}）"), summary(samples));
    }

    // Return at the top: every line after it moves down one.
    editor.place_caret(0);
    let mut samples = Vec::with_capacity(KEYSTROKES);
    for _ in 0..KEYSTROKES {
        let started = Instant::now();
        editor.key(Action::Newline);
        let _ = editor.journal_step(|| 1);
        editor.decorate();
        editor.surface.render();
        samples.push(started.elapsed());
    }
    report("回车到画完（文首）", summary(samples));

    // Scrolling from the top.
    editor.place_caret(0);
    let mut samples = Vec::with_capacity(SCROLL_FRAMES);
    for _ in 0..SCROLL_FRAMES {
        let started = Instant::now();
        editor.surface.scroll_by(SCROLL_STEP);
        editor.decorate();
        editor.surface.render();
        samples.push(started.elapsed());
    }
    report("连续滚动一帧", summary(samples));

    // Saving to another file.
    let target = path.with_extension("saved.md");
    let started = Instant::now();
    let (text, _) = editor.snapshot();
    let copied = started.elapsed();
    let written = Instant::now();
    files::save(&target, &text, &editor.format).expect("cannot save");
    report("保存：界面线程上取全文", ms(copied));
    report("保存：编码和写盘（工作线程）", ms(written.elapsed()));
    drop(text);
    let _ = std::fs::remove_file(&target);

    // Recovery record: the first edit opens it with the whole text, every
    // checkpoint copies the whole text again.
    let folder = std::env::temp_dir().join(format!("aster-large-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let writer = RecordWriter::spawn(RecoveryRoot::new(&folder), |_| {});
    let mut fresh = MarkdownEditor::from_prepared(
        content::prepare(editor.document.text().clone()),
        editor.format,
        None,
        None,
        &tokens,
    );
    drop(editor);
    fresh.surface.resize(VIEW.0, VIEW.1, VIEW.2);
    fresh.place_caret(0);
    fresh.key(Action::Insert("a".into()));
    let started = Instant::now();
    let step = fresh.journal_step(|| 7);
    report("恢复记录：开始（界面线程）", ms(started.elapsed()));
    if let JournalStep::Open(text) = step {
        let started = Instant::now();
        writer.open(
            7,
            Vec::new(),
            aster_storage::Contents::later(move || rope_bytes(&text)),
        );
        writer.flush();
        report("恢复记录：开始（写线程，含刷盘）", ms(started.elapsed()));
    }
    let mut ui = Duration::ZERO;
    let mut checkpoint = None;
    for _ in 0..8192 {
        fresh.key(Action::Insert("a".into()));
        let started = Instant::now();
        let step = fresh.journal_step(|| 7);
        ui = ui.max(started.elapsed());
        if let JournalStep::Append(entries, text) = step {
            for entry in entries {
                writer.append(7, entry);
            }
            if let Some(text) = text {
                checkpoint = Some(ui);
                let started = Instant::now();
                writer.checkpoint(7, aster_storage::Contents::later(move || rope_bytes(&text)));
                writer.flush();
                report("恢复记录：检查点（写线程，含刷盘）", ms(started.elapsed()));
                break;
            }
        }
    }
    match checkpoint {
        Some(ui) => report("恢复记录：检查点（界面线程，最慢一次）", ms(ui)),
        None => report("恢复记录：检查点", "8192 次按键内没有发生".into()),
    }
    drop(writer);
    let _ = std::fs::remove_dir_all(&folder);
    report("常驻内存峰值", format!("{} MB", peak_mb()));

    if preview {
        let started = Instant::now();
        let layout = PreviewBuilder::new().build(
            &fresh.document.text().to_string(),
            VIEW.0 / 2,
            VIEW.2,
            &preview_style(&tokens),
        );
        report("预览：排版全文（工作线程）", ms(started.elapsed()));
        report("预览：文档高度", format!("{:.0} px", layout.height()));
    }
    report("文件大小", format!("{:.1} MB", size as f64 / MB as f64));
}

/// Current resident memory, from `ps`.
fn resident_mb() -> u64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

/// The most memory this process has had resident.
fn peak_mb() -> u64 {
    #[repr(C)]
    struct TimeVal {
        sec: i64,
        usec: i64,
    }
    #[repr(C)]
    struct Usage {
        utime: TimeVal,
        stime: TimeVal,
        maxrss: i64,
        rest: [i64; 13],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Usage) -> i32;
    }
    let mut usage = Usage {
        utime: TimeVal { sec: 0, usec: 0 },
        stime: TimeVal { sec: 0, usec: 0 },
        maxrss: 0,
        rest: [0; 13],
    };
    // SAFETY: `usage` matches `struct rusage` on 64-bit macOS and Linux.
    if unsafe { getrusage(0, &mut usage) } != 0 {
        return 0;
    }
    let bytes = if cfg!(target_os = "macos") {
        usage.maxrss as u64
    } else {
        usage.maxrss as u64 * 1024
    };
    bytes / MB as u64
}

#[derive(Clone, Copy)]
enum Variant {
    Mixed,
    OneLine,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::Mixed => "mixed",
            Variant::OneLine => "one-line",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Variant::Mixed => "中日英混排",
            Variant::OneLine => "只有一行",
        }
    }
}

const SENTENCES: [&str; 12] = [
    "这份季度报告整理了各地区的销售数据和客户反馈，",
    "我们在第三季度完成了新版编辑器的设计评审。",
    "日本語の文章も混ざっています。かなとカタカナ、漢字を含みます。",
    "The quick brown fox jumps over the lazy dog while the team reviews the draft.",
    "**重点**：所有改动都要先在本地跑过检查。",
    "参见 [设计文档](https://example.com/design) 和 `cargo test` 的输出。",
    "회의는 다음 주 화요일에 열립니다.",
    "Performance budgets matter: one frame is about 16 milliseconds at 60 Hz.",
    "*注意*：大文件打开时界面不能冻住。",
    "数据来源包括 www.example.org 上公开的统计。",
    "これはテスト用の段落です。読みやすさを確認します。",
    "Lists, tables and code blocks appear every few paragraphs, as in real notes.",
];

/// Deterministic Markdown of exactly `size` bytes.
fn generate(size: usize, variant: Variant) -> Vec<u8> {
    let mut out = String::with_capacity(size + 4096);
    let mut n: usize = 0;
    let mut next = |count: usize| {
        n = n
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (n >> 33) % count
    };
    let mut section = 0;
    while out.len() < size {
        match variant {
            Variant::OneLine => {
                out.push_str(SENTENCES[next(SENTENCES.len())]);
                out.push(' ');
            }
            Variant::Mixed => {
                if next(20) == 0 {
                    section += 1;
                    let _ = write!(out, "## 第 {section} 节 Section {section}\n\n");
                }
                match next(10) {
                    0 => {
                        for i in 0..3 {
                            let _ =
                                writeln!(out, "- 列表项 {i}：{}", SENTENCES[next(SENTENCES.len())]);
                        }
                        out.push('\n');
                    }
                    1 => {
                        out.push_str(
                            "```rust\nfn main() {\n    println!(\"你好，世界\");\n}\n```\n\n",
                        );
                    }
                    2 => {
                        out.push_str("| 名称 | 数量 |\n|---|---|\n| 苹果 | 3 |\n| 香蕉 | 12 |\n\n");
                    }
                    _ => {
                        for _ in 0..(2 + next(5)) {
                            out.push_str(SENTENCES[next(SENTENCES.len())]);
                        }
                        out.push_str("\n\n");
                    }
                }
            }
        }
    }
    // Cut to the exact size on a character boundary, padding with spaces.
    let mut cut = size.min(out.len());
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    while out.len() < size {
        out.push(' ');
    }
    out.into_bytes()
}
