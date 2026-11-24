//! Text edits on a [`TextBody`]: positions, typing, new paragraphs,
//! deleting, and formatting a range.
//!
//! A position is a paragraph and a byte offset in its text (the runs'
//! texts one after another). A line break inside a paragraph is `\n` in
//! a run's text; a new paragraph is a new [`Paragraph`]. Runs keep their
//! sources through edits: text typed into a run becomes part of it, a run
//! split by formatting gives both halves its source, and a paragraph split
//! by Enter gives the new one its properties. After every edit runs that
//! look alike and came from the same place are joined again and the
//! numbering is worked out anew.

use std::ops::Range;
use std::sync::Arc;

use crate::{Paragraph, Run, RunSource, TextBody};

/// A place in a text body: paragraph index, byte offset in its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
    pub para: usize,
    pub offset: usize,
}

impl TextPos {
    pub const fn new(para: usize, offset: usize) -> Self {
        Self { para, offset }
    }
}

/// A line break inside a paragraph, as text to insert.
pub const LINE_BREAK: char = '\u{b}';

impl TextBody {
    /// Make sure there is at least one paragraph to type into.
    pub fn ensure_paragraph(&mut self) {
        if self.paragraphs.is_empty() {
            let levels = self.level_looks();
            self.paragraphs.push(Paragraph::new_at(0, &levels));
        }
    }

    /// The end of the text.
    pub fn end(&self) -> TextPos {
        match self.paragraphs.len() {
            0 => TextPos::default(),
            n => TextPos::new(n - 1, self.paragraphs[n - 1].len()),
        }
    }

    /// `pos` moved onto a character boundary inside the text.
    pub fn clamp(&self, pos: TextPos) -> TextPos {
        let Some(last) = self.paragraphs.len().checked_sub(1) else {
            return TextPos::default();
        };
        let para = pos.para.min(last);
        let text = self.paragraphs[para].text();
        let mut offset = pos.offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        TextPos::new(para, offset)
    }

    /// One character back, into the paragraph before at its start.
    pub fn before(&self, pos: TextPos) -> Option<TextPos> {
        let pos = self.clamp(pos);
        if pos.offset > 0 {
            let text = self.paragraphs[pos.para].text();
            let (at, _) = text[..pos.offset].char_indices().next_back()?;
            return Some(TextPos::new(pos.para, at));
        }
        let para = pos.para.checked_sub(1)?;
        Some(TextPos::new(para, self.paragraphs[para].len()))
    }

    /// One character on, into the next paragraph at its end.
    pub fn after(&self, pos: TextPos) -> Option<TextPos> {
        let pos = self.clamp(pos);
        let text = self.paragraphs.get(pos.para)?.text();
        if let Some(c) = text[pos.offset..].chars().next() {
            return Some(TextPos::new(pos.para, pos.offset + c.len_utf8()));
        }
        (pos.para + 1 < self.paragraphs.len()).then(|| TextPos::new(pos.para + 1, 0))
    }

    /// The text between two positions, paragraphs ending in `\n`.
    pub fn text_in(&self, start: TextPos, end: TextPos) -> String {
        let (start, end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        let mut out = String::new();
        for (para, paragraph) in self
            .paragraphs
            .iter()
            .enumerate()
            .take(end.para + 1)
            .skip(start.para)
        {
            let text = paragraph.text();
            let from = if para == start.para { start.offset } else { 0 };
            let to = if para == end.para {
                end.offset
            } else {
                text.len()
            };
            out.push_str(&text[from..to.max(from)]);
            if para != end.para {
                out.push('\n');
            }
        }
        out
    }

    /// Insert `text` at `at`: `\n` starts a new paragraph, [`LINE_BREAK`]
    /// breaks the line inside one. Returns the position after it.
    pub fn insert(&mut self, at: TextPos, text: &str) -> TextPos {
        self.ensure_paragraph();
        let mut pos = self.clamp(at);
        for (i, piece) in text.split('\n').enumerate() {
            if i > 0 {
                pos = self.split_at(pos);
            }
            let piece = piece.replace(['\r'], "").replace(LINE_BREAK, "\n");
            if !piece.is_empty() {
                pos = self.insert_plain(pos, &piece);
            }
        }
        self.tidy();
        pos
    }

    /// Start a new paragraph at `at`, as Enter does. Returns the start of
    /// the new paragraph.
    pub fn split(&mut self, at: TextPos) -> TextPos {
        self.ensure_paragraph();
        let pos = self.split_at(self.clamp(at));
        self.tidy();
        pos
    }

    /// Delete the text between two positions, joining paragraphs. Returns
    /// where the caret goes.
    pub fn delete(&mut self, start: TextPos, end: TextPos) -> TextPos {
        if self.paragraphs.is_empty() {
            return TextPos::default();
        }
        let (start, end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        if start == end {
            return start;
        }
        if start.para == end.para {
            self.remove_in(start.para, start.offset..end.offset);
        } else {
            let tail_len = self.paragraphs[end.para].len();
            let first_len = self.paragraphs[start.para].len();
            self.remove_in(start.para, start.offset..first_len);
            self.remove_in(end.para, 0..end.offset.min(tail_len));
            let tail = self.paragraphs.remove(end.para);
            self.paragraphs.drain(start.para + 1..end.para);
            let first = &mut self.paragraphs[start.para];
            if first.runs.is_empty() && !tail.runs.is_empty() {
                // Everything before went: the text left keeps its looks.
                first.end = tail.end.clone();
            }
            first.runs.extend(tail.runs);
        }
        self.tidy();
        start
    }

    /// Apply `f` to the runs between two positions, splitting runs at the
    /// ends. An empty range formats the word at it, or an empty
    /// paragraph's end mark.
    pub fn format(&mut self, start: TextPos, end: TextPos, mut f: impl FnMut(&mut Run)) {
        if self.paragraphs.is_empty() {
            return;
        }
        let (mut start, mut end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        if start == end {
            let word = self.word_at(start);
            if word.0 == word.1 {
                let paragraph = &mut self.paragraphs[start.para];
                if paragraph.is_empty() {
                    f(&mut paragraph.end);
                    paragraph.end_size = paragraph.end.size;
                }
                return;
            }
            (start, end) = word;
        }
        for (para, paragraph) in self
            .paragraphs
            .iter_mut()
            .enumerate()
            .take(end.para + 1)
            .skip(start.para)
        {
            let len = paragraph.len();
            let from = if para == start.para { start.offset } else { 0 };
            let to = if para == end.para { end.offset } else { len };
            if from >= to {
                if paragraph.is_empty() {
                    f(&mut paragraph.end);
                    paragraph.end_size = paragraph.end.size;
                }
                continue;
            }
            split_run_at(paragraph, from);
            split_run_at(paragraph, to);
            let mut at = 0;
            for run in &mut paragraph.runs {
                let run_end = at + run.text.len();
                if at >= from && run_end <= to && !run.text.is_empty() {
                    f(run);
                }
                at = run_end;
            }
        }
        self.tidy();
    }

    /// The runs a range covers, or the run before a caret; for showing what
    /// the toolbar's buttons apply to.
    pub fn run_at(&self, at: TextPos) -> Option<&Run> {
        let at = self.clamp(at);
        let paragraph = self.paragraphs.get(at.para)?;
        let mut start = 0;
        for run in &paragraph.runs {
            let end = start + run.text.len();
            if at.offset > start && at.offset <= end || at.offset == 0 && start == 0 {
                return Some(run);
            }
            start = end;
        }
        paragraph.runs.last().or(Some(&paragraph.end))
    }

    /// The paragraphs a range touches.
    pub fn paragraphs_in(&self, start: TextPos, end: TextPos) -> Range<usize> {
        if self.paragraphs.is_empty() {
            return 0..0;
        }
        let (start, end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        start.para..end.para + 1
    }

    /// The word around `at`: letters and digits, a stretch of Han, of kana,
    /// of Hangul, of spaces, or one other character.
    pub fn word_at(&self, at: TextPos) -> (TextPos, TextPos) {
        let at = self.clamp(at);
        let Some(paragraph) = self.paragraphs.get(at.para) else {
            return (at, at);
        };
        let text = paragraph.text();
        let class_at = |offset: usize| text[offset..].chars().next().map(word_class);
        // The character after the caret, else the one before.
        let (anchor, class) = match class_at(at.offset) {
            Some(class) if class != WordClass::Break => (at.offset, class),
            _ => match text[..at.offset].char_indices().next_back() {
                Some((i, c)) if word_class(c) != WordClass::Break => (i, word_class(c)),
                _ => return (at, at),
            },
        };
        if class == WordClass::Other {
            let len = text[anchor..].chars().next().map_or(0, char::len_utf8);
            return (
                TextPos::new(at.para, anchor),
                TextPos::new(at.para, anchor + len),
            );
        }
        let mut start = anchor;
        for (i, c) in text[..anchor].char_indices().rev() {
            if word_class(c) != class {
                break;
            }
            start = i;
        }
        let mut end = anchor;
        for c in text[anchor..].chars() {
            if word_class(c) != class {
                break;
            }
            end += c.len_utf8();
        }
        (TextPos::new(at.para, start), TextPos::new(at.para, end))
    }

    // --- Internals ---

    fn insert_plain(&mut self, at: TextPos, text: &str) -> TextPos {
        let paragraph = &mut self.paragraphs[at.para];
        if paragraph.runs.is_empty() {
            let mut run = paragraph.end.clone();
            run.text = text.to_owned();
            paragraph.runs.push(run);
            return TextPos::new(at.para, text.len());
        }
        // Into the run that ends at or holds the caret: typed text takes the
        // look of the text before it.
        let mut start = 0;
        let count = paragraph.runs.len();
        for (i, run) in paragraph.runs.iter_mut().enumerate() {
            let end = start + run.text.len();
            if at.offset <= end && (at.offset > start || start == 0 || i + 1 == count) {
                let inside = (at.offset - start).min(run.text.len());
                run.text.insert_str(inside, text);
                return TextPos::new(at.para, at.offset + text.len());
            }
            start = end;
        }
        let last = paragraph.runs.last_mut().expect("not empty");
        last.text.push_str(text);
        TextPos::new(at.para, paragraph.len())
    }

    fn split_at(&mut self, at: TextPos) -> TextPos {
        let paragraph = &mut self.paragraphs[at.para];
        split_run_at(paragraph, at.offset);
        let mut offset = 0;
        let mut keep = paragraph.runs.len();
        for (i, run) in paragraph.runs.iter().enumerate() {
            if offset >= at.offset {
                keep = i;
                break;
            }
            offset += run.text.len();
        }
        let moved: Vec<Run> = paragraph.runs.drain(keep..).collect();
        let mut next = paragraph.clone();
        next.runs = moved;
        // Typing on in an emptied line keeps the look of the text there.
        if paragraph.runs.is_empty()
            && let Some(first) = next.runs.first()
        {
            paragraph.end = Run {
                text: String::new(),
                ..first.clone()
            };
            paragraph.end_size = paragraph.end.size;
        }
        if next.runs.is_empty()
            && let Some(last) = paragraph.runs.last()
        {
            next.end = Run {
                text: String::new(),
                ..last.clone()
            };
            next.end_size = next.end.size;
        }
        self.paragraphs.insert(at.para + 1, next);
        TextPos::new(at.para + 1, 0)
    }

    fn remove_in(&mut self, para: usize, range: Range<usize>) {
        let paragraph = &mut self.paragraphs[para];
        let had_runs = !paragraph.runs.is_empty();
        let first_look = paragraph.runs.first().cloned();
        let mut start = 0;
        for run in &mut paragraph.runs {
            let end = start + run.text.len();
            let from = range.start.max(start);
            let to = range.end.min(end);
            if from < to {
                run.text.replace_range(from - start..to - start, "");
            }
            start = end;
        }
        paragraph.runs.retain(|run| !run.text.is_empty());
        if had_runs
            && paragraph.runs.is_empty()
            && let Some(first) = first_look
        {
            paragraph.end = Run {
                text: String::new(),
                ..first
            };
            paragraph.end_size = paragraph.end.size;
        }
    }

    /// Join runs that can be one, drop empty ones, renumber.
    fn tidy(&mut self) {
        for paragraph in &mut self.paragraphs {
            paragraph.runs.retain(|run| !run.text.is_empty());
            let mut joined: Vec<Run> = Vec::with_capacity(paragraph.runs.len());
            for run in paragraph.runs.drain(..) {
                match joined.last_mut() {
                    Some(last) if last.joins(&run) => last.text.push_str(&run.text),
                    _ => joined.push(run),
                }
            }
            paragraph.runs = joined;
        }
        self.renumber();
    }
}

/// Split the run that holds `offset` (not at its ends) in two.
fn split_run_at(paragraph: &mut Paragraph, offset: usize) {
    let mut start = 0;
    for i in 0..paragraph.runs.len() {
        let len = paragraph.runs[i].text.len();
        if offset > start && offset < start + len {
            let tail = paragraph.runs[i].text.split_off(offset - start);
            let mut second = paragraph.runs[i].clone();
            second.text = tail;
            paragraph.runs.insert(i + 1, second);
            return;
        }
        start += len;
    }
}

/// A run made from a paragraph's end mark, as text typed into an empty
/// paragraph is.
pub fn end_source(props: &str, original: &Run) -> Arc<RunSource> {
    Arc::new(RunSource {
        xml: String::new(),
        props: props.to_owned(),
        original: Run {
            text: String::new(),
            source: None,
            ..original.clone()
        },
        own_size: false,
        own_bold: false,
        own_italic: false,
        own_color: false,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordClass {
    Word,
    Han,
    Kana,
    Hangul,
    Space,
    Break,
    Other,
}

fn word_class(c: char) -> WordClass {
    match c {
        '\n' => WordClass::Break,
        c if c.is_whitespace() => WordClass::Space,
        '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9F}' => {
            WordClass::Kana
        }
        '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' => WordClass::Hangul,
        '\u{3400}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{20000}'..='\u{3FFFF}' => {
            WordClass::Han
        }
        c if c.is_alphanumeric() || c == '_' || c == '\'' => WordClass::Word,
        _ => WordClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{List, default_levels};

    fn body(lines: &[&[(&str, bool)]]) -> TextBody {
        let mut body = TextBody {
            levels: Some(Arc::new(default_levels())),
            ..TextBody::default()
        };
        for line in lines {
            let mut paragraph = Paragraph::new_at(0, &default_levels());
            for (text, bold) in *line {
                paragraph.runs.push(Run {
                    bold: *bold,
                    ..Run::new(*text, 18.0)
                });
            }
            body.paragraphs.push(paragraph);
        }
        body
    }

    fn texts(body: &TextBody) -> Vec<String> {
        body.paragraphs.iter().map(Paragraph::text).collect()
    }

    #[test]
    fn typing_joins_the_run_before_the_caret() {
        let mut b = body(&[&[("ab", false), ("cd", true)]]);
        let at = b.insert(TextPos::new(0, 2), "X");
        assert_eq!(at, TextPos::new(0, 3));
        assert_eq!(b.paragraphs[0].runs[0].text, "abX");
        assert_eq!(b.paragraphs[0].runs[1].text, "cd");
        let at = b.insert(TextPos::new(0, 0), "東京");
        assert_eq!(at.offset, "東京".len());
        assert_eq!(b.paragraphs[0].runs[0].text, "東京abX");
        // A line break stays in the paragraph.
        b.insert(TextPos::new(0, 0), "a\u{b}");
        assert_eq!(b.paragraphs.len(), 1);
        assert!(b.paragraphs[0].text().starts_with("a\n"));
    }

    #[test]
    fn enter_and_backspace_split_and_join_paragraphs() {
        let mut b = body(&[&[("第一", false), ("段落", true)]]);
        let at = b.split(TextPos::new(0, "第一".len()));
        assert_eq!(at, TextPos::new(1, 0));
        assert_eq!(texts(&b), ["第一", "段落"]);
        assert!(b.paragraphs[1].runs[0].bold);
        let back = b.before(at).unwrap();
        assert_eq!(back, TextPos::new(0, "第一".len()));
        let at = b.delete(back, at);
        assert_eq!(at, TextPos::new(0, "第一".len()));
        assert_eq!(texts(&b), ["第一段落"]);
        assert_eq!(b.paragraphs[0].runs.len(), 2);
        // Enter at the very end: the new paragraph types as the last run.
        let at = b.split(b.end());
        assert_eq!(texts(&b), ["第一段落", ""]);
        b.insert(at, "x");
        assert!(b.paragraphs[1].runs[0].bold);
        // Pasted lines become paragraphs.
        let at = b.insert(TextPos::new(0, 0), "1\n2\n");
        assert_eq!(at, TextPos::new(2, 0));
        assert_eq!(texts(&b), ["1", "2", "第一段落", "x"]);
    }

    #[test]
    fn deleting_across_paragraphs_and_everything() {
        let mut b = body(&[&[("abc", false)], &[("def", true)], &[("ghi", false)]]);
        let at = b.delete(TextPos::new(0, 1), TextPos::new(2, 2));
        assert_eq!(at, TextPos::new(0, 1));
        assert_eq!(texts(&b), ["ai"]);
        assert_eq!(b.text_in(TextPos::new(0, 0), b.end()), "ai");
        let at = b.delete(TextPos::new(0, 0), b.end());
        assert_eq!(at, TextPos::new(0, 0));
        assert_eq!(texts(&b), [""]);
        // The look of what was deleted is kept for typing on.
        let mut b = body(&[&[("bold", true)]]);
        b.delete(TextPos::new(0, 0), b.end());
        b.insert(TextPos::new(0, 0), "again");
        assert!(b.paragraphs[0].runs[0].bold);
    }

    #[test]
    fn formatting_splits_runs_and_joins_them_back() {
        let mut b = body(&[&[("hello world", false)]]);
        b.format(TextPos::new(0, 0), TextPos::new(0, 5), |run| {
            run.bold = true
        });
        let runs = &b.paragraphs[0].runs;
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].text.as_str(), runs[0].bold), ("hello", true));
        b.format(TextPos::new(0, 0), TextPos::new(0, 5), |run| {
            run.bold = false
        });
        assert_eq!(b.paragraphs[0].runs.len(), 1);
        // A caret formats the word it is in.
        b.format(TextPos::new(0, 8), TextPos::new(0, 8), |run| {
            run.italic = true
        });
        let runs = &b.paragraphs[0].runs;
        assert_eq!(runs[1].text, "world");
        assert!(runs[1].italic);
        assert_eq!(b.text_in(TextPos::new(0, 3), TextPos::new(0, 8)), "lo wo");
        // An empty paragraph's end mark takes the format.
        let mut b = body(&[&[]]);
        b.format(TextPos::new(0, 0), TextPos::new(0, 0), |run| {
            run.size = 40.0
        });
        b.insert(TextPos::new(0, 0), "big");
        assert_eq!(b.paragraphs[0].runs[0].size, 40.0);
    }

    #[test]
    fn words_by_script() {
        let b = body(&[&[("Hello, 東京タワー です", false)]]);
        let word = |offset: usize| {
            let (a, b2) = b.word_at(TextPos::new(0, offset));
            b.text_in(a, b2)
        };
        assert_eq!(word(1), "Hello");
        assert_eq!(word(5), ",");
        let tokyo = "Hello, ".len();
        assert_eq!(word(tokyo + 3), "東京");
        assert_eq!(word(tokyo + "東京".len()), "タワー");
    }

    #[test]
    fn numbering_counts_by_level_and_moves_with_it() {
        let mut levels = default_levels();
        levels[1].size = 12.0;
        levels[1].list = Some(List::Char("–".into()));
        let mut b = body(&[&[("a", false)], &[("b", false)], &[("c", false)]]);
        b.levels = Some(Arc::new(levels.clone()));
        for p in &mut b.paragraphs {
            p.list = Some(List::Number("arabicPeriod".into(), 1));
        }
        b.renumber();
        let marks: Vec<_> = b.paragraphs.iter().map(|p| p.bullet.clone()).collect();
        assert_eq!(
            marks,
            [Some("1.".into()), Some("2.".into()), Some("3.".into())]
        );
        b.paragraphs[1].set_level(1, &levels);
        b.renumber();
        assert_eq!(b.paragraphs[1].runs[0].size, 12.0);
        assert_eq!(b.paragraphs[1].bullet.as_deref(), Some("–"));
        assert_eq!(b.paragraphs[2].bullet.as_deref(), Some("2."));
        assert_eq!(crate::number_mark("romanUcPeriod", 4), "IV.");
        assert_eq!(crate::number_mark("alphaLcParenR", 28), "ab)");
    }

    #[test]
    fn moving_by_characters_crosses_paragraphs() {
        let b = body(&[&[("a東", false)], &[("", false)], &[("z", false)]]);
        let mut at = TextPos::default();
        let mut seen = vec![at];
        while let Some(next) = b.after(at) {
            at = next;
            seen.push(at);
        }
        assert_eq!(
            seen,
            [
                TextPos::new(0, 0),
                TextPos::new(0, 1),
                TextPos::new(0, 4),
                TextPos::new(1, 0),
                TextPos::new(2, 0),
                TextPos::new(2, 1)
            ]
        );
        assert_eq!(b.before(TextPos::new(2, 0)), Some(TextPos::new(1, 0)));
        assert_eq!(b.before(TextPos::new(0, 0)), None);
        assert_eq!(b.clamp(TextPos::new(0, 2)), TextPos::new(0, 1));
    }
}
