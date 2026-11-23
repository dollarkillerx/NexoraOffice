//! Golden line breaks for Chinese and Japanese samples, and a digest of them.
//!
//! Built on the synthetic metrics, so no font is involved and the record must
//! come out byte for byte the same on macOS, Windows and Linux. The tests
//! check it against `golden/breaks-18em.txt` and a digest over six widths.
//! Real fonts additionally need the same font files on every machine.

use std::fmt::Write;

use crate::breaker::{BreakKind, EndTreatment, Fx, Line, Options, break_lines};
use crate::segment::{SYNTHETIC_EM, synthetic_clusters};

pub struct Sample {
    pub name: &'static str,
    /// BCP 47 language of the content; selects the line-breaking tailoring.
    pub language: &'static str,
    pub text: &'static str,
}

/// The first five are the G3 gate's samples, unchanged; the rest were added
/// when the breaker became part of the product.
pub const SAMPLES: [Sample; 8] = [
    Sample {
        name: "zh-prose",
        language: "zh-Hans",
        text: "排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。连续的标点，例如“」，「”或者“）。”，中间的空白应当被挤压，否则版面会显得松散。",
    },
    Sample {
        name: "ja-prose",
        language: "ja",
        text: "日本語の組版では、行頭に句読点や閉じ括弧（」）、小書きの仮名（ゃ・ゅ・ょ・っ）、長音記号「ー」などを置くことはできません。これを「行頭禁則」と呼びます。コンピューターでチェックしましょう。",
    },
    Sample {
        name: "mixed",
        language: "zh-Hans",
        text: "Aster Office 使用 Rust 1.95 和 Slint 1.19 开发，在 macOS、Windows 11 与 Linux 上运行。打开一个 10 MB 的 DOCX 文件不应超过 1.5 秒，网址 https://example.com/docs/long-path 只能在斜杠之后断开。",
    },
    Sample {
        name: "dialogue",
        language: "zh-Hans",
        text: "他说：“今天的会议改到下午三点。”\n她问：“（真的吗？）那我们还来得及吗？”\n“来得及。”他回答，“放心吧……”",
    },
    Sample {
        name: "english",
        language: "en",
        text: "Line breaking for Latin text must keep words whole, break after hyphens in well-known compounds, and never strand punctuation at the start of a line.",
    },
    Sample {
        name: "zh-exclaim",
        language: "zh-Hans",
        text: "真的吗？！太好了！我们下周一（11月30日）出发——记得带上《使用手册》和【附件二】；集合地点：北京西站，时间：上午八点。",
    },
    Sample {
        name: "ja-quotes",
        language: "ja",
        text: "彼は「『吾輩は猫である』を読んだか？」と聞いた。わたしは「まだです……」と答え、ソフトウェア・エンジニアリングの本（第３版）を開いた。",
    },
    Sample {
        name: "zh-hant",
        language: "zh-Hant",
        text: "臺灣的出版物多半採用直式排版，但網頁與簡報仍以橫式為主。標點符號「，」「。」置中，與簡體中文的字形不同。",
    },
];

/// Line widths, in ems, that the digest covers.
pub const DIGEST_WIDTHS: [Fx; 6] = [6, 9, 13, 18, 25, 34];

fn describe(line: &Line, text: &str) -> String {
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
    let width = i64::from(line.width) * 100 / i64::from(SYNTHETIC_EM);
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
            writeln!(out, "{}", describe(line, text)).unwrap();
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

    /// The digest of the synthetic dumps. If this test fails on one platform
    /// and passes on another, line breaking is not the same on both. Only
    /// macOS has run it so far.
    const EXPECTED_DIGEST: u64 = 0x993999b07a585027;

    #[test]
    fn digest_is_the_same_everywhere() {
        assert_eq!(digest(), EXPECTED_DIGEST, "digest is {:#018x}", digest());
    }

    #[test]
    fn dump_matches_the_golden_file() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/breaks-18em.txt");
        let actual = dump(18);
        if std::env::var_os("ASTER_BLESS").is_some() {
            std::fs::write(path, &actual).unwrap();
        }
        let expected = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            actual == expected,
            "golden file is out of date; rerun with ASTER_BLESS=1\n{actual}"
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

    /// The first five samples are G3's; with them alone the record must be
    /// what the gate recorded, so the move changed nothing.
    #[test]
    fn the_gate_samples_break_as_they_did_in_g3() {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for width in DIGEST_WIDTHS {
            let dump = dump(width);
            let gate: String = dump
                .split_inclusive('\n')
                .scan(true, |keep, line| {
                    if let Some(name) = line.strip_prefix("== ") {
                        *keep = SAMPLES[..5]
                            .iter()
                            .any(|sample| name.starts_with(&format!("{} ·", sample.name)));
                    }
                    Some(if *keep { line } else { "" })
                })
                .collect();
            for byte in gate.bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        assert_eq!(hash, 0x8515cd8a10c36b74, "gate digest is {hash:#018x}");
    }
}
