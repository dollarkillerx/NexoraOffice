//! The layout context: shaping with its cache, vertical stacks of blocks
//! (the inside of a cell, a header, a text box, a note) and putting them
//! into display items.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use aster_text::Shaper;
use aster_writer::{
    Block, Border, BorderStyle, Document, Drawing, DrawingContent, Float, FloatAt, FloatFrom,
    Paragraph, Path, Wrap,
};

use crate::display::{FrameItem, Item, LeaderItem, PictureItem, Rect, Rgba, RuleStyle, TextItem};
use crate::fonts::Families;
use crate::shape::{
    Env, NoteNumbers, PageValues, Shaped, context, emu_pt, fully_hidden, pt, shape,
};
use crate::table::TableLayout;

/// The paint order of a page: shading under everything, then drawings
/// behind the text, text and pictures, borders and rules, and drawings in
/// front of the text.
#[derive(Default)]
pub(crate) struct Layers {
    pub shade: Vec<Item>,
    pub behind: Vec<Item>,
    pub body: Vec<Item>,
    pub rules: Vec<Item>,
    pub front: Vec<Item>,
}

impl Layers {
    pub fn into_items(self) -> Vec<Item> {
        let mut items = self.shade;
        items.extend(self.behind);
        items.extend(self.body);
        items.extend(self.rules);
        items.extend(self.front);
        items
    }
}

/// A paragraph, or some lines of it, at a place on a page: what queries
/// look at.
pub(crate) struct Frag {
    pub path: Path,
    pub shaped: Arc<Shaped>,
    pub page: usize,
    /// Where the paragraph's origin is on the page.
    pub x: f32,
    pub y: f32,
    pub lines: Range<usize>,
    /// The area a click belongs to this fragment in: the lines, or the
    /// cell they are in.
    pub bounds: Rect,
    /// A copy: a header row repeated on a later page.
    pub repeat: bool,
}

pub(crate) struct Out<'a> {
    pub layers: &'a mut Layers,
    pub frags: &'a mut Vec<Frag>,
}

/// Where things are being put, for floating drawings and for fragments.
#[derive(Clone, Copy)]
pub(crate) struct EmitCtx {
    pub page: usize,
    pub page_rect: Rect,
    pub margin: Rect,
    pub column: Rect,
    /// Set inside a cell: what a click anywhere in it belongs to.
    pub bounds: Option<Rect>,
    pub repeat: bool,
    pub record: bool,
    pub depth: u8,
}

/// The box around paragraphs with borders or shading.
#[derive(Clone, Debug)]
pub(crate) struct ParaBox {
    pub borders: [Option<Border>; 4],
    pub between: Option<Border>,
    pub shading: Option<Rgba>,
    pub join_prev: bool,
    pub join_next: bool,
    pub gap_prev: f32,
    pub gap_next: f32,
}

#[derive(Clone)]
pub(crate) struct ParaEntry {
    pub para: Arc<Paragraph>,
    pub shaped: Arc<Shaped>,
    pub path: Option<Path>,
    /// The paragraph's origin: relative to the stack, or to the page.
    pub x: f32,
    pub top: f32,
    pub bx: Option<ParaBox>,
    /// Where the floating drawings anchored in it go (by index in
    /// `Shaped::floats`), where the flow has already worked that out.
    pub floats: Vec<(usize, Rect)>,
}

pub(crate) enum Entry {
    Para(ParaEntry),
    /// A table inside the stack, its left edge at `x`, its top at `top`.
    Table(Arc<TableLayout>, f32, f32),
}

/// Blocks laid out one under the other from the stack's top-left corner.
pub(crate) struct Stack {
    pub entries: Vec<Entry>,
    pub height: f32,
}

/// Where the paths of a stack's blocks come from.
#[derive(Clone)]
pub(crate) enum Paths {
    None,
    /// Cell `col` of row `row` of the table at this path.
    Cell(Path, usize, usize),
}

struct Key {
    ptr: usize,
    width: i32,
    ctx: u64,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr && self.width == other.width && self.ctx == other.ctx
    }
}
impl Eq for Key {}
impl std::hash::Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ptr.hash(state);
        self.width.hash(state);
        self.ctx.hash(state);
    }
}

struct Entry2 {
    /// Keeps the paragraph alive so that its address stays its identity.
    _keep: Arc<Paragraph>,
    shaped: Arc<Shaped>,
    generation: u64,
}

/// Lays out documents. Keeps what it shaped, so that laying out again after
/// an edit shapes only the paragraphs that changed.
pub struct Layouter {
    pub(crate) shaper: Shaper,
    pub(crate) families: Families,
    cache: HashMap<Key, Entry2>,
    generation: u64,
    shaped_count: usize,
}

impl Default for Layouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Families tried first for Han characters, where installed.
const HAN_FALLBACKS: [&str; 4] = ["Hiragino Sans GB", "Heiti SC", "Songti SC", "Hiragino Sans"];

impl Layouter {
    pub fn new() -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACKS);
        Self::with_shaper(shaper)
    }

    /// Use a shaper the caller has set up.
    pub fn with_shaper(shaper: Shaper) -> Self {
        Self {
            shaper,
            families: Families::default(),
            cache: HashMap::new(),
            generation: 0,
            shaped_count: 0,
        }
    }

    /// How many paragraphs have been shaped (not found in the cache) since
    /// this layouter was made.
    pub fn shaped_count(&self) -> usize {
        self.shaped_count
    }

    /// Forget everything shaped so far.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub(crate) fn begin(&mut self) {
        self.generation += 1;
    }

    /// Drop what the layout just finished did not use.
    pub(crate) fn finish(&mut self) {
        let generation = self.generation;
        self.cache.retain(|_, entry| entry.generation == generation);
    }
}

pub(crate) struct Cx<'a> {
    pub lay: &'a mut Layouter,
    pub doc: &'a Document,
    pub notes: NoteNumbers,
    pub grid: Option<f32>,
    pub max_object: f32,
    pub background: Option<[u8; 4]>,
    /// Cells covered by a vertical merge, and where their places are.
    pub redirects: Vec<(Path, Path)>,
    pub page_values: Option<PageValues>,
}

/// Space a paragraph's borders take above and below the text.
pub(crate) fn pads(para: &Paragraph) -> (f32, f32) {
    if fully_hidden(para) {
        return (0.0, 0.0);
    }
    let take = |border: &Option<Border>| border.map_or(0.0, |b| b.width + b.space);
    (take(&para.props.borders[1]), take(&para.props.borders[3]))
}

fn same_box(a: &Paragraph, b: &Paragraph) -> bool {
    let has =
        |p: &Paragraph| p.props.borders.iter().any(Option::is_some) || p.props.shading.is_some();
    has(a)
        && has(b)
        && a.props.borders == b.props.borders
        && a.props.shading == b.props.shading
        && a.props.indent_left == b.props.indent_left
        && a.props.indent_right == b.props.indent_right
        && a.props.first_line == b.props.first_line
}

/// What automatic paragraph spacing comes to, twips: the 14 points of an
/// HTML paragraph.
const AUTO_SPACE: i32 = 280;

/// The space above a paragraph that follows nothing.
pub(crate) fn space_before(para: &Paragraph) -> f32 {
    if fully_hidden(para) {
        0.0
    } else {
        pt(para.props.space_before)
    }
}

/// The space between two paragraphs.
///
/// Automatic spacing (`beforeAutospacing`, `afterAutospacing`) is what HTML
/// has: 14 points, but two of them next to each other are one, and list
/// items have none between them. Other spaces add up; contextual spacing
/// drops the space between paragraphs of one style.
pub(crate) fn gap(prev: &Paragraph, next: &Paragraph) -> f32 {
    // A paragraph that is hidden altogether has no spaces of its own.
    match (fully_hidden(prev), fully_hidden(next)) {
        (true, true) => return 0.0,
        (true, false) => return space_before(next),
        (false, true) => return pt(prev.props.space_after.max(0)),
        (false, false) => {}
    }
    let same_style = prev.props.style == next.props.style;
    let items = prev.props.list.is_some() && next.props.list.is_some();
    let after_auto = prev.props.space_after_auto;
    let before_auto = next.props.space_before_auto;
    let after = if prev.props.contextual_spacing && same_style {
        0
    } else if after_auto {
        if items { 0 } else { AUTO_SPACE }
    } else {
        prev.props.space_after.max(0)
    };
    let before = if next.props.contextual_spacing && same_style {
        0
    } else if before_auto {
        if items { 0 } else { AUTO_SPACE }
    } else {
        next.props.space_before.max(0)
    };
    if after_auto && before_auto {
        return pt(after.max(before));
    }
    pt(after) + pt(before)
}

pub(crate) fn rule_style(style: BorderStyle) -> RuleStyle {
    match style {
        BorderStyle::Double => RuleStyle::Double,
        BorderStyle::Dotted => RuleStyle::Dotted,
        BorderStyle::Dashed | BorderStyle::DotDash => RuleStyle::Dashed,
        _ => RuleStyle::Single,
    }
}

pub(crate) fn border_color(border: &Border) -> Rgba {
    border.color.unwrap_or([0, 0, 0, 255])
}

impl Cx<'_> {
    /// The paragraph shaped for a text area `width` wide.
    pub fn shaped(&mut self, para: &Arc<Paragraph>, width: f32) -> Arc<Shaped> {
        self.shaped_around(para, width, &[])
    }

    /// The same for a paragraph whose text flows around `obstacles`
    /// (rectangles from its top-left corner).
    pub fn shaped_around(
        &mut self,
        para: &Arc<Paragraph>,
        width: f32,
        obstacles: &[Rect],
    ) -> Arc<Shaped> {
        let env = Env {
            obstacles,
            settings: &self.doc.settings,
            grid: self.grid,
            notes: &self.notes,
            page: self.page_values.as_ref(),
            max_object: self.max_object,
            background: self.background,
        };
        let key = Key {
            ptr: Arc::as_ptr(para) as usize,
            width: (width * 16.0).round() as i32,
            ctx: context(para, &env),
        };
        let generation = self.lay.generation;
        if let Some(entry) = self.lay.cache.get_mut(&key) {
            entry.generation = generation;
            return entry.shaped.clone();
        }
        let shaped = Arc::new(shape(
            &mut self.lay.shaper,
            &mut self.lay.families,
            para,
            width,
            &env,
        ));
        self.lay.shaped_count += 1;
        self.lay.cache.insert(
            key,
            Entry2 {
                _keep: para.clone(),
                shaped: shaped.clone(),
                generation,
            },
        );
        shaped
    }

    /// The text area width of a paragraph in a container `width` wide.
    pub fn text_width(para: &Paragraph, width: f32) -> f32 {
        (width - pt(para.props.indent_left) - pt(para.props.indent_right)).max(1.0)
    }

    /// Lay blocks out from the top-left of a stack `width` wide.
    pub fn layout_stack(
        &mut self,
        blocks: &[Block],
        paths: &Paths,
        width: f32,
        depth: u8,
    ) -> Stack {
        let width = width.max(1.0);
        let mut entries = Vec::with_capacity(blocks.len());
        let mut y = 0.0f32;
        let mut prev: Option<Arc<Paragraph>> = None;
        for (k, block) in blocks.iter().enumerate() {
            let path = match paths {
                Paths::None => None,
                Paths::Cell(table, row, col) => Some(table.cell_block(*row, *col, k)),
            };
            match block {
                Block::Paragraph(para) => {
                    let shaped = self.shaped(para, Self::text_width(para, width));
                    let (pad_top, pad_bottom) = pads(para);
                    let space = prev.as_deref().map_or(space_before(para), |p| gap(p, para));
                    let joined = prev.as_deref().is_some_and(|p| same_box(p, para));
                    let top_pad = if joined { 0.0 } else { pad_top };
                    let top = y + space + top_pad;
                    let next_joined = blocks
                        .get(k + 1)
                        .is_some_and(|next| next.as_paragraph().is_some_and(|n| same_box(para, n)));
                    let bx = (para.props.borders.iter().any(Option::is_some)
                        || para.props.shading.is_some())
                    .then(|| ParaBox {
                        borders: para.props.borders,
                        between: para.props.border_between,
                        shading: para.props.shading,
                        join_prev: joined,
                        join_next: next_joined,
                        gap_prev: space,
                        gap_next: blocks
                            .get(k + 1)
                            .and_then(Block::as_paragraph)
                            .map_or(0.0, |n| gap(para, n)),
                    });
                    y = top
                        + shaped.paragraph.height()
                        + if next_joined { 0.0 } else { pad_bottom };
                    entries.push(Entry::Para(ParaEntry {
                        para: para.clone(),
                        x: pt(para.props.indent_left),
                        top,
                        shaped,
                        path,
                        bx,
                        floats: Vec::new(),
                    }));
                    prev = Some(para.clone());
                }
                Block::Table(table) => {
                    let table_path = match paths {
                        Paths::None => Path::default(),
                        Paths::Cell(..) => path.clone().unwrap_or_default(),
                    };
                    let space = prev
                        .as_deref()
                        .map_or(0.0, |p| pt(p.props.space_after.max(0)));
                    y += space;
                    let layout = self.layout_table(
                        table,
                        matches!(paths, Paths::Cell(..)).then_some(&table_path),
                        width,
                        depth + 1,
                    );
                    let x = layout.x;
                    entries.push(Entry::Table(Arc::new(layout), x, y));
                    let Some(Entry::Table(layout, _, _)) = entries.last() else {
                        unreachable!()
                    };
                    y += layout.height();
                    prev = None;
                }
            }
        }
        if let Some(last) = prev {
            y += pt(last.props.space_after.max(0));
        }
        Stack { entries, height: y }
    }

    /// Where a floating drawing goes.
    pub fn float_rect(
        &self,
        drawing: &Drawing,
        float: &Float,
        ec: &EmitCtx,
        anchor: (f32, f32, f32),
        para_top: f32,
        para_bottom: f32,
    ) -> Rect {
        let (w, h) = (
            emu_pt(drawing.size.0).max(0.0),
            emu_pt(drawing.size.1).max(0.0),
        );
        let (anchor_x, line_top, line_bottom) = anchor;
        let place = |from: FloatFrom, at: FloatAt, size: f32, horizontal: bool| -> f32 {
            let (start, end) = match (from, horizontal) {
                (FloatFrom::Page, true) => (ec.page_rect.x0, ec.page_rect.x1),
                (FloatFrom::Page, false) => (ec.page_rect.y0, ec.page_rect.y1),
                (FloatFrom::Margin, true) => (ec.margin.x0, ec.margin.x1),
                (FloatFrom::Margin, false) => (ec.margin.y0, ec.margin.y1),
                (FloatFrom::Column, true) | (FloatFrom::Paragraph, true) => {
                    (ec.column.x0, ec.column.x1)
                }
                (FloatFrom::Column, false) => (ec.margin.y0, ec.margin.y1),
                (FloatFrom::Text, true) => (anchor_x, anchor_x),
                (FloatFrom::Text, false) => (line_top, line_bottom),
                (FloatFrom::Paragraph, false) => (para_top, para_bottom),
            };
            match at {
                FloatAt::Offset(emu) => start + emu_pt(emu),
                FloatAt::Start => start,
                FloatAt::Center => (start + end - size) / 2.0,
                FloatAt::End => end - size,
            }
        };
        let x = place(float.x.from, float.x.at, w, true);
        let y = place(float.y.from, float.y.at, h, false);
        Rect::new(x, y, x + w, y + h)
    }

    /// Put a drawing in a rectangle.
    pub fn emit_drawing(
        &mut self,
        drawing: &Drawing,
        rect: Rect,
        layer: Layer,
        ec: &EmitCtx,
        out: &mut Out<'_>,
    ) {
        match &drawing.content {
            DrawingContent::Picture(picture) => {
                let item = Item::Picture(PictureItem {
                    image: picture.image.clone(),
                    rect,
                    crop: picture.crop,
                });
                layer.of(out.layers).push(item);
            }
            DrawingContent::TextBox(text_box) => {
                let [l, t, r, b] = text_box.insets.map(emu_pt);
                let frame = FrameItem {
                    rect,
                    fill: text_box.fill,
                    line: text_box
                        .line
                        .as_ref()
                        .map(|border| (border.width.max(0.25), border_color(border))),
                };
                layer.of(out.layers).push(Item::Frame(frame));
                if ec.depth >= 3 {
                    return;
                }
                let inner = Rect::new(
                    rect.x0 + l,
                    rect.y0 + t,
                    (rect.x1 - r).max(rect.x0 + l),
                    (rect.y1 - b).max(rect.y0 + t),
                );
                let stack =
                    self.layout_stack(&text_box.blocks, &Paths::None, inner.width(), ec.depth + 1);
                let inner_ec = EmitCtx {
                    record: false,
                    depth: ec.depth + 1,
                    bounds: None,
                    ..*ec
                };
                // The box's content has layers of its own so that it stays
                // together in the paint order.
                let mut nested = Layers::default();
                let mut frags = Vec::new();
                {
                    let mut sub = Out {
                        layers: &mut nested,
                        frags: &mut frags,
                    };
                    self.emit_stack(
                        &stack,
                        inner.x0,
                        inner.y0,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        &inner_ec,
                        &mut sub,
                    );
                }
                let target = layer.of(out.layers);
                target.push(Item::PushClip(rect));
                target.extend(nested.into_items());
                target.push(Item::PopClip);
            }
            DrawingContent::Other(label) => {
                layer.of(out.layers).push(Item::Placeholder {
                    rect,
                    label: label.clone(),
                });
            }
        }
    }

    /// Put some lines of a paragraph on the page, with what goes with
    /// them. `(x, y)` is where the paragraph's origin is. Returns the
    /// rectangles of floating drawings that text must keep clear of.
    pub fn emit_para(
        &mut self,
        entry: &ParaEntry,
        lines: Range<usize>,
        x: f32,
        y: f32,
        ec: &EmitCtx,
        out: &mut Out<'_>,
    ) -> Vec<(Rect, Wrap)> {
        let shaped = &entry.shaped;
        let paragraph = &shaped.paragraph;
        let all = paragraph.lines();
        let lines = lines.start.min(all.len())..lines.end.min(all.len());
        if lines.is_empty() {
            return Vec::new();
        }
        let first = &all[lines.start];
        let last = &all[lines.end - 1];
        let (top, bottom) = (y + first.top, y + last.top + last.height);
        if let Some(bx) = &entry.bx {
            self.emit_box(entry, bx, &lines, x, y, top, bottom, ec, out);
        }
        out.layers.body.push(Item::Text(TextItem {
            paragraph: paragraph.clone(),
            x,
            y,
            lines: lines.clone(),
        }));
        for (range, color) in &shaped.backs {
            for r in paragraph.text_rects_in(range.clone(), lines.clone()) {
                out.layers.shade.push(Item::Fill {
                    rect: Rect::new(x + r[0], y + r[1], x + r[2], y + r[3]),
                    color: *color,
                });
            }
        }
        if shaped.has_tabs {
            for gap in paragraph.tab_gaps() {
                if lines.contains(&gap.line) && gap.leader != aster_text::Leader::None {
                    out.layers.body.push(Item::Leader(LeaderItem {
                        x0: x + gap.x0,
                        x1: x + gap.x1,
                        baseline: y + gap.baseline,
                        size: gap.size,
                        kind: gap.leader,
                        color: gap.color,
                    }));
                }
            }
        }
        if !shaped.objects.is_empty() {
            for rect in paragraph.objects() {
                if !lines.contains(&rect.line) {
                    continue;
                }
                let Some((_, drawing)) = shaped.objects.iter().find(|(o, _)| *o == rect.offset)
                else {
                    continue;
                };
                let r = Rect::new(
                    x + rect.x,
                    y + rect.y,
                    x + rect.x + rect.width,
                    y + rect.y + rect.height,
                );
                let drawing = drawing.clone();
                self.emit_drawing(&drawing, r, Layer::Body, ec, out);
            }
        }
        for ruby in &shaped.rubies {
            let Some(r) = paragraph
                .text_rects_in(ruby.range.clone(), lines.clone())
                .into_iter()
                .next()
            else {
                continue;
            };
            let language = aster_text::segment::guess_language(&ruby.text, "ja");
            let laid = self.lay.shaper.lay_out(&aster_text::Spec {
                text: &ruby.text,
                base: ruby.style.clone(),
                spans: &[],
                language,
                width: None,
                scale: 1.0,
                line_height: 1.0,
                align: aster_text::Align::Start,
                rules: aster_text::Rules::for_language(language),
            });
            let (w, h) = (laid.width(), laid.height());
            let centre = x + (r[0] + r[2]) / 2.0;
            out.layers.body.push(Item::Text(TextItem {
                paragraph: Arc::new(laid),
                x: centre - w / 2.0,
                y: y + r[1] - h,
                lines: 0..1,
            }));
        }
        let mut keep_clear = Vec::new();
        for (n, (offset, drawing)) in shaped.floats.iter().enumerate() {
            let (line_index, caret_x) = paragraph.caret(*offset);
            if !lines.contains(&line_index) {
                continue;
            }
            let Some(float) = drawing.float else {
                continue;
            };
            let line = &all[line_index];
            let rect = match entry.floats.iter().find(|(i, _)| *i == n) {
                Some((_, rect)) => *rect,
                None => self.float_rect(
                    drawing,
                    &float,
                    ec,
                    (x + caret_x, y + line.top, y + line.top + line.height),
                    y,
                    y + paragraph.height(),
                ),
            };
            let layer = if float.behind {
                Layer::Behind
            } else {
                Layer::Front
            };
            let drawing = drawing.clone();
            self.emit_drawing(&drawing, rect, layer, ec, out);
            if float.wrap != Wrap::None {
                let [dl, dt, dr, db] = float.distance.map(emu_pt);
                keep_clear.push((
                    Rect::new(rect.x0 - dl, rect.y0 - dt, rect.x1 + dr, rect.y1 + db),
                    float.wrap,
                ));
            }
        }
        if ec.record
            && let Some(path) = &entry.path
        {
            let mut bounds = ec.bounds.unwrap_or(Rect::new(
                x.min(ec.column.x0),
                top,
                ec.column.x1.max(x + shaped.width),
                bottom,
            ));
            if ec.bounds.is_none() {
                bounds.x0 = ec.column.x0;
                bounds.x1 = ec.column.x1;
            }
            out.frags.push(Frag {
                path: path.clone(),
                shaped: shaped.clone(),
                page: ec.page,
                x,
                y,
                lines,
                bounds,
                repeat: ec.repeat,
            });
        }
        keep_clear
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_box(
        &mut self,
        entry: &ParaEntry,
        bx: &ParaBox,
        lines: &Range<usize>,
        x: f32,
        _y: f32,
        top: f32,
        bottom: f32,
        _ec: &EmitCtx,
        out: &mut Out<'_>,
    ) {
        let para = &entry.para;
        let all = entry.shaped.paragraph.lines();
        let starts = lines.start == 0;
        let ends = lines.end >= all.len();
        let take = |b: &Option<Border>| b.map_or(0.0, |b| b.width + b.space);
        let x0 = x + pt(para.props.first_line).min(0.0) - take(&bx.borders[0]);
        let x1 = x + entry.shaped.width + take(&bx.borders[2]);
        let y0 = if starts {
            top - if bx.join_prev {
                bx.gap_prev
            } else {
                take(&bx.borders[1])
            }
        } else {
            top
        };
        let y1 = if ends {
            bottom
                + if bx.join_next {
                    bx.gap_next
                } else {
                    take(&bx.borders[3])
                }
        } else {
            bottom
        };
        if let Some(color) = bx.shading {
            out.layers.shade.push(Item::Fill {
                rect: Rect::new(x0, y0, x1, y1),
                color,
            });
        }
        let mut rule = |border: &Border, from: (f32, f32), to: (f32, f32)| {
            out.layers.rules.push(Item::Rule {
                from,
                to,
                width: border.width.max(0.25),
                color: border_color(border),
                style: rule_style(border.style),
            });
        };
        if let Some(b) = &bx.borders[0] {
            let at = x0 + b.width / 2.0;
            rule(b, (at, y0), (at, y1));
        }
        if let Some(b) = &bx.borders[2] {
            let at = x1 - b.width / 2.0;
            rule(b, (at, y0), (at, y1));
        }
        if starts {
            if bx.join_prev {
                if let Some(b) = &bx.between {
                    rule(b, (x0, y0), (x1, y0));
                }
            } else if let Some(b) = &bx.borders[1] {
                let at = y0 + b.width / 2.0;
                rule(b, (x0, at), (x1, at));
            }
        }
        if ends
            && !bx.join_next
            && let Some(b) = &bx.borders[3]
        {
            let at = y1 - b.width / 2.0;
            rule(b, (x0, at), (x1, at));
        }
    }

    /// Put the part of a stack whose pieces start in `s0..s1` on the page,
    /// its top-left at `(ox, oy)` minus `s0`.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_stack(
        &mut self,
        stack: &Stack,
        ox: f32,
        oy: f32,
        s0: f32,
        s1: f32,
        ec: &EmitCtx,
        out: &mut Out<'_>,
    ) -> Vec<(Rect, Wrap)> {
        let shift = if s0.is_finite() { s0 } else { 0.0 };
        let mut clear = Vec::new();
        for entry in &stack.entries {
            match entry {
                Entry::Para(p) => {
                    let lines = p.shaped.paragraph.lines();
                    let from = lines.partition_point(|l| p.top + l.top < s0);
                    let to = lines.partition_point(|l| p.top + l.top < s1);
                    if from < to {
                        clear.extend(self.emit_para(
                            p,
                            from..to,
                            ox + p.x,
                            oy + p.top - shift,
                            ec,
                            out,
                        ));
                    }
                }
                Entry::Table(table, x, top) => {
                    self.emit_nested_table(
                        table,
                        ox + x,
                        oy + top - shift,
                        s0 - top,
                        s1 - top,
                        ec,
                        out,
                    );
                }
            }
        }
        clear
    }
}

/// Which layer of the page an item goes into.
#[derive(Clone, Copy)]
pub(crate) enum Layer {
    Behind,
    Body,
    Front,
}

impl Layer {
    pub fn of(self, layers: &mut Layers) -> &mut Vec<Item> {
        match self {
            Layer::Behind => &mut layers.behind,
            Layer::Body => &mut layers.body,
            Layer::Front => &mut layers.front,
        }
    }
}
