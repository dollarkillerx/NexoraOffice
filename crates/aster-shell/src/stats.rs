//! Counting what is in a document, and what the status bar says about it.

use ropey::Rope;

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

/// Whether counting starts afresh after this character: it ends any word.
/// Counting depends only on the text since the last such character, which is
/// what lets an edit be counted locally.
fn ends_words(c: char) -> bool {
    c == '\n'
        || c == '\r'
        || is_cjk(c)
        || !(c.is_alphanumeric() || c == '\'' || c == '’' || c == '-')
}

/// How far around an edit [`TextStats::edited`] looks for word boundaries
/// before giving up.
const EDIT_CONTEXT: usize = 1024;

impl TextStats {
    pub fn of(text: &str) -> Self {
        Self::of_chunks(std::iter::once(text))
    }

    /// The counts of a rope, chunk by chunk, without copying it.
    pub fn of_rope(text: &Rope) -> Self {
        Self::of_chunks(text.chunks())
    }

    fn of_chunks<'a>(chunks: impl Iterator<Item = &'a str>) -> Self {
        let mut stats = Self::default();
        let mut in_word = false;
        for c in chunks.flat_map(str::chars) {
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

    /// Update the counts for an edit already made to `text`: at byte `at`,
    /// `removed` was replaced by `inserted`. Only the words touching the edit
    /// are counted again. Returns false, leaving the counts alone, when no
    /// word boundary is found within [`EDIT_CONTEXT`] bytes on either side;
    /// the caller then counts the whole text.
    pub fn edited(&mut self, text: &Rope, at: usize, removed: &str, inserted: &str) -> bool {
        // Back to just after a character that ends words.
        let mut chars = text.chars_at(text.byte_to_char(at));
        let mut before = String::new();
        loop {
            match chars.prev() {
                None => break,
                Some(c) if ends_words(c) => break,
                Some(c) => {
                    if before.len() > EDIT_CONTEXT {
                        return false;
                    }
                    before.insert(0, c);
                }
            }
        }
        // Forward to just after one.
        let end = at + inserted.len();
        let mut after = String::new();
        for c in text.chars_at(text.byte_to_char(end)) {
            if after.len() > EDIT_CONTEXT {
                return false;
            }
            after.push(c);
            if ends_words(c) {
                break;
            }
        }
        let old = Self::of_chunks([before.as_str(), removed, after.as_str()].into_iter());
        let new = Self::of_chunks([before.as_str(), inserted, after.as_str()].into_iter());
        self.words = self.words + new.words - old.words;
        self.chars = self.chars + new.chars - old.chars;
        self.chars_no_spaces = self.chars_no_spaces + new.chars_no_spaces - old.chars_no_spaces;
        true
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

    #[test]
    fn counting_an_edit_matches_counting_everything() {
        let pieces = [
            "中文",
            "日本語",
            "word",
            " ",
            "\n",
            "don't",
            "-",
            "x",
            "한국어",
            "，",
            "a1",
            "’",
        ];
        let mut text = String::from("Hello 世界, this is a test.\n第二行 don't stop");
        let mut rope = Rope::from_str(&text);
        let mut stats = TextStats::of(&text);
        let mut seed: u64 = 3;
        let mut next = |bound: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as usize) % bound.max(1)
        };
        for _ in 0..2000 {
            let char_count = text.chars().count();
            let start_char = next(char_count + 1);
            let end_char = (start_char + next(4)).min(char_count);
            let start = text
                .char_indices()
                .nth(start_char)
                .map_or(text.len(), |(i, _)| i);
            let end = text
                .char_indices()
                .nth(end_char)
                .map_or(text.len(), |(i, _)| i);
            let inserted = if next(3) == 0 {
                ""
            } else {
                pieces[next(pieces.len())]
            };
            let removed = text[start..end].to_owned();
            text.replace_range(start..end, inserted);
            let (cs, ce) = (rope.byte_to_char(start), rope.byte_to_char(end));
            rope.remove(cs..ce);
            rope.insert(cs, inserted);
            assert!(stats.edited(&rope, start, &removed, inserted));
            assert_eq!(stats, TextStats::of(&text), "{text:?}");
        }
        assert_eq!(TextStats::of_rope(&rope), TextStats::of(&text));
    }

    #[test]
    fn a_long_word_is_counted_again_in_full() {
        let text = "a".repeat(5000);
        let rope = Rope::from_str(&text);
        let mut stats = TextStats::of(&text);
        assert!(!stats.edited(&rope, 2500, "", ""));
        assert_eq!(stats, TextStats::of(&text));
    }
}
