//! Counting what is in a document, and what the status bar says about it.

/// Counts shown in the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStats {
    /// Words the way CJK word processors count them: every Han character, kana
    /// or Hangul syllable is one, and so is every run of other letters and digits.
    pub words: usize,
    /// Characters, not counting line breaks.
    pub chars: usize,
    /// Characters that are neither spaces nor line breaks.
    pub chars_no_spaces: usize,
}

/// Whether a character counts as a word on its own.
fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),
        0x3040..=0x30ff      // Hiragana, Katakana
        | 0x31f0..=0x31ff    // Katakana phonetic extensions
        | 0x3400..=0x4dbf    // Han extension A
        | 0x4e00..=0x9fff    // Han
        | 0xac00..=0xd7af    // Hangul syllables
        | 0xf900..=0xfaff    // Han compatibility
        | 0x20000..=0x323af  // Han extensions B and later
    ) && c != '・'
        && c != 'ー'
}

impl TextStats {
    pub fn of(text: &str) -> Self {
        let mut stats = Self::default();
        let mut in_word = false;
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                in_word = false;
                continue;
            }
            stats.chars += 1;
            if !c.is_whitespace() {
                stats.chars_no_spaces += 1;
            }
            if is_cjk(c) {
                stats.words += 1;
                in_word = false;
            } else if c.is_alphanumeric() {
                if !in_word {
                    stats.words += 1;
                }
                in_word = true;
            } else if c != '\'' && c != '’' && c != '-' {
                // Apostrophes and hyphens stay inside a word: "don't", "off-line".
                in_word = false;
            }
        }
        stats
    }
}

/// What the shell knows about the input method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImeState {
    /// No input-method event has arrived yet.
    #[default]
    Unknown,
    /// The platform reported the input method as active.
    Enabled,
    /// Composition text is on screen.
    Composing,
    /// The platform reported the input method as switched off.
    Disabled,
}

/// Thousands separators for the counts: 12345 becomes "12,345".
pub fn group_digits(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> usize {
        TextStats::of(text).words
    }

    #[test]
    fn empty_text_counts_nothing() {
        assert_eq!(TextStats::of(""), TextStats::default());
        assert_eq!(TextStats::of("\n\n"), TextStats::default());
    }

    #[test]
    fn latin_words_are_runs_of_letters() {
        assert_eq!(words("Hello, world"), 2);
        assert_eq!(words("don't stop off-line"), 3);
        assert_eq!(words("  a  b\tc\nd"), 4);
        assert_eq!(words("3.14 2026"), 3);
    }

    #[test]
    fn cjk_characters_count_one_each() {
        assert_eq!(words("你好世界"), 4);
        assert_eq!(words("日本語にほんごカタカナ"), 11);
        assert_eq!(words("한국어"), 3);
        // Punctuation is not a word.
        assert_eq!(words("你好，世界。"), 4);
    }

    #[test]
    fn mixed_text() {
        let stats = TextStats::of("Aster Office 是办公套件\n第二行 line2");
        // Aster, Office, 是办公套件 (5), 第二行 (3), line2.
        assert_eq!(stats.words, 11);
        // Line breaks are not characters; spaces are.
        assert_eq!(
            stats.chars,
            "Aster Office 是办公套件".chars().count() + "第二行 line2".chars().count()
        );
        assert_eq!(stats.chars - stats.chars_no_spaces, 3);
    }

    #[test]
    fn a_cjk_character_ends_a_latin_word() {
        assert_eq!(words("abc你def"), 3);
    }

    #[test]
    fn digits_are_grouped_in_threes() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
