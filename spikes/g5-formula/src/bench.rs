//! Time one engine on one workload and check what it calculated.

use std::time::{Duration, Instant};

use crate::engine::{Candidate, Engine, Source, Value};
use crate::workload::{Expected, Probe, Workload};
use crate::xlsx;

/// What one run found. Times are in milliseconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub file_bytes: usize,
    pub load_ms: f64,
    pub first_ms: f64,
    /// Edits made before the time budget ran out.
    pub edits: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Edits after which a probed cell did not hold the expected value.
    pub wrong: usize,
    /// The first disagreement, including one right after the first calculation.
    pub first_wrong: Option<String>,
    /// Resident memory with the workbook loaded and edited, where it can be read.
    pub resident_mb: Option<f64>,
    /// What the engine itself had to say.
    pub note: Option<String>,
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The value at `share` of the way through the sorted samples.
pub fn percentile(sorted: &[f64], share: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (share * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn disagreement(engine: &mut dyn Engine, probes: &[Probe]) -> Option<String> {
    for (at, expected) in probes {
        let got = match engine.value(*at) {
            Ok(value) => value,
            Err(why) => return Some(format!("{}: {why}", at.a1())),
        };
        // Bit for bit: the workloads are built so that no rounding can occur.
        let same = match (expected, &got) {
            (Expected::Number(want), Value::Number(have)) => want == have,
            (Expected::Text(want), Value::Text(have)) => want == have,
            _ => false,
        };
        if !same {
            return Some(format!("{} should be {expected:?}, is {got:?}", at.a1()));
        }
    }
    None
}

fn resident_mb() -> Option<f64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kib: f64 = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
    Some(kib / 1024.0)
}

/// Load, calculate everything once, then make the workload's edits one by one
/// until they are done or `budget` has been spent on them.
pub fn run(
    candidate: &Candidate,
    workload: &Workload,
    scale: u32,
    budget: Duration,
) -> Result<Report, String> {
    let bytes = xlsx::write(&workload.book);
    let source = Source {
        workload: workload.name,
        scale,
        sheets: workload
            .book
            .sheets
            .iter()
            .map(|sheet| sheet.name)
            .collect(),
        xlsx: &bytes,
    };
    let mut report = Report {
        file_bytes: bytes.len(),
        ..Report::default()
    };

    let started = Instant::now();
    let mut engine = (candidate.open)(&source)?;
    report.load_ms = millis(started.elapsed());

    let started = Instant::now();
    engine.calculate_all()?;
    report.first_ms = millis(started.elapsed());
    report.first_wrong = disagreement(engine.as_mut(), &workload.initial)
        .map(|what| format!("after the first calculation: {what}"));

    let mut samples = Vec::with_capacity(workload.steps.len());
    let mut spent = Duration::ZERO;
    for (index, step) in workload.steps.iter().enumerate() {
        let started = Instant::now();
        engine.edit(step.at, step.value)?;
        let took = started.elapsed();
        samples.push(millis(took));
        spent += took;
        if let Some(what) = disagreement(engine.as_mut(), &step.probes) {
            report.wrong += 1;
            report
                .first_wrong
                .get_or_insert_with(|| format!("after edit {}: {what}", index + 1));
        }
        if spent >= budget {
            break;
        }
    }
    report.edits = samples.len();
    samples.sort_by(f64::total_cmp);
    report.p50_ms = percentile(&samples, 0.50);
    report.p95_ms = percentile(&samples, 0.95);
    report.max_ms = samples.last().copied().unwrap_or(0.0);
    report.resident_mb = resident_mb();
    report.note = engine.note();
    Ok(report)
}

impl Report {
    /// One line that survives a pipe between two processes.
    pub fn encode(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.file_bytes,
            self.load_ms,
            self.first_ms,
            self.edits,
            self.p50_ms,
            self.p95_ms,
            self.max_ms,
            self.wrong,
            self.resident_mb.unwrap_or(-1.0),
            self.first_wrong
                .as_deref()
                .unwrap_or("")
                .replace(['\t', '\n'], " "),
            self.note
                .as_deref()
                .unwrap_or("")
                .replace(['\t', '\n'], " "),
        )
    }

    pub fn decode(line: &str) -> Option<Report> {
        let fields: Vec<&str> = line.split('\t').collect();
        let [
            file_bytes,
            load,
            first,
            edits,
            p50,
            p95,
            max,
            wrong,
            resident,
            first_wrong,
            note,
        ] = fields.as_slice()
        else {
            return None;
        };
        let resident: f64 = resident.parse().ok()?;
        Some(Report {
            file_bytes: file_bytes.parse().ok()?,
            load_ms: load.parse().ok()?,
            first_ms: first.parse().ok()?,
            edits: edits.parse().ok()?,
            p50_ms: p50.parse().ok()?,
            p95_ms: p95.parse().ok()?,
            max_ms: max.parse().ok()?,
            wrong: wrong.parse().ok()?,
            resident_mb: (resident >= 0.0).then_some(resident),
            first_wrong: (!first_wrong.is_empty()).then(|| (*first_wrong).to_owned()),
            note: (!note.is_empty()).then(|| (*note).to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native;
    use crate::workload;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&samples, 0.50), 50.0);
        assert_eq!(percentile(&samples, 0.95), 95.0);
        assert_eq!(percentile(&samples[..10], 0.95), 10.0);
        assert_eq!(percentile(&samples[..1], 0.50), 1.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    #[test]
    fn a_report_survives_encoding() {
        let report = Report {
            file_bytes: 123,
            load_ms: 1.5,
            first_ms: 2.25,
            edits: 7,
            p50_ms: 0.1,
            p95_ms: 0.2,
            max_ms: 0.3,
            wrong: 2,
            first_wrong: Some("after edit 3: H5 should be Number(1.0), is Empty".into()),
            resident_mb: Some(64.5),
            note: Some("gave up on 3 cells".into()),
        };
        assert_eq!(Report::decode(&report.encode()), Some(report.clone()));
        let bare = Report {
            first_wrong: None,
            resident_mb: None,
            note: None,
            ..report
        };
        assert_eq!(Report::decode(&bare.encode()), Some(bare));
    }

    /// The expected values and the hand-written arithmetic were written
    /// separately; they have to agree on every probe of every workload.
    #[test]
    fn the_hand_written_arithmetic_agrees_with_the_expected_values() {
        for name in workload::NAMES {
            let workload = workload::by_name(name, 25, 1).unwrap();
            let report = run(&native::CANDIDATE, &workload, 1, Duration::from_secs(60)).unwrap();
            assert_eq!(report.edits, 25, "{name}");
            assert_eq!(report.first_wrong, None, "{name}");
            assert_eq!(report.wrong, 0, "{name}");
        }
    }

    /// A stand-in that never recalculates, to check that the check notices.
    #[test]
    fn stale_values_are_counted_as_wrong() {
        struct Stale(Box<dyn Engine>);
        impl Engine for Stale {
            fn calculate_all(&mut self) -> Result<(), String> {
                self.0.calculate_all()
            }
            fn edit(&mut self, _: crate::workload::At, _: f64) -> Result<(), String> {
                Ok(())
            }
            fn value(&mut self, at: crate::workload::At) -> Result<Value, String> {
                self.0.value(at)
            }
        }
        let stale = Candidate {
            name: "stale",
            about: "",
            open: |source| Ok(Box::new(Stale((native::CANDIDATE.open)(source)?))),
        };
        for name in workload::NAMES {
            let workload = workload::by_name(name, 10, 1).unwrap();
            let report = run(&stale, &workload, 1, Duration::from_secs(60)).unwrap();
            assert_eq!(
                report
                    .first_wrong
                    .as_deref()
                    .map(|w| w.starts_with("after edit 1")),
                Some(true),
                "{name}"
            );
            assert!(report.wrong >= 9, "{name}: {}", report.wrong);
        }
    }
}
