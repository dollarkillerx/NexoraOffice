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
//! What is lost by parsing blocks apart: link reference definitions in one
//! block are given to the others through the broken-link callback, so
//! `[text][label]` still highlights; footnotes are matched within a block only.
//! Highlighting is a hint for the eye, so that is good enough.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign, Range};

use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, LinkType, Parser, Tag, TagEnd};

use crate::syntax;

/// What a stretch of source text is. Several can apply at once: a link inside
/// a heading is both.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Marks(u16);

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

    const NAMES: [&'static str; 16] = [
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

    /// Any of the marks that stand for syntax rather than text.
    pub const fn is_syntax(self) -> bool {
        self.intersects(Marks(
            Self::MARKER.0
                | Self::LIST_MARKER.0
                | Self::QUOTE_MARKER.0
                | Self::TABLE.0
                | Self::RULE.0,
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
        let names: Vec<_> = (0..16)
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

#[derive(Clone, Debug)]
struct Block {
    start: usize,
    end: usize,
    /// Relative to `start`.
    spans: Vec<Span>,
    /// Link reference definitions in this block, normalised.
    labels: Vec<String>,
}

/// The highlighting of one document. Tell it about every edit with
/// [`Highlighter::edit`]; ask it with [`Highlighter::spans`].
#[derive(Clone, Debug)]
pub struct Highlighter {
    /// In order, without gaps, covering the whole text. Never empty.
    blocks: Vec<Block>,
    /// Every link reference label defined somewhere, with how many times.
    labels: HashMap<String, usize>,
    /// Bytes parsed by the last call to `new` or `edit`.
    parsed: usize,
}

impl Highlighter {
    pub fn new(text: &str) -> Self {
        let (cuts, _) = cut(text, 0, |_| false);
        let mut highlighter = Self {
            blocks: Vec::with_capacity(cuts.len()),
            labels: HashMap::new(),
            parsed: 0,
        };
        for (range, labels) in cuts {
            highlighter.count_labels(&labels, true);
            highlighter.blocks.push(Block {
                start: range.start,
                end: range.end,
                spans: Vec::new(),
                labels,
            });
        }
        highlighter.parse_blocks(text, 0..highlighter.blocks.len());
        highlighter
    }

    /// Bring the highlighting in line with an edit already made to the text:
    /// `old` is the byte range that was replaced, `inserted` how many bytes
    /// took its place, and `text` the text after the edit.
    pub fn edit(&mut self, text: &str, old: Range<usize>, inserted: usize) {
        let shift = |offset: usize| (offset + inserted) - old.len();

        // The block the edit starts in. An edit at the very start of a block
        // can join it to the one before (a space typed there indents it), so
        // start one block earlier.
        let mut first = self
            .blocks
            .partition_point(|block| block.end <= old.start)
            .min(self.blocks.len() - 1);
        if first > 0 && self.blocks[first].start == old.start {
            first -= 1;
        }
        let from = self.blocks[first].start;

        // Old blocks that start after the edit, and so are unchanged text.
        // Cutting stops at the first new cut that lands on one of them.
        let mut next = self.blocks.partition_point(|block| block.start <= old.end);
        let blocks = &self.blocks;
        let (cuts, resynced) = cut(text, from, |at| {
            while next < blocks.len() && shift(blocks[next].start) < at {
                next += 1;
            }
            next < blocks.len() && shift(blocks[next].start) == at
        });
        let last = if resynced { next } else { self.blocks.len() };

        // Swap the blocks in, and move the ones after them.
        let added = cuts.len();
        let replaced: Vec<Block> = self
            .blocks
            .splice(
                first..last,
                cuts.into_iter().map(|(range, labels)| Block {
                    start: range.start,
                    end: range.end,
                    spans: Vec::new(),
                    labels,
                }),
            )
            .collect();
        let new = first..first + added;
        for block in &mut self.blocks[new.end..] {
            block.start = shift(block.start);
            block.end = shift(block.end);
        }

        // Link labels: if the set of defined labels changed, links anywhere
        // may have appeared or gone, so everything is parsed again.
        let mut set_changed = false;
        for block in &replaced {
            set_changed |= self.count_labels(&block.labels, false);
        }
        for index in new.clone() {
            let labels = std::mem::take(&mut self.blocks[index].labels);
            set_changed |= self.count_labels(&labels, true);
            self.blocks[index].labels = labels;
        }
        self.parsed = 0;
        if set_changed {
            self.parse_blocks(text, 0..self.blocks.len());
        } else {
            self.parse_blocks(text, new);
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

    fn parse_blocks(&mut self, text: &str, range: Range<usize>) {
        for index in range {
            let block = &self.blocks[index];
            let source = &text[block.start..block.end];
            let spans = highlight_block(source, &self.labels);
            self.parsed += source.len();
            self.blocks[index].spans = spans;
        }
    }

    /// The highlighted spans that overlap `range`, in document offsets.
    pub fn spans(&self, range: Range<usize>) -> Vec<Span> {
        let first = self
            .blocks
            .partition_point(|block| block.end <= range.start);
        let mut out = Vec::new();
        for block in &self.blocks[first..] {
            if block.start >= range.end && !(range.is_empty() && block.start == range.start) {
                break;
            }
            for span in &block.spans {
                let absolute = block.start + span.range.start..block.start + span.range.end;
                if absolute.end > range.start && absolute.start < range.end {
                    out.push(Span {
                        range: absolute,
                        marks: span.marks,
                    });
                }
            }
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
        self.blocks
            .iter()
            .map(|block| block.start..block.end)
            .collect()
    }
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
/// its link reference labels.
fn cut(text: &str, from: usize, mut stop: impl FnMut(usize) -> bool) -> (Vec<Cut>, bool) {
    let mut blocks = Vec::new();
    let mut start = from;
    let mut labels = Vec::new();
    let mut open = Open::Nothing;
    let mut previous_blank = false;
    let mut at = from;
    while at < text.len() {
        let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
        let line = &text[at..end];
        let blank = line.trim().is_empty();
        if at != start
            && previous_blank
            && open == Open::Nothing
            && !blank
            && !line.starts_with([' ', '\t'])
        {
            blocks.push((start..at, std::mem::take(&mut labels)));
            if stop(at) {
                return (blocks, true);
            }
            start = at;
        }
        open = match open {
            Open::Nothing => {
                if let Some(label) = syntax::definition_label(line) {
                    labels.push(label);
                }
                fence_open(line)
                    .or_else(|| html_open(line))
                    .unwrap_or(Open::Nothing)
            }
            Open::Fence { fence, len, indent } => {
                if fence_closes(line, fence, len, indent) {
                    Open::Nothing
                } else {
                    open
                }
            }
            Open::Html(closing) => {
                if line.to_ascii_lowercase().contains(closing) {
                    Open::Nothing
                } else {
                    open
                }
            }
        };
        previous_blank = blank;
        at = end;
    }
    blocks.push((start..text.len(), labels));
    (blocks, false)
}

/// Highlight one block, parsed on its own. Offsets are relative to `source`.
fn highlight_block(source: &str, labels: &HashMap<String, usize>) -> Vec<Span> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let callback = |link: BrokenLink<'_>| {
        labels
            .contains_key(&syntax::normalize_label(&link.reference))
            .then_some((CowStr::Borrowed(""), CowStr::Borrowed("")))
    };
    let parser = Parser::new_with_broken_link_callback(source, syntax::options(), Some(callback));
    let mut marks = Collector::new(source);
    for (event, range) in parser.into_offset_iter() {
        marks.event(event, range);
    }
    flatten(marks.raw)
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
                if self.verbatim == 0 {
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
                if let CodeBlockKind::Fenced(_) = kind {
                    self.fence_markers(range);
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
                self.verbatim = self.verbatim.saturating_sub(1)
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
        for bit in 0..16 {
            if marks.0 & (1 << bit) != 0 {
                edges.push((range.start, true, bit));
                edges.push((range.end, false, bit));
            }
        }
    }
    edges.sort_unstable_by_key(|edge| edge.0);
    let mut counts = [0u32; 16];
    let mut spans: Vec<Span> = Vec::new();
    let mut previous = 0;
    let mut index = 0;
    while index < edges.len() {
        let at = edges[index].0;
        let current = Marks(
            (0..16u16)
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

    fn all(text: &str) -> Vec<Span> {
        Highlighter::new(text).spans(0..text.len())
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
    fn an_unclosed_fence_runs_to_the_end() {
        let text = "前文\n\n```\n代码\n\n# 不是标题\n\n**也不是**";
        assert_eq!(marks_at(text, "前文", 0), Marks::NONE);
        assert_eq!(marks_at(text, "代码", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "# 不是", 0), Marks::CODE_BLOCK);
        assert_eq!(marks_at(text, "也不是", 0), Marks::CODE_BLOCK);
        let highlighter = Highlighter::new(text);
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
            let highlighter = Highlighter::new(text);
            assert!(highlighter.spans(0..text.len()).is_empty());
            assert_eq!(highlighter.block_ranges(), vec![0..text.len()]);
        }
        let mut highlighter = Highlighter::new("");
        highlighter.edit("*a*", 0..0, 3);
        assert_eq!(highlighter.spans(0..3).len(), 3);
        highlighter.edit("", 0..3, 0);
        assert!(highlighter.spans(0..0).is_empty());
    }

    #[test]
    fn a_very_long_line() {
        let mut text = "长".repeat(100_000);
        text.push_str(" **尾巴** https://example.com");
        let spans = all(&text);
        check_spans(&text, &spans);
        assert_eq!(marks_at(&text, "尾巴", 0), Marks::STRONG);
        assert_eq!(marks_at(&text, "https", 0), Marks::LINK);
    }

    #[test]
    fn blank_lines_inside_constructs_are_not_cuts() {
        let text =
            "- 项\n\n  续\n\n```\na\n\nb\n```\n\n<!--\n\n-->\n\n    code\n\n    more\n\n段落\n";
        let ranges = Highlighter::new(text).block_ranges();
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
        const PIECES: [&str; 18] = [
            "\n",
            "\n\n",
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
            "# 标题\n\n段落 *强调*\n\n- 列表\n- [x] 任务\n\n```\n代码\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        let mut highlighter = Highlighter::new(&text);
        for step in 0..3000 {
            let start = boundary(&text, rng.next(text.len() + 1));
            let end = boundary(&text, start + rng.next(6)).max(start);
            let insert = if rng.next(3) == 0 {
                ""
            } else {
                PIECES[rng.next(PIECES.len())]
            };
            text.replace_range(start..end, insert);
            highlighter.edit(&text, start..end, insert.len());
            let fresh = Highlighter::new(&text);
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
        }
    }

    #[test]
    fn an_edit_parses_only_nearby_blocks() {
        let paragraph = "这是一段比较长的中文段落，带有 **粗体** 和 `代码`。\n\n";
        let text = paragraph.repeat(2000);
        let mut highlighter = Highlighter::new(&text);
        assert_eq!(highlighter.last_parsed(), text.len());
        let at = paragraph.len() * 1000 + 3;
        let mut edited = text.clone();
        edited.insert(at, '字');
        highlighter.edit(&edited, at..at, "字".len());
        assert!(
            highlighter.last_parsed() < paragraph.len() * 3,
            "parsed {} bytes",
            highlighter.last_parsed()
        );
        assert_eq!(
            highlighter.spans(0..edited.len()),
            Highlighter::new(&edited).spans(0..edited.len())
        );
    }

    #[test]
    fn spans_are_found_by_range() {
        let text = "**a**\n\n**b**\n";
        let highlighter = Highlighter::new(text);
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
