//! A document paragraph turned into shaped text: its runs become one string
//! with spans, tabs, objects and decorations, and `aster-text` lays that out.
//!
//! The shaped string is not always the paragraph's text: hidden runs are
//! left out, a page number shows its real value, a note reference shows its
//! number, a list label comes first. [`Shaped`] keeps the map between the
//! two so that positions in the document and in the shaped text translate
//! both ways.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use aster_text::{
    Above, Align as TextAlign, Band, Decor, Extra, Flow, InlineObject, LineRule, Natural, Rules,
    Scaling, Shaper, Slot, Spacing, Span, Spec, Stroke, Style, TabAlign, TabStop,
};
use aster_writer::{
    Align, Break, Drawing, Inline, LineSpacing, PageField, Paragraph, RunLook, Settings, Suffix,
    TWIPS_PER_POINT, TabAlign as WTabAlign, TabLeader, UnderlineStyle, VertAlign,
};

use crate::display::Rect;
use crate::fonts::Families;

/// Points per EMU.
pub(crate) const PT_PER_EMU: f32 = 1.0 / 12_700.0;

pub(crate) fn pt(twips: i32) -> f32 {
    twips as f32 / TWIPS_PER_POINT as f32
}

pub(crate) fn emu_pt(emu: i64) -> f32 {
    emu as f32 * PT_PER_EMU
}

/// Numbers notes show, from the order of their references.
#[derive(Default)]
pub(crate) struct NoteNumbers {
    pub foot: HashMap<i32, u32>,
    pub end: HashMap<i32, u32>,
}

/// The page-number fields' values where there is a page to ask.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct PageValues {
    pub page: String,
    pub count: String,
    pub section_pages: String,
}

/// What shaping a paragraph depends on besides the paragraph.
pub(crate) struct Env<'a> {
    pub settings: &'a Settings,
    /// The section's grid line pitch, points.
    pub grid: Option<f32>,
    pub notes: &'a NoteNumbers,
    pub page: Option<&'a PageValues>,
    /// Tallest an object in a line may be, points.
    pub max_object: f32,
    /// What text is on, where the paragraph itself has no shading (a cell's).
    pub background: Option<[u8; 4]>,
    /// Places text flows around, from the paragraph's top-left corner
    /// (floating drawings that text wraps beside).
    pub obstacles: &'a [Rect],
    /// How far down from the paragraph's top the page has room for a line
    /// (a line that would end below it is moved to the next page, where the
    /// obstacles are not): infinite when there is no such limit.
    pub limit: f32,
}

#[derive(Clone, Copy, Debug)]
struct Piece {
    orig: (usize, usize),
    shaped: (usize, usize),
    /// The shaped text is the original, byte for byte.
    copy: bool,
}

/// A paragraph laid out at one width.
pub(crate) struct Shaped {
    pub paragraph: Arc<aster_text::Paragraph>,
    pieces: Vec<Piece>,
    /// Bytes of the shaped text before the paragraph's own text (the list
    /// label and its suffix).
    prefix: usize,
    orig_len: usize,
    /// Width of the text area the lines were broken for.
    pub width: f32,
    /// Inline drawings: where in the shaped text, and what.
    pub objects: Vec<(usize, Arc<Drawing>)>,
    /// Floating drawings: the place they are anchored at.
    pub floats: Vec<(usize, Arc<Drawing>)>,
    /// Note references: the place and the note's id.
    pub notes: Vec<(usize, i32, bool)>,
    /// Page and column breaks: the place of the line break that stands for it.
    pub breaks: Vec<(usize, Break)>,
    /// Backgrounds behind text: shaded ranges and highlighted ranges.
    pub backs: Vec<(Range<usize>, [u8; 4])>,
    /// The text has tabs (and so maybe leaders).
    pub has_tabs: bool,
    /// Readings above ranges of the text.
    pub rubies: Vec<RubyMark>,
}

/// A reading drawn above a range of shaped text.
pub(crate) struct RubyMark {
    pub range: Range<usize>,
    pub text: String,
    pub style: Style,
}

impl Shaped {
    pub fn lines(&self) -> &[aster_text::LineBox] {
        self.paragraph.lines()
    }

    /// The shaped offset of a document offset.
    pub fn to_shaped(&self, offset: usize) -> usize {
        let offset = offset.min(self.orig_len);
        let index = self.pieces.partition_point(|p| p.orig.1 < offset);
        let Some(piece) = self.pieces.get(index) else {
            return self.pieces.last().map_or(self.prefix, |p| p.shaped.1);
        };
        if offset <= piece.orig.0 {
            return piece.shaped.0;
        }
        if piece.copy {
            piece.shaped.0 + (offset - piece.orig.0)
        } else {
            piece.shaped.1
        }
    }

    /// The document offset of a shaped offset.
    pub fn to_orig(&self, shaped: usize) -> usize {
        if shaped <= self.prefix {
            return 0;
        }
        let index = self.pieces.partition_point(|p| p.shaped.1 <= shaped);
        let Some(piece) = self.pieces.get(index) else {
            return self.orig_len;
        };
        if piece.copy {
            piece.orig.0 + (shaped - piece.shaped.0)
        } else if shaped <= piece.shaped.0 {
            piece.orig.0
        } else {
            piece.orig.1
        }
    }
}

/// What a run that is a field or a note reference shows, if it is one.
fn replacement(run: &aster_writer::Run, env: &Env<'_>) -> Option<String> {
    match &run.kind {
        Inline::Footnote(id) => Some(
            env.notes
                .foot
                .get(id)
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
        ),
        Inline::Endnote(id) => Some(
            env.notes
                .end
                .get(id)
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
        ),
        Inline::PageNumber(field) => env.page.map(|values| {
            match field {
                PageField::Page => &values.page,
                PageField::PageCount => &values.count,
                PageField::SectionPages => &values.section_pages,
            }
            .clone()
        }),
        _ => None,
    }
}

/// Everything besides the paragraph and its width that decides how it is
/// shaped, as a number to key the cache on.
pub(crate) fn context(para: &Paragraph, env: &Env<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    env.settings.default_tab.hash(&mut hasher);
    env.settings.compress_punctuation.hash(&mut hasher);
    env.settings.east_asian_language.hash(&mut hasher);
    env.settings.language.hash(&mut hasher);
    env.grid.map(f32::to_bits).hash(&mut hasher);
    env.max_object.to_bits().hash(&mut hasher);
    env.background.hash(&mut hasher);
    for o in env.obstacles {
        for v in [o.x0, o.y0, o.x1, o.y1] {
            ((v * 16.0).round() as i32).hash(&mut hasher);
        }
    }
    if !env.obstacles.is_empty() && env.limit.is_finite() {
        ((env.limit * 16.0).round() as i32).hash(&mut hasher);
    }
    for run in &para.runs {
        if let Some(text) = replacement(run, env) {
            text.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn upper_same_length(c: char) -> Option<char> {
    let mut upper = c.to_uppercase();
    let first = upper.next()?;
    (upper.next().is_none() && first.len_utf8() == c.len_utf8() && first != c).then_some(first)
}

fn stroke_of(style: UnderlineStyle) -> Stroke {
    match style {
        UnderlineStyle::Double => Stroke::Double,
        UnderlineStyle::Thick => Stroke::Thick,
        UnderlineStyle::Dotted => Stroke::Dotted,
        UnderlineStyle::Dashed => Stroke::Dashed,
        UnderlineStyle::Wave => Stroke::Wave,
        _ => Stroke::Single,
    }
}

/// Automatic colour: black, or white on a dark background.
fn automatic(look: &RunLook, background: Option<[u8; 4]>) -> [u8; 4] {
    let back = look.highlight.or(look.shading).or(background);
    match back {
        Some([r, g, b, a])
            if a > 0
                && (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) < 100.0 =>
        {
            [255, 255, 255, 255]
        }
        _ => [0, 0, 0, 255],
    }
}

fn text_style(
    families: &mut Families,
    look: &RunLook,
    small: bool,
    background: Option<[u8; 4]>,
) -> Style {
    let mut size = look.size.max(1.0);
    let mut rise = look.raise;
    match look.position {
        VertAlign::Baseline => {}
        VertAlign::Superscript => {
            rise += size * 0.33;
            size *= 0.65;
        }
        VertAlign::Subscript => {
            rise -= size * 0.12;
            size *= 0.65;
        }
    }
    if small {
        size *= 0.8;
    }
    Style {
        family: families.get(look),
        size: size.max(1.0),
        color: look.color.unwrap_or_else(|| automatic(look, background)),
        bold: look.bold,
        italic: look.italic,
        underline: false,
        strike: false,
        rise,
    }
}

fn leader_of(leader: TabLeader) -> aster_text::Leader {
    match leader {
        TabLeader::None => aster_text::Leader::None,
        TabLeader::Dots => aster_text::Leader::Dots,
        TabLeader::Hyphens => aster_text::Leader::Hyphens,
        TabLeader::Underscore => aster_text::Leader::Underscore,
        TabLeader::MiddleDots => aster_text::Leader::MiddleDots,
    }
}

/// Builds the shaped string of a paragraph.
struct Builder<'a, 'e> {
    text: String,
    spans: Vec<Span>,
    pieces: Vec<Piece>,
    decor: Vec<Decor>,
    natural: Vec<Natural>,
    above: Vec<Above>,
    scaling: Vec<Scaling>,
    rubies: Vec<RubyMark>,
    background: Option<[u8; 4]>,
    spacing: Vec<Spacing>,
    objects: Vec<InlineObject>,
    drawings: Vec<(usize, Arc<Drawing>)>,
    floats: Vec<(usize, Arc<Drawing>)>,
    notes: Vec<(usize, i32, bool)>,
    breaks: Vec<(usize, Break)>,
    backs: Vec<(Range<usize>, [u8; 4])>,
    families: &'a mut Families,
    env: &'a Env<'e>,
}

impl Builder<'_, '_> {
    fn span(&mut self, range: Range<usize>, style: Style) {
        if range.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == range.start
            && last.style == style
        {
            last.range.end = range.end;
            return;
        }
        self.spans.push(Span { range, style });
    }

    fn decorate(&mut self, range: Range<usize>, look: &RunLook) {
        if range.is_empty() {
            return;
        }
        let strike = if look.double_strike {
            Stroke::Double
        } else if look.strike {
            Stroke::Single
        } else {
            Stroke::None
        };
        let (underline, words_only, color) = match look.underline {
            Some(u) => (
                stroke_of(u.style),
                u.style == UnderlineStyle::Words,
                u.color,
            ),
            None => (Stroke::None, false, None),
        };
        if underline == Stroke::None && strike == Stroke::None {
            return;
        }
        let decor = Decor {
            range,
            underline,
            words_only,
            strike,
            color,
        };
        self.decor.push(decor);
    }

    fn add_run(&mut self, run: &aster_writer::Run, orig: Range<usize>) {
        let look = &run.look;
        let start = self.text.len();
        let hidden = look.hidden;
        let mut copy = false;
        let mut small_runs: Vec<(Range<usize>, bool)> = Vec::new();
        if !hidden {
            match &run.kind {
                Inline::Text | Inline::Ruby(_) => {
                    copy = true;
                    if look.small_caps {
                        for c in run.text.chars() {
                            let (shown, small) = match upper_same_length(c) {
                                Some(upper) if c.is_lowercase() => (upper, true),
                                _ => (c, false),
                            };
                            let at = self.text.len();
                            self.text.push(shown);
                            match small_runs.last_mut() {
                                Some((range, kind)) if *kind == small => {
                                    range.end = self.text.len()
                                }
                                _ => small_runs.push((at..self.text.len(), small)),
                            }
                        }
                    } else if look.caps {
                        self.text
                            .extend(run.text.chars().map(|c| upper_same_length(c).unwrap_or(c)));
                    } else {
                        self.text.push_str(&run.text);
                    }
                }
                Inline::Tab => {
                    copy = true;
                    self.text.push('\t');
                }
                Inline::Break(Break::Line) => {
                    copy = true;
                    self.text.push('\n');
                }
                Inline::Break(kind) => {
                    self.breaks.push((self.text.len(), *kind));
                    self.text.push('\n');
                }
                Inline::Drawing(drawing) => match &drawing.float {
                    None => {
                        let (mut w, mut h) = (
                            emu_pt(drawing.size.0).max(0.0),
                            emu_pt(drawing.size.1).max(0.0),
                        );
                        let limit = self.env.max_object.max(1.0);
                        if h > limit {
                            w *= limit / h;
                            h = limit;
                        }
                        self.objects.push(InlineObject {
                            offset: self.text.len(),
                            width: w,
                            height: h,
                        });
                        self.drawings.push((self.text.len(), drawing.clone()));
                        self.text.push('\u{fffc}');
                    }
                    Some(_) => self.floats.push((self.text.len(), drawing.clone())),
                },
                Inline::Footnote(id) | Inline::Endnote(id) => {
                    let end = matches!(run.kind, Inline::Endnote(_));
                    self.notes.push((self.text.len(), *id, end));
                    let shown = replacement(run, self.env).unwrap_or_default();
                    self.text.push_str(&shown);
                }
                Inline::PageNumber(_) => {
                    let shown = replacement(run, self.env).unwrap_or_else(|| run.text.clone());
                    self.text.push_str(&shown);
                }
                Inline::Marker => {}
            }
        }
        let end = self.text.len();
        self.pieces.push(Piece {
            orig: (orig.start, orig.end),
            shaped: (start, end),
            copy: copy && end - start == orig.len(),
        });
        if small_runs.is_empty() {
            let style = text_style(self.families, look, false, self.background);
            self.span(start..end, style);
        } else {
            for (range, small) in small_runs {
                let style = text_style(self.families, look, small, self.background);
                self.span(range, style);
            }
        }
        self.decorate(start..end, look);
        if let Inline::Ruby(ruby) = &run.kind
            && end > start
            && !hidden
        {
            let mut style = text_style(self.families, look, false, self.background);
            style.size = ruby.size.max(1.0);
            style.rise = 0.0;
            self.above.push(Above {
                range: start..end,
                height: ruby.size.max(1.0) * 1.2,
            });
            self.rubies.push(RubyMark {
                range: start..end,
                text: ruby.text.clone(),
                style,
            });
        }
        if (look.scale - 1.0).abs() > 0.001 && end > start {
            self.scaling.push(Scaling {
                range: start..end,
                factor: look.scale,
            });
        }
        if let Some(ratio) = self.families.ratio(look, &self.text[start..end])
            && end > start
        {
            self.natural.push(Natural {
                range: start..end,
                ratio,
            });
        }
        if look.spacing != 0 && end > start {
            self.spacing.push(Spacing {
                range: start..end,
                amount: pt(look.spacing),
            });
        }
        if let Some(color) = look.shading {
            self.backs.push((start..end, color));
        }
        if let Some(color) = look.highlight {
            self.backs.push((start..end, color));
        }
    }
}

/// Lay out `para` in a text area `width` points wide (the container's width
/// less the indents).
pub(crate) fn shape(
    shaper: &mut Shaper,
    families: &mut Families,
    para: &Paragraph,
    width: f32,
    env: &Env<'_>,
) -> Shaped {
    for run in &para.runs {
        families.ensure(shaper, &run.look);
    }
    families.ensure(shaper, &para.base);
    families.ensure(shaper, &para.mark);
    if let Some(label) = &para.label {
        families.ensure(shaper, &label.look);
    }
    let mut builder = Builder {
        text: String::new(),
        spans: Vec::new(),
        pieces: Vec::new(),
        decor: Vec::new(),
        natural: Vec::new(),
        above: Vec::new(),
        scaling: Vec::new(),
        rubies: Vec::new(),
        background: para.props.shading.or(env.background),
        spacing: Vec::new(),
        objects: Vec::new(),
        drawings: Vec::new(),
        floats: Vec::new(),
        notes: Vec::new(),
        breaks: Vec::new(),
        backs: Vec::new(),
        families,
        env,
    };
    let props = &para.props;
    // The list label comes first, then what separates it from the text.
    if let Some(label) = &para.label {
        builder.text.push_str(&label.text);
        let suffix = match label.suffix {
            Suffix::Tab => "\t",
            Suffix::Space => " ",
            Suffix::Nothing => "",
        };
        let label_end = builder.text.len();
        builder.text.push_str(suffix);
        let style = text_style(builder.families, &label.look, false, builder.background);
        builder.span(0..builder.text.len(), style);
        builder.decorate(0..label_end, &label.look);
        if let Some(ratio) = builder.families.ratio(&label.look, &builder.text) {
            let end = builder.text.len();
            builder.natural.push(Natural {
                range: 0..end,
                ratio,
            });
        }
    }
    let prefix = builder.text.len();
    let mut at = 0;
    for run in &para.runs {
        let end = at + run.text.len();
        builder.add_run(run, at..end);
        at = end;
    }
    let orig_len = at;

    let base_look = if para.runs.iter().all(|run| run.text.is_empty()) && para.label.is_none() {
        &para.mark
    } else {
        &para.base
    };
    let builder_base_natural = builder.families.ratio(base_look, "");
    let base = text_style(builder.families, base_look, false, builder.background);
    let text = std::mem::take(&mut builder.text);
    let has_tabs = text.contains('\t');

    let align = match props.align {
        Align::Left | Align::Justify | Align::Distribute => TextAlign::Start,
        Align::Center => TextAlign::Center,
        Align::Right => TextAlign::End,
    };
    let fallback = env
        .settings
        .east_asian_language
        .as_deref()
        .unwrap_or("zh-Hans");
    let has_kana_or_han = text.chars().any(|c| (c as u32) >= 0x2e80);
    let default_language = if has_kana_or_han {
        fallback
    } else {
        env.settings.language.as_deref().unwrap_or("en")
    };
    let language = aster_text::segment::guess_language(&text, default_language);
    let mut rules = Rules::for_language(language);
    rules.compress = rules.compress && env.settings.compress_punctuation;
    rules.justify = matches!(props.align, Align::Justify | Align::Distribute);

    let mut stops: Vec<TabStop> = props
        .tabs
        .iter()
        .filter(|stop| stop.align != WTabAlign::Bar)
        .map(|stop| TabStop {
            position: pt(stop.position),
            align: match stop.align {
                WTabAlign::Center => TabAlign::Center,
                WTabAlign::Right => TabAlign::Right,
                WTabAlign::Decimal => TabAlign::Decimal,
                _ => TabAlign::Left,
            },
            leader: leader_of(stop.leader),
        })
        .collect();
    if props.first_line < 0 && props.indent_left > 0 {
        // A hanging indent is a tab stop: the text after a label lines up.
        let hang = pt(props.indent_left);
        if !stops.iter().any(|stop| (stop.position - hang).abs() < 0.05) {
            stops.push(TabStop {
                position: hang,
                align: TabAlign::Left,
                leader: aster_text::Leader::None,
            });
        }
    }
    stops.sort_by(|a, b| a.position.total_cmp(&b.position));

    let grid = if props.snap_to_grid { env.grid } else { None };
    let line = match props.line {
        _ if fully_hidden(para) => LineRule::Exactly(0.0),
        LineSpacing::Multiple(m) => LineRule::Multiple(m.max(0.0)),
        LineSpacing::AtLeast(v) => LineRule::AtLeast(pt(v)),
        LineSpacing::Exactly(v) => LineRule::Exactly(pt(v)),
    };
    let grid_fit = grid;
    let base_natural = builder_base_natural;
    let mark = (para.mark != *base_look && !text.is_empty()).then(|| aster_text::Mark {
        style: text_style(builder.families, &para.mark, false, builder.background),
        natural: builder.families.ratio(&para.mark, ""),
    });
    let text_width = width.max(1.0);
    let spec = Spec {
        text: &text,
        base,
        spans: &builder.spans,
        language,
        width: Some(text_width),
        scale: 1.0,
        line_height: 1.0,
        align,
        rules,
    };
    let extra = Extra {
        first_line: pt(props.first_line),
        tabs: &stops,
        margin_offset: pt(props.indent_left),
        default_tab: pt(env.settings.default_tab).max(0.0),
        objects: &builder.objects,
        spacing: &builder.spacing,
        decor: &builder.decor,
        line,
        natural: &builder.natural,
        above: &builder.above,
        scaling: &builder.scaling,
        base_natural,
        mark,
        grid: grid_fit,
        distribute: props.align == Align::Distribute,
        snap: false,
        trailing_line: true,
        flow: None,
    };
    let around = |band: Band| free_stretch(env.obstacles, env.limit, band);
    let extra = Extra {
        flow: (!env.obstacles.is_empty()).then_some(Flow(&around)),
        ..extra
    };
    let paragraph = shaper.lay_out_with(&spec, &extra);
    Shaped {
        paragraph: Arc::new(paragraph),
        pieces: builder.pieces,
        prefix,
        orig_len,
        width: text_width,
        objects: builder.drawings,
        floats: builder.floats,
        notes: builder.notes,
        breaks: builder.breaks,
        backs: builder.backs,
        has_tabs,
        rubies: builder.rubies,
    }
}

/// A paragraph Word does not show at all: its mark is hidden and so is
/// everything in it. It takes no room, spaces included.
pub(crate) fn fully_hidden(para: &Paragraph) -> bool {
    para.mark.hidden
        && para.runs.iter().all(|run| {
            run.look.hidden
                || (matches!(run.kind, Inline::Text | Inline::Marker) && run.text.is_empty())
        })
}

/// The widest stretch of a band that none of `obstacles` covers, and where
/// to go to get below them (they are rectangles from the paragraph's
/// top-left corner). With nothing in the way it is the band itself. A band
/// that reaches below `limit` is for a line that goes to the next page, and
/// nothing on this page is in the way of that.
pub(crate) fn free_stretch(obstacles: &[Rect], limit: f32, band: Band) -> Slot {
    const EPS: f32 = 0.01;
    let bottom = band.top + band.height;
    if bottom > limit + EPS {
        return Slot {
            left: band.left,
            right: band.right,
            below: band.top,
        };
    }
    let mut free = vec![(band.left, band.right)];
    let mut below = band.top;
    for o in obstacles {
        if o.y0 >= bottom - EPS || o.y1 <= band.top + EPS {
            continue;
        }
        below = below.max(o.y1);
        let mut next = Vec::with_capacity(free.len() + 1);
        for (a, b) in free {
            if o.x1 <= a || o.x0 >= b {
                next.push((a, b));
                continue;
            }
            if o.x0 > a {
                next.push((a, o.x0));
            }
            if o.x1 < b {
                next.push((o.x1, b));
            }
        }
        free = next;
    }
    // The wider side of what is left; the left one when they are equal.
    let best = free
        .into_iter()
        .fold(None, |best: Option<(f32, f32)>, (a, b)| match best {
            Some((x, y)) if y - x >= b - a => Some((x, y)),
            _ => Some((a, b)),
        });
    match best {
        Some((left, right)) => Slot { left, right, below },
        None => Slot {
            left: band.left,
            right: band.left,
            below,
        },
    }
}

/// The text of a paragraph for the outline: without objects, trimmed.
pub(crate) fn outline_text(para: &Paragraph) -> String {
    let text: String = para
        .runs
        .iter()
        .filter(|run| !run.look.hidden && matches!(run.kind, Inline::Text | Inline::Tab))
        .map(|run| run.text.replace('\t', " "))
        .collect();
    text.trim().to_owned()
}
