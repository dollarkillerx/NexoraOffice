//! Where lines may break, and a font-free way to get cluster advances.

use icu_locale_core::LanguageIdentifier;
use icu_segmenter::LineSegmenter;
use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness};

use crate::breaker::{Cluster, Fx};
use crate::class::{classify, is_wide};

/// Byte offsets at which UAX #14, in strict mode and tailored for `language`,
/// allows a line to end. Strict mode is what forbids a break before closing
/// marks, small kana and the prolonged sound mark.
pub fn break_opportunities(text: &str, language: &str) -> Vec<usize> {
    let language: Option<LanguageIdentifier> = language.parse().ok();
    let mut options = LineBreakOptions::default();
    options.strictness = Some(LineBreakStrictness::Strict);
    options.content_locale = language.as_ref();
    LineSegmenter::new_auto(options).segment_str(text).collect()
}

/// One em of the synthetic metrics: 16 px in 26.6 fixed point.
pub const SYNTHETIC_EM: Fx = 64 * 16;

/// Clusters with made-up advances: one em for characters a CJK font draws full
/// width, half an em for everything else, nothing for a newline.
///
/// No font is involved, so the result is the same on every machine. Tests and
/// the cross-platform digest use these; the breaker cannot tell the difference.
pub fn synthetic_clusters(text: &str, language: &str) -> Vec<Cluster> {
    let breaks = break_opportunities(text, language);
    text.char_indices()
        .map(|(offset, c)| Cluster {
            text: offset..offset + c.len_utf8(),
            class: classify(c),
            advance: match c {
                '\n' => 0,
                c if is_wide(c) => SYNTHETIC_EM,
                _ => SYNTHETIC_EM / 2,
            },
            em: SYNTHETIC_EM,
            break_before: offset > 0 && breaks.binary_search(&offset).is_ok(),
            newline: c == '\n',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(text: &str, language: &str, before: char) -> bool {
        let offset = text.find(before).expect("character is in the text");
        break_opportunities(text, language).contains(&offset)
    }

    #[test]
    fn han_characters_can_be_separated() {
        assert!(allowed("排版引擎", "zh-Hans", '版'));
        assert!(allowed("排版引擎", "zh-Hans", '擎'));
    }

    #[test]
    fn closing_marks_cannot_start_a_line() {
        for mark in ['。', '，', '、', '」', '）', '》', '”'] {
            let text = format!("前文{mark}后文");
            assert!(
                !allowed(&text, "zh-Hans", mark),
                "break allowed before {mark}"
            );
        }
    }

    #[test]
    fn opening_marks_cannot_end_a_line() {
        for mark in ['「', '（', '《', '“'] {
            let text = format!("前文{mark}后文");
            assert!(
                !allowed(&text, "zh-Hans", '后'),
                "break allowed after {mark}"
            );
        }
    }

    #[test]
    fn small_kana_and_the_long_vowel_mark_cannot_start_a_line() {
        assert!(!allowed("きゃく", "ja", 'ゃ'));
        assert!(!allowed("コンピューター", "ja", 'ー'));
        assert!(!allowed("ちょっと", "ja", 'ょ'));
        assert!(!allowed("ちょっと", "ja", 'っ'));
    }

    #[test]
    fn latin_words_stay_whole() {
        let text = "使用 Rust 开发";
        assert!(!allowed(text, "zh-Hans", 'u'));
        assert!(allowed(text, "zh-Hans", 'R'));
    }

    #[test]
    fn synthetic_metrics_follow_the_width_model() {
        let clusters = synthetic_clusters("中a。\n", "zh-Hans");
        let advances: Vec<Fx> = clusters.iter().map(|c| c.advance).collect();
        assert_eq!(advances, [SYNTHETIC_EM, SYNTHETIC_EM / 2, SYNTHETIC_EM, 0]);
        assert!(clusters[3].newline);
        assert!(!clusters[0].break_before);
    }
}
