//! A plain-text record of how the samples break, and a digest of it.
//!
//! Built on the synthetic metrics, so it involves no font and must come out
//! byte-for-byte the same on macOS, Windows and Linux. That is the check the
//! gate asks for; with real fonts it additionally needs the same font files on
//! every machine.

use std::fmt::Write;

use crate::breaker::{BreakKind, EndTreatment, Fx, Line, Options, break_lines};
use crate::metrics::{SYNTHETIC_EM, synthetic_clusters};
use crate::samples::SAMPLES;

/// Line widths, in ems, that the digest covers.
pub const DIGEST_WIDTHS: [Fx; 6] = [6, 9, 13, 18, 25, 34];

pub fn describe(line: &Line, em: Fx, text: &str) -> String {
    let kind = match line.kind {
        BreakKind::Soft => 'S',
        BreakKind::Hard => 'H',
        BreakKind::Emergency => '!',
        BreakKind::End => 'E',
    };
    let end = match line.end {
        EndTreatment::None => '-',
        EndTreatment::Compressed => 'C',
        EndTreatment::Hung => 'G',
    };
    let justified = if line.justified { 'J' } else { '-' };
    // Hundredths of an em, in integers.
    let width = i64::from(line.width) * 100 / i64::from(em);
    format!(
        "{kind}{end}{justified} {:>3}.{:02} |{text}|",
        width / 100,
        width % 100
    )
}

/// Every sample broken at `width_em` ems with all rules on.
pub fn dump(width_em: Fx) -> String {
    let mut out = String::new();
    for sample in &SAMPLES {
        let clusters = synthetic_clusters(sample.text, sample.language);
        let lines = break_lines(&clusters, Options::cjk(width_em * SYNTHETIC_EM));
        writeln!(out, "== {} · {} em", sample.name, width_em).unwrap();
        for line in &lines {
            let range =
                clusters[line.clusters.start].text.start..clusters[line.clusters.end - 1].text.end;
            let text = sample.text[range].trim_end_matches('\n');
            writeln!(out, "{}", describe(line, SYNTHETIC_EM, text)).unwrap();
        }
    }
    out
}

/// FNV-1a over the dumps at every digest width.
pub fn digest() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for width in DIGEST_WIDTHS {
        for byte in dump(width).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Class;

    /// The digest of the synthetic dumps. If this test fails on one platform and
    /// passes on another, line breaking is not deterministic across them.
    const EXPECTED_DIGEST: u64 = 0x8515cd8a10c36b74;

    #[test]
    fn digest_is_the_same_everywhere() {
        assert_eq!(digest(), EXPECTED_DIGEST, "digest is {:#018x}", digest());
    }

    #[test]
    fn dump_matches_the_golden_file() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/synthetic-18em.txt");
        let actual = dump(18);
        if std::env::var_os("G3_BLESS").is_some() {
            std::fs::write(path, &actual).unwrap();
        }
        let expected = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            actual == expected,
            "golden file is out of date; rerun with G3_BLESS=1\n{actual}"
        );
    }

    #[test]
    fn rules_hold_for_every_sample_at_every_width() {
        for sample in &SAMPLES {
            let clusters = synthetic_clusters(sample.text, sample.language);
            for width_em in 4..40 {
                let max = width_em * SYNTHETIC_EM;
                let lines = break_lines(&clusters, Options::cjk(max));
                let context = format!("{} at {width_em} em", sample.name);

                // Nothing lost, nothing repeated.
                let covered: Vec<usize> = lines.iter().flat_map(|l| l.clusters.clone()).collect();
                assert_eq!(
                    covered,
                    (0..clusters.len()).collect::<Vec<_>>(),
                    "{context}"
                );

                for (n, line) in lines.iter().enumerate() {
                    let visible: Vec<_> = line
                        .clusters
                        .clone()
                        .filter(|&i| clusters[i].class != Class::Space && !clusters[i].newline)
                        .collect();
                    let (Some(&first), Some(&last)) = (visible.first(), visible.last()) else {
                        continue;
                    };
                    let after_emergency = n > 0 && lines[n - 1].kind == BreakKind::Emergency;
                    if !after_emergency {
                        assert!(
                            !clusters[first].class.has_blank_trailing_half(),
                            "{context}: line {n} starts with a closing mark"
                        );
                    }
                    if line.kind == BreakKind::Soft {
                        assert!(
                            clusters[last].class != Class::Opening,
                            "{context}: line {n} ends with an opening mark"
                        );
                    }
                    if line.kind != BreakKind::Emergency {
                        assert!(line.width <= max, "{context}: line {n} is too wide");
                    }
                    if line.justified {
                        assert_eq!(line.width, max, "{context}: line {n} is not flush");
                    }
                }
            }
        }
    }
}
