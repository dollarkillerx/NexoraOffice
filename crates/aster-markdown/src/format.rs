//! Formatting commands on Markdown source: wrap or unwrap emphasis and code,
//! make lines headings, list items or quotes, insert links, images and
//! tables. The editor writes the syntax for the user; nothing here knows
//! about a view.
//!
//! Each command reads only the lines the selection touches, so it costs the
//! same in a 100 MB file as in a short one, and returns one [`Edit`]: a
//! range of the text, what replaces it, and the selection afterwards. The
//! document applies it as one undo step.

use std::ops::Range;

use ropey::Rope;

use crate::document::{Selection, slice};

/// A replacement to make, and where the selection goes afterwards (offsets
/// in the text after the replacement).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Selection,
}

/// Emphasis and code, written around text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Strike,
    Code,
}

impl Inline {
    fn mark(self) -> &'static str {
        match self {
            Inline::Bold => "**",
            Inline::Italic => "*",
            Inline::Strike => "~~",
            Inline::Code => "`",
        }
    }

    /// The character the mark is made of.
    fn unit(self) -> char {
        self.mark().chars().next().expect("marks are not empty")
    }

    /// Whether runs of `left` and `right` mark characters around some text
    /// mean it already has this style.
    fn surrounds(self, left: usize, right: usize) -> bool {
        match self {
            // `***x***` is bold and italic.
            Inline::Bold => matches!(left, 2 | 3) && matches!(right, 2 | 3),
            Inline::Italic => matches!(left, 1 | 3) && matches!(right, 1 | 3),
            Inline::Strike => left == 2 && right == 2,
            Inline::Code => left >= 1 && left == right,
        }
    }
}

/// Line-level styles that are written as a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Bullet,
    Numbered,
    Task,
    Quote,
}

/// The lines a selection touches: from the start of its first line to the
/// end of its last line, without the last line break. A selection that ends
/// at the very start of a line does not take that line.
fn line_span(text: &Rope, range: &Range<usize>) -> Range<usize> {
    let first = text.byte_to_line(range.start);
    let mut last = text.byte_to_line(range.end);
    if last > first && text.line_to_byte(last) == range.end {
        last -= 1;
    }
    let start = text.line_to_byte(first);
    let end = if last + 1 < text.len_lines() {
        text.line_to_byte(last + 1) - 1
    } else {
        text.len_bytes()
    };
    start..end.max(start)
}

/// A few bytes either side of `range`, clamped to the text.
fn around(text: &Rope, range: &Range<usize>, reach: usize) -> (String, String) {
    let len = text.len_bytes();
    let before = floor(text, range.start.saturating_sub(reach));
    let after = ceil(text, (range.end + reach).min(len));
    (
        slice(text, before..range.start),
        slice(text, range.end..after),
    )
}

fn floor(text: &Rope, mut at: usize) -> usize {
    while !crate::document::is_char_boundary(text, at) {
        at -= 1;
    }
    at
}

fn ceil(text: &Rope, mut at: usize) -> usize {
    while at < text.len_bytes() && !crate::document::is_char_boundary(text, at) {
        at += 1;
    }
    at
}

fn run_at_end(text: &str, unit: char) -> usize {
    text.chars().rev().take_while(|c| *c == unit).count()
}

fn run_at_start(text: &str, unit: char) -> usize {
    text.chars().take_while(|c| *c == unit).count()
}

/// Make the selection bold, italic, struck through or code, or take that
/// style away if it already has it. With nothing selected the marks go in
/// with the caret between them. Over several lines each line is wrapped on
/// its own, since emphasis cannot cross a paragraph.
pub fn toggle_inline(text: &Rope, selection: Selection, kind: Inline) -> Edit {
    let range = selection.range();
    let unit = kind.unit();
    let width = kind.mark().len();

    // Marks just outside the selection: `**|text|**`.
    let (before, after) = around(text, &range, 3);
    let left = run_at_end(&before, unit);
    let right = run_at_start(&after, unit);
    if !range.is_empty() && kind.surrounds(left, right) {
        let take = if kind == Inline::Code { left } else { width };
        let inner = slice(text, range.clone());
        let start = range.start - take;
        return Edit {
            range: start..range.end + take,
            text: inner.clone(),
            selection: Selection {
                anchor: start,
                focus: start + inner.len(),
            },
        };
    }

    let selected = slice(text, range.clone());
    // The selection includes its marks: `|**text**|`.
    if selected.len() > 2 * width {
        let lead = run_at_start(&selected, unit);
        let trail = run_at_end(&selected, unit);
        if kind.surrounds(lead, trail) && !selected.contains('\n') {
            let take = if kind == Inline::Code { lead } else { width };
            let inner = selected[take..selected.len() - take].to_owned();
            return Edit {
                range: range.clone(),
                text: inner.clone(),
                selection: Selection {
                    anchor: range.start,
                    focus: range.start + inner.len(),
                },
            };
        }
    }

    if range.is_empty() {
        let mark = kind.mark();
        return Edit {
            range: range.clone(),
            text: format!("{mark}{mark}"),
            selection: Selection::caret(range.start + mark.len()),
        };
    }

    // Wrap each line's text, leaving white space at its ends outside the
    // marks: `** word **` is not emphasis. A line taken from its start
    // keeps its heading, list or quote marker outside the marks too.
    let starts_line = range.start == text.line_to_byte(text.byte_to_line(range.start));
    let mut out = String::with_capacity(selected.len() + 8);
    let mut first_skip = 0;
    for (index, whole) in selected.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let skip = if index > 0 || starts_line {
            line_marker(whole)
        } else {
            0
        };
        if index == 0 {
            first_skip = skip;
        }
        out.push_str(&whole[..skip]);
        let line = &whole[skip..];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push_str(line);
            continue;
        }
        let lead = &line[..line.len() - line.trim_start().len()];
        let trail = &line[line.trim_end().len()..];
        let mark = match kind {
            // Backticks inside code need a longer fence, and a space
            // between the fence and a backtick at either end.
            Inline::Code => {
                let longest = trimmed.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                "`".repeat(longest + 1)
            }
            _ => kind.mark().to_owned(),
        };
        let pad = if kind == Inline::Code && (trimmed.starts_with('`') || trimmed.ends_with('`')) {
            " "
        } else {
            ""
        };
        out.push_str(lead);
        out.push_str(&mark);
        out.push_str(pad);
        out.push_str(trimmed);
        out.push_str(pad);
        out.push_str(&mark);
        out.push_str(trail);
    }
    let selection = if selected.contains('\n') {
        Selection {
            anchor: range.start,
            focus: range.start + out.len(),
        }
    } else {
        // The same text stays selected, now inside its marks.
        let rest = &selected[first_skip..];
        let lead = first_skip + rest.len() - rest.trim_start().len();
        let opening = out[lead..].len() - out[lead..].trim_start_matches(unit).len();
        let start = range.start + lead + opening;
        Selection {
            anchor: start,
            focus: start + rest.trim().len(),
        }
    };
    Edit {
        range,
        text: out,
        selection,
    }
}

/// The parts of a line: indentation, the block prefix, and the rest.
struct Parsed<'a> {
    indent: &'a str,
    prefix: &'a str,
    rest: &'a str,
}

fn heading_prefix(line: &str) -> usize {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return 0;
    }
    let hashes = run_at_start(&line[indent..], '#');
    if !(1..=6).contains(&hashes) {
        return 0;
    }
    let after = &line[indent + hashes..];
    if after.is_empty() {
        return line.len();
    }
    if !after.starts_with([' ', '\t']) {
        return 0;
    }
    indent + hashes + (after.len() - after.trim_start_matches([' ', '\t']).len())
}

/// The heading level of the line the caret is on; 0 for body text. Reads
/// only the start of the line: it runs on every caret move, and a line can
/// be a whole 100 MB file.
pub fn heading_level(text: &Rope, caret: usize) -> u8 {
    let span = line_span(
        text,
        &(caret.min(text.len_bytes())..caret.min(text.len_bytes())),
    );
    let end = ceil(text, (span.start + 16).min(span.end));
    let line = slice(text, span.start..end);
    let prefix = heading_prefix(&line);
    if prefix == 0 {
        return 0;
    }
    run_at_start(line.trim_start_matches(' '), '#') as u8
}

/// How many bytes at the start of a line are its heading, list or quote
/// marker (with the indentation before it), which emphasis must not cover.
fn line_marker(line: &str) -> usize {
    let heading = heading_prefix(line);
    if heading > 0 {
        return heading.min(line.len());
    }
    let (parsed, found) = parse_block(line);
    if found.is_some() {
        parsed.indent.len() + parsed.prefix.len()
    } else {
        0
    }
}

/// The list or quote marker at the start of a line, after its indentation.
fn parse_block(line: &str) -> (Parsed<'_>, Option<Block>) {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, body) = line.split_at(indent_len);
    let found = |len: usize, block: Block| {
        (
            Parsed {
                indent,
                prefix: &body[..len],
                rest: &body[len..],
            },
            Some(block),
        )
    };
    for task in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "+ [ ] "] {
        if body.starts_with(task) {
            return found(task.len(), Block::Task);
        }
    }
    for bullet in ["- ", "* ", "+ "] {
        if body.starts_with(bullet) {
            return found(bullet.len(), Block::Bullet);
        }
    }
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &body[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return found(digits + 2, Block::Numbered);
        }
    }
    if let Some(after) = body.strip_prefix('>') {
        let len = if after.starts_with(' ') { 2 } else { 1 };
        return found(len, Block::Quote);
    }
    (
        Parsed {
            indent,
            prefix: "",
            rest: body,
        },
        None,
    )
}

/// Rewrite each line of the selection's lines with `rewrite`, which gets a
/// line and its index among the non-blank lines and returns the new line and
/// how many bytes of the old line's start became how many of the new one's
/// (so the selection can follow). One [`Edit`] for all of them.
fn rewrite_lines(
    text: &Rope,
    selection: Selection,
    skip_blank: bool,
    mut rewrite: impl FnMut(&str, usize) -> (String, usize, usize),
) -> Edit {
    let range = selection.range();
    let span = line_span(text, &range);
    let old = slice(text, span.clone());
    let mut new = String::with_capacity(old.len() + 16);
    // Old line start, old prefix length, new line start, new prefix length.
    let mut map: Vec<(usize, usize, usize, usize)> = Vec::new();
    let several = old.contains('\n');
    let mut counted = 0;
    let mut old_at = span.start;
    for (index, line) in old.split('\n').enumerate() {
        if index > 0 {
            new.push('\n');
        }
        let new_at = span.start + new.len();
        if skip_blank && several && line.trim().is_empty() {
            new.push_str(line);
            map.push((old_at, 0, new_at, 0));
        } else {
            let (rewritten, old_prefix, new_prefix) = rewrite(line, counted);
            counted += 1;
            new.push_str(&rewritten);
            map.push((old_at, old_prefix, new_at, new_prefix));
        }
        old_at += line.len() + 1;
    }
    // A selection that starts at the very start of a line keeps the whole
    // line, marker included; a caret there moves past the marker.
    let selecting = !selection.is_caret();
    let first = range.start;
    let shift = |offset: usize| -> usize {
        let index = map
            .partition_point(|(start, ..)| *start <= offset)
            .saturating_sub(1);
        let (old_start, old_prefix, new_start, new_prefix) = map[index];
        let within = offset - old_start;
        if selecting && offset == first && within == 0 {
            new_start
        } else if within <= old_prefix {
            new_start + new_prefix
        } else {
            new_start + new_prefix + (within - old_prefix)
        }
    };
    Edit {
        range: span,
        text: new,
        selection: Selection {
            anchor: shift(selection.anchor),
            focus: shift(selection.focus),
        },
    }
}

/// Make the selection's lines headings of `level` (1 to 6), or body text
/// with level 0. A line keeps its text; only the `#` prefix changes.
pub fn set_heading(text: &Rope, selection: Selection, level: u8) -> Edit {
    let level = level.min(6) as usize;
    rewrite_lines(text, selection, true, |line, _| {
        let old = heading_prefix(line);
        let body = &line[old..];
        let prefix = if level == 0 {
            String::new()
        } else {
            format!("{} ", "#".repeat(level))
        };
        (format!("{prefix}{body}"), old, prefix.len())
    })
}

/// Turn the selection's lines into list items or a quote, or back into plain
/// lines if they all already are. A list replaces another kind of list on
/// the same lines; numbered items are numbered from 1. Indentation is kept.
pub fn toggle_block(text: &Rope, selection: Selection, kind: Block) -> Edit {
    let span = line_span(text, &selection.range());
    let old = slice(text, span);
    let lines: Vec<&str> = old.split('\n').collect();
    let several = lines.len() > 1;
    let all = lines
        .iter()
        .filter(|line| !(several && line.trim().is_empty()))
        .all(|line| parse_block(line).1 == Some(kind));
    rewrite_lines(text, selection, true, |line, index| {
        let (parsed, found) = parse_block(line);
        let old_prefix = parsed.indent.len() + parsed.prefix.len();
        if all {
            let new = format!("{}{}", parsed.indent, parsed.rest);
            return (new, old_prefix, parsed.indent.len());
        }
        let marker = match kind {
            Block::Bullet => "- ".to_owned(),
            Block::Numbered => format!("{}. ", index + 1),
            Block::Task => "- [ ] ".to_owned(),
            Block::Quote => "> ".to_owned(),
        };
        // A quote goes around whatever the line is; a list replaces another
        // list's marker but not a quote's.
        let (keep, old_prefix) = match (kind, found) {
            (Block::Quote, _) | (_, None | Some(Block::Quote)) => (line, 0),
            _ => (parsed.rest, old_prefix),
        };
        let (indent, keep) = if kind == Block::Quote || keep == line {
            ("", keep)
        } else {
            (parsed.indent, keep)
        };
        let new = format!("{indent}{marker}{keep}");
        let new_prefix = indent.len() + marker.len();
        (new, old_prefix, new_prefix)
    })
}

/// Whether text looks like a link target rather than link text.
fn is_address(text: &str) -> bool {
    let text = text.trim();
    !text.contains(char::is_whitespace)
        && ["http://", "https://", "www.", "mailto:", "./", "../", "/"]
            .iter()
            .any(|prefix| text.starts_with(prefix))
}

/// Make the selection a link (or with `image`, an image). Selected text
/// becomes the link text and the caret waits in the empty address; a
/// selected address becomes the address and the caret waits in the text.
pub fn insert_link(text: &Rope, selection: Selection, image: bool) -> Edit {
    let range = selection.range();
    let selected = slice(text, range.clone());
    let bang = if image { "!" } else { "" };
    let (out, caret) = if is_address(&selected) {
        let out = format!("{bang}[]({})", selected.trim());
        let caret = bang.len() + 1;
        (out, caret)
    } else {
        let out = format!("{bang}[{selected}]()");
        let caret = out.len() - 1;
        (out, caret)
    };
    Edit {
        range: range.clone(),
        text: out,
        selection: Selection::caret(range.start + caret),
    }
}

/// Put an empty two-column table on lines of its own at the caret, with a
/// blank line before it, and the caret in its first heading cell. A
/// selection is replaced.
pub fn insert_table(text: &Rope, selection: Selection) -> Edit {
    let range = selection.range();
    let (before, after) = around(text, &range, 2);
    let mut out = String::new();
    // Start on a fresh line, after a blank one.
    if !before.is_empty() {
        if !before.ends_with('\n') {
            out.push_str("\n\n");
        } else if !before.ends_with("\n\n") && range.start > 1 {
            out.push('\n');
        }
    }
    let caret = out.len() + 2;
    out.push_str("|  |  |\n| --- | --- |\n|  |  |\n");
    if !after.is_empty() && !after.starts_with('\n') {
        out.push('\n');
    }
    Edit {
        range: range.clone(),
        text: out,
        selection: Selection::caret(range.start + caret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply an edit to plain text; returns the text and the selected part
    /// marked with `[` and `]` (a caret as `|`).
    fn run(text: &str, selection: Range<usize>, edit: impl Fn(&Rope, Selection) -> Edit) -> String {
        let rope = Rope::from_str(text);
        let edit = edit(
            &rope,
            Selection {
                anchor: selection.start,
                focus: selection.end,
            },
        );
        let mut out = String::from(text);
        out.replace_range(edit.range.clone(), &edit.text);
        let range = edit.selection.range();
        if range.is_empty() {
            out.insert(range.start, '|');
        } else {
            out.insert(range.end, ']');
            out.insert(range.start, '[');
        }
        out
    }

    fn at(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).expect("needle in text");
        start..start + needle.len()
    }

    fn inline(text: &str, needle: &str, kind: Inline) -> String {
        run(text, at(text, needle), |rope, sel| {
            toggle_inline(rope, sel, kind)
        })
    }

    #[test]
    fn emphasis_wraps_and_unwraps() {
        let text = "一段中文文字。";
        let bold = inline(text, "中文", Inline::Bold);
        assert_eq!(bold, "一段**[中文]**文字。");
        // Toggling again with the same selection takes it away.
        let unbolded = run(
            "一段**中文**文字。",
            at("一段**中文**文字。", "中文"),
            |r, s| toggle_inline(r, s, Inline::Bold),
        );
        assert_eq!(unbolded, "一段[中文]文字。");
        // So does selecting the marks with the text.
        assert_eq!(
            inline("一段**中文**文字。", "**中文**", Inline::Bold),
            "一段[中文]文字。"
        );
        assert_eq!(
            inline("日本語の文", "日本語", Inline::Italic),
            "*[日本語]*の文"
        );
        assert_eq!(inline("x ~~y~~ z", "y", Inline::Strike), "x [y] z");
        assert_eq!(
            inline("emoji 👍 here", "👍", Inline::Strike),
            "emoji ~~[👍]~~ here"
        );
    }

    #[test]
    fn bold_and_italic_tell_their_stars_apart() {
        // Italic inside bold-italic comes off, bold stays.
        assert_eq!(inline("***x***", "x", Inline::Italic), "**[x]**");
        assert_eq!(inline("***x***", "x", Inline::Bold), "*[x]*");
        // Bold text is not mistaken for italic: italic is added.
        assert_eq!(inline("**x**", "x", Inline::Italic), "***[x]***");
    }

    #[test]
    fn an_empty_selection_gets_both_marks_and_the_caret_between() {
        assert_eq!(
            run("ab", 1..1, |r, s| toggle_inline(r, s, Inline::Bold)),
            "a**|**b"
        );
        assert_eq!(
            run("", 0..0, |r, s| toggle_inline(r, s, Inline::Code)),
            "`|`"
        );
    }

    #[test]
    fn white_space_stays_outside_and_lines_are_wrapped_one_by_one() {
        assert_eq!(
            inline("a  word  b", "  word  ", Inline::Bold),
            "a  **[word]**  b"
        );
        let text = "第一行\n\n第二行";
        let out = run(text, 0..text.len(), |r, s| {
            toggle_inline(r, s, Inline::Italic)
        });
        assert_eq!(out, "[*第一行*\n\n*第二行*]");
    }

    #[test]
    fn marks_go_inside_heading_and_list_markers() {
        let text = "# 会议纪要\n\n1. 第二项\n> 引用\n- [ ] 任务";
        let out = run(text, 0..text.len(), |r, s| {
            toggle_inline(r, s, Inline::Bold)
        });
        assert_eq!(
            out,
            "[# **会议纪要**\n\n1. **第二项**\n> **引用**\n- [ ] **任务**]"
        );
        // One line selected from its start.
        assert_eq!(inline("## 标题", "## 标题", Inline::Italic), "## *[标题]*");
        // A selection inside the text is not affected.
        assert_eq!(
            inline("- 一项文字", "文字", Inline::Bold),
            "- 一项**[文字]**"
        );
    }

    #[test]
    fn code_with_backticks_gets_a_longer_fence() {
        assert_eq!(
            inline("use a`b here", "a`b", Inline::Code),
            "use ``[a`b]`` here"
        );
        assert_eq!(inline("x `y` z", "`y`", Inline::Code), "x [y] z");
        assert_eq!(inline("x ``a`b`` z", "a`b", Inline::Code), "x [a`b] z");
    }

    #[test]
    fn headings_replace_their_prefix_and_keep_the_caret_in_the_text() {
        let text = "标题文字\n下一行";
        assert_eq!(
            run(text, 3..3, |r, s| set_heading(r, s, 2)),
            "## 标|题文字\n下一行"
        );
        assert_eq!(
            run("## 标题文字", 6..6, |r, s| set_heading(r, s, 1)),
            "# 标|题文字"
        );
        assert_eq!(run("### 标题", 4..4, |r, s| set_heading(r, s, 0)), "|标题");
        // A caret inside the old prefix lands at the start of the text.
        assert_eq!(run("###### x", 2..2, |r, s| set_heading(r, s, 3)), "### |x");
        assert_eq!(heading_level(&Rope::from_str("前\n## 二级\n后"), 5), 2);
        assert_eq!(heading_level(&Rope::from_str("#不是标题"), 1), 0);
        assert_eq!(heading_level(&Rope::from_str("    # 代码"), 5), 0);
    }

    #[test]
    fn lists_toggle_over_several_lines_and_skip_blank_ones() {
        let text = "苹果\n香蕉\n\nりんご";
        let all = 0..text.len();
        assert_eq!(
            run(text, all.clone(), |r, s| toggle_block(r, s, Block::Bullet)),
            "[- 苹果\n- 香蕉\n\n- りんご]"
        );
        assert_eq!(
            run(text, all.clone(), |r, s| toggle_block(
                r,
                s,
                Block::Numbered
            )),
            "[1. 苹果\n2. 香蕉\n\n3. りんご]"
        );
        let listed = "- 苹果\n- 香蕉";
        // All already bullets: they become plain lines.
        assert_eq!(
            run(listed, 0..listed.len(), |r, s| toggle_block(
                r,
                s,
                Block::Bullet
            )),
            "[苹果\n香蕉]"
        );
        // Another kind of list is replaced, indentation kept.
        let nested = "  - 子项\n  * 另一项";
        assert_eq!(
            run(nested, 0..nested.len(), |r, s| toggle_block(
                r,
                s,
                Block::Task
            )),
            "[  - [ ] 子项\n  - [ ] 另一项]"
        );
        // A partly numbered selection is numbered throughout.
        let mixed = "1. 一\n二";
        assert_eq!(
            run(mixed, 0..mixed.len(), |r, s| toggle_block(
                r,
                s,
                Block::Numbered
            )),
            "[1. 一\n2. 二]"
        );
    }

    #[test]
    fn quotes_go_around_lists_and_come_off_again() {
        let text = "- 项目";
        let quoted = run(text, 2..2, |r, s| toggle_block(r, s, Block::Quote));
        assert_eq!(quoted, "> - |项目");
        let back = run("> - 项目", 4..4, |r, s| toggle_block(r, s, Block::Quote));
        assert_eq!(back, "- |项目");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_leaves_that_line_alone() {
        let text = "一\n二\n三";
        // From the start of 一 to the start of 三.
        assert_eq!(
            run(text, 0..4 + 4, |r, s| toggle_block(r, s, Block::Bullet)),
            "[- 一\n- 二\n]三"
        );
    }

    #[test]
    fn links_and_images() {
        assert_eq!(
            run("见文档。", at("见文档。", "文档"), |r, s| {
                insert_link(r, s, false)
            }),
            "见[文档](|)。"
        );
        let url = "https://example.com/页";
        assert_eq!(
            run(url, 0..url.len(), |r, s| insert_link(r, s, false)),
            "[|](https://example.com/页)"
        );
        assert_eq!(run("x", 1..1, |r, s| insert_link(r, s, true)), "x![](|)");
    }

    #[test]
    fn tables_go_on_lines_of_their_own() {
        // The caret shows as `|`, here in the first cell: `| | |  |`.
        assert_eq!(
            run("段落", 6..6, insert_table),
            "段落\n\n| | |  |\n| --- | --- |\n|  |  |\n"
        );
        assert_eq!(
            run("", 0..0, insert_table),
            "| | |  |\n| --- | --- |\n|  |  |\n"
        );
        // On an empty line between two paragraphs.
        assert_eq!(
            run("上\n\n下", 4..4, insert_table),
            "上\n\n| | |  |\n| --- | --- |\n|  |  |\n\n下"
        );
    }

    #[test]
    fn the_heading_level_of_a_huge_line_is_found_from_its_start() {
        let text = format!("## {}", "长".repeat(1_000_000));
        let rope = Rope::from_str(&text);
        assert_eq!(heading_level(&rope, text.len() / 2), 2);
        assert_eq!(heading_level(&rope, text.len() + 10), 2);
    }

    #[test]
    fn only_the_selected_lines_are_read() {
        // A long text: the edit covers one line, wherever it is.
        let mut text = "填充\n".repeat(50_000);
        let start = text.len();
        text.push_str("目标行\n");
        text.push_str(&"填充\n".repeat(50_000));
        let rope = Rope::from_str(&text);
        let edit = set_heading(&rope, Selection::caret(start + 3), 1);
        assert_eq!(edit.range, start..start + "目标行".len());
        assert_eq!(edit.text, "# 目标行");
    }
}
