//! What text from a PDF may contain once it reaches the window: titles in
//! the outline, page text, and text copied to the clipboard all go through
//! [`is_unwanted`].
//!
//! Besides control characters (Cc), it removes the format characters that
//! change how surrounding text is shown without being seen themselves:
//! bidirectional embeddings, overrides and isolates (which can make a title
//! read differently from what it is), zero-width characters, the soft hyphen
//! and the byte order mark, plus the line and paragraph separators.

/// Whether `c` must not be shown or copied. `'\n'` is unwanted too; callers
/// that keep line breaks check for it first.
pub fn is_unwanted(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Bidirectional marks, embeddings, overrides and isolates.
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            // Zero-width space, non-joiner, joiner, word joiner, invisible
            // operators, and the byte order mark (zero-width no-break space).
            | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
            // Soft hyphen, Mongolian vowel separator.
            | '\u{00AD}' | '\u{180E}'
            // Line and paragraph separators (Zl, Zp).
            | '\u{2028}' | '\u{2029}'
            // Interlinear annotation and object replacement controls.
            | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// A title or other single line: unwanted characters removed.
pub fn line(text: &str) -> String {
    text.chars().filter(|&c| !is_unwanted(c)).collect()
}

/// Text that may span lines: unwanted characters removed, `'\n'` kept.
pub fn lines(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || !is_unwanted(c))
        .collect()
}

/// Like [`lines`], but each unwanted character becomes a space, so the
/// text keeps its length in `char`s (page text, whose characters line up
/// with their boxes).
pub fn lines_in_place(text: &mut String) {
    let bad = |c: char| c != '\n' && is_unwanted(c);
    if text.chars().any(bad) {
        *text = text.chars().map(|c| if bad(c) { ' ' } else { c }).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_bidi_zero_width_and_separators_go() {
        let sneaky = "a\u{202E}evil\u{202C}b\u{2066}c\u{2069}d\u{200B}e\u{FEFF}f\u{2028}g\u{2029}h\u{7}i\u{200F}j\tk\u{00AD}l";
        assert_eq!(line(sneaky), "aevilbcdefghijkl");
        assert_eq!(lines("one\ntwo\u{2028}three"), "one\ntwothree");
        let mut text = String::from("x\u{202E}y\nz");
        lines_in_place(&mut text);
        assert_eq!(text, "x y\nz");
        assert_eq!(text.chars().count(), 5);
    }

    #[test]
    fn ordinary_text_is_kept() {
        let text = "Hello, 世界！ Ünïcödé — ﬁ ①";
        assert_eq!(line(text), text);
        assert_eq!(lines("中文\n日本語"), "中文\n日本語");
    }
}
