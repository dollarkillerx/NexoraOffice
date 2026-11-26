//! Finding text in the body of a document.

use aster_writer::{Document, Pos};

use super::text::word_at;

/// Most matches a search keeps.
pub const MAX_MATCHES: usize = 10_000;

/// What the find bar asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocQuery {
    pub pattern: String,
    pub case_sensitive: bool,
    /// The match must be whole words: its ends are where a double click
    /// would end a word (Chinese and Japanese included).
    pub whole_word: bool,
}

/// A match: both ends are in the same paragraph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: Pos,
    pub end: Pos,
}

/// The matches of the query now in the find bar, and which one is current.
pub(crate) struct Finding {
    pub query: DocQuery,
    pub matches: Vec<Match>,
    /// The page each match starts on (so, in order).
    pub pages: Vec<usize>,
    pub current: Option<usize>,
    pub truncated: bool,
}

/// The text lowercased one character at a time (characters whose lowercase
/// is more than one character stay as they are), and for each byte of the
/// result the byte offset of its character in the original, plus the
/// original's length at the end.
fn fold(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    for (at, c) in text.char_indices() {
        let mut lower = c.to_lowercase();
        let c = match (lower.next(), lower.next()) {
            (Some(one), None) => one,
            _ => c,
        };
        for _ in 0..c.len_utf8() {
            map.push(at);
        }
        folded.push(c);
    }
    map.push(text.len());
    (folded, map)
}

fn fold_pattern(pattern: &str) -> String {
    fold(pattern).0
}

/// Every match of the query in the body, in reading order, and whether the
/// search stopped at [`MAX_MATCHES`].
pub(crate) fn search(document: &Document, query: &DocQuery) -> (Vec<Match>, bool) {
    let mut found = Vec::new();
    if query.pattern.is_empty() {
        return (found, false);
    }
    let pattern = if query.case_sensitive {
        query.pattern.clone()
    } else {
        fold_pattern(&query.pattern)
    };
    for (path, paragraph) in document.paragraphs() {
        let text = paragraph.text();
        let (folded, map) = if query.case_sensitive {
            (text.clone(), (0..=text.len()).collect())
        } else {
            fold(&text)
        };
        for (at, hit) in folded.match_indices(pattern.as_str()) {
            let (start, end) = (map[at], map[at + hit.len()]);
            if query.whole_word && !is_whole_word(&text, start, end) {
                continue;
            }
            if found.len() >= MAX_MATCHES {
                return (found, true);
            }
            found.push(Match {
                start: Pos::new(path.clone(), start),
                end: Pos::new(path.clone(), end),
            });
        }
    }
    (found, false)
}

/// Whether `start..end` of `text` begins and ends at word boundaries.
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }
    let first = word_at(text, start);
    if first.start != start {
        return false;
    }
    let last_char = text[..end].char_indices().next_back().map_or(0, |(i, _)| i);
    word_at(text, last_char).end == end
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_writer::{Block, Paragraph, Run, RunLook};
    use std::sync::Arc;

    fn document(lines: &[&str]) -> Document {
        let mut document = Document::default();
        for line in lines {
            let paragraph = Paragraph {
                runs: vec![Run::text(*line, RunLook::default())],
                ..Paragraph::default()
            };
            document
                .body
                .blocks
                .push(Block::Paragraph(Arc::new(paragraph)));
        }
        document
    }

    fn query(pattern: &str, case: bool, whole: bool) -> DocQuery {
        DocQuery {
            pattern: pattern.into(),
            case_sensitive: case,
            whole_word: whole,
        }
    }

    #[test]
    fn case_folding_keeps_offsets_of_the_original() {
        let document = document(&["Straße and KELVIN \u{212a}elvin", "no match here"]);
        let (found, truncated) = search(&document, &query("kelvin", false, false));
        assert!(!truncated);
        assert_eq!(found.len(), 2);
        let text = document.paragraphs()[0].1.text();
        assert_eq!(&text[found[0].start.offset..found[0].end.offset], "KELVIN");
        assert_eq!(
            &text[found[1].start.offset..found[1].end.offset],
            "\u{212a}elvin"
        );
        let (exact, _) = search(&document, &query("kelvin", true, false));
        assert!(exact.is_empty());
    }

    #[test]
    fn whole_words_follow_the_word_segmentation() {
        let document = document(&["cat concat cat. category", "猫が好きです"]);
        let (found, _) = search(&document, &query("cat", true, true));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].start.offset, 0);
        assert_eq!(found[1].start.offset, 11);
        let (found, _) = search(&document, &query("猫", false, true));
        assert_eq!(found.len(), 1);
        let (found, _) = search(&document, &query("cat", true, false));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn matches_do_not_span_paragraphs_and_the_count_is_capped() {
        let document = document(&["ab", "cd"]);
        assert!(search(&document, &query("b\nc", true, false)).0.is_empty());
        assert!(search(&document, &query("bc", true, false)).0.is_empty());
        let many = "a".repeat(MAX_MATCHES + 5);
        let document = self::document(&[many.as_str()]);
        let (found, truncated) = search(&document, &query("a", true, false));
        assert_eq!(found.len(), MAX_MATCHES);
        assert!(truncated);
    }
}
