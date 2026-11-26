//! The Writer document model.
//!
//! A [`Document`] is a body of [`Block`]s (paragraphs and tables) in reading
//! order, the sections that give its pages their size and margins, the
//! headers, footers and notes, and the style sheet and list definitions the
//! file came with.
//!
//! **What is resolved.** Everything needed to show the document is final in
//! the model: a paragraph's [`ParaProps`] and a run's [`RunLook`] already
//! hold what document defaults, styles, table styles, list levels and direct
//! formatting add up to; theme fonts and colours are worked out. The reader
//! does that. Nothing downstream walks style chains.
//!
//! **Units.** Lengths are in twips (a twentieth of a point, 1,440 per inch),
//! the unit of the file format; drawing sizes are in EMU (914,400 per inch,
//! 635 per twip); text sizes are in points.
//!
//! **Positions.** A paragraph's text is its runs' texts joined; an offset is
//! a byte offset into that. Everything that takes up a place in a line is in
//! that text: a tab is `\t`, a line break `\n`, and a picture, a page break
//! or a note reference is one U+FFFC ([`OBJECT`]). A [`Pos`] is a paragraph
//! (by its [`Path`]) and an offset.
//!
//! **Sharing.** Blocks are behind [`Arc`]s: cloning a document is cheap, an
//! edit copies only the blocks it changes ([`Arc::make_mut`]), and a block
//! that is the same `Arc` as before has not changed. Undo keeps earlier
//! documents; layout caches by block.
//!
//! **Where it came from.** Paragraphs, runs, tables and sections read from a
//! file carry a source: the byte range of their element in the part they
//! were read from, and for paragraphs and runs what they looked like. Saving
//! writes back, byte for byte, whatever is still as it was read
//! ([`Paragraph::is_as_read`], [`Run::is_as_read`]), and rewrites only the
//! rest. Things the model does not understand but must keep in place inside
//! a paragraph (bookmarks, comment ranges, field codes, deleted text of
//! tracked changes) are zero-width [`Inline::Marker`] runs.
//!
//! Nothing here reads files; `aster-ooxml` turns DOCX packages into this
//! model. Nothing here knows about layout or the GUI toolkit either.

pub mod edit;
pub mod numbering;

use std::ops::Range;
use std::sync::Arc;

/// Twips per point.
pub const TWIPS_PER_POINT: f64 = 20.0;
/// Twips per inch.
pub const TWIPS_PER_INCH: Twips = 1_440;
/// EMU per twip.
pub const EMU_PER_TWIP: i64 = 635;
/// Text size when nothing says otherwise (Word's own default is 10 points;
/// almost every file sets its own).
pub const DEFAULT_TEXT_POINTS: f32 = 10.0;
/// The text of a run that is one object in the line: a drawing, a page or
/// column break, a note reference.
pub const OBJECT: &str = "\u{FFFC}";

/// A length in twentieths of a point.
pub type Twips = i32;
/// Red, green, blue, alpha.
pub type Rgba = [u8; 4];

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Document {
    pub body: Story,
    /// The section the document ends with. Earlier sections end at the
    /// paragraph that carries them ([`Paragraph::section`]).
    pub last_section: Arc<Section>,
    /// Headers and footers; sections refer to them by index.
    pub marginals: Vec<Arc<Story>>,
    pub footnotes: Notes,
    pub endnotes: Notes,
    pub styles: Arc<StyleSheet>,
    pub numbering: Arc<Numbering>,
    pub settings: Settings,
}

/// A flow of blocks: the body, a header or footer, the inside of a text
/// box.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Story {
    pub blocks: Vec<Block>,
    /// The part it was read from (`word/document.xml`, `word/header2.xml`);
    /// `None` for one made here. Source ranges of its blocks are byte ranges
    /// of that part as it was read.
    pub part: Option<Arc<str>>,
}

/// The footnotes or the endnotes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Notes {
    /// Without the separator notes the file format keeps beside them.
    pub notes: Vec<Note>,
    pub part: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// As the file numbers it; [`Inline::Footnote`] and [`Inline::Endnote`]
    /// refer to it.
    pub id: i32,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Distance between the tab stops a paragraph has where it sets none.
    pub default_tab: Twips,
    /// Even pages take the sections' even headers and footers.
    pub even_and_odd_headers: bool,
    /// BCP 47, as the document defaults name them (`en-US`, `zh-CN`,
    /// `ja-JP`).
    pub language: Option<String>,
    pub east_asian_language: Option<String>,
    /// Squeeze the blank halves of full-width punctuation
    /// (`characterSpacingControl` is `compressPunctuation`).
    pub compress_punctuation: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_tab: 720,
            even_and_odd_headers: false,
            language: None,
            east_asian_language: None,
            compress_punctuation: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Arc<Paragraph>),
    Table(Arc<Table>),
}

impl Block {
    pub fn paragraph(paragraph: Paragraph) -> Self {
        Block::Paragraph(Arc::new(paragraph))
    }

    pub fn table(table: Table) -> Self {
        Block::Table(Arc::new(table))
    }

    pub fn as_paragraph(&self) -> Option<&Paragraph> {
        match self {
            Block::Paragraph(paragraph) => Some(paragraph),
            Block::Table(_) => None,
        }
    }

    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Block::Table(table) => Some(table),
            Block::Paragraph(_) => None,
        }
    }

    /// The same `Arc` as `other`: nothing in it has changed.
    pub fn is(&self, other: &Block) -> bool {
        match (self, other) {
            (Block::Paragraph(a), Block::Paragraph(b)) => Arc::ptr_eq(a, b),
            (Block::Table(a), Block::Table(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// Which paragraph (or table) of the body. `[b]` is body block `b`;
/// `[b, r, c, k]` is block `k` of cell `c` of row `r` of the table that is
/// body block `b`; three more steps for each further table inside. Compared
/// element by element, paths are in reading order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Path(pub Vec<u32>);

impl Path {
    pub fn body(block: usize) -> Self {
        Path(vec![block as u32])
    }

    /// Block `block` of cell `cell` of row `row` of the table at `self`.
    pub fn cell_block(&self, row: usize, cell: usize, block: usize) -> Self {
        let mut steps = self.0.clone();
        steps.extend([row as u32, cell as u32, block as u32]);
        Path(steps)
    }

    /// Inside a table: the path of the innermost table and the row, cell
    /// and block within it.
    pub fn in_cell(&self) -> Option<(Path, usize, usize, usize)> {
        let n = self.0.len();
        (n >= 4).then(|| {
            (
                Path(self.0[..n - 3].to_vec()),
                self.0[n - 3] as usize,
                self.0[n - 2] as usize,
                self.0[n - 1] as usize,
            )
        })
    }

    /// The body block it is in or under.
    pub fn top(&self) -> usize {
        self.0.first().map_or(0, |step| *step as usize)
    }
}

/// A place in the body's text: a paragraph and a byte offset into its text.
/// Ordered in reading order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub path: Path,
    pub offset: usize,
}

impl Pos {
    pub fn new(path: Path, offset: usize) -> Self {
        Self { path, offset }
    }
}

fn block_at<'a>(blocks: &'a [Block], steps: &[u32]) -> Option<&'a Block> {
    let (first, rest) = steps.split_first()?;
    let block = blocks.get(*first as usize)?;
    if rest.is_empty() {
        return Some(block);
    }
    let [row, cell, rest @ ..] = rest else {
        return None;
    };
    let table = block.as_table()?;
    let cell = table.rows.get(*row as usize)?.cells.get(*cell as usize)?;
    block_at(&cell.blocks, rest)
}

fn block_at_mut<'a>(blocks: &'a mut [Block], steps: &[u32]) -> Option<&'a mut Block> {
    let (first, rest) = steps.split_first()?;
    let block = blocks.get_mut(*first as usize)?;
    if rest.is_empty() {
        return Some(block);
    }
    let [row, cell, rest @ ..] = rest else {
        return None;
    };
    let Block::Table(table) = block else {
        return None;
    };
    // Check before copying: a path that leads nowhere must not unshare.
    table.rows.get(*row as usize)?.cells.get(*cell as usize)?;
    let table = Arc::make_mut(table);
    let cell = &mut table.rows[*row as usize].cells[*cell as usize];
    block_at_mut(&mut cell.blocks, rest)
}

fn walk<'a>(
    blocks: &'a [Block],
    prefix: &mut Vec<u32>,
    visit: &mut dyn FnMut(&Path, &'a Arc<Paragraph>),
) {
    for (index, block) in blocks.iter().enumerate() {
        prefix.push(index as u32);
        match block {
            Block::Paragraph(paragraph) => visit(&Path(prefix.clone()), paragraph),
            Block::Table(table) => {
                for (r, row) in table.rows.iter().enumerate() {
                    for (c, cell) in row.cells.iter().enumerate() {
                        prefix.extend([r as u32, c as u32]);
                        walk(&cell.blocks, prefix, visit);
                        prefix.truncate(prefix.len() - 2);
                    }
                }
            }
        }
        prefix.pop();
    }
}

/// Every paragraph under `blocks`, tables included, in reading order, with
/// its path from `blocks`.
pub fn paragraphs_of(blocks: &[Block]) -> Vec<(Path, &Arc<Paragraph>)> {
    let mut out = Vec::new();
    walk(blocks, &mut Vec::new(), &mut |path, paragraph| {
        out.push((path.clone(), paragraph));
    });
    out
}

impl Document {
    /// The block at `path` in the body.
    pub fn block(&self, path: &Path) -> Option<&Block> {
        block_at(&self.body.blocks, &path.0)
    }

    /// The paragraph at `path` in the body.
    pub fn paragraph(&self, path: &Path) -> Option<&Paragraph> {
        self.block(path)?.as_paragraph()
    }

    /// The paragraph at `path`, to change: it and the tables it is in are
    /// copied if they are shared.
    pub fn paragraph_mut(&mut self, path: &Path) -> Option<&mut Paragraph> {
        match block_at_mut(&mut self.body.blocks, &path.0)? {
            Block::Paragraph(paragraph) => Some(Arc::make_mut(paragraph)),
            Block::Table(_) => None,
        }
    }

    /// The blocks `path`'s block is one of: the body's, or a table cell's.
    pub fn siblings(&self, path: &Path) -> Option<&[Block]> {
        match path.in_cell() {
            None => (path.0.len() == 1).then_some(self.body.blocks.as_slice()),
            Some((table, row, cell, _)) => {
                let table = self.block(&table)?.as_table()?;
                Some(table.rows.get(row)?.cells.get(cell)?.blocks.as_slice())
            }
        }
    }

    /// The same, to change.
    pub fn siblings_mut(&mut self, path: &Path) -> Option<&mut Vec<Block>> {
        match path.in_cell() {
            None => (path.0.len() == 1).then_some(&mut self.body.blocks),
            Some((table, row, cell, _)) => {
                self.block(&table)?
                    .as_table()?
                    .rows
                    .get(row)?
                    .cells
                    .get(cell)?;
                let Block::Table(table) = block_at_mut(&mut self.body.blocks, &table.0)? else {
                    return None;
                };
                Some(&mut Arc::make_mut(table).rows[row].cells[cell].blocks)
            }
        }
    }

    /// Every paragraph of the body in reading order, those in tables too.
    pub fn paragraphs(&self) -> Vec<(Path, &Arc<Paragraph>)> {
        paragraphs_of(&self.body.blocks)
    }

    /// The first and the last place of the body.
    pub fn start(&self) -> Option<Pos> {
        let paragraphs = self.paragraphs();
        let (path, _) = paragraphs.first()?;
        Some(Pos::new(path.clone(), 0))
    }

    pub fn end(&self) -> Option<Pos> {
        let paragraphs = self.paragraphs();
        let (path, paragraph) = paragraphs.last()?;
        Some(Pos::new(path.clone(), paragraph.len()))
    }

    /// The body's text, one paragraph per line; table cells in reading
    /// order.
    pub fn plain_text(&self) -> String {
        self.paragraphs()
            .iter()
            .map(|(_, paragraph)| paragraph.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The section each body block is in, in order: every earlier section
    /// ends at the paragraph that carries it, the last is
    /// [`Document::last_section`]. One entry per section, as the index of
    /// its last body block and the section; the last entry's index is the
    /// last block's (0 for an empty body).
    pub fn sections(&self) -> Vec<(usize, &Arc<Section>)> {
        let mut out = Vec::new();
        for (index, block) in self.body.blocks.iter().enumerate() {
            if let Some(section) = block.as_paragraph().and_then(|p| p.section.as_ref()) {
                out.push((index, section));
            }
        }
        out.push((self.body.blocks.len().saturating_sub(1), &self.last_section));
        out
    }

    /// Work out every paragraph's list label ([`Paragraph::label`]) from
    /// its list and the paragraphs before it. Call after reading and after
    /// any edit that adds, removes or moves paragraphs or changes a list.
    /// Paragraphs whose label does not change are left as the same `Arc`.
    pub fn renumber(&mut self) {
        numbering::renumber(self);
    }
}

// ---------------------------------------------------------------------------
// Paragraphs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Paragraph {
    pub runs: Vec<Run>,
    pub props: ParaProps,
    /// What a run of this paragraph looks like when it has no formatting
    /// or character style of its own: document defaults, the table style
    /// where it is in a table, and the paragraph style.
    pub base: RunLook,
    /// The look of the paragraph mark: what text typed into the empty
    /// paragraph gets, and what gives an empty paragraph its height.
    pub mark: RunLook,
    /// Its list number or bullet, worked out by [`Document::renumber`].
    pub label: Option<Label>,
    /// This paragraph is the last of a section with these properties.
    pub section: Option<Arc<Section>>,
    /// Where it was read from; `None` for a paragraph made here.
    pub source: Option<Arc<ParaSource>>,
}

impl Paragraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// Its length in bytes.
    pub fn len(&self) -> usize {
        self.runs.iter().map(|run| run.text.len()).sum()
    }

    /// No text and no objects; markers do not count.
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }

    /// The run that has the byte at `offset` and the offset within it. At a
    /// boundary between runs it is the run that ends there (text typed
    /// there continues it), skipping markers; at 0 the first run.
    pub fn run_at(&self, offset: usize) -> Option<(usize, usize)> {
        let mut start = 0;
        let mut found = None;
        for (index, run) in self.runs.iter().enumerate() {
            let end = start + run.text.len();
            if run.text.is_empty() {
                continue;
            }
            if offset > start && offset <= end {
                return Some((index, offset - start));
            }
            if offset == start && found.is_none() {
                found = Some((index, 0));
            }
            start = end;
        }
        found
    }

    /// The look text typed at `offset` gets: that of the run it would
    /// continue, or the paragraph mark's in an empty paragraph.
    pub fn look_at(&self, offset: usize) -> &RunLook {
        match self.run_at(offset) {
            Some((index, _)) => &self.runs[index].look,
            None => &self.mark,
        }
    }

    /// Nothing that saving writes has changed since it was read: the same
    /// runs in the same order, each as read, and the same properties.
    /// False for a paragraph made here.
    pub fn is_as_read(&self) -> bool {
        let Some(source) = &self.source else {
            return false;
        };
        self.props == source.props
            && self.mark == source.mark
            && match (&self.section, &source.section) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                _ => false,
            }
            && self.runs.len() == source.runs.len()
            && self.runs.iter().zip(&source.runs).all(|(run, read)| {
                run.source
                    .as_ref()
                    .is_some_and(|own| Arc::ptr_eq(own, read))
                    && run.is_as_read()
            })
    }

    /// The same paragraph with no memory of a file, for pasting into
    /// another document.
    pub fn detached(&self) -> Paragraph {
        let mut copy = self.clone();
        copy.source = None;
        copy.section = None;
        copy.runs.retain(|run| !matches!(run.kind, Inline::Marker));
        for run in &mut copy.runs {
            run.source = None;
        }
        copy
    }
}

/// A paragraph as read.
#[derive(Clone, Debug, PartialEq)]
pub struct ParaSource {
    /// The `<w:p>` element, in the bytes of its story's part.
    pub range: Range<usize>,
    /// Its runs as read, in order.
    pub runs: Vec<Arc<RunSource>>,
    pub props: ParaProps,
    pub mark: RunLook,
    pub section: Option<Arc<Section>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    /// Both edges, the last line left.
    Justify,
    /// Both edges, the last line too.
    Distribute,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineSpacing {
    /// A multiple of the line's natural height: 1.0 is single.
    Multiple(f32),
    /// The natural height, but no less than this.
    AtLeast(Twips),
    Exactly(Twips),
}

impl Default for LineSpacing {
    fn default() -> Self {
        LineSpacing::Multiple(1.0)
    }
}

/// A paragraph's formatting, final.
#[derive(Clone, Debug, PartialEq)]
pub struct ParaProps {
    /// The paragraph style's id.
    pub style: Option<Arc<str>>,
    pub align: Align,
    /// From the margins to the text. The first line starts `first_line`
    /// further in; negative for a hanging indent.
    pub indent_left: Twips,
    pub indent_right: Twips,
    pub first_line: Twips,
    pub space_before: Twips,
    pub space_after: Twips,
    pub line: LineSpacing,
    /// No space between this paragraph and a neighbour of the same style.
    pub contextual_spacing: bool,
    /// Stay on the page of the next paragraph.
    pub keep_next: bool,
    /// All lines on one page.
    pub keep_lines: bool,
    pub page_break_before: bool,
    /// Never leave one line alone at the top or the bottom of a page.
    pub widow_control: bool,
    /// 0 for the top level of the outline (Heading 1), up to 8; `None` for
    /// body text.
    pub outline_level: Option<u8>,
    pub list: Option<ListRef>,
    /// Sorted by position.
    pub tabs: Vec<TabStop>,
    /// Left, top, right, bottom.
    pub borders: [Option<Border>; 4],
    /// Between this paragraph and a neighbour with the same borders.
    pub border_between: Option<Border>,
    /// The background.
    pub shading: Option<Rgba>,
    /// Lines keep to the section's grid, where it has one.
    pub snap_to_grid: bool,
}

impl Default for ParaProps {
    fn default() -> Self {
        Self {
            style: None,
            align: Align::Left,
            indent_left: 0,
            indent_right: 0,
            first_line: 0,
            space_before: 0,
            space_after: 0,
            line: LineSpacing::default(),
            contextual_spacing: false,
            keep_next: false,
            keep_lines: false,
            page_break_before: false,
            widow_control: true,
            outline_level: None,
            list: None,
            tabs: Vec::new(),
            borders: [None; 4],
            border_between: None,
            shading: None,
            snap_to_grid: true,
        }
    }
}

/// Which list a paragraph is an item of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRef {
    /// [`List::id`].
    pub list: u32,
    /// 0 to 8.
    pub level: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabStop {
    /// From the left margin (not from the indent).
    pub position: Twips,
    pub align: TabAlign,
    pub leader: TabLeader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabAlign {
    #[default]
    Left,
    Center,
    Right,
    Decimal,
    /// Draws a vertical bar and takes no text.
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabLeader {
    #[default]
    None,
    Dots,
    Hyphens,
    Underscore,
    MiddleDots,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    pub style: BorderStyle,
    /// Points.
    pub width: f32,
    /// `None` for automatic: the text colour.
    pub color: Option<Rgba>,
    /// Between the border and what it surrounds, points.
    pub space: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Dotted,
    Dashed,
    DotDash,
    Wave,
    /// Thick, triple and the decorative ones; drawn as a single line.
    Other,
}

/// A list number or bullet as it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// `1.`, `(a)`, `•`, `第一章`.
    pub text: String,
    pub look: RunLook,
    pub suffix: Suffix,
    pub align: Align,
}

/// What follows a list label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Suffix {
    /// A tab to the paragraph's left indent (or the next tab stop).
    #[default]
    Tab,
    Space,
    Nothing,
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// A stretch of a paragraph with one look, or one thing in the line.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Run {
    /// What it adds to the paragraph's text; see [`Inline`] for what each
    /// kind holds here.
    pub text: String,
    pub kind: Inline,
    pub look: RunLook,
    /// The character style's id.
    pub style: Option<Arc<str>>,
    pub link: Option<Arc<Link>>,
    /// Where it was read from; `None` for a run made here.
    pub source: Option<Arc<RunSource>>,
}

impl Run {
    pub fn text(text: impl Into<String>, look: RunLook) -> Self {
        Self {
            text: text.into(),
            kind: Inline::Text,
            look,
            style: None,
            link: None,
            source: None,
        }
    }

    /// One thing in the line that is not text, with the text its kind
    /// stands for. A page number or a ruby starts empty: give it the text
    /// it shows.
    pub fn object(kind: Inline, look: RunLook) -> Self {
        let text = match &kind {
            Inline::Text | Inline::PageNumber(_) | Inline::Ruby(_) => String::new(),
            Inline::Tab => "\t".to_owned(),
            Inline::Break(Break::Line) => "\n".to_owned(),
            Inline::Marker => String::new(),
            Inline::Break(_) | Inline::Drawing(_) | Inline::Footnote(_) | Inline::Endnote(_) => {
                OBJECT.to_owned()
            }
        };
        Self {
            text,
            kind,
            look,
            style: None,
            link: None,
            source: None,
        }
    }

    /// Text that typing can be added to and that can be split. Everything
    /// else is taken or left whole.
    pub fn is_text(&self) -> bool {
        matches!(self.kind, Inline::Text)
    }

    /// Nothing that saving writes has changed since it was read. False for
    /// a run made here.
    pub fn is_as_read(&self) -> bool {
        self.source.as_ref().is_some_and(|source| {
            self.text == source.text
                && self.look == source.look
                && self.style == source.style
                && match (&self.link, &source.link) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                    _ => false,
                }
        })
    }

    /// The same look, style and link, so that the two could be one run.
    pub fn same_look(&self, other: &Run) -> bool {
        self.look == other.look && self.style == other.style && self.link == other.link
    }
}

/// What a run is.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Inline {
    /// Its text, without tabs or line breaks.
    #[default]
    Text,
    /// `\t`.
    Tab,
    /// `\n` for a line break, [`OBJECT`] for a page or column break.
    Break(Break),
    /// A picture, text box or other graphic, in the line or anchored at
    /// this place. [`OBJECT`].
    Drawing(Arc<Drawing>),
    /// A reference to [`Note::id`] in the footnotes or endnotes; the number
    /// that shows is worked out in layout. [`OBJECT`].
    Footnote(i32),
    Endnote(i32),
    /// A page-number field. Its text is the result the file had; layout
    /// shows the real number. Taken or left whole.
    PageNumber(PageField),
    /// Text with its reading above it (furigana, pinyin). Its text is the
    /// base text. Taken or left whole.
    Ruby(Arc<Ruby>),
    /// Something kept in place that shows nothing and takes no room: a
    /// bookmark, a comment range, a field's code, deleted text of a tracked
    /// change. Its text is empty. Edits never remove a marker except with
    /// the whole document content around it; when paragraphs are joined
    /// their markers stay.
    Marker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Break {
    Line,
    Page,
    Column,
}

/// The reading shown above a run's text.
#[derive(Clone, Debug, PartialEq)]
pub struct Ruby {
    pub text: String,
    /// Points.
    pub size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageField {
    /// The number of the page it is on.
    Page,
    /// Pages in the document.
    PageCount,
    /// Pages in the section.
    SectionPages,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// A URL, or `#name` for a bookmark in the document.
    pub target: String,
    pub tooltip: Option<String>,
}

/// A run as read.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSource {
    /// What the run was made from, in the bytes of its story's part: a
    /// whole `<w:r>` when the run is all of it, or the one child of a
    /// `<w:r>` (a `<w:t>`, a `<w:tab/>`, a `<w:drawing>`) when the `<w:r>`
    /// became several runs; for a marker, the element or elements kept.
    pub range: Range<usize>,
    /// The enclosing `<w:r>` where `range` is one of its children.
    pub parent: Option<Range<usize>>,
    pub text: String,
    pub look: RunLook,
    pub style: Option<Arc<str>>,
    pub link: Option<Arc<Link>>,
}

/// How text looks, final.
#[derive(Clone, Debug, PartialEq)]
pub struct RunLook {
    /// Typefaces for Latin and for East Asian text, as the file names them
    /// (theme fonts already looked up).
    pub font: Option<Arc<str>>,
    pub east_asian_font: Option<Arc<str>>,
    /// Points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Underline>,
    pub strike: bool,
    pub double_strike: bool,
    /// `None` for automatic: black, or white on a dark background.
    pub color: Option<Rgba>,
    /// The highlighter pen.
    pub highlight: Option<Rgba>,
    /// Character shading.
    pub shading: Option<Rgba>,
    pub position: VertAlign,
    pub caps: bool,
    pub small_caps: bool,
    /// Hidden text: not shown, takes no room.
    pub hidden: bool,
    /// Added between characters; negative to tighten.
    pub spacing: Twips,
    /// Horizontal stretch, 1.0 for none.
    pub scale: f32,
    /// Raised above the baseline, points; negative for lowered.
    pub raise: f32,
}

impl Default for RunLook {
    fn default() -> Self {
        Self {
            font: None,
            east_asian_font: None,
            size: DEFAULT_TEXT_POINTS,
            bold: false,
            italic: false,
            underline: None,
            strike: false,
            double_strike: false,
            color: None,
            highlight: None,
            shading: None,
            position: VertAlign::Baseline,
            caps: false,
            small_caps: false,
            hidden: false,
            spacing: 0,
            scale: 1.0,
            raise: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Underline {
    pub style: UnderlineStyle,
    /// `None` for the text's colour.
    pub color: Option<Rgba>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    Single,
    Double,
    Thick,
    Dotted,
    Dashed,
    Wave,
    /// Under words only.
    Words,
}

/// Changes to a look: what is `Some` is set, the rest is left. What a
/// toolbar command applies to a selection, and what a list level adds to
/// its label.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RunPatch {
    pub font: Option<Option<Arc<str>>>,
    pub east_asian_font: Option<Option<Arc<str>>>,
    pub size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<Option<Underline>>,
    pub strike: Option<bool>,
    pub color: Option<Option<Rgba>>,
    pub highlight: Option<Option<Rgba>>,
    pub position: Option<VertAlign>,
}

impl RunPatch {
    pub fn apply(&self, look: &mut RunLook) {
        if let Some(font) = &self.font {
            look.font = font.clone();
        }
        if let Some(font) = &self.east_asian_font {
            look.east_asian_font = font.clone();
        }
        if let Some(size) = self.size {
            look.size = size;
        }
        if let Some(bold) = self.bold {
            look.bold = bold;
        }
        if let Some(italic) = self.italic {
            look.italic = italic;
        }
        if let Some(underline) = self.underline {
            look.underline = underline;
        }
        if let Some(strike) = self.strike {
            look.strike = strike;
        }
        if let Some(color) = self.color {
            look.color = color;
        }
        if let Some(highlight) = self.highlight {
            look.highlight = highlight;
        }
        if let Some(position) = self.position {
            look.position = position;
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == RunPatch::default()
    }
}

// ---------------------------------------------------------------------------
// Drawings
// ---------------------------------------------------------------------------

/// A graphic: in the line like a big character, or floating, anchored at
/// its run's place.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawing {
    /// Width and height, EMU.
    pub size: (i64, i64),
    pub content: DrawingContent,
    /// `None` when it sits in the line.
    pub float: Option<Float>,
    /// Its description for screen readers.
    pub alt: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawingContent {
    Picture(Picture),
    TextBox(TextBox),
    /// Something this version does not draw, kept in the file: what it is
    /// (`chart`, `shape`, `diagram`, `object`), for a placeholder.
    Other(String),
}

/// A picture: the file's own bytes (PNG, JPEG or another format), decoded
/// when drawn, and how much is cropped off each side.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub image: Arc<[u8]>,
    /// Shares of the picture cut off the left, top, right and bottom.
    pub crop: [f64; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    /// In the same part as the paragraph that anchors it.
    pub blocks: Vec<Block>,
    pub fill: Option<Rgba>,
    pub line: Option<Border>,
    /// Left, top, right, bottom, EMU.
    pub insets: [i64; 4],
}

/// Where a floating drawing goes and how text gets out of its way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Float {
    pub x: FloatAxis,
    pub y: FloatAxis,
    pub wrap: Wrap,
    /// Drawn under the text rather than over it.
    pub behind: bool,
    /// Kept clear around it: left, top, right, bottom, EMU.
    pub distance: [i64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatAxis {
    pub from: FloatFrom,
    pub at: FloatAt,
}

/// What a floating drawing's position is measured from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatFrom {
    Page,
    Margin,
    Column,
    /// Horizontally the character, vertically the line it is anchored in.
    Text,
    /// Vertically: the top of the anchoring paragraph.
    Paragraph,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FloatAt {
    /// EMU from the reference's left or top.
    Offset(i64),
    /// Left or top.
    Start,
    Center,
    /// Right or bottom.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Wrap {
    /// Text runs over or under it.
    None,
    #[default]
    Square,
    Tight,
    /// Text above and below only.
    TopAndBottom,
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A table as the file has it: a grid of column widths and rows of cells.
/// A cell covers [`Cell::span`] grid columns; a row's spans, with the
/// columns it skips before and after, add up to the grid. Cells merged
/// downwards are separate cells marked [`VMerge`].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Table {
    pub columns: Vec<Twips>,
    pub rows: Vec<Row>,
    pub props: TableProps,
    /// The `<w:tbl>` element, in the bytes of its story's part.
    pub source: Option<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableProps {
    pub style: Option<Arc<str>>,
    /// Left, centre or right between the margins.
    pub align: Align,
    /// From the left margin to the table's edge.
    pub indent: Twips,
    /// What cells have inside their borders unless they set their own:
    /// left, top, right, bottom.
    pub cell_margins: [Twips; 4],
    /// Column widths are as given; otherwise Word may widen columns to fit
    /// their content.
    pub fixed_layout: bool,
}

impl Default for TableProps {
    fn default() -> Self {
        Self {
            style: None,
            align: Align::Left,
            indent: 0,
            // Word's defaults: 0.08 inch left and right.
            cell_margins: [115, 0, 115, 0],
            fixed_layout: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub height: RowHeight,
    /// Repeated at the top of every page the table continues on.
    pub header: bool,
    /// Not to be split between pages.
    pub cant_split: bool,
    /// Grid columns left empty before the first and after the last cell.
    pub grid_before: u32,
    pub grid_after: u32,
    /// The `<w:tr>` element.
    pub source: Option<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RowHeight {
    /// As high as its content.
    #[default]
    Auto,
    AtLeast(Twips),
    Exactly(Twips),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// At least one paragraph.
    pub blocks: Vec<Block>,
    /// Grid columns it covers, at least 1.
    pub span: u32,
    pub merge: VMerge,
    /// Left, top, right, bottom; final: what the table style, the table's
    /// borders (its inside lines too) and the cell's own add up to.
    pub borders: [Option<Border>; 4],
    pub shading: Option<Rgba>,
    /// Left, top, right, bottom, final.
    pub margins: [Twips; 4],
    pub align: CellAlign,
    /// The `<w:tc>` element.
    pub source: Option<Range<usize>>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            span: 1,
            merge: VMerge::None,
            borders: [None; 4],
            shading: None,
            margins: [115, 0, 115, 0],
            align: CellAlign::Top,
            source: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VMerge {
    #[default]
    None,
    /// The first of cells merged downwards: it has the content.
    Start,
    /// Covered by the cell above.
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// The pages of one stretch of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Page width and height.
    pub page: (Twips, Twips),
    pub margins: Margins,
    pub columns: Columns,
    pub start: SectionStart,
    /// Indexes into [`Document::marginals`]. A section that names none
    /// already has the previous section's here.
    pub headers: Marginals,
    pub footers: Marginals,
    /// The first page takes the `first` header and footer.
    pub title_page: bool,
    /// Page numbers restart at this number.
    pub page_number_start: Option<u32>,
    pub page_number_format: NumberFormat,
    pub grid: Option<DocGrid>,
    /// Where the text sits on a page it does not fill.
    pub vertical_align: CellAlign,
    /// Text runs top to bottom in columns from the right. Not laid out in
    /// this version: shown horizontally.
    pub vertical_text: bool,
    /// The `<w:sectPr>` element, in the bytes of the main part.
    pub source: Option<Range<usize>>,
}

impl Default for Section {
    /// A4 upright with one-inch margins.
    fn default() -> Self {
        Self {
            page: (11_906, 16_838),
            margins: Margins::default(),
            columns: Columns::default(),
            start: SectionStart::NextPage,
            headers: Marginals::default(),
            footers: Marginals::default(),
            title_page: false,
            page_number_start: None,
            page_number_format: NumberFormat::Decimal,
            grid: None,
            vertical_align: CellAlign::Top,
            vertical_text: false,
            source: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Margins {
    pub top: Twips,
    pub right: Twips,
    pub bottom: Twips,
    pub left: Twips,
    /// From the page's edge to the header's top and the footer's bottom.
    pub header: Twips,
    pub footer: Twips,
    pub gutter: Twips,
}

impl Default for Margins {
    fn default() -> Self {
        Self {
            top: 1_440,
            right: 1_440,
            bottom: 1_440,
            left: 1_440,
            header: 720,
            footer: 720,
            gutter: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Columns {
    /// At least 1.
    pub count: u32,
    /// Between columns of equal width.
    pub space: Twips,
    /// For columns of unequal width: each one's width and the space after
    /// it. Empty for equal widths.
    pub widths: Vec<(Twips, Twips)>,
    /// A line between columns.
    pub separator: bool,
}

impl Default for Columns {
    fn default() -> Self {
        Self {
            count: 1,
            space: 720,
            widths: Vec::new(),
            separator: false,
        }
    }
}

/// Where a section starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SectionStart {
    #[default]
    NextPage,
    /// On the same page.
    Continuous,
    EvenPage,
    OddPage,
    NextColumn,
}

/// Which header or footer story the pages of a section take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Marginals {
    pub default: Option<usize>,
    pub first: Option<usize>,
    pub even: Option<usize>,
}

/// The grid East Asian documents set lines and characters on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocGrid {
    pub kind: GridKind,
    /// The height of a line of the grid.
    pub line_pitch: Twips,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridKind {
    /// Lines only: each line of text takes a whole number of grid lines.
    Lines,
    /// Lines and characters.
    LinesAndChars,
    /// Characters snap, lines too.
    SnapToChars,
}

// ---------------------------------------------------------------------------
// Styles and lists
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct StyleSheet {
    /// In the file's order.
    pub styles: Vec<Style>,
    /// What a paragraph and its runs have with no style at all.
    pub default_para: ParaProps,
    pub default_run: RunLook,
    /// The id of the style paragraphs have when they name none (`Normal`).
    pub default_style: Option<Arc<str>>,
}

impl StyleSheet {
    pub fn get(&self, id: &str) -> Option<&Style> {
        self.styles.iter().find(|style| &*style.id == id)
    }

    /// The formatting a paragraph with style `id` has before its own: the
    /// style's, the default style's for `None` or an unknown id, or the
    /// document defaults.
    pub fn paragraph_look(&self, id: Option<&str>) -> (&ParaProps, &RunLook) {
        id.and_then(|id| self.get(id))
            .filter(|style| style.kind == StyleKind::Paragraph)
            .or_else(|| self.default_style.as_deref().and_then(|id| self.get(id)))
            .map_or((&self.default_para, &self.default_run), |style| {
                (&style.para, &style.run)
            })
    }

    /// The paragraph styles to offer in a style list: not hidden, best
    /// first.
    pub fn paragraph_styles(&self) -> Vec<&Style> {
        let mut styles: Vec<&Style> = self
            .styles
            .iter()
            .filter(|style| style.kind == StyleKind::Paragraph && !style.hidden)
            .collect();
        styles.sort_by_key(|style| (!style.quick, style.priority));
        styles
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub id: Arc<str>,
    /// The name shown to people (`heading 1`, `Normal`, `标题 1`).
    pub name: String,
    pub kind: StyleKind,
    pub based_on: Option<Arc<str>>,
    /// The style of the paragraph after one of this style.
    pub next: Option<Arc<str>>,
    /// Final, through the styles it is based on down to the document
    /// defaults. For a paragraph style: what a paragraph of that style has.
    pub para: ParaProps,
    /// For a paragraph style: its paragraphs' [`Paragraph::base`]. For a
    /// character style: the document defaults with what the style sets.
    pub run: RunLook,
    /// Not offered in style lists.
    pub hidden: bool,
    /// One of the styles a document puts forward (`qFormat`).
    pub quick: bool,
    /// Lower first in style lists.
    pub priority: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleKind {
    Paragraph,
    Character,
    Table,
    Numbering,
}

/// The lists of the document.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Numbering {
    pub lists: Vec<List>,
}

impl Numbering {
    pub fn get(&self, id: u32) -> Option<&List> {
        self.lists.iter().find(|list| list.id == id)
    }
}

/// A list: what paragraphs refer to (`numId`).
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub id: u32,
    /// The definition it is an instance of (`abstractNumId`). Lists of the
    /// same definition count on from each other unless a level restarts.
    pub definition: u32,
    /// Level 0 first; up to nine.
    pub levels: Vec<ListLevel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListLevel {
    pub format: NumberFormat,
    /// `%1.`, `%1.%2`, `第%1章`, or the bullet character; `%n` is the
    /// number of level `n`.
    pub text: String,
    pub start: u32,
    /// This list starts the level again at this number, whatever its
    /// definition has counted (`startOverride`).
    pub restart: Option<u32>,
    pub suffix: Suffix,
    pub align: Align,
    /// What the level adds to the paragraph mark's look for the label.
    pub look: RunPatch,
    /// The indents the level gives its paragraphs. The reader has already
    /// put them into [`ParaProps`] where the paragraph sets none; they are
    /// here for paragraphs put on the level later.
    pub indent_left: Twips,
    pub first_line: Twips,
    /// Numbers in the label show as plain decimal whatever their own
    /// format (`isLgl`).
    pub legal: bool,
}

impl Default for ListLevel {
    fn default() -> Self {
        Self {
            format: NumberFormat::Decimal,
            text: "%1.".to_owned(),
            start: 1,
            restart: None,
            suffix: Suffix::Tab,
            align: Align::Left,
            look: RunPatch::default(),
            indent_left: 720,
            first_line: -360,
            legal: false,
        }
    }
}

/// How numbers are written, for lists and page numbers.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum NumberFormat {
    #[default]
    Decimal,
    /// 01, 02.
    DecimalZero,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    /// The level's text is the bullet.
    Bullet,
    /// No number.
    None,
    /// ①②③.
    DecimalEnclosedCircle,
    /// １２３.
    DecimalFullWidth,
    /// 一、二、三 … 十、十一 (`chineseCounting`, `japaneseCounting`,
    /// `chineseCountingThousand`, `taiwaneseCounting`).
    ChineseCounting,
    /// 壹、贰、叁 (`chineseLegalSimplified`).
    ChineseLegal,
    /// 一〇、一一: digits one by one (`ideographDigital`,
    /// `japaneseDigitalTenThousand`).
    IdeographDigital,
    /// 甲乙丙丁 (`ideographTraditional`).
    IdeographTraditional,
    /// 子丑寅卯 (`ideographZodiac`).
    IdeographZodiac,
    /// アイウ (`aiueoFullWidth`, `aiueo`).
    Aiueo,
    /// イロハ (`irohaFullWidth`, `iroha`).
    Iroha,
    /// 1st, 2nd.
    Ordinal,
    /// Any other, as the file names it; written as decimal.
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Block {
        Block::paragraph(Paragraph {
            runs: vec![Run::text(s, RunLook::default())],
            ..Paragraph::default()
        })
    }

    fn sample() -> Document {
        let cell = |s: &str| Cell {
            blocks: vec![text(s)],
            ..Cell::default()
        };
        let table = Table {
            columns: vec![2_000, 2_000],
            rows: vec![
                Row {
                    cells: vec![cell("a"), cell("b")],
                    ..Row::default()
                },
                Row {
                    cells: vec![cell("c"), cell("d")],
                    ..Row::default()
                },
            ],
            ..Table::default()
        };
        Document {
            body: Story {
                blocks: vec![text("one"), Block::table(table), text("two")],
                part: None,
            },
            ..Document::default()
        }
    }

    #[test]
    fn paragraphs_come_in_reading_order_with_their_paths() {
        let document = sample();
        let found: Vec<(Vec<u32>, String)> = document
            .paragraphs()
            .into_iter()
            .map(|(path, paragraph)| (path.0, paragraph.text()))
            .collect();
        assert_eq!(
            found,
            vec![
                (vec![0], "one".to_owned()),
                (vec![1, 0, 0, 0], "a".to_owned()),
                (vec![1, 0, 1, 0], "b".to_owned()),
                (vec![1, 1, 0, 0], "c".to_owned()),
                (vec![1, 1, 1, 0], "d".to_owned()),
                (vec![2], "two".to_owned()),
            ]
        );
        assert_eq!(document.plain_text(), "one\na\nb\nc\nd\ntwo");
        let mut paths: Vec<Path> = document.paragraphs().into_iter().map(|(p, _)| p).collect();
        let sorted = paths.clone();
        paths.sort();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn changing_a_paragraph_in_a_table_leaves_the_other_blocks_shared() {
        let before = sample();
        let mut after = before.clone();
        let path = Path::body(1).cell_block(1, 0, 0);
        after.paragraph_mut(&path).unwrap().runs[0].text = "changed".to_owned();
        assert_eq!(after.paragraph(&path).unwrap().text(), "changed");
        assert_eq!(before.paragraph(&path).unwrap().text(), "c");
        assert!(after.body.blocks[0].is(&before.body.blocks[0]));
        assert!(!after.body.blocks[1].is(&before.body.blocks[1]));
        assert!(after.body.blocks[2].is(&before.body.blocks[2]));
        // A path that leads nowhere changes nothing.
        let mut same = before.clone();
        assert!(same.paragraph_mut(&Path(vec![1, 9, 9, 9])).is_none());
        assert!(same.body.blocks[1].is(&before.body.blocks[1]));
    }

    #[test]
    fn siblings_are_the_body_or_the_cell() {
        let mut document = sample();
        assert_eq!(document.siblings(&Path::body(2)).unwrap().len(), 3);
        let inner = Path::body(1).cell_block(0, 1, 0);
        assert_eq!(document.siblings(&inner).unwrap().len(), 1);
        document.siblings_mut(&inner).unwrap().push(text("more"));
        assert_eq!(document.plain_text(), "one\na\nb\nmore\nc\nd\ntwo");
    }

    #[test]
    fn run_at_prefers_the_run_that_ends_at_a_boundary() {
        let look = RunLook::default();
        let paragraph = Paragraph {
            runs: vec![
                Run::text("ab", look.clone()),
                Run::object(Inline::Marker, look.clone()),
                Run::text("cd", look.clone()),
            ],
            ..Paragraph::default()
        };
        assert_eq!(paragraph.run_at(0), Some((0, 0)));
        assert_eq!(paragraph.run_at(1), Some((0, 1)));
        assert_eq!(paragraph.run_at(2), Some((0, 2)));
        assert_eq!(paragraph.run_at(3), Some((2, 1)));
        assert_eq!(paragraph.run_at(4), Some((2, 2)));
        assert_eq!(Paragraph::default().run_at(0), None);
    }

    #[test]
    fn a_paragraph_is_as_read_until_something_changes() {
        let look = RunLook::default();
        let run_source = Arc::new(RunSource {
            range: 10..40,
            parent: None,
            text: "hello".to_owned(),
            look: look.clone(),
            style: None,
            link: None,
        });
        let mut run = Run::text("hello", look.clone());
        run.source = Some(run_source.clone());
        let mut paragraph = Paragraph {
            runs: vec![run],
            ..Paragraph::default()
        };
        paragraph.source = Some(Arc::new(ParaSource {
            range: 0..50,
            runs: vec![run_source],
            props: paragraph.props.clone(),
            mark: paragraph.mark.clone(),
            section: None,
        }));
        assert!(paragraph.is_as_read());

        let mut typed = paragraph.clone();
        typed.runs[0].text.push('!');
        assert!(!typed.is_as_read());

        let mut bold = paragraph.clone();
        bold.runs[0].look.bold = true;
        assert!(!bold.is_as_read());

        let mut centred = paragraph.clone();
        centred.props.align = Align::Center;
        assert!(!centred.is_as_read());

        let mut added = paragraph.clone();
        added.runs.push(Run::text("more", look));
        assert!(!added.is_as_read());

        assert!(!paragraph.detached().is_as_read());
    }

    #[test]
    fn a_patch_sets_only_what_it_names() {
        let mut look = RunLook {
            italic: true,
            color: Some([1, 2, 3, 255]),
            ..RunLook::default()
        };
        RunPatch {
            bold: Some(true),
            color: Some(None),
            ..RunPatch::default()
        }
        .apply(&mut look);
        assert!(look.bold && look.italic);
        assert_eq!(look.color, None);
        assert!(RunPatch::default().is_empty());
    }

    #[test]
    fn sections_end_at_the_paragraphs_that_carry_them() {
        let mut document = sample();
        let landscape = Arc::new(Section {
            page: (16_838, 11_906),
            ..Section::default()
        });
        document.paragraph_mut(&Path::body(0)).unwrap().section = Some(landscape.clone());
        let sections = document.sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, 0);
        assert!(Arc::ptr_eq(sections[0].1, &landscape));
        assert_eq!(sections[1].0, 2);
    }
}
