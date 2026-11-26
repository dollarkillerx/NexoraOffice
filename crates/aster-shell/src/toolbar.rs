//! What the single-row toolbar shows for each kind of document.
//!
//! The model is plain data. A control is enabled only when its command is in
//! [`Command::is_available`] for the view that is in front; everything else is
//! laid out where it will live but drawn disabled, so the toolbar shows the
//! intended design without pretending to work.

use aster_markdown::format::{Block, Styles};

use crate::i18n::Strings;
use crate::icons::Icon;
use crate::tabs::DocKind;

/// Everything a toolbar or status-bar control can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Undo,
    Redo,
    Print,
    ZoomOut,
    ZoomReset,
    ZoomIn,
    ParagraphStyle,
    FontFamily,
    FontSizeDown,
    FontSize,
    FontSizeUp,
    Bold,
    Italic,
    Underline,
    Strikethrough,
    TextColor,
    Highlight,
    InsertLink,
    InsertImage,
    InsertTable,
    InsertComment,
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignJustify,
    BulletList,
    NumberedList,
    Checklist,
    LineSpacing,
    ClearFormat,
    Heading,
    Quote,
    InlineCode,
    ViewSource,
    ViewSplit,
    ViewPreview,
    NumberFormat,
    Currency,
    Percent,
    FillColor,
    Borders,
    MergeCells,
    WrapText,
    AutoSum,
    SortRange,
    FilterRange,
    InsertChart,
    SelectTool,
    InsertTextBox,
    InsertShape,
    NewSlide,
    SlideLayout,
    Present,
    PageThumbnails,
    PageNumber,
    FitWidth,
    RotatePage,
    Annotate,
    FindInDocument,
    IndentMore,
    IndentLess,
    More,
}

impl Command {
    pub const ALL: &'static [Command] = &[
        Command::Undo,
        Command::Redo,
        Command::Print,
        Command::ZoomOut,
        Command::ZoomReset,
        Command::ZoomIn,
        Command::ParagraphStyle,
        Command::FontFamily,
        Command::FontSizeDown,
        Command::FontSize,
        Command::FontSizeUp,
        Command::Bold,
        Command::Italic,
        Command::Underline,
        Command::Strikethrough,
        Command::TextColor,
        Command::Highlight,
        Command::InsertLink,
        Command::InsertImage,
        Command::InsertTable,
        Command::InsertComment,
        Command::AlignLeft,
        Command::AlignCenter,
        Command::AlignRight,
        Command::AlignJustify,
        Command::BulletList,
        Command::NumberedList,
        Command::Checklist,
        Command::LineSpacing,
        Command::ClearFormat,
        Command::Heading,
        Command::Quote,
        Command::InlineCode,
        Command::ViewSource,
        Command::ViewSplit,
        Command::ViewPreview,
        Command::NumberFormat,
        Command::Currency,
        Command::Percent,
        Command::FillColor,
        Command::Borders,
        Command::MergeCells,
        Command::WrapText,
        Command::AutoSum,
        Command::SortRange,
        Command::FilterRange,
        Command::InsertChart,
        Command::SelectTool,
        Command::InsertTextBox,
        Command::InsertShape,
        Command::NewSlide,
        Command::SlideLayout,
        Command::Present,
        Command::PageThumbnails,
        Command::PageNumber,
        Command::FitWidth,
        Command::RotatePage,
        Command::Annotate,
        Command::FindInDocument,
        Command::IndentMore,
        Command::IndentLess,
        Command::More,
    ];

    /// Stable number, used to hand the command to the UI layer and back.
    pub fn id(self) -> i32 {
        Self::ALL
            .iter()
            .position(|command| *command == self)
            .expect("ALL lists every command") as i32
    }

    pub fn from_id(id: i32) -> Option<Self> {
        usize::try_from(id)
            .ok()
            .and_then(|index| Self::ALL.get(index))
            .copied()
    }

    /// Whether this build can carry the command out on a view of this kind:
    /// zoom on the plain-text surface behind Writer tabs; undo, redo, the
    /// three views and the formatting commands in Markdown tabs; nothing
    /// else. Whether there is anything to undo right now is a separate
    /// question, answered by [`ViewState`].
    pub fn is_available(self, kind: DocKind) -> bool {
        match kind {
            DocKind::Writer => matches!(
                self,
                Command::Undo
                    | Command::Redo
                    | Command::ZoomOut
                    | Command::ZoomReset
                    | Command::ZoomIn
                    | Command::ParagraphStyle
                    | Command::FontFamily
                    | Command::FontSizeDown
                    | Command::FontSize
                    | Command::FontSizeUp
                    | Command::Bold
                    | Command::Italic
                    | Command::Underline
                    | Command::Strikethrough
                    | Command::TextColor
                    | Command::Highlight
                    | Command::AlignLeft
                    | Command::AlignCenter
                    | Command::AlignRight
                    | Command::AlignJustify
                    | Command::IndentMore
                    | Command::IndentLess
                    | Command::LineSpacing
                    | Command::BulletList
                    | Command::NumberedList
                    | Command::InsertLink
                    | Command::InsertImage
                    | Command::InsertTable
            ),
            DocKind::Markdown => {
                matches!(
                    self,
                    Command::Undo
                        | Command::Redo
                        | Command::ViewSource
                        | Command::ViewSplit
                        | Command::ViewPreview
                        | Command::ZoomIn
                        | Command::ZoomOut
                        | Command::ZoomReset
                ) || self.formats_markdown()
            }
            DocKind::Sheets => matches!(
                self,
                Command::Undo
                    | Command::Redo
                    | Command::Bold
                    | Command::Italic
                    | Command::Underline
                    | Command::Strikethrough
                    | Command::FontSizeDown
                    | Command::FontSize
                    | Command::FontSizeUp
                    | Command::Currency
                    | Command::Percent
                    | Command::NumberFormat
                    | Command::FillColor
                    | Command::TextColor
                    | Command::Borders
                    | Command::MergeCells
                    | Command::AlignLeft
                    | Command::AlignCenter
                    | Command::AlignRight
                    | Command::WrapText
                    | Command::AutoSum
                    | Command::SortRange
            ),
            DocKind::Slides => matches!(
                self,
                Command::Undo
                    | Command::Redo
                    | Command::NewSlide
                    | Command::InsertTextBox
                    | Command::InsertShape
                    | Command::InsertImage
                    | Command::Bold
                    | Command::Italic
                    | Command::Underline
                    | Command::Strikethrough
                    | Command::FontSizeDown
                    | Command::FontSize
                    | Command::FontSizeUp
                    | Command::TextColor
                    | Command::AlignLeft
                    | Command::AlignCenter
                    | Command::AlignRight
                    | Command::BulletList
                    | Command::NumberedList
                    | Command::Present
            ),
            DocKind::Pdf => matches!(
                self,
                Command::PageThumbnails
                    | Command::PageNumber
                    | Command::ZoomOut
                    | Command::ZoomReset
                    | Command::ZoomIn
                    | Command::FitWidth
            ),
        }
    }

    /// The Slides commands that change text and need some to act on.
    pub fn formats_slide_text(self) -> bool {
        matches!(
            self,
            Command::Bold
                | Command::Italic
                | Command::Underline
                | Command::Strikethrough
                | Command::FontSizeDown
                | Command::FontSize
                | Command::FontSizeUp
                | Command::TextColor
                | Command::AlignLeft
                | Command::AlignCenter
                | Command::AlignRight
                | Command::BulletList
                | Command::NumberedList
        )
    }
}

impl Command {
    /// The commands that write Markdown syntax into the source. Underline
    /// is not one: Markdown has no underline.
    pub fn formats_markdown(self) -> bool {
        matches!(
            self,
            Command::Heading
                | Command::Bold
                | Command::Italic
                | Command::Strikethrough
                | Command::InlineCode
                | Command::BulletList
                | Command::NumberedList
                | Command::Checklist
                | Command::Quote
                | Command::InsertLink
                | Command::InsertImage
                | Command::InsertTable
        )
    }
}

/// How a letter button draws its letter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LetterStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// A square button with a line icon.
    Button(Icon),
    /// A square button drawn as a letter, such as B for bold.
    Letter(&'static str, LetterStyle),
    /// A field that opens a list, such as the font family. `width` is in
    /// logical pixels.
    Select { value: String, width: f32 },
    /// A value shown between two stepper buttons, such as the font size.
    /// Activating it resets the value.
    Value { text: String, width: f32 },
    /// A colour button: its face (a line icon, or a letter) over a bar of
    /// the colour in effect, opening a palette. The list's first option is
    /// "none" (or "automatic"), then [`PALETTE`] in order.
    Swatch {
        icon: Option<Icon>,
        letter: &'static str,
        color: Option<[u8; 4]>,
    },
    /// A thin vertical rule between groups.
    Separator,
    /// Pushes what follows to the right-hand end.
    Spacer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub control: Control,
    /// `None` for separators and spacers.
    pub command: Option<Command>,
    pub enabled: bool,
    /// Shown pressed: the choice in effect, such as the current view.
    pub active: bool,
    /// For a field that opens a list: what the list offers, in order.
    pub options: Vec<String>,
}

/// How a Markdown tab shows its document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// The source only.
    #[default]
    Edit,
    /// The source on the left, the preview on the right.
    Split,
    /// The preview only.
    Preview,
}

impl ViewMode {
    pub fn command(self) -> Command {
        match self {
            ViewMode::Edit => Command::ViewSource,
            ViewMode::Split => Command::ViewSplit,
            ViewMode::Preview => Command::ViewPreview,
        }
    }

    pub fn of(command: Command) -> Option<ViewMode> {
        match command {
            Command::ViewSource => Some(ViewMode::Edit),
            Command::ViewSplit => Some(ViewMode::Split),
            Command::ViewPreview => Some(ViewMode::Preview),
            _ => None,
        }
    }

    /// The source is on screen.
    pub fn shows_source(self) -> bool {
        self != ViewMode::Preview
    }

    /// The preview is on screen.
    pub fn shows_preview(self) -> bool {
        self != ViewMode::Edit
    }
}

/// What the toolbar needs to know about the view in front.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom_percent: u32,
    /// There is a step to take back.
    pub can_undo: bool,
    /// There is an undone step to do again.
    pub can_redo: bool,
    pub view_mode: ViewMode,
    /// Markdown tabs: the heading level of the line with the caret, 0 for
    /// body text.
    pub heading: u8,
    /// Markdown tabs: the styles the selection has, whose buttons show
    /// pressed.
    pub styles: Styles,
    /// Sheets tabs: how the active cell looks. Slides tabs: how the text
    /// the buttons act on looks.
    pub cell: CellLook,
    /// Slides tabs: there is text to format (a shape or text selected), and
    /// its first paragraph's list: 0 none, 1 bullets, 2 numbers.
    pub slide_text: bool,
    pub slide_list: u8,
    /// Slides tabs: there is a slide to put things on.
    pub has_slide: bool,
    /// PDF tabs: the page on screen (from 1) and the number of pages.
    pub page: (usize, usize),
    /// PDF tabs: the page thumbnails are shown.
    pub thumbnails: bool,
}

/// How the text at the caret or over the selection looks, for the Writer
/// toolbar. Anything that is only partly so shows as off.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocLook {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// The typeface name, empty if mixed or the document's own.
    pub font: String,
    /// Points; 0 if mixed.
    pub size: f32,
    /// The name of the paragraph style, empty if mixed.
    pub style: String,
    /// 1 left, 2 centre, 3 right, 4 justified, 0 mixed.
    pub align: u8,
    pub bullets: bool,
    pub numbering: bool,
    pub color: Option<[u8; 4]>,
    pub highlight: Option<[u8; 4]>,
}

impl DocLook {
    fn has(&self, command: Command) -> bool {
        match command {
            Command::Bold => self.bold,
            Command::Italic => self.italic,
            Command::Underline => self.underline,
            Command::Strikethrough => self.strike,
            Command::AlignLeft => self.align == 1,
            Command::AlignCenter => self.align == 2,
            Command::AlignRight => self.align == 3,
            Command::AlignJustify => self.align == 4,
            Command::BulletList => self.bullets,
            Command::NumberedList => self.numbering,
            _ => false,
        }
    }
}

/// How the active cell of a workbook looks, for the toolbar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellLook {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// 0 by value, 1 left, 2 centre, 3 right.
    pub align: u8,
    pub wrap: bool,
    pub merged: bool,
    pub filled: bool,
    pub bordered: bool,
    /// The fill and text colours, `None` for none and automatic.
    pub fill: Option<[u8; 4]>,
    pub font_color: Option<[u8; 4]>,
    /// Points.
    pub size: f32,
    /// Index into [`number_formats`], or `None` for another format.
    pub format: Option<usize>,
}

impl Default for CellLook {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            align: 0,
            wrap: false,
            merged: false,
            filled: false,
            bordered: false,
            fill: None,
            font_color: None,
            size: 11.0,
            format: Some(0),
        }
    }
}

/// The colours a colour button offers, as Excel's palette has them: light
/// tints for fills, then the standard colours.
pub const PALETTE: [[u8; 4]; 16] = [
    [0xFF, 0xFF, 0xFF, 0xFF],
    [0xF2, 0xF2, 0xF2, 0xFF],
    [0xD9, 0xD9, 0xD9, 0xFF],
    [0xFC, 0xE4, 0xD6, 0xFF],
    [0xFF, 0xF2, 0xCC, 0xFF],
    [0xE2, 0xEF, 0xDA, 0xFF],
    [0xDD, 0xEB, 0xF7, 0xFF],
    [0x00, 0x00, 0x00, 0xFF],
    [0xC0, 0x00, 0x00, 0xFF],
    [0xFF, 0x00, 0x00, 0xFF],
    [0xFF, 0xC0, 0x00, 0xFF],
    [0xFF, 0xFF, 0x00, 0xFF],
    [0x92, 0xD0, 0x50, 0xFF],
    [0x00, 0xB0, 0x50, 0xFF],
    [0x00, 0x70, 0xC0, 0xFF],
    [0x70, 0x30, 0xA0, 0xFF],
];

/// The colour a palette entry stands for: `None` for the first ("none" or
/// "automatic"), else the palette's.
pub fn palette_color(index: usize) -> Option<Option<[u8; 4]>> {
    match index {
        0 => Some(None),
        i => PALETTE.get(i - 1).map(|c| Some(*c)),
    }
}

/// The number formats the field of a Sheets tab lists, in order: General,
/// Number, Currency, Percent, Date, Time, Text.
pub fn number_formats(strings: &Strings) -> Vec<String> {
    [
        strings.number_general,
        strings.number_number,
        strings.number_currency,
        strings.number_percent,
        strings.number_date,
        strings.number_time,
        strings.number_text,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Font sizes the size buttons step through, points.
pub const FONT_SIZES: [f32; 16] = [
    8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0, 36.0, 48.0, 72.0, 96.0,
];

/// The next size up or down from `size`.
pub fn font_size_step(size: f32, up: bool) -> f32 {
    if up {
        FONT_SIZES.into_iter().find(|s| *s > size).unwrap_or(size)
    } else {
        FONT_SIZES
            .into_iter()
            .rev()
            .find(|s| *s < size)
            .unwrap_or(size)
    }
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom_percent: 100,
            can_undo: false,
            can_redo: false,
            view_mode: ViewMode::Edit,
            heading: 0,
            styles: Styles::default(),
            cell: CellLook::default(),
            slide_text: false,
            slide_list: 0,
            has_slide: false,
            page: (0, 0),
            thumbnails: false,
        }
    }
}

impl ViewState {
    /// Whether the active cell has what a formatting command writes.
    fn cell_has(&self, command: Command) -> bool {
        let cell = &self.cell;
        match command {
            Command::Bold => cell.bold,
            Command::Italic => cell.italic,
            Command::Underline => cell.underline,
            Command::Strikethrough => cell.strike,
            Command::AlignLeft => cell.align == 1,
            Command::AlignCenter => cell.align == 2,
            Command::AlignRight => cell.align == 3,
            Command::WrapText => cell.wrap,
            Command::MergeCells => cell.merged,
            Command::FillColor => cell.filled,
            Command::Borders => cell.bordered,
            Command::BulletList => self.slide_list == 1,
            Command::NumberedList => self.slide_list == 2,
            _ => false,
        }
    }

    /// Whether the selection already has what a formatting command writes.
    fn has(&self, command: Command) -> bool {
        let styles = &self.styles;
        match command {
            Command::Bold => styles.bold,
            Command::Italic => styles.italic,
            Command::Strikethrough => styles.strike,
            Command::InlineCode => styles.code,
            Command::BulletList => styles.block == Some(Block::Bullet),
            Command::NumberedList => styles.block == Some(Block::Numbered),
            Command::Checklist => styles.block == Some(Block::Task),
            Command::Quote => styles.block == Some(Block::Quote),
            _ => false,
        }
    }

    /// Whether a command the view supports has something to act on right now.
    fn allows(&self, command: Command) -> bool {
        match command {
            Command::Undo => self.can_undo,
            Command::Redo => self.can_redo,
            // Formatting writes into the source, which must be on screen.
            command if command.formats_markdown() => self.view_mode.shows_source(),
            _ => true,
        }
    }

    /// The same for a Slides tab.
    fn allows_on_slides(&self, command: Command) -> bool {
        match command {
            command if command.formats_slide_text() => self.slide_text,
            Command::InsertTextBox
            | Command::InsertShape
            | Command::InsertImage
            | Command::Present => self.has_slide,
            _ => self.allows(command),
        }
    }
}

/// The size shown in the size field: whole numbers plain, nothing if mixed.
fn size_text(size: f32) -> String {
    if size <= 0.0 {
        String::new()
    } else if size.fract() == 0.0 {
        format!("{size:.0}")
    } else {
        format!("{size}")
    }
}

/// The line spacings the line spacing button lists, as multiples.
pub const LINE_SPACINGS: [f32; 6] = [1.0, 1.15, 1.5, 2.0, 2.5, 3.0];

fn line_spacings() -> Vec<String> {
    LINE_SPACINGS.iter().map(|v| format!("{v}")).collect()
}

/// The typefaces the font field of a Writer tab lists besides the one the
/// text has.
pub const FONT_FAMILIES: [&str; 14] = [
    "Calibri",
    "Arial",
    "Times New Roman",
    "Courier New",
    "Georgia",
    "Verdana",
    "Helvetica",
    "SimSun",
    "SimHei",
    "Microsoft YaHei",
    "KaiTi",
    "MS Mincho",
    "MS Gothic",
    "Noto Sans CJK",
];

/// The family name shown in the font field until real font handling exists.
/// It is the bundled interface and body face the plan names.
const DEFAULT_FONT: &str = "Noto Sans CJK";

struct Builder {
    kind: DocKind,
    view: ViewState,
    doc: DocLook,
    items: Vec<Item>,
}

impl Builder {
    fn push(&mut self, control: Control, command: Command) -> &mut Self {
        let allowed = if self.kind == DocKind::Slides {
            self.view.allows_on_slides(command)
        } else {
            self.view.allows(command)
        };
        let enabled = command.is_available(self.kind) && allowed;
        self.items.push(Item {
            control,
            command: Some(command),
            enabled,
            active: enabled
                && (ViewMode::of(command) == Some(self.view.view_mode)
                    || (self.kind == DocKind::Markdown && self.view.has(command))
                    || (matches!(self.kind, DocKind::Sheets | DocKind::Slides)
                        && self.view.cell_has(command))
                    || (command == Command::PageThumbnails && self.view.thumbnails)
                    || (self.kind == DocKind::Writer && self.doc.has(command))),
            options: Vec::new(),
        });
        self
    }

    fn icon(&mut self, icon: Icon, command: Command) -> &mut Self {
        self.push(Control::Button(icon), command)
    }

    fn letter(&mut self, text: &'static str, style: LetterStyle, command: Command) -> &mut Self {
        self.push(Control::Letter(text, style), command)
    }

    fn select(&mut self, value: &str, width: f32, command: Command) -> &mut Self {
        let value = value.to_owned();
        self.push(Control::Select { value, width }, command)
    }

    fn swatch(
        &mut self,
        icon: Option<Icon>,
        letter: &'static str,
        color: Option<[u8; 4]>,
        command: Command,
    ) -> &mut Self {
        self.push(
            Control::Swatch {
                icon,
                letter,
                color,
            },
            command,
        )
    }

    fn value(&mut self, text: String, width: f32, command: Command) -> &mut Self {
        self.push(Control::Value { text, width }, command)
    }

    fn layout(&mut self, control: Control) -> &mut Self {
        self.items.push(Item {
            control,
            command: None,
            enabled: false,
            active: false,
            options: Vec::new(),
        });
        self
    }

    fn sep(&mut self) -> &mut Self {
        self.layout(Control::Separator)
    }

    fn spacer(&mut self) -> &mut Self {
        self.layout(Control::Spacer)
    }

    fn history(&mut self) -> &mut Self {
        self.icon(Icon::Undo, Command::Undo)
            .icon(Icon::Redo, Command::Redo)
    }

    fn zoom(&mut self, view: ViewState) -> &mut Self {
        self.icon(Icon::Minus, Command::ZoomOut)
            .value(format!("{}%", view.zoom_percent), 48.0, Command::ZoomReset)
            .icon(Icon::Plus, Command::ZoomIn)
    }

    fn font(&mut self, size: &str) -> &mut Self {
        self.select(DEFAULT_FONT, 132.0, Command::FontFamily)
            .sep()
            .icon(Icon::Minus, Command::FontSizeDown)
            .value(size.to_owned(), 32.0, Command::FontSize)
            .icon(Icon::Plus, Command::FontSizeUp)
    }

    fn emphasis(&mut self) -> &mut Self {
        let plain = LetterStyle::default();
        let bold = LetterStyle {
            bold: true,
            ..plain
        };
        let italic = LetterStyle {
            italic: true,
            ..plain
        };
        let underline = LetterStyle {
            underline: true,
            ..plain
        };
        let strike = LetterStyle {
            strike: true,
            ..plain
        };
        self.letter("B", bold, Command::Bold)
            .letter("I", italic, Command::Italic)
            .letter("U", underline, Command::Underline)
            .letter("S", strike, Command::Strikethrough)
    }

    fn alignment(&mut self) -> &mut Self {
        self.icon(Icon::AlignLeft, Command::AlignLeft)
            .icon(Icon::AlignCenter, Command::AlignCenter)
            .icon(Icon::AlignRight, Command::AlignRight)
    }
}

/// Width of the font size field. A select field spends 8 + 4 on its
/// paddings, 2 on the gap and 14 on the chevron, which leaves 28 for the
/// text at 13: "22.5" and "10.5" take about 27.
const SIZE_FIELD_WIDTH: f32 = 56.0;

/// Body text, then heading levels 1 to 6, as the paragraph style field of
/// a Markdown tab lists them.
pub fn heading_names(strings: &Strings) -> Vec<String> {
    std::iter::once(strings.style_body.to_owned())
        .chain(
            (1..=6)
                .map(|level| crate::i18n::fill(strings.heading_level, "level", &level.to_string())),
        )
        .collect()
}

/// The toolbar for a document of this kind. `strings` supplies the values
/// shown in its fields.
pub fn toolbar(kind: DocKind, view: ViewState, strings: &Strings) -> Vec<Item> {
    toolbar_for(kind, view, &DocLook::default(), strings)
}

/// The toolbar of a Writer tab, showing how the text at the caret looks.
pub fn toolbar_for(kind: DocKind, view: ViewState, doc: &DocLook, strings: &Strings) -> Vec<Item> {
    let mut b = Builder {
        kind,
        view,
        doc: doc.clone(),
        items: Vec::new(),
    };
    let plain = LetterStyle::default();
    match kind {
        DocKind::Writer => {
            let doc = b.doc.clone();
            b.history()
                .icon(Icon::Print, Command::Print)
                .sep()
                .zoom(view)
                .sep()
                .select(
                    if doc.style.is_empty() {
                        strings.style_body
                    } else {
                        &doc.style
                    },
                    120.0,
                    Command::ParagraphStyle,
                )
                .sep();
            b.select(
                if doc.font.is_empty() {
                    DEFAULT_FONT
                } else {
                    &doc.font
                },
                132.0,
                Command::FontFamily,
            )
            .sep()
            .icon(Icon::Minus, Command::FontSizeDown)
            .select(&size_text(doc.size), SIZE_FIELD_WIDTH, Command::FontSize)
            .icon(Icon::Plus, Command::FontSizeUp)
            .sep()
            .emphasis()
            .swatch(None, "A", doc.color, Command::TextColor);
            if let Some(item) = b.items.last_mut() {
                item.options = vec![strings.font_auto.to_owned()];
            }
            b.swatch(Some(Icon::Highlight), "", doc.highlight, Command::Highlight);
            if let Some(item) = b.items.last_mut() {
                item.options = vec![strings.fill_none.to_owned()];
            }
            b.sep()
                .alignment()
                .icon(Icon::AlignJustify, Command::AlignJustify)
                .icon(Icon::IndentLess, Command::IndentLess)
                .icon(Icon::IndentMore, Command::IndentMore)
                .icon(Icon::LineSpacing, Command::LineSpacing);
            if let Some(item) = b.items.last_mut() {
                item.options = line_spacings();
            }
            b.sep()
                .icon(Icon::BulletList, Command::BulletList)
                .icon(Icon::NumberedList, Command::NumberedList)
                .sep()
                .icon(Icon::Link, Command::InsertLink)
                .icon(Icon::Image, Command::InsertImage)
                .icon(Icon::Table, Command::InsertTable)
                .icon(Icon::Comment, Command::InsertComment)
                .spacer()
                .icon(Icon::More, Command::More);
        }
        DocKind::Markdown => {
            let levels = heading_names(strings);
            b.history().sep().select(
                &levels[usize::from(view.heading.min(6))],
                92.0,
                Command::Heading,
            );
            if let Some(item) = b.items.last_mut() {
                item.options = levels;
            }
            b.sep()
                .emphasis()
                .icon(Icon::Code, Command::InlineCode)
                .sep()
                .icon(Icon::BulletList, Command::BulletList)
                .icon(Icon::NumberedList, Command::NumberedList)
                .icon(Icon::Checklist, Command::Checklist)
                .icon(Icon::Quote, Command::Quote)
                .sep()
                .icon(Icon::Link, Command::InsertLink)
                .icon(Icon::Image, Command::InsertImage)
                .icon(Icon::Table, Command::InsertTable)
                .spacer()
                .icon(Icon::Pen, Command::ViewSource)
                .icon(Icon::SplitView, Command::ViewSplit)
                .icon(Icon::Eye, Command::ViewPreview);
        }
        DocKind::Sheets => {
            b.history()
                .icon(Icon::Print, Command::Print)
                .sep()
                .zoom(view)
                .sep()
                .letter("¥", plain, Command::Currency)
                .letter("%", plain, Command::Percent);
            let formats = number_formats(strings);
            let shown = view
                .cell
                .format
                .and_then(|i| formats.get(i).cloned())
                .unwrap_or_else(|| strings.number_other.to_owned());
            b.select(&shown, 76.0, Command::NumberFormat);
            if let Some(item) = b.items.last_mut() {
                item.options = formats;
            }
            let size = view.cell.size;
            let size = if size.fract() == 0.0 {
                format!("{size:.0}")
            } else {
                format!("{size}")
            };
            b.sep()
                .font(&size)
                .sep()
                .emphasis()
                .swatch(
                    Some(Icon::FillColor),
                    "",
                    view.cell.fill,
                    Command::FillColor,
                )
                .swatch(None, "A", view.cell.font_color, Command::TextColor);
            let fill_index = b.items.len() - 2;
            b.items[fill_index].options = vec![strings.fill_none.to_owned()];
            if let Some(item) = b.items.last_mut() {
                item.options = vec![strings.font_auto.to_owned()];
            }
            b.icon(Icon::Borders, Command::Borders)
                .icon(Icon::MergeCells, Command::MergeCells)
                .sep()
                .alignment()
                .icon(Icon::WrapText, Command::WrapText)
                .sep()
                .icon(Icon::Sum, Command::AutoSum)
                .icon(Icon::Sort, Command::SortRange);
            if let Some(item) = b.items.last_mut() {
                item.options = vec![
                    strings.sort_ascending.to_owned(),
                    strings.sort_descending.to_owned(),
                ];
            }
            b.icon(Icon::Filter, Command::FilterRange)
                .icon(Icon::Chart, Command::InsertChart)
                .spacer()
                .icon(Icon::More, Command::More);
        }
        DocKind::Slides => {
            // New Slide lists the presentation's layouts and Shapes the
            // shapes it inserts; the shell fills those lists in.
            let size = view.cell.size;
            let size = if !view.slide_text {
                String::new()
            } else if size.fract() == 0.0 {
                format!("{size:.0}")
            } else {
                format!("{size}")
            };
            b.history()
                .sep()
                .icon(Icon::NewSlide, Command::NewSlide)
                .sep()
                .icon(Icon::TextBox, Command::InsertTextBox)
                .icon(Icon::Shapes, Command::InsertShape)
                .icon(Icon::Image, Command::InsertImage)
                .sep()
                .icon(Icon::Minus, Command::FontSizeDown)
                .value(size, 32.0, Command::FontSize)
                .icon(Icon::Plus, Command::FontSizeUp)
                .sep()
                .letter(
                    "B",
                    LetterStyle {
                        bold: true,
                        ..plain
                    },
                    Command::Bold,
                )
                .letter(
                    "I",
                    LetterStyle {
                        italic: true,
                        ..plain
                    },
                    Command::Italic,
                )
                .letter(
                    "U",
                    LetterStyle {
                        underline: true,
                        ..plain
                    },
                    Command::Underline,
                )
                .swatch(None, "A", view.cell.font_color, Command::TextColor);
            if let Some(item) = b.items.last_mut() {
                item.options = vec![strings.font_auto.to_owned()];
            }
            b.sep()
                .alignment()
                .sep()
                .icon(Icon::BulletList, Command::BulletList)
                .icon(Icon::NumberedList, Command::NumberedList)
                .spacer()
                .icon(Icon::Play, Command::Present);
        }
        DocKind::Pdf => {
            b.icon(Icon::Thumbnails, Command::PageThumbnails)
                .sep()
                .value(
                    format!("{} / {}", view.page.0.max(1), view.page.1.max(1)),
                    72.0,
                    Command::PageNumber,
                )
                .sep()
                .zoom(view)
                .icon(Icon::FitWidth, Command::FitWidth)
                .icon(Icon::Rotate, Command::RotatePage)
                .sep()
                .icon(Icon::Highlight, Command::Highlight)
                .icon(Icon::Pen, Command::Annotate)
                .icon(Icon::Comment, Command::InsertComment)
                .spacer()
                .icon(Icon::Search, Command::FindInDocument)
                .icon(Icon::Print, Command::Print);
        }
    }
    b.items
}

/// Zoom steps the zoom-in and zoom-out commands move between, in percent.
pub const ZOOM_STEPS: [u32; 11] = [50, 67, 80, 90, 100, 110, 125, 150, 175, 200, 300];

/// The zoom level one step up or down from the current one.
pub fn zoom_step(current: u32, zoom_in: bool) -> u32 {
    if zoom_in {
        ZOOM_STEPS
            .into_iter()
            .find(|step| *step > current)
            .unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1])
    } else {
        ZOOM_STEPS
            .into_iter()
            .rev()
            .find(|step| *step < current)
            .unwrap_or(ZOOM_STEPS[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{EN, ZH_HANS};

    fn all_toolbars() -> impl Iterator<Item = (DocKind, Vec<Item>)> {
        DocKind::ALL
            .into_iter()
            .map(|kind| (kind, toolbar(kind, ViewState::default(), &ZH_HANS)))
    }

    #[test]
    fn command_ids_round_trip() {
        for (index, command) in Command::ALL.iter().enumerate() {
            assert_eq!(command.id(), index as i32);
            assert_eq!(Command::from_id(index as i32), Some(*command));
        }
        assert_eq!(Command::from_id(-1), None);
        assert_eq!(Command::from_id(Command::ALL.len() as i32), None);
    }

    #[test]
    fn every_kind_has_a_toolbar_that_starts_with_a_control() {
        for (kind, items) in all_toolbars() {
            assert!(items.len() >= 8, "{kind:?} toolbar is nearly empty");
            assert!(items[0].command.is_some(), "{kind:?} starts with a gap");
            assert!(
                items[items.len() - 1].command.is_some(),
                "{kind:?} ends with a gap"
            );
        }
    }

    #[test]
    fn separators_never_touch() {
        for (kind, items) in all_toolbars() {
            for pair in items.windows(2) {
                let gap =
                    |item: &Item| matches!(item.control, Control::Separator | Control::Spacer);
                assert!(
                    !(gap(&pair[0]) && gap(&pair[1])),
                    "{kind:?} has two separators in a row"
                );
            }
            let spacers = items
                .iter()
                .filter(|item| item.control == Control::Spacer)
                .count();
            assert!(spacers <= 1, "{kind:?} has {spacers} spacers");
        }
    }

    #[test]
    fn a_control_is_enabled_only_if_its_command_is_available() {
        // With something to undo and redo, "available" is the whole story.
        let busy = ViewState {
            can_undo: true,
            can_redo: true,
            slide_text: true,
            has_slide: true,
            ..ViewState::default()
        };
        for kind in DocKind::ALL {
            for item in toolbar(kind, busy, &ZH_HANS) {
                match item.command {
                    Some(command) => {
                        assert_eq!(
                            item.enabled,
                            command.is_available(kind),
                            "{kind:?} {command:?}"
                        );
                    }
                    None => assert!(!item.enabled),
                }
            }
        }
    }

    #[test]
    fn the_current_view_is_shown_pressed() {
        for mode in [ViewMode::Edit, ViewMode::Split, ViewMode::Preview] {
            let view = ViewState {
                view_mode: mode,
                ..ViewState::default()
            };
            let active: Vec<_> = toolbar(DocKind::Markdown, view, &ZH_HANS)
                .into_iter()
                .filter(|item| item.active)
                .filter_map(|item| item.command)
                .collect();
            assert_eq!(active, [mode.command()]);
            assert_eq!(ViewMode::of(mode.command()), Some(mode));
        }
        // Nothing is pressed where the views do not exist.
        for kind in DocKind::ALL {
            if kind != DocKind::Markdown {
                assert!(
                    toolbar(kind, ViewState::default(), &ZH_HANS)
                        .iter()
                        .all(|item| !item.active)
                );
            }
        }
        assert!(ViewMode::Split.shows_source() && ViewMode::Split.shows_preview());
        assert!(!ViewMode::Edit.shows_preview() && !ViewMode::Preview.shows_source());
    }

    #[test]
    fn what_works_today_is_writer_zoom_and_markdown_editing() {
        let busy = ViewState {
            can_undo: true,
            can_redo: true,
            slide_text: true,
            has_slide: true,
            ..ViewState::default()
        };
        for kind in DocKind::ALL {
            let enabled: Vec<_> = toolbar(kind, busy, &ZH_HANS)
                .into_iter()
                .filter(|item| item.enabled)
                .filter_map(|item| item.command)
                .collect();
            match kind {
                DocKind::Writer => assert_eq!(
                    enabled,
                    [
                        Command::Undo,
                        Command::Redo,
                        Command::ZoomOut,
                        Command::ZoomReset,
                        Command::ZoomIn,
                        Command::ParagraphStyle,
                        Command::FontFamily,
                        Command::FontSizeDown,
                        Command::FontSize,
                        Command::FontSizeUp,
                        Command::Bold,
                        Command::Italic,
                        Command::Underline,
                        Command::Strikethrough,
                        Command::TextColor,
                        Command::Highlight,
                        Command::AlignLeft,
                        Command::AlignCenter,
                        Command::AlignRight,
                        Command::AlignJustify,
                        Command::IndentLess,
                        Command::IndentMore,
                        Command::LineSpacing,
                        Command::BulletList,
                        Command::NumberedList,
                        Command::InsertLink,
                        Command::InsertImage,
                        Command::InsertTable,
                    ]
                ),
                DocKind::Markdown => assert_eq!(
                    enabled,
                    [
                        Command::Undo,
                        Command::Redo,
                        Command::Heading,
                        Command::Bold,
                        Command::Italic,
                        Command::Strikethrough,
                        Command::InlineCode,
                        Command::BulletList,
                        Command::NumberedList,
                        Command::Checklist,
                        Command::Quote,
                        Command::InsertLink,
                        Command::InsertImage,
                        Command::InsertTable,
                        Command::ViewSource,
                        Command::ViewSplit,
                        Command::ViewPreview
                    ]
                ),
                DocKind::Sheets => assert_eq!(
                    enabled,
                    [
                        Command::Undo,
                        Command::Redo,
                        Command::Currency,
                        Command::Percent,
                        Command::NumberFormat,
                        Command::FontSizeDown,
                        Command::FontSize,
                        Command::FontSizeUp,
                        Command::Bold,
                        Command::Italic,
                        Command::Underline,
                        Command::Strikethrough,
                        Command::FillColor,
                        Command::TextColor,
                        Command::Borders,
                        Command::MergeCells,
                        Command::AlignLeft,
                        Command::AlignCenter,
                        Command::AlignRight,
                        Command::WrapText,
                        Command::AutoSum,
                        Command::SortRange
                    ]
                ),
                DocKind::Slides => assert_eq!(
                    enabled,
                    [
                        Command::Undo,
                        Command::Redo,
                        Command::NewSlide,
                        Command::InsertTextBox,
                        Command::InsertShape,
                        Command::InsertImage,
                        Command::FontSizeDown,
                        Command::FontSize,
                        Command::FontSizeUp,
                        Command::Bold,
                        Command::Italic,
                        Command::Underline,
                        Command::TextColor,
                        Command::AlignLeft,
                        Command::AlignCenter,
                        Command::AlignRight,
                        Command::BulletList,
                        Command::NumberedList,
                        Command::Present
                    ]
                ),
                DocKind::Pdf => assert_eq!(
                    enabled,
                    [
                        Command::PageThumbnails,
                        Command::PageNumber,
                        Command::ZoomOut,
                        Command::ZoomReset,
                        Command::ZoomIn,
                        Command::FitWidth
                    ]
                ),
            }
        }
    }

    #[test]
    fn markdown_formatting_needs_the_source_and_has_no_underline() {
        let items = |view: ViewState| toolbar(DocKind::Markdown, view, &ZH_HANS);
        let enabled = |view: ViewState, command: Command| {
            items(view)
                .into_iter()
                .find(|item| item.command == Some(command))
                .is_some_and(|item| item.enabled)
        };
        let source = ViewState::default();
        let preview = ViewState {
            view_mode: ViewMode::Preview,
            ..ViewState::default()
        };
        let split = ViewState {
            view_mode: ViewMode::Split,
            ..ViewState::default()
        };
        assert!(enabled(source, Command::Bold) && enabled(split, Command::Bold));
        assert!(!enabled(preview, Command::Bold));
        assert!(!enabled(preview, Command::Heading));
        assert!(enabled(preview, Command::ViewSource));
        assert!(!enabled(source, Command::Underline));
        // The style field shows the level of the caret's line and offers all.
        let heading = |view: ViewState| {
            items(view)
                .into_iter()
                .find(|item| item.command == Some(Command::Heading))
                .expect("a style field")
        };
        let body = heading(source);
        assert_eq!(
            body.control,
            Control::Select {
                value: "正文".into(),
                width: 92.0
            }
        );
        assert_eq!(body.options.len(), 7);
        let two = heading(ViewState {
            heading: 2,
            ..ViewState::default()
        });
        assert_eq!(
            two.control,
            Control::Select {
                value: "标题 2".into(),
                width: 92.0
            }
        );
    }

    #[test]
    fn format_buttons_show_the_styles_of_the_selection() {
        let pressed = |kind: DocKind, view: ViewState| {
            toolbar(kind, view, &ZH_HANS)
                .into_iter()
                .filter(|item| item.active)
                .filter_map(|item| item.command)
                .collect::<Vec<_>>()
        };
        let view = ViewState {
            styles: Styles {
                bold: true,
                code: true,
                block: Some(Block::Task),
                ..Styles::default()
            },
            ..ViewState::default()
        };
        assert_eq!(
            pressed(DocKind::Markdown, view),
            [
                Command::Bold,
                Command::InlineCode,
                Command::Checklist,
                Command::ViewSource
            ]
        );
        // Nothing is pressed while the buttons cannot be used.
        let preview = ViewState {
            view_mode: ViewMode::Preview,
            ..view
        };
        assert_eq!(pressed(DocKind::Markdown, preview), [Command::ViewPreview]);
        // Other kinds of tab do not read Markdown styles.
        assert!(!pressed(DocKind::Writer, view).contains(&Command::Bold));
    }

    #[test]
    fn undo_and_redo_follow_the_document() {
        let enabled = |view: ViewState, command: Command| {
            toolbar(DocKind::Markdown, view, &ZH_HANS)
                .into_iter()
                .find(|item| item.command == Some(command))
                .is_some_and(|item| item.enabled)
        };
        let fresh = ViewState::default();
        assert!(!enabled(fresh, Command::Undo));
        assert!(!enabled(fresh, Command::Redo));
        let edited = ViewState {
            can_undo: true,
            ..fresh
        };
        assert!(enabled(edited, Command::Undo));
        assert!(!enabled(edited, Command::Redo));
        // So does a Writer tab.
        let writer_undo = |view: ViewState| {
            toolbar(DocKind::Writer, view, &ZH_HANS)
                .into_iter()
                .find(|item| item.command == Some(Command::Undo))
                .expect("Writer shows an undo button")
                .enabled
        };
        assert!(writer_undo(edited));
        assert!(!writer_undo(fresh));
    }

    #[test]
    fn a_command_appears_once_per_toolbar() {
        for (kind, items) in all_toolbars() {
            let mut seen = std::collections::HashSet::new();
            for command in items.iter().filter_map(|item| item.command) {
                assert!(seen.insert(command), "{kind:?} repeats {command:?}");
            }
        }
    }

    #[test]
    fn the_zoom_readout_follows_the_view() {
        let view = ViewState {
            zoom_percent: 125,
            ..ViewState::default()
        };
        let items = toolbar(DocKind::Writer, view, &ZH_HANS);
        let readout = items
            .iter()
            .find(|item| item.command == Some(Command::ZoomReset))
            .expect("Writer has a zoom readout");
        assert!(
            matches!(&readout.control, Control::Value { text, .. } if text == "125%"),
            "{:?}",
            readout.control
        );
    }

    #[test]
    fn field_values_follow_the_language() {
        let value = |strings: &Strings| {
            toolbar(DocKind::Writer, ViewState::default(), strings)
                .into_iter()
                .find(|item| item.command == Some(Command::ParagraphStyle))
                .map(|item| item.control)
        };
        assert!(matches!(value(&ZH_HANS), Some(Control::Select { value, .. }) if value == "正文"));
        assert!(matches!(value(&EN), Some(Control::Select { value, .. }) if value == "Body"));
    }

    #[test]
    fn zoom_steps_walk_the_scale_and_stop_at_the_ends() {
        assert_eq!(zoom_step(100, true), 110);
        assert_eq!(zoom_step(100, false), 90);
        // From a value between two steps.
        assert_eq!(zoom_step(95, true), 100);
        assert_eq!(zoom_step(95, false), 90);
        assert_eq!(zoom_step(300, true), 300);
        assert_eq!(zoom_step(50, false), 50);
        assert!(ZOOM_STEPS.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
