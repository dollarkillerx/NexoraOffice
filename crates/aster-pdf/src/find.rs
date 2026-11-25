//! Searching a document's text.
//!
//! A search is a plain comparison of characters, one by one, over the text of
//! a page ([`Document::page_text`]). Line breaks count as spaces, so a phrase
//! that runs over two lines is found. Case is folded one character at a time
//! (so positions stay aligned with the text) when the search does not match
//! case. Nothing else is folded: full-width and half-width forms, ligatures
//! (a "ﬁ" does not match "fi") and accents are different characters here.

use crate::{Document, FindHit, PageText, TextBox};

impl Document {
    /// Matches of `query` on `page`, at most `max`.
    pub fn find_in_page(
        &self,
        page: usize,
        query: &str,
        match_case: bool,
        max: usize,
    ) -> Vec<FindHit> {
        if max == 0 || query.is_empty() {
            return Vec::new();
        }
        let Ok(text) = self.page_text(page) else {
            return Vec::new();
        };
        find_in_text(
            &text,
            query,
            match_case,
            max.min(self.limits.max_find_hits),
            self.limits.max_query_chars,
        )
    }
}

/// One character as compared: a line break is a space, and without case
/// matching a character is lowercased when that gives one character.
fn fold(c: char, match_case: bool) -> char {
    let c = if c == '\n' { ' ' } else { c };
    if match_case {
        return c;
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(only), None) => only,
        _ => c,
    }
}

fn find_in_text(
    text: &PageText,
    query: &str,
    match_case: bool,
    max: usize,
    max_query_chars: usize,
) -> Vec<FindHit> {
    let needle: Vec<char> = query
        .chars()
        .take(max_query_chars)
        .map(|c| fold(c, match_case))
        .collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let haystack: Vec<char> = text.text.chars().map(|c| fold(c, match_case)).collect();
    let mut hits = Vec::new();
    let mut start = 0;
    while hits.len() < max && start + needle.len() <= haystack.len() {
        if haystack[start..start + needle.len()] == needle[..] {
            hits.push(FindHit {
                page: text.page,
                start: start as u32,
                len: needle.len() as u32,
                boxes: line_boxes(&text.boxes[start..start + needle.len()]),
            });
            start += needle.len();
        } else {
            start += 1;
        }
    }
    hits
}

/// The boxes of some characters, one box for each run of characters on the
/// same line: the ones whose top and bottom are about the same.
fn line_boxes(boxes: &[Option<TextBox>]) -> Vec<TextBox> {
    let mut lines: Vec<TextBox> = Vec::new();
    for tbox in boxes.iter().flatten() {
        match lines.last_mut() {
            Some(line) if same_line(line, tbox) => {
                line[0] = line[0].min(tbox[0]);
                line[1] = line[1].min(tbox[1]);
                line[2] = line[2].max(tbox[2]);
                line[3] = line[3].max(tbox[3]);
            }
            _ => lines.push(*tbox),
        }
    }
    lines
}

/// Whether two boxes stand on one line: they overlap in height by most of
/// the smaller one, and the second does not start far before the first ends
/// (text that jumped back is another line).
fn same_line(line: &TextBox, next: &TextBox) -> bool {
    let height = (line[3] - line[1]).min(next[3] - next[1]).max(0.01);
    let overlap = line[3].min(next[3]) - line[1].max(next[1]);
    let back = line[2] - next[0];
    overlap >= 0.7 * height && back <= height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::builder::text_page;
    use crate::text::testing::with_document;

    const TWO_LINES: &[u8] =
        b"BT /F1 12 Tf 10 80 Td (Hello World, hello) Tj 0 -20 Td (brave new world) Tj ET";

    fn hits(content: &[u8], query: &str, match_case: bool, max: usize) -> Vec<FindHit> {
        let query = query.to_owned();
        with_document(
            text_page("0 0 200 100", "", content),
            Limits::default(),
            move |d| d.find_in_page(0, &query, match_case, max),
        )
    }

    fn starts(hits: &[FindHit]) -> Vec<(u32, u32)> {
        hits.iter().map(|h| (h.start, h.len)).collect()
    }

    #[test]
    fn case_sensitive_and_insensitive() {
        // "Hello World, hello\nbrave new world"
        let sensitive = hits(TWO_LINES, "hello", true, 100);
        assert_eq!(starts(&sensitive), [(13, 5)]);
        let insensitive = hits(TWO_LINES, "HELLO", false, 100);
        assert_eq!(starts(&insensitive), [(0, 5), (13, 5)]);
        let worlds = hits(TWO_LINES, "world", false, 100);
        assert_eq!(starts(&worlds), [(6, 5), (29, 5)]);
        assert!(hits(TWO_LINES, "World", true, 100).len() == 1);
        assert!(hits(TWO_LINES, "absent", false, 100).is_empty());
    }

    #[test]
    fn a_phrase_over_a_line_break() {
        // The line break stands for a space.
        let found = hits(TWO_LINES, "hello brave", false, 100);
        assert_eq!(starts(&found), [(13, 11)]);
        // Two lines: two boxes, the second below the first.
        assert_eq!(found[0].boxes.len(), 2, "{:?}", found[0].boxes);
        assert!(found[0].boxes[1][1] > found[0].boxes[0][1]);
        // A query with a line break matches too.
        assert_eq!(hits(TWO_LINES, "hello\nbrave", true, 100).len(), 1);
    }

    #[test]
    fn boxes_are_merged_per_line() {
        let found = hits(TWO_LINES, "World", true, 100);
        assert_eq!(found[0].boxes.len(), 1);
        let [l, t, r, b] = found[0].boxes[0];
        // Five letters of 12 point Helvetica: about 40 points wide.
        assert!(r - l > 30.0 && r - l < 50.0, "{l} {r}");
        assert!(b - t > 8.0 && b - t < 16.0);
        // "o W" spans a space: still one box.
        let across = hits(TWO_LINES, "lo Wor", true, 100);
        assert_eq!(across[0].boxes.len(), 1);
    }

    #[test]
    fn cjk_text_is_found() {
        let content = b"BT /F2 20 Tf 10 50 Td <4F60597D4E16754C> Tj ET";
        let found = hits(content, "\u{597D}\u{4E16}", true, 100);
        assert_eq!(starts(&found), [(1, 2)]);
        assert_eq!(found[0].boxes.len(), 1);
        let [l, _, r, _] = found[0].boxes[0];
        assert!((r - l - 40.0).abs() < 2.0, "{l} {r}");
        assert!((l - 30.0).abs() < 2.0, "{l}");
    }

    #[test]
    fn at_most_max_hits_without_overlap() {
        let content = b"BT /F1 12 Tf 10 50 Td (aaaaaaaa) Tj ET";
        assert_eq!(
            starts(&hits(content, "aa", true, 100)),
            [(0, 2), (2, 2), (4, 2), (6, 2)]
        );
        assert_eq!(hits(content, "aa", true, 3).len(), 3);
        assert!(hits(content, "aa", true, 0).is_empty());
        let limits = Limits {
            max_find_hits: 2,
            ..Limits::default()
        };
        let capped = with_document(text_page("0 0 200 100", "", content), limits, |d| {
            d.find_in_page(0, "a", true, 100)
        });
        assert_eq!(capped.len(), 2);
    }

    #[test]
    fn empty_and_long_queries() {
        assert!(hits(TWO_LINES, "", false, 10).is_empty());
        // A query longer than the limit is cut to it (256 characters).
        let long = format!("Hello{}", "x".repeat(300));
        assert!(hits(TWO_LINES, &long, true, 10).is_empty());
        let content = format!("BT /F1 8 Tf 0 50 Td ({}) Tj ET", "ab".repeat(200));
        let found = hits(content.as_bytes(), &"ab".repeat(200), true, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].len, 256);
    }

    #[test]
    fn nothing_is_folded_but_case() {
        // A full-width letter is a different character.
        assert!(hits(TWO_LINES, "\u{FF28}ello", false, 10).is_empty());
        // A page that does not exist has no matches.
        let none = with_document(
            text_page("0 0 200 100", "", TWO_LINES),
            Limits::default(),
            |d| d.find_in_page(5, "hello", false, 10),
        );
        assert!(none.is_empty());
    }

    #[test]
    fn folding_keeps_positions() {
        let text = PageText {
            page: 0,
            text: "\u{130}a A".into(),
            boxes: vec![None; 4],
        };
        // U+0130 lowercases to two characters; it is left as it is.
        let found = find_in_text(&text, "A", false, 10, 256);
        assert_eq!(found.iter().map(|h| h.start).collect::<Vec<_>>(), [1, 3]);
    }
}
