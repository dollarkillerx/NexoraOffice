//! What the preview shows, as a tree of blocks with styled text, before any
//! layout. Every block remembers which part of the source it came from, so
//! the preview and the editor can be scrolled to the same place.
//!
//! Some things are shown as they are written instead of rendered: HTML,
//! `$math$` and footnotes appear as code, and an image as a box with its
//! description, since the preview loads nothing from disk or the network.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Parser, Tag, TagEnd};

use crate::syntax;

/// A whole document, ready to lay out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    /// Byte range of the block in the source.
    pub source: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlockKind {
    Paragraph(Inline),
    Heading {
        /// 1 to 6.
        level: u8,
        text: Inline,
    },
    /// Shown verbatim in a monospaced font: code blocks, and also HTML blocks
    /// and display math, whose `language` is then `html` or `math`.
    Code {
        language: String,
        text: String,
    },
    Quote(Vec<Block>),
    List {
        /// Number of the first item of an ordered list.
        start: Option<u64>,
        items: Vec<Item>,
    },
    Table {
        alignments: Vec<Alignment>,
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
    /// An image on a paragraph of its own, shown as a box with its
    /// description. Images inside text are runs.
    Image {
        description: String,
    },
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// `Some` for a task list item: whether it is ticked.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
    pub source: Range<usize>,
}

/// Text with styled stretches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    /// Stretches of `text` that are not plain, in order, not overlapping.
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Byte range in [`Inline::text`].
    pub range: Range<usize>,
    pub style: Style,
    /// Where a link goes, as written; `mailto:` is added to e-mail autolinks
    /// and `http://` to bare `www.` addresses.
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    /// Inline code, and what is shown like it: inline HTML, math, footnote
    /// references.
    pub code: bool,
    /// The description of an image that sits inside text.
    pub image: bool,
}

impl Style {
    fn is_plain(self) -> bool {
        self == Style::default()
    }
}

/// Read a whole document.
pub fn outline(source: &str) -> Outline {
    let mut builder = Builder::new();
    for (event, range) in Parser::new_ext(source, syntax::options()).into_offset_iter() {
        builder.event(event, range);
    }
    builder.finish()
}

/// Whether a character belongs to a script written without spaces between
/// words, so a line break inside a paragraph joins without one.
fn joins_without_space(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{2FFF}'     // CJK radicals, Kangxi
        | '\u{3000}'..='\u{303F}'   // CJK punctuation
        | '\u{3040}'..='\u{30FF}'   // Hiragana, Katakana
        | '\u{3100}'..='\u{312F}'   // Bopomofo
        | '\u{3190}'..='\u{31FF}'
        | '\u{3400}'..='\u{4DBF}'   // CJK extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK unified ideographs
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FFEF}'   // full-width forms
        | '\u{20000}'..='\u{3FFFF}')
}

/// Collects text and styles for one paragraph, heading or table cell.
#[derive(Default)]
struct InlineBuilder {
    inline: Inline,
    source: Range<usize>,
    strong: usize,
    emphasis: usize,
    strike: usize,
    links: Vec<String>,
    /// Inside an image: its description is being collected.
    image: Option<(String, String)>,
    /// A line break in the source that becomes a space, or nothing between
    /// two CJK characters, depending on what follows.
    soft_break: bool,
}

impl InlineBuilder {
    fn new(source: Range<usize>) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    fn style(&self) -> Style {
        Style {
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            strike: self.strike > 0,
            ..Style::default()
        }
    }

    fn push(&mut self, text: &str, style: Style, link: Option<String>) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.soft_break) {
            let before = self.inline.text.chars().next_back();
            let after = text.chars().next();
            let join =
                before.is_some_and(joins_without_space) && after.is_some_and(joins_without_space);
            if !join {
                self.push(" ", self.style(), self.links.last().cloned());
            }
        }
        let start = self.inline.text.len();
        self.inline.text.push_str(text);
        if style.is_plain() && link.is_none() {
            return;
        }
        let end = self.inline.text.len();
        match self.inline.runs.last_mut() {
            Some(last) if last.range.end == start && last.style == style && last.link == link => {
                last.range.end = end;
            }
            _ => self.inline.runs.push(Run {
                range: start..end,
                style,
                link,
            }),
        }
    }

    /// Text from the source: inside a link it links there, outside one any
    /// bare web address in it becomes a link of its own.
    fn text(&mut self, text: &str) {
        if let Some((description, _)) = &mut self.image {
            description.push_str(text);
            return;
        }
        let style = self.style();
        if let Some(link) = self.links.last().cloned() {
            self.push(text, style, Some(link));
            return;
        }
        let mut at = 0;
        for url in syntax::find_bare_urls(text) {
            self.push(&text[at..url.range.start], style, None);
            self.push(&text[url.range.clone()], style, Some(url.url));
            at = url.range.end;
        }
        self.push(&text[at..], style, None);
    }

    fn code(&mut self, text: &str) {
        if let Some((description, _)) = &mut self.image {
            description.push_str(text);
            return;
        }
        let style = Style {
            code: true,
            ..self.style()
        };
        self.push(text, style, self.links.last().cloned());
    }

    fn finish(self) -> Inline {
        self.inline
    }
}

enum Frame {
    Quote(Range<usize>, Vec<Block>),
    List {
        source: Range<usize>,
        start: Option<u64>,
        items: Vec<Item>,
    },
    Item {
        source: Range<usize>,
        task: Option<bool>,
        blocks: Vec<Block>,
    },
    Footnote {
        source: Range<usize>,
        label: String,
        blocks: Vec<Block>,
    },
}

enum Leaf {
    None,
    Inline {
        builder: InlineBuilder,
        /// `None` for a paragraph, else the heading level.
        heading: Option<u8>,
        /// The paragraph is the implicit one of a tight list item.
        implicit: bool,
    },
    Code {
        source: Range<usize>,
        language: String,
        text: String,
    },
    Table {
        source: Range<usize>,
        alignments: Vec<Alignment>,
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
        in_head: bool,
        cell: Option<InlineBuilder>,
    },
}

struct Builder {
    frames: Vec<Frame>,
    leaf: Leaf,
    top: Vec<Block>,
}

impl Builder {
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            leaf: Leaf::None,
            top: Vec::new(),
        }
    }

    fn finish(mut self) -> Outline {
        self.close_implicit();
        // A well-formed event stream closes everything; this is for safety.
        while let Some(frame) = self.frames.pop() {
            let block = frame_block(frame);
            self.add(block);
        }
        Outline { blocks: self.top }
    }

    /// Where a finished block goes: into the innermost container.
    fn add(&mut self, block: Block) {
        match self.frames.last_mut() {
            Some(Frame::Quote(_, blocks))
            | Some(Frame::Item { blocks, .. })
            | Some(Frame::Footnote { blocks, .. }) => blocks.push(block),
            // Only items go into lists; anything else is a parser surprise.
            Some(Frame::List { .. }) | None => self.top.push(block),
        }
    }

    /// The inline text being collected, starting the implicit paragraph of
    /// a tight list item if needed.
    fn inline(&mut self, at: usize) -> Option<&mut InlineBuilder> {
        if matches!(self.leaf, Leaf::None) && matches!(self.frames.last(), Some(Frame::Item { .. }))
        {
            self.leaf = Leaf::Inline {
                builder: InlineBuilder::new(at..at),
                heading: None,
                implicit: true,
            };
        }
        match &mut self.leaf {
            Leaf::Inline { builder, .. } => Some(builder),
            Leaf::Table { cell, .. } => cell.as_mut(),
            _ => None,
        }
    }

    fn close_implicit(&mut self) {
        if matches!(self.leaf, Leaf::Inline { implicit: true, .. }) {
            self.close_inline(None);
        }
    }

    fn close_inline(&mut self, end: Option<usize>) {
        let Leaf::Inline {
            builder, heading, ..
        } = std::mem::replace(&mut self.leaf, Leaf::None)
        else {
            return;
        };
        let mut source = builder.source.clone();
        if let Some(end) = end {
            source.end = end;
        }
        let inline = builder.finish();
        let kind = match heading {
            Some(level) => BlockKind::Heading {
                level,
                text: inline,
            },
            None => paragraph_kind(inline),
        };
        if let BlockKind::Paragraph(inline) = &kind
            && inline.text.trim().is_empty()
        {
            return;
        }
        self.add(Block { kind, source });
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag, range),
            Event::Text(text) => {
                if let Leaf::Code { text: code, .. } = &mut self.leaf {
                    code.push_str(&text);
                } else if let Some(inline) = self.inline(range.start) {
                    inline.source.end = inline.source.end.max(range.end);
                    inline.text(&text);
                }
            }
            Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::InlineHtml(text) => {
                if let Some(inline) = self.inline(range.start) {
                    inline.source.end = inline.source.end.max(range.end);
                    inline.code(&text);
                }
            }
            Event::Html(text) => {
                if let Leaf::Code { text: code, .. } = &mut self.leaf {
                    code.push_str(&text);
                } else if let Some(inline) = self.inline(range.start) {
                    inline.code(&text);
                }
            }
            Event::FootnoteReference(label) => {
                if let Some(inline) = self.inline(range.start) {
                    inline.code(&format!("[{label}]"));
                }
            }
            Event::SoftBreak => {
                if let Some(inline) = self.inline(range.start) {
                    if let Some((description, _)) = &mut inline.image {
                        description.push(' ');
                    } else {
                        inline.soft_break = true;
                    }
                }
            }
            Event::HardBreak => {
                if let Some(inline) = self.inline(range.start) {
                    let style = inline.style();
                    inline.soft_break = false;
                    inline.push("\n", style, None);
                }
            }
            Event::Rule => {
                self.close_implicit();
                self.add(Block {
                    kind: BlockKind::Rule,
                    source: range,
                });
            }
            Event::TaskListMarker(checked) => {
                if let Some(Frame::Item { task, .. }) = self.frames.last_mut() {
                    *task = Some(checked);
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        let block_level = matches!(
            tag,
            Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::HtmlBlock
                | Tag::List(_)
                | Tag::Item
                | Tag::FootnoteDefinition(_)
                | Tag::Table(_)
        );
        if block_level {
            self.close_implicit();
        }
        match tag {
            Tag::Paragraph => {
                self.leaf = Leaf::Inline {
                    builder: InlineBuilder::new(range),
                    heading: None,
                    implicit: false,
                }
            }
            Tag::Heading { level, .. } => {
                self.leaf = Leaf::Inline {
                    builder: InlineBuilder::new(range),
                    heading: Some(level as u8),
                    implicit: false,
                }
            }
            Tag::BlockQuote(_) => self.frames.push(Frame::Quote(range, Vec::new())),
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.leaf = Leaf::Code {
                    source: range,
                    language,
                    text: String::new(),
                };
            }
            Tag::HtmlBlock => {
                self.leaf = Leaf::Code {
                    source: range,
                    language: "html".to_owned(),
                    text: String::new(),
                }
            }
            Tag::List(start) => self.frames.push(Frame::List {
                source: range,
                start,
                items: Vec::new(),
            }),
            Tag::Item => self.frames.push(Frame::Item {
                source: range,
                task: None,
                blocks: Vec::new(),
            }),
            Tag::FootnoteDefinition(label) => self.frames.push(Frame::Footnote {
                source: range,
                label: label.to_string(),
                blocks: Vec::new(),
            }),
            Tag::Table(alignments) => {
                self.leaf = Leaf::Table {
                    source: range,
                    alignments: alignments
                        .into_iter()
                        .map(|alignment| match alignment {
                            pulldown_cmark::Alignment::None => Alignment::None,
                            pulldown_cmark::Alignment::Left => Alignment::Left,
                            pulldown_cmark::Alignment::Center => Alignment::Center,
                            pulldown_cmark::Alignment::Right => Alignment::Right,
                        })
                        .collect(),
                    head: Vec::new(),
                    rows: Vec::new(),
                    in_head: false,
                    cell: None,
                }
            }
            Tag::TableHead => {
                if let Leaf::Table { in_head, .. } = &mut self.leaf {
                    *in_head = true;
                }
            }
            Tag::TableRow => {
                if let Leaf::Table { rows, .. } = &mut self.leaf {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Leaf::Table { cell, .. } = &mut self.leaf {
                    *cell = Some(InlineBuilder::new(range));
                }
            }
            Tag::Emphasis => {
                if let Some(inline) = self.inline(range.start) {
                    inline.emphasis += 1;
                }
            }
            Tag::Strong => {
                if let Some(inline) = self.inline(range.start) {
                    inline.strong += 1;
                }
            }
            Tag::Strikethrough => {
                if let Some(inline) = self.inline(range.start) {
                    inline.strike += 1;
                }
            }
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                if let Some(inline) = self.inline(range.start) {
                    let url = if link_type == LinkType::Email && !dest_url.starts_with("mailto:") {
                        format!("mailto:{dest_url}")
                    } else {
                        dest_url.to_string()
                    };
                    inline.links.push(url);
                }
            }
            Tag::Image { dest_url, .. } => {
                if let Some(inline) = self.inline(range.start) {
                    inline.image = Some((String::new(), dest_url.to_string()));
                }
            }
            Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd, range: Range<usize>) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.close_inline(Some(range.end)),
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                if let Leaf::Code {
                    source,
                    language,
                    mut text,
                } = std::mem::replace(&mut self.leaf, Leaf::None)
                {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.add(Block {
                        kind: BlockKind::Code { language, text },
                        source,
                    });
                }
            }
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item | TagEnd::FootnoteDefinition => {
                self.close_implicit();
                match self.frames.pop() {
                    Some(Frame::Item {
                        source,
                        task,
                        blocks,
                    }) => {
                        let item = Item {
                            task,
                            blocks,
                            source,
                        };
                        match self.frames.last_mut() {
                            Some(Frame::List { items, .. }) => items.push(item),
                            // An item outside a list: keep its content.
                            _ => {
                                for block in item.blocks {
                                    self.add(block);
                                }
                            }
                        }
                    }
                    Some(frame) => {
                        let block = frame_block(frame);
                        self.add(block);
                    }
                    None => {}
                }
            }
            TagEnd::Table => {
                if let Leaf::Table {
                    source,
                    alignments,
                    head,
                    rows,
                    ..
                } = std::mem::replace(&mut self.leaf, Leaf::None)
                {
                    self.add(Block {
                        kind: BlockKind::Table {
                            alignments,
                            head,
                            rows,
                        },
                        source,
                    });
                }
            }
            TagEnd::TableHead => {
                if let Leaf::Table { in_head, .. } = &mut self.leaf {
                    *in_head = false;
                }
            }
            TagEnd::TableCell => {
                if let Leaf::Table {
                    head,
                    rows,
                    in_head,
                    cell,
                    ..
                } = &mut self.leaf
                    && let Some(builder) = cell.take()
                {
                    let inline = builder.finish();
                    if *in_head {
                        head.push(inline);
                    } else if let Some(row) = rows.last_mut() {
                        row.push(inline);
                    }
                }
            }
            TagEnd::Emphasis => {
                if let Some(inline) = self.inline(range.start) {
                    inline.emphasis = inline.emphasis.saturating_sub(1);
                }
            }
            TagEnd::Strong => {
                if let Some(inline) = self.inline(range.start) {
                    inline.strong = inline.strong.saturating_sub(1);
                }
            }
            TagEnd::Strikethrough => {
                if let Some(inline) = self.inline(range.start) {
                    inline.strike = inline.strike.saturating_sub(1);
                }
            }
            TagEnd::Link => {
                if let Some(inline) = self.inline(range.start) {
                    inline.links.pop();
                }
            }
            TagEnd::Image => {
                if let Some(inline) = self.inline(range.start)
                    && let Some((description, url)) = inline.image.take()
                {
                    let shown = if description.trim().is_empty() {
                        url.rsplit('/').next().unwrap_or(&url).to_owned()
                    } else {
                        description
                    };
                    let style = Style {
                        image: true,
                        ..inline.style()
                    };
                    let link = inline.links.last().cloned();
                    inline.push(&shown, style, link);
                }
            }
            TagEnd::TableRow
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }
}

fn frame_block(frame: Frame) -> Block {
    match frame {
        Frame::Quote(source, blocks) => Block {
            kind: BlockKind::Quote(blocks),
            source,
        },
        Frame::List {
            source,
            start,
            items,
        } => Block {
            kind: BlockKind::List { start, items },
            source,
        },
        // Only reached for an item left open, which the parser never does;
        // its content is kept rather than lost.
        Frame::Item { source, blocks, .. } => Block {
            kind: BlockKind::List {
                start: None,
                items: vec![Item {
                    task: None,
                    blocks,
                    source: source.clone(),
                }],
            },
            source,
        },
        Frame::Footnote {
            source,
            label,
            blocks,
        } => Block {
            kind: BlockKind::Footnote { label, blocks },
            source,
        },
    }
}

/// A paragraph that is nothing but one image becomes an image block.
fn paragraph_kind(inline: Inline) -> BlockKind {
    if let [run] = inline.runs.as_slice()
        && run.style.image
        && run.range == (0..inline.text.len())
    {
        return BlockKind::Image {
            description: inline.text,
        };
    }
    BlockKind::Paragraph(inline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<BlockKind> {
        outline(source)
            .blocks
            .into_iter()
            .map(|block| block.kind)
            .collect()
    }

    fn paragraph(source: &str) -> Inline {
        match kinds(source).into_iter().next() {
            Some(BlockKind::Paragraph(inline)) => inline,
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    fn styled(inline: &Inline, style: Style) -> Vec<&str> {
        inline
            .runs
            .iter()
            .filter(|run| run.style == style)
            .map(|run| &inline.text[run.range.clone()])
            .collect()
    }

    #[test]
    fn empty_documents_have_no_blocks() {
        assert!(outline("").blocks.is_empty());
        assert!(outline("\n\n  \n").blocks.is_empty());
    }

    #[test]
    fn headings_paragraphs_and_their_sources() {
        let source = "# 一级标题\n\n第一段\n第二行\n\n## Two\n";
        let blocks = outline(source).blocks;
        assert_eq!(blocks.len(), 3);
        match &blocks[0].kind {
            BlockKind::Heading { level: 1, text } => assert_eq!(text.text, "一级标题"),
            other => panic!("{other:?}"),
        }
        assert_eq!(&source[blocks[0].source.clone()].trim_end(), &"# 一级标题");
        match &blocks[1].kind {
            // Lines of CJK text join without a space.
            BlockKind::Paragraph(inline) => assert_eq!(inline.text, "第一段第二行"),
            other => panic!("{other:?}"),
        }
        assert!(source[blocks[1].source.clone()].starts_with("第一段"));
        assert!(matches!(
            blocks[2].kind,
            BlockKind::Heading { level: 2, .. }
        ));
        assert!(
            blocks
                .windows(2)
                .all(|pair| pair[0].source.end <= pair[1].source.start)
        );
    }

    #[test]
    fn latin_lines_join_with_a_space_and_hard_breaks_stay() {
        assert_eq!(paragraph("one\ntwo").text, "one two");
        assert_eq!(paragraph("中文\nEnglish").text, "中文 English");
        assert_eq!(paragraph("one  \ntwo").text, "one\ntwo");
    }

    #[test]
    fn inline_styles() {
        let inline = paragraph("普通 **粗** *斜* ~~删~~ `码` ***两者*** <b>h</b> $x$");
        let strong = Style {
            strong: true,
            ..Style::default()
        };
        assert_eq!(styled(&inline, strong), ["粗"]);
        assert_eq!(
            styled(
                &inline,
                Style {
                    emphasis: true,
                    ..Style::default()
                }
            ),
            ["斜"]
        );
        assert_eq!(
            styled(
                &inline,
                Style {
                    strike: true,
                    ..Style::default()
                }
            ),
            ["删"]
        );
        assert_eq!(
            styled(
                &inline,
                Style {
                    code: true,
                    ..Style::default()
                }
            ),
            ["码", "<b>", "</b>", "x"]
        );
        assert_eq!(
            styled(
                &inline,
                Style {
                    strong: true,
                    emphasis: true,
                    ..Style::default()
                }
            ),
            ["两者"]
        );
        assert!(inline.text.starts_with("普通 粗 斜"));
    }

    #[test]
    fn links_and_bare_addresses() {
        let inline = paragraph(
            "[网站](https://example.com) <me@example.com> 或 www.example.org、https://a.example/x。",
        );
        let links: Vec<(&str, &str)> = inline
            .runs
            .iter()
            .filter_map(|run| Some((&inline.text[run.range.clone()], run.link.as_deref()?)))
            .collect();
        assert_eq!(
            links,
            [
                ("网站", "https://example.com"),
                ("me@example.com", "mailto:me@example.com"),
                ("www.example.org", "http://www.example.org"),
                ("https://a.example/x", "https://a.example/x"),
            ]
        );
        // No addresses inside code.
        let inline = paragraph("`https://example.com`");
        assert!(inline.runs.iter().all(|run| run.link.is_none()));
    }

    #[test]
    fn images_are_placeholders() {
        assert_eq!(
            kinds("![一张图](pic.png)"),
            [BlockKind::Image {
                description: "一张图".into()
            }]
        );
        assert_eq!(
            kinds("![](dir/photo.jpg)"),
            [BlockKind::Image {
                description: "photo.jpg".into()
            }]
        );
        let inline = paragraph("文字 ![小图](a.png) 文字");
        assert_eq!(
            styled(
                &inline,
                Style {
                    image: true,
                    ..Style::default()
                }
            ),
            ["小图"]
        );
    }

    #[test]
    fn code_blocks_html_and_unclosed_fences() {
        assert_eq!(
            kinds("```rust title\nfn main() {}\n```\n"),
            [BlockKind::Code {
                language: "rust".into(),
                text: "fn main() {}".into()
            }]
        );
        assert_eq!(
            kinds("```\n代码\n\n# 不是标题"),
            [BlockKind::Code {
                language: String::new(),
                text: "代码\n\n# 不是标题".into()
            }]
        );
        assert_eq!(
            kinds("<div>\n块\n</div>\n"),
            [BlockKind::Code {
                language: "html".into(),
                text: "<div>\n块\n</div>".into()
            }]
        );
    }

    #[test]
    fn lists_tight_loose_ordered_and_tasks() {
        let blocks = kinds("- 一\n- 二\n  - 嵌套\n\n3. 三\n4. 四\n\n- [ ] 待办\n- [x] 完成\n");
        let BlockKind::List { start: None, items } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].blocks,
            [Block {
                kind: BlockKind::Paragraph(Inline {
                    text: "一".into(),
                    runs: vec![]
                }),
                source: 2..5
            }]
        );
        assert!(matches!(items[1].blocks[1].kind, BlockKind::List { .. }));
        assert!(matches!(blocks[1], BlockKind::List { start: Some(3), .. }));
        let BlockKind::List { items, .. } = &blocks[2] else {
            panic!();
        };
        assert_eq!(
            items.iter().map(|item| item.task).collect::<Vec<_>>(),
            [Some(false), Some(true)]
        );
    }

    #[test]
    fn quotes_rules_and_footnotes() {
        let blocks = kinds("> 引用\n>\n> > 嵌套\n\n---\n\n注[^a]\n\n[^a]: 脚注内容\n");
        let BlockKind::Quote(inner) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert!(matches!(inner[1].kind, BlockKind::Quote(_)));
        assert_eq!(blocks[1], BlockKind::Rule);
        let BlockKind::Paragraph(inline) = &blocks[2] else {
            panic!();
        };
        assert_eq!(inline.text, "注[a]");
        assert!(matches!(&blocks[3], BlockKind::Footnote { label, .. } if label == "a"));
    }

    #[test]
    fn tables_including_ragged_ones() {
        let blocks = kinds("| 甲 | 乙 |\n|:--|--:|\n| 1 | 2 |\n| 只有一格 |\n| a | b | c |\n");
        let BlockKind::Table {
            alignments,
            head,
            rows,
        } = &blocks[0]
        else {
            panic!("{blocks:?}");
        };
        assert_eq!(alignments, &[Alignment::Left, Alignment::Right]);
        assert_eq!(
            head.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["甲", "乙"]
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][1].text, "2");
        // GitHub pads short rows and drops extra cells; the parser does too.
        assert!(rows.iter().all(|row| row.len() <= 2));
        // Without a delimiter row it is a paragraph.
        assert!(matches!(kinds("| 甲 | 乙 |\n")[0], BlockKind::Paragraph(_)));
    }

    #[test]
    fn a_very_long_line_is_one_paragraph() {
        let source = "字".repeat(200_000);
        let inline = paragraph(&source);
        assert_eq!(inline.text.len(), source.len());
    }
}
