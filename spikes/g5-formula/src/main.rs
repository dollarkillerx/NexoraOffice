//! Phase 0 gate G5: formula engines.
//!
//! g5-formula bench [--only <engine>] [--workload <name>] [--edits <n>] [--scale <n>]
//!                                    every engine on every workload; scale 10
//!                                    is a million formulas
//! g5-formula answers                 a set of small formulas whose results in
//!                                    Excel are known, through every engine
//! g5-formula write <dir>             the three workbooks as XLSX files, to open
//!                                    in Excel and compare
//!
//! Each engine runs each workload in a process of its own, so that one that
//! crashes or never returns does not take the others with it, and so that the
//! memory reading belongs to one workbook in one engine.

// In a build without any engine, parts of the interface have no user.
#![cfg_attr(
    not(any(
        feature = "ironcalc",
        feature = "formualizer",
        feature = "betteroffice"
    )),
    allow(dead_code)
)]

mod answers;
mod bench;
mod engine;
mod workload;
mod xlsx;

use std::io::Read;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use bench::Report;

const USAGE: &str =
    "usage: g5-formula bench [--only <engine>] [--workload <name>] [--edits <n>] [--scale <n>]
       g5-formula answers
       g5-formula write <dir>";

/// The gate: recalculation after one edit, 95th percentile.
const LIMIT_MS: f64 = 100.0;
/// How long one engine may spend on the edits of one workload.
const EDIT_BUDGET: Duration = Duration::from_secs(60);
/// How long a child process may run in all before it is stopped.
const CHILD_LIMIT: Duration = Duration::from_secs(600);

fn slug(name: &str) -> String {
    name.replace(' ', "-")
}

/// Run in this process and print the report as one line.
fn one(args: &[String]) -> Result<(), String> {
    let [name, workload, edits, scale] = args else {
        return Err(USAGE.into());
    };
    let edits: usize = edits.parse().map_err(|_| USAGE)?;
    let scale: u32 = scale.parse().map_err(|_| USAGE)?;
    let candidate = engine::all()
        .into_iter()
        .find(|candidate| slug(candidate.name) == *name)
        .ok_or_else(|| format!("no engine called `{name}` in this build"))?;
    let workload = workload::by_name(workload, edits, scale)
        .ok_or_else(|| format!("no workload called `{workload}`"))?;
    let report = bench::run(&candidate, &workload, scale, EDIT_BUDGET)?;
    println!("{}", report.encode());
    Ok(())
}

enum Outcome {
    Done(Report),
    Failed(String),
}

fn child(name: &str, workload: &str, edits: usize, scale: u32) -> Outcome {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return Outcome::Failed(err.to_string()),
    };
    let spawned = Command::new(exe)
        .args([
            "one",
            &slug(name),
            workload,
            &edits.to_string(),
            &scale.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => return Outcome::Failed(err.to_string()),
    };
    // Read both pipes while the child runs: an importer that prints a line
    // per cell fills a pipe long before it is done.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_string(&mut text).ok();
            }
            text
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > CHILD_LIMIT => {
                child.kill().ok();
                child.wait().ok();
                return Outcome::Failed(format!(
                    "stopped after {} s without finishing",
                    CHILD_LIMIT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => return Outcome::Failed(err.to_string()),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    let report = out.lines().last().and_then(Report::decode);
    match (status.success(), report) {
        (true, Some(report)) => Outcome::Done(report),
        _ => {
            let last = err.lines().rev().find(|line| !line.trim().is_empty());
            Outcome::Failed(format!("{status}: {}", last.unwrap_or("no message")))
        }
    }
}

fn bench(args: &[String]) -> Result<(), String> {
    let mut only: Option<String> = None;
    let mut workloads: Vec<&str> = workload::NAMES.to_vec();
    let mut edits = 100_usize;
    let mut scale = 1_u32;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--only" => only = Some(slug(value)),
            "--workload" => {
                let name = workload::NAMES
                    .into_iter()
                    .find(|name| name == value)
                    .ok_or_else(|| format!("no workload called `{value}`"))?;
                workloads = vec![name];
            }
            "--edits" => edits = value.parse().map_err(|_| USAGE)?,
            "--scale" => {
                scale = value
                    .parse()
                    .ok()
                    .filter(|scale| (1..=10).contains(scale))
                    .ok_or(USAGE)?
            }
            _ => return Err(USAGE.into()),
        }
    }
    let candidates: Vec<_> = engine::all()
        .into_iter()
        .filter(|candidate| {
            only.as_ref()
                .is_none_or(|only| slug(candidate.name) == *only)
        })
        .collect();
    if candidates.is_empty() {
        return Err("no such engine in this build".into());
    }

    for name in workloads {
        let workload = workload::by_name(name, 0, scale).ok_or("unknown workload")?;
        println!(
            "{name}: {} formulas, one edit reaches {}",
            workload.book.formula_count(),
            workload.reach
        );
        println!(
            "  {:16} {:>9} {:>9} {:>6} {:>9} {:>9} {:>9} {:>6} {:>8}  gate",
            "engine",
            "load ms",
            "first ms",
            "edits",
            "p50 ms",
            "p95 ms",
            "max ms",
            "wrong",
            "mem MB"
        );
        for candidate in &candidates {
            match child(candidate.name, name, edits, scale) {
                Outcome::Failed(why) => println!("  {:16} FAILED: {why}", candidate.name),
                Outcome::Done(report) => {
                    let correct = report.wrong == 0 && report.first_wrong.is_none();
                    let gate = match (correct, report.p95_ms <= LIMIT_MS) {
                        (false, _) => "WRONG RESULTS",
                        (true, true) => "within 100 ms",
                        (true, false) => "over 100 ms",
                    };
                    println!(
                        "  {:16} {:>9.1} {:>9.1} {:>6} {:>9.3} {:>9.3} {:>9.3} {:>6} {:>8}  {gate}",
                        candidate.name,
                        report.load_ms,
                        report.first_ms,
                        report.edits,
                        report.p50_ms,
                        report.p95_ms,
                        report.max_ms,
                        report.wrong,
                        report
                            .resident_mb
                            .map_or("-".to_owned(), |mb| format!("{mb:.0}")),
                    );
                    if let Some(what) = &report.first_wrong {
                        println!("  {:16} {what}", "");
                    }
                    if let Some(note) = &report.note {
                        println!("  {:16} note: {note}", "");
                    }
                }
            }
        }
        println!();
    }
    println!("engines");
    for candidate in &candidates {
        println!("  {:16} {}", candidate.name, candidate.about);
    }
    Ok(())
}

/// The workbooks as files, and what a few of their cells must show once Excel
/// has calculated them. The files carry wrong cached results on purpose and ask
/// for a full calculation on load.
fn write(args: &[String]) -> Result<(), String> {
    let [dir] = args else {
        return Err(USAGE.into());
    };
    std::fs::create_dir_all(dir).map_err(|err| format!("{dir}: {err}"))?;
    for name in workload::NAMES {
        let workload = workload::by_name(name, 0, 1).ok_or("unknown workload")?;
        let path = std::path::Path::new(dir).join(format!("{name}.xlsx"));
        std::fs::write(&path, xlsx::write(&workload.book))
            .map_err(|err| format!("{}: {err}", path.display()))?;
        println!("{}", path.display());
        for (at, expected) in &workload.initial {
            let sheet = workload.book.sheets[at.sheet].name;
            match expected {
                workload::Expected::Number(value) => println!("  {sheet}!{} = {value}", at.a1()),
                workload::Expected::Text(value) => println!("  {sheet}!{} = {value}", at.a1()),
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "bench" => bench(rest),
        Some((command, rest)) if command == "write" => write(rest),
        Some((command, [])) if command == "answers" => {
            print!("{}", answers::table(&answers::engines()));
            Ok(())
        }
        Some((command, rest)) if command == "one" => one(rest),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            ExitCode::FAILURE
        }
    }
}
