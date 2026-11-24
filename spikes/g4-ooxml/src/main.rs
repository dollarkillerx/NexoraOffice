//! Phase 0 gate G4: OOXML round trip.
//!
//! g4-ooxml run [--only <candidate>] [dir ...]            the result table
//! g4-ooxml summary [--only <candidate>] [dir ...]        totals, and what is
//!                                                        lost most often
//! g4-ooxml detail <candidate> <resave|edit> <sample> [dir ...]
//!                                                        what one result lost
//! g4-ooxml export <out-dir> [dir ...]                    write the samples and
//!                                                        every candidate's output
//!                                                        as files, to open them
//!                                                        in an office application
//!
//! Without directories the three built-in samples are used. A directory adds
//! every .docx, .xlsx and .pptx below it.

mod candidate;
mod compare;
mod corpus;
mod package;
mod run;
mod validate;
mod xml;

use std::path::Path;
use std::process::ExitCode;

use corpus::Sample;
use run::Scenario;

const USAGE: &str = "usage: g4-ooxml run [--only <candidate>] [dir ...]
       g4-ooxml summary [--only <candidate>] [dir ...]
       g4-ooxml detail <candidate> <resave|edit> <sample> [dir ...]
       g4-ooxml export <out-dir> [dir ...]
       g4-ooxml diff <before> <after>";

fn samples(dirs: &[String]) -> Result<Vec<Sample>, String> {
    let mut samples = corpus::built_in()?;
    for dir in dirs {
        samples.extend(corpus::from_dir(Path::new(dir))?);
    }
    Ok(samples)
}

fn candidates(only: Option<&str>) -> Result<Vec<Box<dyn candidate::Candidate>>, String> {
    let all = candidate::all();
    let Some(only) = only else { return Ok(all) };
    let chosen: Vec<_> = all
        .into_iter()
        .filter(|candidate| candidate.name().replace(' ', "-") == only.replace(' ', "-"))
        .collect();
    if chosen.is_empty() {
        return Err(format!("no candidate is called `{only}` in this build"));
    }
    Ok(chosen)
}

fn run(args: &[String], full: bool) -> Result<(), String> {
    let (only, dirs) = match args {
        [flag, name, dirs @ ..] if flag == "--only" => (Some(name.as_str()), dirs),
        dirs => (None, dirs),
    };
    let outcomes = run::run(&samples(dirs)?, &candidates(only)?);
    if full {
        print!("{}", run::table(&outcomes));
        println!();
    }
    print!("{}", run::totals(&outcomes));
    if !full {
        println!();
        print!("{}", run::losses(&outcomes));
    }
    Ok(())
}

fn detail(args: &[String]) -> Result<(), String> {
    let [name, scenario, sample, dirs @ ..] = args else {
        return Err(USAGE.into());
    };
    let scenario = Scenario::ALL
        .into_iter()
        .find(|candidate| candidate.name() == scenario)
        .ok_or(USAGE)?;
    let samples = samples(dirs)?;
    let sample = samples
        .iter()
        .find(|candidate| candidate.name == *sample)
        .ok_or_else(|| format!("no sample is called `{sample}`"))?;
    for candidate in candidates(Some(name))? {
        print!(
            "{}",
            run::detail(&run::evaluate(sample, candidate.as_ref(), scenario))
        );
    }
    Ok(())
}

fn export(args: &[String]) -> Result<(), String> {
    let [out, dirs @ ..] = args else {
        return Err(USAGE.into());
    };
    let out = Path::new(out);
    let write = |path: &Path, bytes: &[u8]| -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        std::fs::write(path, bytes).map_err(|err| format!("{}: {err}", path.display()))
    };
    for sample in samples(dirs)? {
        let file = sample.name.replace('/', "_");
        write(&out.join("original").join(&file), &sample.bytes)?;
        for candidate in candidate::all() {
            let dir = out.join(candidate.name().replace(' ', "-"));
            match candidate.resave(sample.kind, &sample.bytes) {
                Ok(bytes) => write(&dir.join("resave").join(&file), &bytes)?,
                Err(why) => eprintln!("{} · {} · resave: {why}", sample.name, candidate.name()),
            }
            match candidate.edit(sample.kind, &sample.bytes) {
                Ok(edited) => write(&dir.join("edit").join(&file), &edited.bytes)?,
                Err(why) => eprintln!("{} · {} · edit: {why}", sample.name, candidate.name()),
            }
        }
    }
    println!("{}", out.display());
    Ok(())
}

/// Compare two files part by part: which parts have the same bytes, which
/// have other bytes with the same content, which differ and how, which are
/// gone or new. Used to check what saving an edited file changed.
fn diff(args: &[String]) -> Result<(), String> {
    let [before, after] = args else {
        return Err(USAGE.into());
    };
    let read = |path: &str| -> Result<package::Package, String> {
        let bytes = std::fs::read(path).map_err(|err| format!("{path}: {err}"))?;
        package::Package::read(&bytes).map_err(|err| format!("{path}: {err}"))
    };
    let comparison = compare::compare(&read(before)?, &read(after)?);
    let mut same = 0;
    for (name, change) in &comparison.parts {
        match change {
            compare::Change::Identical => same += 1,
            compare::Change::Different(delta) => {
                println!("{name}: different");
                for (what, count) in delta.lost_elements.iter().chain(&delta.gained_elements) {
                    println!("    element {what} ×{count}");
                }
                for (what, before, after) in &delta.changed_attributes {
                    println!("    {what}: {before} → {after}");
                }
                for (what, count) in delta.lost_text.iter() {
                    println!("    text gone: {what} ×{count}");
                }
                for (what, count) in delta.gained_text.iter() {
                    println!("    text new: {what} ×{count}");
                }
            }
            other => println!("{name}: {other:?}"),
        }
    }
    for name in &comparison.added {
        println!("{name}: added");
    }
    println!(
        "{same} of {} parts byte for byte the same",
        comparison.parts.len()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "run" => run(rest, true),
        Some((command, rest)) if command == "summary" => run(rest, false),
        Some((command, rest)) if command == "detail" => detail(rest),
        Some((command, rest)) if command == "export" => export(rest),
        Some((command, rest)) if command == "diff" => diff(rest),
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
