//! Moving through and cutting up the text of paragraphs: grapheme clusters,
//! words, and the plain text of a range of the body.

use std::ops::Range;

use aster_markdown::syntax::word_range;
use aster_writer::{Document, OBJECT, Path, Pos};
use icu_segmenter::GraphemeClusterSegmenter;

/// The start of the grapheme cluster that ends at `at`; 0 at the start.
pub(crate) fn previous_cluster(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    GraphemeClusterSegmenter::new()
        .segment_str(text)
        .take_while(|boundary| *boundary < at)
        .last()
        .unwrap_or(0)
}

/// The end of the grapheme cluster that starts at `at`; the length at the end.
pub(crate) fn next_cluster(text: &str, at: usize) -> usize {
    GraphemeClusterSegmenter::new()
        .segment_str(text)
        .find(|boundary| *boundary > at)
        .unwrap_or(text.len())
}

fn is_blank(text: &str) -> bool {
    !text.is_empty() && text.chars().all(char::is_whitespace)
}

/// The start of the word before `at`, over the spaces between; `None` at the
/// start of the text.
pub(crate) fn word_before(text: &str, at: usize) -> Option<usize> {
    let mut at = floor_boundary(text, at);
    while at > 0 {
        let previous = text[..at].char_indices().next_back().map_or(0, |(i, _)| i);
        let range = word_range(text, previous);
        if is_blank(&text[range.clone()]) {
            at = range.start.min(previous);
            continue;
        }
        return Some(range.start.min(previous));
    }
    None
}

/// The end of the word after `at`, and the spaces after it; `None` at the end
/// of the text.
pub(crate) fn word_after(text: &str, at: usize) -> Option<usize> {
    let at = floor_boundary(text, at);
    if at >= text.len() {
        return None;
    }
    let mut end = word_range(text, at).end.max(next_char(text, at));
    while end < text.len() {
        let range = word_range(text, end);
        if range.start == end && is_blank(&text[range.clone()]) {
            end = range.end;
        } else {
            break;
        }
    }
    Some(end)
}

fn next_char(text: &str, at: usize) -> usize {
    at + text[at..].chars().next().map_or(0, char::len_utf8)
}

/// The word a double click at `at` picks: the segment the character there is
/// in (the last one at the end of the text).
pub(crate) fn word_at(text: &str, at: usize) -> Range<usize> {
    word_range(text, at)
}

/// The nearest `char` boundary at or before `offset`, within the text.
pub(crate) fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The text between two places of a story, in either order: one paragraph
/// per line, pictures and note references left out, tabs kept. Nothing for
/// places in different stories.
pub fn text_between(document: &Document, a: &Pos, b: &Pos) -> String {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    if from == to || from.path.story != to.path.story {
        return String::new();
    }
    let mut lines = Vec::new();
    for (path, paragraph) in document.paragraphs_in(from.path.story) {
        if path < from.path || path > to.path {
            continue;
        }
        let text = paragraph.text();
        let start = if path == from.path {
            floor_boundary(&text, from.offset)
        } else {
            0
        };
        let end = if path == to.path {
            floor_boundary(&text, to.offset)
        } else {
            text.len()
        };
        lines.push(text[start.min(end)..end].replace(OBJECT, ""));
    }
    lines.join("\n")
}

/// `pos` moved onto the nearest `char` boundary of the paragraph it names;
/// `None` for a path that is no paragraph of its story.
pub(crate) fn sanitized(document: &Document, pos: Pos) -> Option<Pos> {
    let paragraph = document.paragraph(&pos.path)?;
    let text = paragraph.text();
    let offset = floor_boundary(&text, pos.offset);
    Some(Pos::new(pos.path, offset))
}

/// Every paragraph path of the body, in reading order.
pub(crate) fn paragraph_paths(document: &Document) -> Vec<Path> {
    document.paragraphs().into_iter().map(|(p, _)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_keep_combining_marks_and_emoji_together() {
        let text = "ae\u{301}x👨‍👩‍👧y";
        let e = 1;
        let after_e = next_cluster(text, e);
        assert_eq!(&text[e..after_e], "e\u{301}");
        assert_eq!(previous_cluster(text, after_e), e);
        let family = next_cluster(text, after_e + 1);
        assert_eq!(&text[after_e + 1..family], "👨‍👩‍👧");
        assert_eq!(previous_cluster(text, family), after_e + 1);
        assert_eq!(previous_cluster(text, 0), 0);
        assert_eq!(next_cluster(text, text.len()), text.len());
    }

    #[test]
    fn words_move_over_spaces() {
        let text = "alpha  beta gamma";
        assert_eq!(word_after(text, 0), Some(7));
        assert_eq!(word_after(text, 7), Some(12));
        assert_eq!(word_after(text, 12), Some(17));
        assert_eq!(word_after(text, 17), None);
        assert_eq!(word_before(text, 17), Some(12));
        assert_eq!(word_before(text, 12), Some(7));
        assert_eq!(word_before(text, 7), Some(0));
        assert_eq!(word_before(text, 0), None);
    }

    #[test]
    fn chinese_words_are_cut_by_the_dictionary() {
        let text = "我们喜欢办公软件";
        let end = word_after(text, 0).unwrap();
        assert!(end > 0 && end < text.len());
        assert!(text.is_char_boundary(end));
        assert_eq!(word_before(text, end), Some(0));
    }
}
