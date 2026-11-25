//! Finding and replacing text.
//!
//! Every kind of query — plain text, whole words, regular expressions, with or
//! without case — is compiled to one regular expression of the `regex`
//! project (`regex-automata`'s meta engine, the engine `regex` itself is built
//! on). Matching is by Unicode character, never by byte: a match always
//! starts and ends between characters, `.` is one character, and case is
//! folded the Unicode way.
//!
//! A search runs through the text in windows of [`WINDOW`] bytes and checks
//! a cancel flag between them, so a search through 100 MB on a worker thread
//! stops soon after it is told to. Each window may look [`OVERLAP`] bytes past
//! its end, so a match that starts in a window and ends in the next is still
//! found whole, as long as it is shorter than that.
//!
//! Not done: full-width and half-width forms, or hiragana and katakana, are
//! different characters here, as are composed and decomposed accents.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use regex_automata::meta::Regex;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};

/// Bytes searched between two looks at the cancel flag.
pub const WINDOW: usize = 1024 * 1024;
/// How far past its window a match may run and still be found.
pub const OVERLAP: usize = 64 * 1024;
/// A search stops collecting after this many matches.
pub const MAX_MATCHES: usize = 1_000_000;

/// What to look for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// `pattern` is a regular expression rather than plain text.
    pub regex: bool,
}

/// The matches of a search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Byte ranges, in order, not overlapping. Empty ranges are matches of
    /// nothing, such as `^` in a regular expression.
    pub matches: Vec<Range<usize>>,
    /// More than [`MAX_MATCHES`] matched; only that many were kept.
    pub truncated: bool,
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Finder {
    regex: Regex,
    query: Query,
}

impl Finder {
    /// Compile a query. `Ok(None)` for an empty pattern, which matches
    /// nothing; `Err` with the reason for a regular expression that does
    /// not compile.
    pub fn new(query: &Query) -> Result<Option<Self>, String> {
        Self::build(query, false)
    }

    /// Like [`Finder::new`], but a match must be the whole of the text it
    /// is looked for in: a spreadsheet's "match entire cell". (`^` and `$`
    /// are line ends here, so this anchors at the ends of the text itself.)
    pub fn new_whole(query: &Query) -> Result<Option<Self>, String> {
        Self::build(query, true)
    }

    fn build(query: &Query, whole_text: bool) -> Result<Option<Self>, String> {
        if query.pattern.is_empty() {
            return Ok(None);
        }
        let body = if query.regex {
            query.pattern.clone()
        } else {
            regex_syntax::escape(&query.pattern)
        };
        let pattern = if whole_text {
            format!(r"\A(?:{body})\z")
        } else if query.whole_word {
            format!(r"\b(?:{body})\b")
        } else {
            body
        };
        let regex = Regex::builder()
            .syntax(
                syntax::Config::new()
                    .case_insensitive(!query.case_sensitive)
                    // `^` and `$` are the ends of lines, as in other editors.
                    .multi_line(true)
                    .crlf(false),
            )
            .build(&pattern)
            .map_err(|err| err.to_string())?;
        Ok(Some(Self {
            regex,
            query: query.clone(),
        }))
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    /// Every match in `text`. Returns `None` if `cancel` was set before the
    /// search finished. `progress` hears the number of matches so far after
    /// each window.
    pub fn find_all(
        &self,
        text: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(usize),
    ) -> Option<Found> {
        let len = text.len();
        let mut found = Found::default();
        let mut at = 0;
        while at <= len {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let end = floor_boundary(text, (at + WINDOW).min(len));
            let end = if end <= at { len } else { end };
            let reach = floor_boundary(text, (end + OVERLAP).min(len)).max(end);
            // Matches that start in [at, end); they may run up to `reach`.
            loop {
                let input = Input::new(text).range(at..reach);
                let Some(found_match) = self.regex.search(&input) else {
                    at = at.max(end);
                    break;
                };
                if found_match.start() >= end && end < len {
                    at = at.max(end);
                    break;
                }
                if found.matches.len() == MAX_MATCHES {
                    found.truncated = true;
                    return Some(found);
                }
                found.matches.push(found_match.range());
                at = if found_match.is_empty() {
                    next_boundary(text, found_match.end())
                } else {
                    found_match.end()
                };
                if at > len {
                    break;
                }
            }
            progress(found.matches.len());
            if end >= len {
                break;
            }
        }
        Some(found)
    }

    /// The first match at or after byte `from`, wrapping to the start of the
    /// text if there is none after it. For small texts searched directly.
    pub fn find_from(&self, text: &str, from: usize) -> Option<Range<usize>> {
        let from = floor_boundary(text, from.min(text.len()));
        let after = self.regex.search(&Input::new(text).range(from..));
        after
            .or_else(|| self.regex.search(&Input::new(text)))
            .map(|m| m.range())
    }

    /// What a match is replaced with: `replacement` as it is for plain
    /// text; for a regular expression, with `$1`, `${name}` and `$$`
    /// expanded from the match's groups. `text` is enough of the document
    /// around the match for look-around such as `\b`; `at` is where the
    /// match is in it.
    pub fn replacement(&self, text: &str, at: Range<usize>, replacement: &str) -> String {
        if !self.query.regex {
            return replacement.to_owned();
        }
        let mut caps = self.regex.create_captures();
        let input = Input::new(text).range(at.clone()).anchored(Anchored::Yes);
        self.regex.search_captures(&input, &mut caps);
        if !caps.is_match() {
            return replacement.to_owned();
        }
        let mut out = String::new();
        caps.interpolate_string_into(text, replacement, &mut out);
        out
    }

    /// Replace every match in `text` at once: the range from the first
    /// match to the end of the last, and what it becomes. `None` if there is
    /// no match or `cancel` was set.
    pub fn replace_all(
        &self,
        text: &str,
        matches: &[Range<usize>],
        replacement: &str,
        cancel: &AtomicBool,
    ) -> Option<(Range<usize>, String)> {
        let first = matches.first()?.start;
        let last = matches.last()?.end;
        let mut out = String::with_capacity(last - first);
        let mut at = first;
        for (index, m) in matches.iter().enumerate() {
            if index % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            out.push_str(&text[at..m.start]);
            out.push_str(&self.replacement(text, m.clone(), replacement));
            at = m.end;
        }
        out.push_str(&text[at..last]);
        Some((first..last, out))
    }
}

impl Found {
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Keep the matches in step with an edit: `old` became `inserted` bytes.
    /// Matches after it move; matches it touched are dropped, since they
    /// may no longer match. Returns whether any was dropped.
    pub fn edited(&mut self, old: Range<usize>, inserted: usize) -> bool {
        let before = self.matches.len();
        let shift = |at: usize| at + inserted - old.len();
        self.matches
            .retain(|m| m.end <= old.start || m.start >= old.end);
        for m in &mut self.matches {
            if m.start >= old.end {
                *m = shift(m.start)..shift(m.end);
            }
        }
        self.matches.len() != before
    }

    /// The first match that starts at or after `offset`, wrapping around.
    pub fn next_from(&self, offset: usize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let index = self.matches.partition_point(|m| m.start < offset);
        Some(if index == self.matches.len() {
            0
        } else {
            index
        })
    }

    /// The last match that starts before `offset`, wrapping around.
    pub fn previous_before(&self, offset: usize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let index = self.matches.partition_point(|m| m.start < offset);
        Some(if index == 0 {
            self.matches.len() - 1
        } else {
            index - 1
        })
    }

    /// The matches that overlap `range`, with the index of the first.
    pub fn within(&self, range: Range<usize>) -> (usize, &[Range<usize>]) {
        let first = self.matches.partition_point(|m| m.end < range.start);
        let last = self.matches.partition_point(|m| m.start <= range.end);
        (first, &self.matches[first..last.max(first)])
    }
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The boundary after the character at `at`, or one past the end.
fn next_boundary(text: &str, at: usize) -> usize {
    match text[at..].chars().next() {
        Some(c) => at + c.len_utf8(),
        None => at + 1,
    }
}

#[cfg(test)]
// The matches are a list of ranges; a list with one match is meant.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn query(pattern: &str) -> Query {
        Query {
            pattern: pattern.to_owned(),
            case_sensitive: true,
            ..Query::default()
        }
    }

    fn find(query: &Query, text: &str) -> Vec<String> {
        let finder = Finder::new(query).unwrap().expect("a pattern");
        let found = finder
            .find_all(text, &AtomicBool::new(false), |_| {})
            .expect("not cancelled");
        found
            .matches
            .iter()
            .map(|range| text[range.clone()].to_owned())
            .collect()
    }

    fn ranges(query: &Query, text: &str) -> Vec<Range<usize>> {
        let finder = Finder::new(query).unwrap().expect("a pattern");
        finder
            .find_all(text, &AtomicBool::new(false), |_| {})
            .unwrap()
            .matches
    }

    #[test]
    fn plain_text_with_and_without_case() {
        let text = "Rust rust RUST 中文 rust";
        assert_eq!(find(&query("rust"), text), ["rust", "rust"]);
        let insensitive = Query {
            case_sensitive: false,
            ..query("rust")
        };
        assert_eq!(find(&insensitive, text).len(), 4);
        // Unicode case folding, not just ASCII.
        let greek = Query {
            case_sensitive: false,
            ..query("σ")
        };
        assert_eq!(find(&greek, "ΣσςX").len(), 3);
        // Regular-expression characters in plain text are plain.
        assert_eq!(find(&query("a.b"), "a.b axb"), ["a.b"]);
        assert_eq!(find(&query("$1 (x)"), "cost $1 (x)"), ["$1 (x)"]);
    }

    #[test]
    fn cjk_and_emoji_match_by_character() {
        let text = "日本語の文章と中文，还有表情👨‍👩‍👧和😀。";
        assert_eq!(find(&query("文章"), text), ["文章"]);
        assert_eq!(find(&query("😀"), text), ["😀"]);
        assert_eq!(find(&query("👨‍👩‍👧"), text), ["👨‍👩‍👧"]);
        let regex = Query {
            regex: true,
            ..query("中.")
        };
        // `.` is one character, not one byte.
        assert_eq!(find(&regex, text), ["中文"]);
        let any = Query {
            regex: true,
            ..query(".")
        };
        let all = ranges(&any, "a中😀");
        assert_eq!(all, [0..1, 1..4, 4..8]);
        // Full-width and half-width forms are different characters.
        assert!(
            Finder::new(&query("ＡＢ"))
                .unwrap()
                .unwrap()
                .find_all("AB", &AtomicBool::new(false), |_| {})
                .unwrap()
                .matches
                .is_empty()
        );
    }

    #[test]
    fn the_whole_text_only() {
        let hits = |query: &Query, text: &str| {
            Finder::new_whole(query)
                .unwrap()
                .unwrap()
                .find_from(text, 0)
                .is_some()
        };
        let cat = query("cat");
        assert!(hits(&cat, "cat"));
        assert!(!hits(&cat, "cat "));
        assert!(!hits(&cat, "a cat"));
        // Not the end of a line: a cell of several lines is one text.
        assert!(!hits(&cat, "cat\ndog"));
        assert!(!hits(&cat, "dog\ncat"));
        let any = Query {
            regex: true,
            ..query("cat|dog")
        };
        assert!(hits(&any, "dog") && !hits(&any, "cat dog"));
        let folded = Query {
            case_sensitive: false,
            ..query("cat")
        };
        assert!(hits(&folded, "CAT"));
        // Characters of the pattern that mean something stay plain text.
        assert!(hits(&query("1+1"), "1+1") && !hits(&query("1+1"), "11"));
    }

    #[test]
    fn whole_words() {
        let whole = Query {
            whole_word: true,
            ..query("cat")
        };
        assert_eq!(ranges(&whole, "cat catalog concat cat."), [0..3, 19..22]);
    }

    #[test]
    fn regular_expressions_across_lines_and_at_line_ends() {
        let text = "# 标题\n\n段落一\n段落二\n";
        let across = Query {
            regex: true,
            ..query(r"标题\n\n段")
        };
        assert_eq!(find(&across, text), ["标题\n\n段"]);
        let line_starts = Query {
            regex: true,
            ..query("^段.+$")
        };
        assert_eq!(find(&line_starts, text), ["段落一", "段落二"]);
    }

    #[test]
    fn empty_matches_and_empty_patterns() {
        assert!(Finder::new(&query("")).unwrap().is_none());
        let starts = Query {
            regex: true,
            ..query("^")
        };
        assert_eq!(ranges(&starts, "a\n中\n"), [0..0, 2..2, 6..6]);
        // An empty match never splits a character and never loops.
        let nothing = Query {
            regex: true,
            ..query("x*")
        };
        let found = ranges(&nothing, "中x");
        assert_eq!(found, [0..0, 3..4, 4..4]);
        assert_eq!(ranges(&nothing, ""), [0..0]);
    }

    #[test]
    fn bad_regular_expressions_are_reported() {
        let bad = Query {
            regex: true,
            ..query("(unclosed")
        };
        assert!(Finder::new(&bad).is_err());
    }

    #[test]
    fn replacements_expand_groups_only_in_regex_mode() {
        let text = "2026-11-23 和 2025-01-02";
        let dates = Query {
            regex: true,
            ..query(r"(\d{4})-(\d{2})-(?P<day>\d{2})")
        };
        let finder = Finder::new(&dates).unwrap().unwrap();
        let found = finder
            .find_all(text, &AtomicBool::new(false), |_| {})
            .unwrap();
        let (range, replaced) = finder
            .replace_all(
                text,
                &found.matches,
                "${day}/$2/$1 ($$)",
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(range, 0..text.len());
        assert_eq!(replaced, "23/11/2026 ($) 和 02/01/2025 ($)");

        let plain = Finder::new(&query("a")).unwrap().unwrap();
        assert_eq!(plain.replacement("a", 0..1, "$1 $$"), "$1 $$");
        let found = plain
            .find_all("banana", &AtomicBool::new(false), |_| {})
            .unwrap();
        let (range, replaced) = plain
            .replace_all("banana", &found.matches, "o", &AtomicBool::new(false))
            .unwrap();
        assert_eq!((range, replaced.as_str()), (1..6, "onono"));
    }

    #[test]
    fn replacing_empty_matches_inserts() {
        let starts = Query {
            regex: true,
            ..query("^")
        };
        let finder = Finder::new(&starts).unwrap().unwrap();
        let text = "一\n二";
        let found = finder
            .find_all(text, &AtomicBool::new(false), |_| {})
            .unwrap();
        let (range, replaced) = finder
            .replace_all(text, &found.matches, "> ", &AtomicBool::new(false))
            .unwrap();
        let mut result = text.to_owned();
        result.replace_range(range, &replaced);
        assert_eq!(result, "> 一\n> 二");
    }

    #[test]
    fn matches_across_window_edges_are_found_once() {
        // Place a match across every window boundary.
        let mut text = String::new();
        while text.len() < 3 * WINDOW {
            text.push_str("填充文字 filler ");
        }
        let mut expected = Vec::new();
        for boundary in [WINDOW, 2 * WINDOW] {
            let at = floor_boundary(&text, boundary - 2);
            text.insert_str(at, "NEEDLE");
            expected.push(at);
        }
        let found = ranges(&query("NEEDLE"), &text);
        assert_eq!(found.iter().map(|r| r.start).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn a_search_can_be_cancelled() {
        let text = "文字 ".repeat(2 * WINDOW / 7);
        let finder = Finder::new(&query("字")).unwrap().unwrap();
        let cancel = AtomicBool::new(false);
        let mut windows = 0;
        let result = finder.find_all(&text, &cancel, |_| {
            windows += 1;
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(result.is_none());
        assert_eq!(windows, 1, "stopped after the first window");
        let cancelled = AtomicBool::new(true);
        assert!(
            finder
                .replace_all(&text, &[0..3], "x", &cancelled)
                .is_none()
        );
    }

    #[test]
    fn find_from_wraps_around() {
        let finder = Finder::new(&query("a")).unwrap().unwrap();
        assert_eq!(finder.find_from("a b a", 1), Some(4..5));
        assert_eq!(finder.find_from("a b a", 5), Some(0..1));
        assert_eq!(finder.find_from("bbb", 0), None);
    }

    #[test]
    fn matches_follow_edits_and_navigation_wraps() {
        let text = "ab ab ab";
        let mut found = Found {
            matches: ranges(&query("ab"), text),
            truncated: false,
        };
        assert_eq!(found.next_from(0), Some(0));
        assert_eq!(found.next_from(1), Some(1));
        assert_eq!(found.next_from(7), Some(0));
        assert_eq!(found.previous_before(3), Some(0));
        assert_eq!(found.previous_before(0), Some(2));
        // Typing before the second match moves the rest.
        assert!(!found.edited(2..2, 3));
        assert_eq!(found.matches, [0..2, 6..8, 9..11]);
        // Typing inside one drops it.
        assert!(found.edited(7..7, 1));
        assert_eq!(found.matches, [0..2, 10..12]);
        // Deleting across one drops it and moves the rest back.
        assert!(found.edited(0..3, 0));
        assert_eq!(found.matches, [7..9]);
        let (first, visible) = found.within(0..8);
        assert_eq!((first, visible), (0, &[7..9][..]));
        assert!(Found::default().next_from(0).is_none());
    }

    /// 100 MB: the search is cancelled from another thread and stops fast.
    /// Slow to set up, so not part of the default tests:
    /// `cargo test --release -p aster-markdown -- --ignored`.
    #[test]
    #[ignore]
    fn a_search_through_100_mb_stops_when_cancelled() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let text: Arc<str> = "这是一段中文 with English 和日本語のテキスト。\n"
            .repeat(100 * 1024 * 1024 / 70)
            .into();
        let finder = Finder::new(&Query {
            regex: true,
            ..query(r"\p{Han}{2}\s\w+")
        })
        .unwrap()
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let worker = {
            let (text, cancel, finder) = (text.clone(), cancel.clone(), finder.clone());
            std::thread::spawn(move || finder.find_all(&text, &cancel, |_| {}))
        };
        std::thread::sleep(Duration::from_millis(50));
        cancel.store(true, Ordering::Relaxed);
        let cancelled_at = Instant::now();
        let result = worker.join().unwrap();
        let stopped = cancelled_at.elapsed();
        assert!(result.is_none(), "finished before it was cancelled");
        assert!(
            stopped < Duration::from_millis(200),
            "took {stopped:?} to stop"
        );
        eprintln!(
            "cancelled after {:?}, stopped {stopped:?} later",
            cancelled_at - started
        );
        // And uncancelled, it finds everything.
        let full = Instant::now();
        let found = finder
            .find_all(&text, &AtomicBool::new(false), |_| {})
            .unwrap();
        eprintln!("{} matches in {:?}", found.matches.len(), full.elapsed());
        assert!(found.matches.len() > 1_000_000 || found.truncated);
    }
}
