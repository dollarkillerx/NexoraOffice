//! Syntax highlighting for the Markdown source, kept up to date edit by edit.
//!
//! The text is cut into blocks at blank lines that cannot be inside anything
//! else: not in a fenced code block or a multi-line HTML block, and followed by
//! a line that starts in the first column, so it cannot continue a list item
//! or an indented code block. Each block is parsed on its own with
//! `pulldown-cmark`. An edit re-cuts the text from the block it touches and
//! stops as soon as a cut falls where an old one did, with the same text after
//! it; only the blocks in between are parsed again. A key press in a large
//! document parses a paragraph, not the document.
//!
//! YAML front matter at the very start of the text ([`syntax::front_matter`])
//! is a block of its own, blank lines and all. An edit that makes, ends or
//! moves the end of the front matter cuts again from the start.
//!
//! What is lost by parsing blocks apart: link reference definitions in one
//! block are given to the others through the broken-link callback, so
//! `[text][label]` still highlights; footnotes are matched within a block only.
//! Highlighting is a hint for the eye, so that is good enough.
//!
//! Fenced code blocks whose info string names a language [`code`] knows
//! are coloured inside: keywords, strings, comments and the like get marks
//! of their own. A code block is always inside one block, so this changes
//! nothing about how edits are followed.
//!
//! Built for large files: the text is a rope and only the lines near an edit
//! are read; blocks are kept in groups, so an edit moves a few hundred of
//! them rather than every block after it; a block over [`MAX_PARSED`] bytes
//! (a single enormous line, a code fence that is never closed) is left
//! unhighlighted instead of being parsed again on every key press.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign, Range};

use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, LinkType, Parser, Tag, TagEnd};
use ropey::Rope;

use crate::code::{self, LineState, Token};
use crate::document::{floor_char_boundary, slice};

use crate::syntax;

/// What a stretch of source text is. Several can apply at once: a link inside
/// a heading is both.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Marks(u32);

impl Marks {
    pub const NONE: Marks = Marks(0);
    /// Text of a heading, its markers included.
    pub const HEADING: Marks = Marks(1 << 0);
    pub const STRONG: Marks = Marks(1 << 1);
    pub const EMPHASIS: Marks = Marks(1 << 2);
    pub const STRIKE: Marks = Marks(1 << 3);
    /// Inline code, its backticks included.
    pub const CODE: Marks = Marks(1 << 4);
    /// A fenced or indented code block, fences included.
    pub const CODE_BLOCK: Marks = Marks(1 << 5);
    /// The text of a link or image, or a bare web address.
    pub const LINK: Marks = Marks(1 << 6);
    /// Where a link goes: `(destination "title")` or `[label]`.
    pub const URL: Marks = Marks(1 << 7);
    /// Inside a block quote.
    pub const QUOTE: Marks = Marks(1 << 8);
    pub const HTML: Marks = Marks(1 << 9);
    /// Punctuation that is syntax rather than text: `#`, `*`, `_`, `~`,
    /// backticks, link brackets, code fences.
    pub const MARKER: Marks = Marks(1 << 10);
    /// `-`, `*`, `+`, `1.` or `1)` starting a list item, and `[ ]` / `[x]`.
    pub const LIST_MARKER: Marks = Marks(1 << 11);
    /// The `>` at the start of a quoted line.
    pub const QUOTE_MARKER: Marks = Marks(1 << 12);
    /// The pipes and the delimiter row of a table.
    pub const TABLE: Marks = Marks(1 << 13);
    /// A thematic break: `---`, `***` or `___`.
    pub const RULE: Marks = Marks(1 << 14);
    /// `$math$` and `$$math$$`.
    pub const MATH: Marks = Marks(1 << 15);
    /// YAML front matter at the start of the document, its `---` lines
    /// included; those are also [`Marks::MARKER`].
    pub const FRONT_MATTER: Marks = Marks(1 << 16);
    /// Inside a fenced code block in a language [`code`] knows: what the
    /// lexer found. At most one of these on any text.
    pub const CODE_KEYWORD: Marks = Marks(1 << 17);
    pub const CODE_TYPE: Marks = Marks(1 << 18);
    pub const CODE_FUNCTION: Marks = Marks(1 << 19);
    pub const CODE_STRING: Marks = Marks(1 << 20);
    pub const CODE_NUMBER: Marks = Marks(1 << 21);
    pub const CODE_COMMENT: Marks = Marks(1 << 22);

    const COUNT: usize = 23;
    /// The first of the code token marks, in the order of [`Token::ALL`].
    const FIRST_TOKEN: u32 = 17;
    const NAMES: [&'static str; Self::COUNT] = [
        "heading",
        "strong",
        "emphasis",
        "strike",
        "code",
        "code-block",
        "link",
        "url",
        "quote",
        "html",
        "marker",
        "list-marker",
        "quote-marker",
        "table",
        "rule",
        "math",
        "front-matter",
        "code-keyword",
        "code-type",
        "code-function",
        "code-string",
        "code-number",
        "code-comment",
    ];

    pub const fn contains(self, other: Marks) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Marks) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The mark of a code token.
    pub const fn token(token: Token) -> Marks {
        Marks(1 << (Self::FIRST_TOKEN + token.index() as u32))
    }

    /// The code token these marks hold, if any.
    pub fn code_token(self) -> Option<Token> {
        Token::ALL
            .into_iter()
            .find(|token| self.contains(Marks::token(*token)))
    }

    /// Any of the marks that stand for syntax or metadata rather than text.
    pub const fn is_syntax(self) -> bool {
        self.intersects(Marks(
            Self::MARKER.0
                | Self::LIST_MARKER.0
                | Self::QUOTE_MARKER.0
                | Self::TABLE.0
                | Self::RULE.0
                | Self::FRONT_MATTER.0,
        ))
    }
}

impl BitOr for Marks {
    type Output = Marks;
    fn bitor(self, other: Marks) -> Marks {
        Marks(self.0 | other.0)
    }
}

impl BitOrAssign for Marks {
    fn bitor_assign(&mut self, other: Marks) {
        self.0 |= other.0;
    }
}

impl std::fmt::Debug for Marks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = (0..Self::COUNT)
            .filter(|bit| self.0 & (1 << bit) != 0)
            .map(|bit| Self::NAMES[bit])
            .collect();
        write!(f, "Marks({})", names.join(" | "))
    }
}

/// A stretch of text and what it is. Spans from [`Highlighter::spans`] never
/// overlap and are in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte range in the document.
    pub range: Range<usize>,
    pub marks: Marks,
}

/// Blocks larger than this are not highlighted: parsing one on every key
/// press would cost more than a frame.
pub const MAX_PARSED: usize = 256 * 1024;
/// Blocks per group. An edit rewrites one group and moves the start of the
/// groups after it.
const GROUP: usize = if cfg!(test) { 4 } else { 512 };
/// How much of a line is read to decide whether it is blank, opens a fence,
/// defines a link label. Longer lines are judged on this much: the decisions
/// err towards fewer cuts, which costs time but never correctness.
const LINE_PREFIX: usize = 256;

/// A span inside a block, packed: 100 MB of Markdown has millions of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Packed {
    start: u32,
    end: u32,
    marks: Marks,
}

#[derive(Clone, Debug)]
struct Block {
    /// Relative to the start of the group while stored in one.
    start: usize,
    end: usize,
    /// Relative to `start`.
    spans: Box<[Packed]>,
    /// Link reference definitions in this block, normalised.
    labels: Box<[String]>,
    /// The parser asked about a label it did not find in the block: the
    /// highlighting depends on labels defined elsewhere.
    uses_labels: bool,
}

#[derive(Clone, Debug)]
struct Group {
    start: usize,
    blocks: Vec<Block>,
}

impl Group {
    fn end(&self) -> usize {
        self.start + self.blocks.last().map_or(0, |block| block.end)
    }
}

/// The highlighting of one document. Tell it about every edit with
/// [`Highlighter::edit`]; ask it with [`Highlighter::spans`].
#[derive(Clone, Debug)]
pub struct Highlighter {
    /// In order, without gaps, covering the whole text. Never empty, and no
    /// group is empty.
    groups: Vec<Group>,
    /// Every link reference label defined somewhere, with how many times.
    labels: HashMap<String, usize>,
    /// Bytes parsed by the last call to `new` or `edit`.
    parsed: usize,
    /// Where the front matter ends, or 0 if there is none.
    front_matter: usize,
}

fn new_block(range: Range<usize>, labels: Vec<String>) -> Block {
    Block {
        start: range.start,
        end: range.end,
        spans: Box::new([]),
        labels: labels.into_boxed_slice(),
        uses_labels: false,
    }
}

impl Highlighter {
    pub fn new(text: &Rope) -> Self {
        let front_matter = front_matter_end(text);
        let (cuts, _) = cut(text, 0, front_matter, |_| false);
        let mut highlighter = Self {
            groups: Vec::new(),
            labels: HashMap::new(),
            parsed: 0,
            front_matter,
        };
        let mut blocks: Vec<Block> = Vec::with_capacity(cuts.len());
        for (range, labels) in cuts {
            highlighter.count_labels(&labels, true);
            blocks.push(new_block(range, labels));
        }
        for block in &mut blocks {
            highlighter.parsed += parse(block, text, &highlighter.labels, front_matter);
        }
        highlighter.groups = regroup(blocks);
        highlighter
    }

    /// Bring the highlighting in line with an edit already made to the text:
    /// `old` is the byte range that was replaced, `inserted` how many bytes
    /// took its place, and `text` the text after the edit.
    pub fn edit(&mut self, text: &Rope, old: Range<usize>, inserted: usize) {
        let shift = |offset: usize| (offset + inserted) - old.len();
        self.parsed = 0;

        // The front matter can end somewhere else now: an edit far down can
        // close it, or turn its closing line into something else. Then the
        // cutting starts over from the top.
        let front_matter = front_matter_end(text);
        let expected = if self.front_matter <= old.start {
            Some(self.front_matter)
        } else if self.front_matter >= old.end {
            Some(shift(self.front_matter))
        } else {
            None
        };
        let moved = expected != Some(front_matter);
        self.front_matter = front_matter;

        // The block the edit starts in. An edit at the very start of a block
        // can join it to the one before (a space typed there indents it), so
        // start one block earlier.
        let (mut g, mut b) = if moved {
            (0, 0)
        } else {
            self.locate(old.start)
        };
        if (g, b) != (0, 0) && self.start_of(g, b) == old.start {
            (g, b) = self.previous(g, b);
        }
        let from = self.start_of(g, b);

        // Old blocks that start after the edit, and so are unchanged text.
        // Cutting stops at the first new cut that lands on one of them.
        let (mut rg, mut rb) = self.first_starting_after(old.end);
        let groups = &self.groups;
        let start = |rg: usize, rb: usize| groups[rg].start + groups[rg].blocks[rb].start;
        let (cuts, resynced) = cut(text, from, front_matter, |at| {
            while rg < groups.len() && shift(start(rg, rb)) < at {
                rb += 1;
                if rb == groups[rg].blocks.len() {
                    (rg, rb) = (rg + 1, 0);
                }
            }
            rg < groups.len() && shift(start(rg, rb)) == at
        });

        // The old blocks from (g, b) up to (rg, rb), or to the end, are
        // replaced. Take the groups they are in out, in absolute offsets.
        let (end_g, end_b) = if resynced {
            (rg, rb)
        } else {
            (self.groups.len(), 0)
        };
        let last_touched = if end_b == 0 { end_g - 1 } else { end_g };
        let before_end: usize = self.groups[g..end_g]
            .iter()
            .map(|group| group.blocks.len())
            .sum();
        let mut blocks: Vec<Block> = Vec::new();
        for group in self.groups.drain(g..=last_touched) {
            for mut block in group.blocks {
                block.start += group.start;
                block.end += group.start;
                blocks.push(block);
            }
        }
        let end_index = before_end + end_b;

        let added = cuts.len();
        let removed: Vec<Block> = blocks
            .splice(
                b..end_index,
                cuts.into_iter()
                    .map(|(range, labels)| new_block(range, labels)),
            )
            .collect();
        for block in &mut blocks[b + added..] {
            block.start = shift(block.start);
            block.end = shift(block.end);
        }

        // Link labels: if the set of defined labels changed, blocks that use
        // labels from elsewhere are parsed again.
        let mut set_changed = false;
        for block in &removed {
            set_changed |= self.count_labels(&block.labels, false);
        }
        for block in &blocks[b..b + added] {
            set_changed |= self.count_labels(&block.labels, true);
        }
        for block in &mut blocks[b..b + added] {
            self.parsed += parse(block, text, &self.labels, front_matter);
        }

        let regrouped = regroup(blocks);
        let count = regrouped.len();
        self.groups.splice(g..g, regrouped);
        for group in &mut self.groups[g + count..] {
            group.start = shift(group.start);
        }

        if set_changed {
            let labels = &self.labels;
            let mut parsed = 0;
            for group in &mut self.groups {
                for block in &mut group.blocks {
                    if block.uses_labels {
                        let mut absolute = block.clone();
                        absolute.start += group.start;
                        absolute.end += group.start;
                        parsed += parse(&mut absolute, text, labels, front_matter);
                        block.spans = absolute.spans;
                        block.uses_labels = absolute.uses_labels;
                    }
                }
            }
            self.parsed += parsed;
        }
    }

    /// The group and block holding byte `offset`; the last block for the end
    /// of the text.
    fn locate(&self, offset: usize) -> (usize, usize) {
        let g = self
            .groups
            .partition_point(|group| group.end() <= offset)
            .min(self.groups.len() - 1);
        let group = &self.groups[g];
        let b = group
            .blocks
            .partition_point(|block| group.start + block.end <= offset)
            .min(group.blocks.len() - 1);
        (g, b)
    }

    fn start_of(&self, g: usize, b: usize) -> usize {
        self.groups[g].start + self.groups[g].blocks[b].start
    }

    fn previous(&self, g: usize, b: usize) -> (usize, usize) {
        if b > 0 {
            (g, b - 1)
        } else {
            (g - 1, self.groups[g - 1].blocks.len() - 1)
        }
    }

    /// The first block that starts after `offset`, or one past the end.
    fn first_starting_after(&self, offset: usize) -> (usize, usize) {
        let g = self.groups.partition_point(|group| group.end() <= offset);
        if g == self.groups.len() {
            return (g, 0);
        }
        let group = &self.groups[g];
        let b = group
            .blocks
            .partition_point(|block| group.start + block.start <= offset);
        if b == group.blocks.len() {
            (g + 1, 0)
        } else {
            (g, b)
        }
    }

    /// Add or remove one block's labels. Returns whether a label started or
    /// stopped being defined.
    fn count_labels(&mut self, labels: &[String], add: bool) -> bool {
        let mut changed = false;
        for label in labels {
            if add {
                let count = self.labels.entry(label.clone()).or_insert(0);
                changed |= *count == 0;
                *count += 1;
            } else if let Some(count) = self.labels.get_mut(label) {
                *count -= 1;
                if *count == 0 {
                    self.labels.remove(label);
                    changed = true;
                }
            }
        }
        changed
    }

    /// The highlighted spans that overlap `range`, in document offsets.
    pub fn spans(&self, range: Range<usize>) -> Vec<Span> {
        let (g, b) = self.locate(range.start);
        let mut out = Vec::new();
        let mut first = b;
        for group in &self.groups[g..] {
            for block in &group.blocks[first..] {
                let block_start = group.start + block.start;
                if block_start >= range.end && !(range.is_empty() && block_start == range.start) {
                    return out;
                }
                for span in block.spans.iter() {
                    let absolute =
                        block_start + span.start as usize..block_start + span.end as usize;
                    if absolute.end > range.start && absolute.start < range.end {
                        out.push(Span {
                            range: absolute,
                            marks: span.marks,
                        });
                    }
                }
            }
            first = 0;
        }
        out
    }

    /// How many bytes the last `new` or `edit` parsed. For checking that an
    /// edit stays local.
    pub fn last_parsed(&self) -> usize {
        self.parsed
    }

    /// Block ranges, for tests.
    #[cfg(test)]
    fn block_ranges(&self) -> Vec<Range<usize>> {
        self.groups
            .iter()
            .flat_map(|group| {
                group
                    .blocks
                    .iter()
                    .map(move |block| group.start + block.start..group.start + block.end)
            })
            .collect()
    }
}

/// Split blocks with absolute offsets into groups of even size.
fn regroup(blocks: Vec<Block>) -> Vec<Group> {
    let count = blocks.len().div_ceil(GROUP).max(1);
    let size = blocks.len().div_ceil(count).max(1);
    let mut groups = Vec::with_capacity(count);
    let mut blocks = blocks.into_iter().peekable();
    while blocks.peek().is_some() {
        let chunk: Vec<Block> = blocks.by_ref().take(size).collect();
        let start = chunk[0].start;
        let chunk = chunk
            .into_iter()
            .map(|mut block| {
                block.start -= start;
                block.end -= start;
                block
            })
            .collect();
        groups.push(Group {
            start,
            blocks: chunk,
        });
    }
    groups
}

/// Where the front matter of `text` ends, or 0 if it has none: what
/// [`syntax::front_matter`] finds in the part of the text it looks at.
fn front_matter_end(text: &Rope) -> usize {
    let len = text.len_bytes();
    if !text.bytes().take(3).eq(*b"---") {
        return 0;
    }
    let head = slice(
        text,
        0..floor_char_boundary(text, len.min(syntax::FRONT_MATTER_LIMIT)),
    );
    match syntax::front_matter(&head) {
        // A closing line cut off where the head ends may go on in the text.
        Some(range) if range.end == head.len() && head.len() < len && !head.ends_with('\n') => 0,
        Some(range) => range.end,
        None => 0,
    }
}

/// Highlight one block with absolute offsets. Returns how many bytes were
/// parsed: none for a block over [`MAX_PARSED`]. The block that ends where
/// the front matter does, at the start of the text, is the front matter.
fn parse(
    block: &mut Block,
    text: &Rope,
    labels: &HashMap<String, usize>,
    front_matter: usize,
) -> usize {
    let len = block.end - block.start;
    if len > MAX_PARSED {
        block.spans = Box::new([]);
        block.uses_labels = false;
        return 0;
    }
    let source = slice(text, block.start..block.end);
    let (spans, uses_labels) = if block.start == 0 && front_matter > 0 && block.end == front_matter
    {
        (highlight_front_matter(&source), false)
    } else {
        highlight_block(&source, labels)
    };
    block.spans = spans
        .into_iter()
        .map(|span| Packed {
            start: span.range.start as u32,
            end: span.range.end as u32,
            marks: span.marks,
        })
        .collect();
    block.uses_labels = uses_labels;
    len
}

/// What kind of multi-line construct a line leaves open, inside which a blank
/// line is not a block boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    Nothing,
    Fence {
        fence: u8,
        len: usize,
        indent: usize,
    },
    /// An HTML block that only ends at this text (lowercase).
    Html(&'static str),
}

/// Leading spaces, tabs and quote markers of a line: `(column of the first
/// other character as a byte count of white space, the rest)`.
fn strip_container(line: &str) -> (usize, &str) {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let mut rest = &line[indent..];
    while let Some(after) = rest.strip_prefix('>') {
        rest = after.trim_start_matches([' ', '\t']);
    }
    (indent, rest)
}

/// The fence a line opens, if it opens one.
fn fence_open(line: &str) -> Option<Open> {
    let (indent, rest) = strip_container(line);
    let fence = *rest.as_bytes().first()?;
    if fence != b'`' && fence != b'~' {
        return None;
    }
    let len = rest.bytes().take_while(|b| *b == fence).count();
    if len < 3 || (fence == b'`' && rest[len..].contains('`')) {
        return None;
    }
    Some(Open::Fence { fence, len, indent })
}

fn fence_closes(line: &str, fence: u8, len: usize, indent: usize) -> bool {
    let (own_indent, rest) = strip_container(line);
    let run = rest.bytes().take_while(|b| *b == fence).count();
    own_indent <= indent + 3 && run >= len && rest[run..].trim().is_empty()
}

/// The HTML block a line opens that may contain blank lines, if any: comments
/// and the raw-text elements. Other HTML blocks end at a blank line anyway.
fn html_open(line: &str) -> Option<Open> {
    let (_, rest) = strip_container(line);
    if !rest.starts_with('<') {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    let (closing, opened) = if let Some(comment) = lower.strip_prefix("<!--") {
        ("-->", comment.contains("-->"))
    } else {
        let tag = ["pre", "script", "style", "textarea"]
            .into_iter()
            .find(|tag| {
                lower[1..].starts_with(tag)
                    && lower[1 + tag.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| c == '>' || c.is_ascii_whitespace())
            })?;
        let closing = match tag {
            "pre" => "</pre>",
            "script" => "</script>",
            "style" => "</style>",
            _ => "</textarea>",
        };
        (closing, lower.contains(closing))
    };
    (!opened).then_some(Open::Html(closing))
}

/// A block found by [`cut`]: its range and its link reference labels.
type Cut = (Range<usize>, Vec<String>);

/// Cut `text` into blocks starting at `from`, which must be a block start.
/// `stop` is asked about every cut after the first block; when it says yes,
/// cutting ends there and the second value is `true`. Each block comes with
/// its link reference labels. The text up to `front_matter` is one block.
fn cut(
    text: &Rope,
    from: usize,
    front_matter: usize,
    mut stop: impl FnMut(usize) -> bool,
) -> (Vec<Cut>, bool) {
    let len = text.len_bytes();
    let mut blocks = Vec::new();
    let mut start = from;
    let mut labels = Vec::new();
    let mut open = Open::Nothing;
    let mut previous_blank = false;
    let mut at = from;
    for line in text.lines_at(text.byte_to_line(from)) {
        if at >= len {
            break;
        }
        let line_len = line.len_bytes();
        if at < front_matter {
            // Inside the front matter: no cuts, fences or labels.
            at += line_len;
            continue;
        }
        if at == front_matter && at != start {
            blocks.push((start..at, std::mem::take(&mut labels)));
            if stop(at) {
                return (blocks, true);
            }
            start = at;
            open = Open::Nothing;
            previous_blank = false;
        }
        // Short lines are read whole; of long ones only the start.
        let whole = line_len <= LINE_PREFIX;
        let prefix = if whole {
            line.to_string()
        } else {
            let end = line.char_to_byte(line.byte_to_char(LINE_PREFIX));
            line.byte_slice(..end).to_string()
        };
        let blank = whole && prefix.trim().is_empty();
        if at != start
            && previous_blank
            && open == Open::Nothing
            && !blank
            && !prefix.starts_with([' ', '\t'])
        {
            blocks.push((start..at, std::mem::take(&mut labels)));
            if stop(at) {
                return (blocks, true);
            }
            start = at;
        }
        open = match open {
            Open::Nothing => {
                if let Some(label) = syntax::definition_label(&prefix) {
                    labels.push(label);
                }
                fence_open(&prefix)
                    .or_else(|| html_open(&prefix))
                    .unwrap_or(Open::Nothing)
            }
            Open::Fence { fence, len, indent } => {
                if whole && fence_closes(&prefix, fence, len, indent) {
                    Open::Nothing
                } else {
                    open
                }
            }
            Open::Html(closing) => {
                if prefix.to_ascii_lowercase().contains(closing) {
                    Open::Nothing
                } else {
                    open
                }
            }
        };
        previous_blank = blank;
        at += line_len;
    }
    blocks.push((start..len, labels));
    (blocks, false)
}

/// Highlight one block, parsed on its own. Offsets are relative to `source`.
/// The second value says whether the parser looked for a label it did not
/// find in the block.
fn highlight_block(source: &str, labels: &HashMap<String, usize>) -> (Vec<Span>, bool) {
    if source.trim().is_empty() {
        return (Vec::new(), false);
    }
    let asked = std::cell::Cell::new(false);
    let callback = |link: BrokenLink<'_>| {
        asked.set(true);
        labels
            .contains_key(&syntax::normalize_label(&link.reference))
            .then_some((CowStr::Borrowed(""), CowStr::Borrowed("")))
    };
    let parser = Parser::new_with_broken_link_callback(source, syntax::options(), Some(callback));
    let mut marks = Collector::new(source);
    for (event, range) in parser.into_offset_iter() {
        marks.event(event, range);
    }
    marks.definitions();
    (flatten(marks.raw), asked.get())
}

/// The front matter, its own text: all of it grey, the `---` lines as markers.
fn highlight_front_matter(source: &str) -> Vec<Span> {
    let end = source.trim_end().len();
    let opening = source.find('\n').unwrap_or(end).min(end);
    let opening = source[..opening].trim_end().len();
    let closing = source[..end].rfind('\n').map_or(end, |newline| newline + 1);
    flatten(vec![
        (0..end, Marks::FRONT_MATTER),
        (0..opening, Marks::MARKER),
        (closing..end, Marks::MARKER),
    ])
}

/// Turns parser events into raw, possibly overlapping marks.
struct Collector<'a> {
    source: &'a str,
    raw: Vec<(Range<usize>, Marks)>,
    quote_depth: usize,
    /// Inside code, a link or HTML: text there is not searched for addresses.
    verbatim: usize,
    /// Open links and images: their range, kind, and where their text starts.
    links: Vec<(Range<usize>, LinkType, usize)>,
    /// End of the latest event, for finding where a link's text ends.
    last_end: usize,
    /// The open table and the ranges of its cells.
    table: Option<(Range<usize>, Vec<Range<usize>>)>,
    /// Inside a fenced code block in a known language: the language and the
    /// state its last line left.
    code: Option<(code::Language, LineState)>,
}

impl<'a> Collector<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            raw: Vec::new(),
            quote_depth: 0,
            verbatim: 0,
            links: Vec::new(),
            last_end: 0,
            table: None,
            code: None,
        }
    }

    fn mark(&mut self, range: Range<usize>, marks: Marks) {
        if range.start < range.end {
            self.raw.push((range, marks));
        }
    }

    /// `range` without trailing white space and line breaks.
    fn trim_end(&self, range: Range<usize>) -> Range<usize> {
        let kept = self.source[range.clone()].trim_end().len();
        range.start..range.start + kept
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        // The start of a link sets `last_end` itself; its range runs to the
        // end of the whole link.
        let moves_end = !matches!(event, Event::Start(Tag::Link { .. } | Tag::Image { .. }));
        match event {
            Event::Start(tag) => self.start(tag, range.clone()),
            Event::End(tag) => self.end(tag),
            Event::Text(_) => {
                if let Some((language, state)) = self.code {
                    // Code lines come one or several to an event, without
                    // the indentation and quote markers of their container.
                    let (tokens, after) =
                        code::highlight(language, &self.source[range.clone()], state);
                    self.code = Some((language, after));
                    for (found, token) in tokens {
                        self.mark(
                            range.start + found.start..range.start + found.end,
                            Marks::token(token),
                        );
                    }
                } else if self.verbatim == 0 {
                    for url in syntax::find_bare_urls(&self.source[range.clone()]) {
                        self.mark(
                            range.start + url.range.start..range.start + url.range.end,
                            Marks::LINK,
                        );
                    }
                }
            }
            Event::Code(_) => {
                let ticks = self.run_at(range.start, b'`');
                self.mark(range.clone(), Marks::CODE);
                self.mark(range.start..range.start + ticks, Marks::MARKER);
                self.mark(
                    range.end.saturating_sub(ticks).max(range.start)..range.end,
                    Marks::MARKER,
                );
            }
            Event::InlineMath(_) | Event::DisplayMath(_) => {
                let dollars = self.run_at(range.start, b'$');
                self.mark(range.clone(), Marks::MATH);
                self.mark(range.start..range.start + dollars, Marks::MARKER);
                self.mark(
                    range.end.saturating_sub(dollars).max(range.start)..range.end,
                    Marks::MARKER,
                );
            }
            Event::Html(_) | Event::InlineHtml(_) => self.mark(range.clone(), Marks::HTML),
            Event::FootnoteReference(_) => self.mark(range.clone(), Marks::LINK),
            Event::Rule => {
                let range = self.trim_end(range.clone());
                self.mark(range, Marks::RULE);
            }
            Event::TaskListMarker(_) => self.mark(range.clone(), Marks::LIST_MARKER),
            Event::SoftBreak | Event::HardBreak => {}
        }
        if moves_end {
            self.last_end = self.last_end.max(range.end);
        }
    }

    /// Length of the run of `byte` starting at `at`.
    fn run_at(&self, at: usize, byte: u8) -> usize {
        self.source.as_bytes()[at..]
            .iter()
            .take_while(|b| **b == byte)
            .count()
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        match tag {
            Tag::Heading { .. } => {
                let range = self.trim_end(range);
                self.mark(range.clone(), Marks::HEADING);
                self.heading_markers(range);
            }
            Tag::BlockQuote(_) => {
                if self.quote_depth == 0 {
                    self.mark(range.clone(), Marks::QUOTE);
                    self.quote_markers(range);
                }
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.verbatim += 1;
                let range = self.trim_end(range);
                self.mark(range.clone(), Marks::CODE_BLOCK);
                if let CodeBlockKind::Fenced(info) = kind {
                    self.fence_markers(range);
                    self.code =
                        code::language(&info).map(|language| (language, LineState::default()));
                }
            }
            Tag::HtmlBlock => {
                self.verbatim += 1;
                self.mark(range, Marks::HTML);
            }
            Tag::Item => {
                let text = &self.source[range.clone()];
                let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
                let rest = &text.as_bytes()[indent..];
                let len = match rest.first() {
                    Some(b'-' | b'*' | b'+') => 1,
                    Some(b'0'..=b'9') => {
                        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                        if matches!(rest.get(digits), Some(b'.' | b')')) {
                            digits + 1
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };
                let at = range.start + indent;
                self.mark(at..at + len, Marks::LIST_MARKER);
            }
            Tag::Emphasis => self.delimited(range, Marks::EMPHASIS, 1),
            Tag::Strong => self.delimited(range, Marks::STRONG, 2),
            Tag::Strikethrough => {
                let tildes = self.run_at(range.start, b'~').clamp(1, 2);
                self.delimited(range, Marks::STRIKE, tildes);
            }
            Tag::Link { link_type, .. } | Tag::Image { link_type, .. } => {
                let image = self.source.as_bytes().get(range.start) == Some(&b'!');
                let text_start = range.start + if image { 2 } else { 1 };
                self.verbatim += 1;
                self.links.push((range, link_type, text_start));
                self.last_end = text_start;
            }
            Tag::Table(_) => self.table = Some((range, Vec::new())),
            Tag::TableCell => {
                if let Some((_, cells)) = &mut self.table {
                    cells.push(range);
                }
            }
            Tag::FootnoteDefinition(_) => {
                let text = &self.source[range.clone()];
                if let Some(at) = text.find("]:") {
                    self.mark(range.start..range.start + at + 2, Marks::MARKER);
                }
            }
            Tag::Paragraph
            | Tag::List(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::BlockQuote(_) => self.quote_depth = self.quote_depth.saturating_sub(1),
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                self.verbatim = self.verbatim.saturating_sub(1);
                self.code = None;
            }
            TagEnd::Link | TagEnd::Image => {
                self.verbatim = self.verbatim.saturating_sub(1);
                if let Some((range, link_type, text_start)) = self.links.pop() {
                    self.link_marks(range, link_type, text_start);
                }
            }
            TagEnd::Table => {
                if let Some((table, cells)) = self.table.take() {
                    let mut at = table.start;
                    for cell in cells.iter().chain(std::iter::once(&(table.end..table.end))) {
                        if cell.start > at {
                            let gap = at..cell.start;
                            self.mark(gap, Marks::TABLE);
                        }
                        at = at.max(cell.end);
                    }
                }
            }
            _ => {}
        }
    }

    fn delimited(&mut self, range: Range<usize>, marks: Marks, len: usize) {
        let len = len.min(range.len() / 2);
        self.mark(range.clone(), marks);
        self.mark(range.start..range.start + len, Marks::MARKER);
        self.mark(range.end - len..range.end, Marks::MARKER);
    }

    fn heading_markers(&mut self, range: Range<usize>) {
        let text = &self.source[range.clone()];
        let indent = text.len() - text.trim_start_matches(' ').len();
        let hashes = text[indent..].bytes().take_while(|b| *b == b'#').count();
        if hashes > 0 {
            // ATX: `## Title ##`.
            let at = range.start + indent;
            self.mark(at..at + hashes, Marks::MARKER);
            let body = &text[indent + hashes..];
            let closing = body.len() - body.trim_end_matches('#').len();
            let before = body.len() - closing;
            if closing > 0 && (before == 0 || body[..before].ends_with([' ', '\t'])) {
                self.mark(range.end - closing..range.end, Marks::MARKER);
            }
        } else if let Some(newline) = text.rfind('\n') {
            // Setext: the underline of `===` or `---`.
            let line = &text[newline + 1..];
            let skip = line.len() - line.trim_start().len();
            self.mark(range.start + newline + 1 + skip..range.end, Marks::MARKER);
        }
    }

    fn quote_markers(&mut self, range: Range<usize>) {
        let mut at = range.start;
        for line in self.source[range.clone()].split_inclusive('\n') {
            let bytes = line.as_bytes();
            let mut i = 0;
            loop {
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'>' {
                    self.raw.push((at + i..at + i + 1, Marks::QUOTE_MARKER));
                    i += 1;
                } else {
                    break;
                }
            }
            at += line.len();
        }
    }

    fn fence_markers(&mut self, range: Range<usize>) {
        let text = &self.source[range.clone()];
        let first = text.find('\n').unwrap_or(text.len());
        self.mark(range.start..range.start + first, Marks::MARKER);
        if let Some(newline) = text.rfind('\n')
            && newline >= first
        {
            let line = &text[newline + 1..];
            let (_, rest) = strip_container(line);
            let fence = rest.bytes().next();
            if matches!(fence, Some(b'`' | b'~'))
                && rest.bytes().all(|b| Some(b) == fence)
                && rest.len() >= 3
            {
                let at = range.start + newline + 1 + (line.len() - rest.len());
                self.mark(at..range.end, Marks::MARKER);
            }
        }
    }

    /// Link reference definitions produce no parser events: find them by
    /// line, outside anything verbatim, and mark `[label]:` as syntax and
    /// the rest as a destination.
    fn definitions(&mut self) {
        let verbatim: Vec<Range<usize>> = self
            .raw
            .iter()
            .filter(|(_, marks)| marks.intersects(Marks::CODE_BLOCK | Marks::HTML))
            .map(|(range, _)| range.clone())
            .collect();
        let mut at = 0;
        for line in self.source.split_inclusive('\n') {
            let start = at;
            at += line.len();
            if syntax::definition_label(line).is_none()
                || verbatim.iter().any(|range| range.contains(&start))
            {
                continue;
            }
            let Some(colon) = line.find("]:") else {
                continue;
            };
            let label_start = start + (line.len() - line.trim_start_matches(' ').len());
            let end = start + line.trim_end().len();
            self.mark(label_start..start + colon + 2, Marks::MARKER);
            self.mark(start + colon + 2..end, Marks::URL);
        }
    }

    fn link_marks(&mut self, range: Range<usize>, link_type: LinkType, text_start: usize) {
        match link_type {
            LinkType::Autolink | LinkType::Email => {
                self.mark(range.start..range.start + 1, Marks::MARKER);
                self.mark(range.start + 1..range.end - 1, Marks::LINK);
                self.mark(range.end - 1..range.end, Marks::MARKER);
            }
            _ => {
                let from = self.last_end.clamp(text_start, range.end);
                let close = self.source[from..range.end]
                    .find(']')
                    .map_or(range.end, |i| from + i);
                self.mark(range.start..text_start, Marks::MARKER);
                self.mark(text_start..close, Marks::LINK);
                self.mark(close..(close + 1).min(range.end), Marks::MARKER);
                self.mark((close + 1).min(range.end)..range.end, Marks::URL);
            }
        }
    }
}

/// Turn overlapping marks into ordered spans that do not overlap, each with
/// every mark that covers it.
fn flatten(raw: Vec<(Range<usize>, Marks)>) -> Vec<Span> {
    let mut edges: Vec<(usize, bool, u16)> = Vec::with_capacity(raw.len() * 2);
    for (range, marks) in raw {
        if range.start >= range.end {
            continue;
        }
        for bit in 0..Marks::COUNT as u16 {
            if marks.0 & (1 << bit) != 0 {
                edges.push((range.start, true, bit));
                edges.push((range.end, false, bit));
            }
        }
    }
    edges.sort_unstable_by_key(|edge| edge.0);
    let mut counts = [0u32; Marks::COUNT];
    let mut spans: Vec<Span> = Vec::new();
    let mut previous = 0;
    let mut index = 0;
    while index < edges.len() {
        let at = edges[index].0;
        let current = Marks(
            (0..Marks::COUNT as u16)
                .filter(|bit| counts[usize::from(*bit)] > 0)
                .fold(0, |acc, bit| acc | (1 << bit)),
        );
        if at > previous && !current.is_empty() {
            match spans.last_mut() {
                Some(last) if last.range.end == previous && last.marks == current => {
                    last.range.end = at;
                }
                _ => spans.push(Span {
                    range: previous..at,
                    marks: current,
                }),
            }
        }
        while index < edges.len() && edges[index].0 == at {
            let (_, opens, bit) = edges[index];
            let count = &mut counts[usize::from(bit)];
            if opens {
                *count += 1;
            } else {
                *count = count.saturating_sub(1);
            }
            index += 1;
        }
        previous = at;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    fn all(text: &str) -> Vec<Span> {
        Highlighter::new(&rope(text)).spans(0..text.len())
    }

    /// The marks on the first byte of the `nth` occurrence of `needle`.
    fn marks_at(text: &str, needle: &str, nth: usize) -> Marks {
        let at = text
            .match_indices(needle)
            .nth(nth)
            .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"))
            .0;
        all(text)
            .into_iter()
            .find(|span| span.range.contains(&at))
            .map_or(Marks::NONE, |span| span.marks)
    }

    fn check_spans(text: &str, spans: &[Span]) {
        let mut end = 0;
        for span in spans {
            assert!(span.range.start >= end, "overlap or disorder: {spans:?}");
            assert!(span.range.start < span.range.end, "empty span {span:?}");
            assert!(span.range.end <= text.len());
            assert!(text.is_char_boundary(span.range.start), "{span:?}");
            assert!(text.is_char_boundary(span.range.end), "{span:?}");
            assert!(!span.marks.is_empty());
            end = span.range.end;
        }
    }

    #[test]
    fn headings_and_their_markers() {
        let text = "# 标题 #\n\nSetext 标题\n===\n";
        assert!(marks_at(text, "# ", 0).contains(Marks::HEADING | Marks::MARKER));
        assert_eq!(marks_at(text, "标题", 0), Marks::HEADING);
        assert!(marks_at(text, "#\n", 0).contains(Marks::MARKER));
        assert_eq!(marks_at(text, "Setext", 0), Marks::HEADING);
        assert!(marks_at(text, "===", 0).contains(Marks::MARKER));
        // A `#` that is part of the title is not a closing sequence.
        assert_eq!(marks_at("# C#\n", "#", 1), Marks::HEADING);
    }

    #[test]
    fn emphasis_strong_strike_and_code() {
        let text = "*斜体* **粗体** ~~删除~~ `代码` ***两者***";
        assert_eq!(marks_at(text, "斜体", 0), Marks::EMPHASIS);
        assert_eq!(marks_at(text, "*", 0), Marks::EMPHASIS | Marks::MARKER);
        assert_eq!(marks_at(text, "粗体", 0), Marks::STRONG);
        assert!(marks_at(text, "**", 0).contains(Marks::MARKER));
        assert_eq!(marks_at(text, "删除", 0), Marks::STRIKE);
        assert!(marks_at(text, "~~", 0).contains(Marks::MARKER));
        assert_eq!(marks_at(text, "代码", 0), Marks::CODE);
        assert_eq!(marks_at(text, "`", 0), Marks::CODE | Marks::MARKER);
        assert_eq!(marks_at(text, "两者", 0), Marks::STRONG | Marks::EMPHASIS);
    }

    #[test]
    fn code_blocks_and_their_fences() {
        let text = "```rust\nlet x = *y*;\n```\n\n    indented\n";
        assert!(marks_at(text, "```rust", 0).contains(Marks::CODE_BLOCK | Marks::MARKER));
        // Markup inside code is code.
        assert_eq!(marks_at(text, "*y*", 0), Marks::CODE_BLOCK);
        assert!(marks_at(text, "```\n", 0).contains(Marks::MARKER));
        assert_eq!(marks_at(text, "indented", 0), Marks::CODE_BLOCK);
    }

    #[test]
    fn fenced_code_is_coloured_by_its_language() {
        let text = "```rust\nfn main() { let s = \"字\"; } // 注\n```\n\n```\nfn plain\n```\n\n> ```py\n> x = 1  # c\n> ```\n\n- item\n\n  ```js\n  /* a\n  b */ let\n  ```\n";
        let code = Marks::CODE_BLOCK;
        assert_eq!(marks_at(text, "fn", 0), code | Marks::CODE_KEYWORD);
        assert_eq!(marks_at(text, "main", 0), code | Marks::CODE_FUNCTION);
        assert_eq!(marks_at(text, "\"字\"", 0), code | Marks::CODE_STRING);
        assert_eq!(marks_at(text, "// 注", 0), code | Marks::CODE_COMMENT);
        assert_eq!(marks_at(text, " {", 0), code);
        // The fences are markers, not code.
        assert_eq!(marks_at(text, "rust", 0), code | Marks::MARKER);
        // A fence without a language, or with one the lexer does not know.
        assert_eq!(marks_at(text, "fn plain", 0), code);
        assert_eq!(marks_at("```text\nfn x\n```", "fn", 0), code);
        // In a quote: the quote markers are not code.
        assert_eq!(
            marks_at(text, "1  #", 0),
            Marks::QUOTE | code | Marks::CODE_NUMBER
        );
        assert_eq!(
            marks_at(text, "# c", 0),
            Marks::QUOTE | code | Marks::CODE_COMMENT
        );
        // In a list item, a comment over two lines.
        assert_eq!(marks_at(text, "b */", 0), code | Marks::CODE_COMMENT);
        assert_eq!(marks_at(text, "let\n", 0), code | Marks::CODE_KEYWORD);
        assert_eq!(marks_at(text, "  b */", 0), code);
        // Each token kind has its mark, and the marks give the kind back.
        for token in Token::ALL {
            assert_eq!(Marks::token(token).code_token(), Some(token));
            assert!(!Marks::token(token).is_syntax());
        }
        assert_eq!(Marks::CODE_BLOCK.code_token(), None);
        assert_eq!(
            format!("{:?}", code | Marks::CODE_COMMENT),
            "Marks(code-block | code-comment)"
        );
    }

    #[test]
    fn edits_recolour_code_blocks() {
        let mut text = String::from("段落\n\n```text\nlet x = 1; /* a\nb */\n```\n\n后面\n");
        let mut highlighter = Highlighter::new(&rope(&text));
        let mut edit = |text: &mut String, range: Range<usize>, insert: &str| {
            text.replace_range(range.clone(), insert);
            highlighter.edit(&rope(text), range, insert.len());
            let spans = highlighter.spans(0..text.len());
            assert_eq!(spans, all(text), "{text:?}");
        };
        assert_eq!(marks_at(&text, "let", 0), Marks::CODE_BLOCK);
        // Naming the language colours the block.
        let at = text.find("text").unwrap();
        edit(&mut text, at..at + 4, "rust");
        assert_eq!(
            marks_at(&text, "let", 0),
            Marks::CODE_BLOCK | Marks::CODE_KEYWORD
        );
        assert_eq!(
            marks_at(&text, "b */", 0),
            Marks::CODE_BLOCK | Marks::CODE_COMMENT
        );
        // Closing the comment early makes what follows code again.
        let at = text.find(" a\n").unwrap();
        edit(&mut text, at..at, " */");
        assert_eq!(marks_at(&text, "b */", 0), Marks::CODE_BLOCK);
        // Opening a string runs it to the end of the block, not further.
        let at = text.find("let").unwrap();
        edit(&mut text, at..at, "\"");
        assert!(marks_at(&text, "b */", 0).contains(Marks::CODE_STRING));
        assert_eq!(marks_at(&text, "后面", 0), Marks::NONE);
        // Another language.
        let at = text.find("rust").unwrap();
        edit(&mut text, at..at + 4, "python");
        assert_eq!(
            marks_at(&text, "1;", 0),
            Marks::CODE_BLOCK | Marks::CODE_STRING
        );
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let text = "前文\n\n```\n代码\n\n# 不是标题\n\n**也不是**";
        assert_eq!(marks_at(text, "前文", 0), Marks::NONE);
        assert_eq!(marks_at(text, "代码", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "# 不是", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "也不是", 0), Marks::CODE_BLOCK);
        let highlighter = Highlighter::new(&rope(text));
        assert_eq!(highlighter.block_ranges().len(), 2);
    }

    #[test]
    fn links_images_and_bare_addresses() {
        let text = "[链接](https://a.example \"t\") ![图](p.png) <https://b.example> 见 https://c.example。[ref][r]\n\n[r]: /u";
        assert_eq!(marks_at(text, "链接", 0), Marks::LINK);
        assert_eq!(marks_at(text, "[", 0), Marks::MARKER);
        assert_eq!(marks_at(text, "](", 0), Marks::MARKER);
        assert_eq!(marks_at(text, "(https", 0), Marks::URL);
        assert_eq!(marks_at(text, "\"t\"", 0), Marks::URL);
        assert_eq!(marks_at(text, "![", 0), Marks::MARKER);
        assert_eq!(marks_at(text, "图", 0), Marks::LINK);
        assert_eq!(marks_at(text, "(p.png)", 0), Marks::URL);
        assert_eq!(marks_at(text, "<https", 0), Marks::MARKER);
        assert_eq!(marks_at(text, "https://b", 0), Marks::LINK);
        assert_eq!(marks_at(text, "https://c", 0), Marks::LINK);
        assert_eq!(marks_at(text, "。", 0), Marks::NONE);
        // The definition is in another block.
        assert_eq!(marks_at(text, "ref", 0), Marks::LINK);
        assert_eq!(marks_at(text, "[r]", 0), Marks::URL);
        // An undefined reference is plain text.
        assert_eq!(marks_at("[ref][nowhere]", "ref", 0), Marks::NONE);
        // The definition itself.
        assert_eq!(marks_at(text, "[r]:", 0), Marks::MARKER);
        assert_eq!(marks_at(text, "/u", 0), Marks::URL);
        // Not inside code.
        assert_eq!(marks_at("```\n[a]: /b\n```", "[a]", 0), Marks::CODE_BLOCK);
    }

    #[test]
    fn list_markers_and_task_boxes() {
        let text = "- 一\n* 二\n\n1. 三\n2) 四\n\n- [ ] 待办\n- [x] 完成\n";
        for marker in ["- 一", "* 二", "1. 三", "2) 四"] {
            assert_eq!(marks_at(text, marker, 0), Marks::LIST_MARKER, "{marker}");
        }
        assert_eq!(marks_at(text, "一", 0), Marks::NONE);
        assert_eq!(marks_at(text, "[ ]", 0), Marks::LIST_MARKER);
        assert_eq!(marks_at(text, "[x]", 0), Marks::LIST_MARKER);
        assert_eq!(marks_at(text, "待办", 0), Marks::NONE);
    }

    #[test]
    fn quotes_tables_and_rules() {
        let text = "> 引用\n> > 嵌套\n\n| 甲 | 乙 |\n|---|:-:|\n| 1 | 2 |\n\n---\n";
        assert_eq!(marks_at(text, ">", 0), Marks::QUOTE | Marks::QUOTE_MARKER);
        assert_eq!(
            marks_at(text, "> 嵌套", 0),
            Marks::QUOTE | Marks::QUOTE_MARKER
        );
        assert_eq!(marks_at(text, "引用", 0), Marks::QUOTE);
        assert_eq!(marks_at(text, "| 甲", 0), Marks::TABLE);
        assert_eq!(marks_at(text, "甲", 0), Marks::NONE);
        assert_eq!(marks_at(text, "|---", 0), Marks::TABLE);
        assert_eq!(marks_at(text, ":-:", 0), Marks::TABLE);
        assert_eq!(marks_at(text, "---\n", 0), Marks::RULE);
    }

    #[test]
    fn half_written_tables_are_text() {
        // No delimiter row yet: not a table.
        let text = "| 甲 | 乙\n";
        assert_eq!(marks_at(text, "|", 0), Marks::NONE);
        // A delimiter row with a missing cell, and a row with too many.
        let text = "| a | b |\n|---|\n| 1 | 2 | 3 |\n";
        check_spans(text, &all(text));
        let text = "| a | b |\n|---|---|\n| 1";
        assert_eq!(marks_at(text, "| 1", 0), Marks::TABLE);
        check_spans(text, &all(text));
    }

    #[test]
    fn html_math_and_footnotes() {
        let text = "<div>\n块\n</div>\n\n文 <b>粗</b> $x^2$ 注[^1]\n\n[^1]: 脚注\n";
        assert!(marks_at(text, "<div>", 0).contains(Marks::HTML));
        assert_eq!(marks_at(text, "<b>", 0), Marks::HTML);
        assert_eq!(marks_at(text, "x^2", 0), Marks::MATH);
        assert_eq!(marks_at(text, "$x", 0), Marks::MATH | Marks::MARKER);
        assert!(marks_at(text, "[^1]:", 0).contains(Marks::MARKER));
    }

    #[test]
    fn empty_and_blank_documents() {
        for text in ["", "\n", "\n\n\n", "   "] {
            let highlighter = Highlighter::new(&rope(text));
            assert!(highlighter.spans(0..text.len()).is_empty());
            assert_eq!(highlighter.block_ranges(), vec![0..text.len()]);
        }
        let mut highlighter = Highlighter::new(&rope(""));
        highlighter.edit(&rope("*a*"), 0..0, 3);
        assert_eq!(highlighter.spans(0..3).len(), 3);
        highlighter.edit(&rope(""), 0..3, 0);
        assert!(highlighter.spans(0..0).is_empty());
    }

    #[test]
    fn a_very_long_line() {
        let mut text = "长".repeat(50_000);
        text.push_str(" **尾巴** https://example.com");
        let spans = all(&text);
        check_spans(&text, &spans);
        assert_eq!(marks_at(&text, "尾巴", 0), Marks::STRONG);
        assert_eq!(marks_at(&text, "https", 0), Marks::LINK);
    }

    #[test]
    fn a_block_over_the_limit_is_left_plain() {
        let mut text = "# 标题\n\n".to_owned();
        text.push_str(&"长".repeat(MAX_PARSED / 3 + 10));
        text.push_str(" **尾巴**\n\n**后面**");
        assert_eq!(marks_at(&text, "标题", 0), Marks::HEADING);
        assert_eq!(marks_at(&text, "尾巴", 0), Marks::NONE);
        assert_eq!(marks_at(&text, "后面", 0), Marks::STRONG);
        // Typing into it parses nothing.
        let mut highlighter = Highlighter::new(&rope(&text));
        let at = text.find("尾巴").unwrap();
        let mut edited = text.clone();
        edited.insert(at, 'x');
        highlighter.edit(&rope(&edited), at..at, 1);
        assert_eq!(highlighter.last_parsed(), 0);
    }

    #[test]
    fn blank_lines_inside_constructs_are_not_cuts() {
        let text =
            "- 项\n\n  续\n\n```\na\n\nb\n```\n\n<!--\n\n-->\n\n    code\n\n    more\n\n段落\n";
        let ranges = Highlighter::new(&rope(text)).block_ranges();
        let starts: Vec<&str> = ranges
            .iter()
            .map(|r| text[r.clone()].lines().next().unwrap_or(""))
            .collect();
        assert_eq!(starts, ["- 项", "```", "<!--", "段落"]);
        // Text after a blank line in a fence is code, not a new block.
        assert_eq!(marks_at(text, "b\n", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "more", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "续", 0), Marks::NONE);
    }

    /// A tiny deterministic generator, so the test needs no dependency.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % bound.max(1)
        }
    }

    fn boundary(text: &str, mut at: usize) -> usize {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    #[test]
    fn edits_give_the_same_result_as_starting_over() {
        const PIECES: [&str; 28] = [
            "```rust\n",
            "```py\n",
            "/*",
            "*/",
            "\"\"\"",
            "\"",
            "#",
            "fn ",
            "\n",
            "\n\n",
            "---\n",
            "...\n",
            "```",
            "~~~",
            "# ",
            "*",
            "**",
            "`",
            "[a]: /u\n",
            "[a]",
            "> ",
            "    ",
            "| x |",
            "|---|",
            "- ",
            "中文",
            "<!--",
            "-->",
        ];
        let mut rng = Lcg(7);
        let mut text = String::from(
            "# 标题\n\n段落 *强调*\n\n- 列表\n- [x] 任务\n\n```\n代码\n```\n\n```rust\nfn a() {} /* 注\n释 */ \"s\"\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        let mut highlighter = Highlighter::new(&rope(&text));
        // How many steps had coloured code, so the test covers it.
        let mut coloured = 0;
        for step in 0..3000 {
            let start = boundary(&text, rng.next(text.len() + 1));
            let end = boundary(&text, start + rng.next(6)).max(start);
            let mut insert = if rng.next(3) == 0 {
                ""
            } else {
                PIECES[rng.next(PIECES.len())]
            };
            // Now and then a whole code block, so that there is code to
            // colour and to break.
            if rng.next(15) == 0 {
                insert = "\n\n```rust\nlet a = 1; /* b\n\"c\" */ fn d()\n```\n\n";
            }
            text.replace_range(start..end, insert);
            highlighter.edit(&rope(&text), start..end, insert.len());
            let fresh = Highlighter::new(&rope(&text));
            assert_eq!(
                highlighter.block_ranges(),
                fresh.block_ranges(),
                "blocks after step {step}: {text:?}"
            );
            let spans = highlighter.spans(0..text.len());
            assert_eq!(
                spans,
                fresh.spans(0..text.len()),
                "spans after step {step}: {text:?}"
            );
            check_spans(&text, &spans);
            coloured += usize::from(spans.iter().any(|span| span.marks.code_token().is_some()));
        }
        assert!(coloured > 300, "code was coloured in only {coloured} steps");
    }

    #[test]
    fn front_matter_is_grey_and_one_block() {
        let text = "---\ntitle: *标题*\n\n# 不是标题\ntags:\n  - a\n...\n# 正文\n\n段落\n";
        assert_eq!(
            marks_at(text, "---", 0),
            Marks::FRONT_MATTER | Marks::MARKER
        );
        assert_eq!(marks_at(text, "标题", 0), Marks::FRONT_MATTER);
        assert_eq!(marks_at(text, "# 不是", 0), Marks::FRONT_MATTER);
        assert_eq!(marks_at(text, "- a", 0), Marks::FRONT_MATTER);
        assert_eq!(
            marks_at(text, "...", 0),
            Marks::FRONT_MATTER | Marks::MARKER
        );
        assert!(marks_at(text, "---", 0).is_syntax());
        assert!(marks_at(text, "# 正文", 0).contains(Marks::HEADING));
        let ranges = Highlighter::new(&rope(text)).block_ranges();
        let end = text.find("# 正文").unwrap();
        assert_eq!(ranges[0], 0..end);
        assert_eq!(ranges.len(), 3);
        // Only at the very start: further down it is a rule and a heading.
        let text = "段落\n\n---\ntitle: 标题\n---\n";
        assert_eq!(marks_at(text, "---", 0), Marks::RULE);
        assert!(marks_at(text, "title", 0).contains(Marks::HEADING));
        // Never closed: Markdown as usual.
        assert_eq!(marks_at("---\ntitle: 标题\n", "---", 0), Marks::RULE);
        // Closed on the last line, without a line break.
        let text = "---\na: 1\n---";
        let spans = all(text);
        check_spans(text, &spans);
        assert_eq!(spans.last().unwrap().range, 9..12);
    }

    #[test]
    fn front_matter_made_and_unmade_by_edits() {
        let mut text = String::from("---\ntitle: 标题\n\n段落\n\n# 标题\n");
        let mut highlighter = Highlighter::new(&rope(&text));
        let check = |highlighter: &Highlighter, text: &str| {
            let fresh = Highlighter::new(&rope(text));
            assert_eq!(highlighter.block_ranges(), fresh.block_ranges(), "{text:?}");
            assert_eq!(
                highlighter.spans(0..text.len()),
                fresh.spans(0..text.len()),
                "{text:?}"
            );
        };
        let mut edit = |text: &mut String, range: Range<usize>, insert: &str| {
            text.replace_range(range.clone(), insert);
            highlighter.edit(&rope(text), range, insert.len());
            check(&highlighter, text);
            highlighter.spans(0..text.len())
        };
        // Closing it far below makes everything above front matter.
        let at = text.find("# 标题").unwrap();
        let spans = edit(&mut text, at..at, "---\n");
        assert!(spans[0].marks.contains(Marks::FRONT_MATTER));
        assert_eq!(spans.last().unwrap().marks, Marks::HEADING);
        // Typing inside it, and after it.
        let at = text.find("段落").unwrap();
        edit(&mut text, at..at, "x: ");
        let end = text.len();
        edit(&mut text, end..end, "\n---\n");
        // Breaking the closing line: the next one down closes it.
        let at = text.find("---\n#").unwrap();
        edit(&mut text, at..at, "-");
        assert_eq!(marks_at(&text, "# 标题", 0), Marks::FRONT_MATTER);
        edit(&mut text, at..at + 1, "");
        assert!(marks_at(&text, "# 标题", 0).contains(Marks::HEADING));
        // Breaking the opening line, and mending it.
        edit(&mut text, 0..1, "");
        edit(&mut text, 0..0, "-");
        assert!(all(&text)[0].marks.contains(Marks::FRONT_MATTER));
        // A blank first line, and text before it.
        edit(&mut text, 4..4, "\n");
        edit(&mut text, 4..5, "");
        edit(&mut text, 0..0, "前\n");
        assert!(!all(&text)[0].marks.contains(Marks::FRONT_MATTER));
    }

    #[test]
    fn random_edits_around_front_matter_give_the_same_result() {
        const PIECES: [&str; 14] = [
            "---\n",
            "...\n",
            "---",
            "-",
            "\n",
            "\n\n",
            "a: b\n",
            "- x\n",
            "# h\n",
            "```",
            "    ",
            "中文",
            "[a]: /u\n",
            "[a]",
        ];
        let mut rng = Lcg(11);
        let mut text = String::from("---\ntitle: 标题\n\n- a\n---\n# 正文\n\n段落 [a]\n");
        let mut highlighter = Highlighter::new(&rope(&text));
        // How often the edits made and unmade front matter.
        let mut changes = 0;
        for step in 0..3000 {
            let had = highlighter.front_matter;
            // Most edits near the top, where front matter is made and unmade.
            let near = if rng.next(3) == 0 {
                text.len() + 1
            } else {
                (text.len() + 1).min(40)
            };
            let mut start = boundary(&text, rng.next(near));
            let mut end = boundary(&text, start + rng.next(5)).max(start);
            let mut insert = if rng.next(4) == 0 {
                ""
            } else {
                PIECES[rng.next(PIECES.len())]
            };
            // Now and then a fresh opening line, so it does not stay broken.
            if rng.next(25) == 0 {
                (start, end, insert) = (0, 0, "---\n");
            }
            text.replace_range(start..end, insert);
            highlighter.edit(&rope(&text), start..end, insert.len());
            let fresh = Highlighter::new(&rope(&text));
            assert_eq!(
                highlighter.block_ranges(),
                fresh.block_ranges(),
                "blocks after step {step}: {text:?}"
            );
            let spans = highlighter.spans(0..text.len());
            assert_eq!(
                spans,
                fresh.spans(0..text.len()),
                "spans after step {step}: {text:?}"
            );
            check_spans(&text, &spans);
            changes += usize::from((had == 0) != (highlighter.front_matter == 0));
        }
        assert!(changes > 20, "front matter changed only {changes} times");
    }

    #[test]
    fn an_edit_parses_only_nearby_blocks() {
        let paragraph = "这是一段比较长的中文段落，带有 **粗体** 和 `代码`。\n\n";
        let text = paragraph.repeat(2000);
        let mut highlighter = Highlighter::new(&rope(&text));
        assert_eq!(highlighter.last_parsed(), text.len());
        let at = paragraph.len() * 1000 + 3;
        let mut edited = text.clone();
        edited.insert(at, '字');
        highlighter.edit(&rope(&edited), at..at, "字".len());
        assert!(
            highlighter.last_parsed() < paragraph.len() * 3,
            "parsed {} bytes",
            highlighter.last_parsed()
        );
        assert_eq!(
            highlighter.spans(0..edited.len()),
            Highlighter::new(&rope(&edited)).spans(0..edited.len())
        );
    }

    #[test]
    fn spans_are_found_by_range() {
        let text = "**a**\n\n**b**\n";
        let highlighter = Highlighter::new(&rope(text));
        let spans = highlighter.spans(7..9);
        assert!(
            spans
                .iter()
                .all(|span| span.range.end > 7 && span.range.start < 9)
        );
        assert!(!spans.is_empty());
        assert!(highlighter.spans(5..7).is_empty());
    }
}
