//! Character classes the line breaker cares about. A deliberately small subset of
//! the JLREQ and CLREQ classes: enough to decide where punctuation can give up
//! its blank half and where Chinese or Japanese text meets Latin text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Opening brackets and quotes. In a CJK font the ink sits in the right half.
    Opening,
    /// Closing brackets and quotes. The ink sits in the left half.
    Closing,
    /// Full stops: `。` and `．`.
    FullStop,
    /// Commas: `、` and `，`.
    Comma,
    /// Middle dots, colons and semicolons, centred in their box.
    MiddleDot,
    /// Han characters and kana.
    Ideographic,
    /// Latin, Greek and Cyrillic letters and digits.
    Western,
    Space,
    Other,
}

impl Class {
    /// Punctuation whose trailing half is blank.
    pub fn has_blank_trailing_half(self) -> bool {
        matches!(self, Class::Closing | Class::FullStop | Class::Comma)
    }

    /// Punctuation that may hang past the end of the line.
    pub fn can_hang(self) -> bool {
        matches!(self, Class::FullStop | Class::Comma)
    }

    /// Any of the punctuation classes that take part in compression.
    pub fn is_cjk_punctuation(self) -> bool {
        matches!(
            self,
            Class::Opening | Class::Closing | Class::FullStop | Class::Comma | Class::MiddleDot
        )
    }
}

pub fn classify(c: char) -> Class {
    match c {
        '（' | '〔' | '［' | '｛' | '〈' | '《' | '「' | '『' | '【' | '〖' | '〘' | '｟' | '“'
        | '‘' => Class::Opening,
        '）' | '〕' | '］' | '｝' | '〉' | '》' | '」' | '』' | '】' | '〗' | '〙' | '｠' | '”'
        | '’' => Class::Closing,
        '。' | '．' => Class::FullStop,
        '、' | '，' => Class::Comma,
        '・' | '：' | '；' => Class::MiddleDot,
        ' ' | '\t' | '\u{a0}' => Class::Space,
        '\u{3040}'..='\u{30ff}'
        | '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{3134f}' => Class::Ideographic,
        c if (c as u32) < 0x2e80 && c.is_alphanumeric() => Class::Western,
        _ => Class::Other,
    }
}

/// Whether a character is drawn one em wide in a CJK font. Used only by the
/// synthetic metrics; with a real font the measured advance decides.
pub fn is_wide(c: char) -> bool {
    classify(c).is_cjk_punctuation()
        || classify(c) == Class::Ideographic
        || matches!(c, '\u{3000}'..='\u{303f}' | '\u{ff01}'..='\u{ff60}' | '\u{ffe0}'..='\u{ffe6}' | '…' | '—')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_classes() {
        assert_eq!(classify('「'), Class::Opening);
        assert_eq!(classify('』'), Class::Closing);
        assert_eq!(classify('。'), Class::FullStop);
        assert_eq!(classify('，'), Class::Comma);
        assert_eq!(classify('・'), Class::MiddleDot);
    }

    #[test]
    fn scripts() {
        for c in ['中', 'あ', 'カ', 'ー', '語'] {
            assert_eq!(classify(c), Class::Ideographic, "{c}");
        }
        for c in ['a', 'Z', '7', 'é', 'я'] {
            assert_eq!(classify(c), Class::Western, "{c}");
        }
        assert_eq!(classify(' '), Class::Space);
        assert_eq!(classify('！'), Class::Other);
    }

    #[test]
    fn width_model() {
        assert!(is_wide('中') && is_wide('。') && is_wide('！') && is_wide('…'));
        assert!(!is_wide('a') && !is_wide(' ') && !is_wide('1'));
    }
}
