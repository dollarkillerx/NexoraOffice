//! The Markdown preview: the outline from `aster-markdown`, laid out with
//! Parley and drawn with vello_cpu, the same text stack and fonts as the
//! editor. No browser engine is involved.
//!
//! Laying out a document is the slow part, so it happens on a worker thread
//! ([`PreviewWorker`]) and produces a [`PreviewLayout`]: a display list with
//! absolute positions. The view on the UI thread ([`PreviewView`]) only draws
//! the part of that list inside the window, which is cheap enough to do on
//! every scroll step.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc;

use aster_markdown::preview::{Alignment, Block, BlockKind, Inline, Item as ListItem, Outline};
use parley::layout::PositionedLayoutItem;
use parley::{
    Affinity, AlignmentOptions, Cursor, FontContext, FontFamily, FontStyle, FontWeight,
    GenericFamily, Layout, LayoutContext, Selection, StyleProperty,
};
use vello_cpu::kurbo::{Affine, BezPath, Rect, RoundedRect, Shape, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::glyphs::{self, GlyphPaint};
use crate::text_surface::{Brush, DEFAULT_LANGUAGE, PageLayout, Rgba8, configure_han_fallback};

/// Code fonts, best first. The generic `monospace` comes out as Courier on
/// macOS, which is hard to read on screen.
const CODE_FONTS: &str = "\"SF Mono\", Menlo, Monaco, Consolas, \"Cascadia Mono\", \"DejaVu Sans Mono\", \"Liberation Mono\", monospace";

fn code_family() -> StyleProperty<'static, Brush> {
    StyleProperty::FontFamily(FontFamily::Source(CODE_FONTS.into()))
}

fn color(rgba: Rgba8) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// Colours and sizes of the preview. All colours must be opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewStyle {
    pub background: Rgba8,
    pub text: Rgba8,
    /// Quotes, footnotes, image descriptions.
    pub secondary: Rgba8,
    /// List markers, unticked boxes.
    pub tertiary: Rgba8,
    /// Links and ticked boxes.
    pub accent: Rgba8,
    /// Code, table headers, image boxes.
    pub fill: Rgba8,
    /// Rules, table lines, the bar of a quote.
    pub line: Rgba8,
    /// Body text size in logical pixels.
    pub font_size: f32,
    pub page: PageLayout,
}

/// Something to draw, at a position in the whole document, in physical pixels.
enum Piece {
    Text {
        x: f64,
        y: f64,
        // Boxed: a layout is large and most pieces are rectangles.
        layout: Box<Layout<Brush>>,
    },
    Rect {
        rect: Rect,
        color: Rgba8,
        radius: f64,
    },
    Frame {
        rect: Rect,
        color: Rgba8,
        width: f64,
    },
    Check {
        rect: Rect,
        color: Rgba8,
        width: f64,
    },
}

struct Painted {
    shape: Piece,
    /// Vertical extent, for skipping what is outside the view.
    top: f64,
    bottom: f64,
}

/// Where a block of the source ended up, for scrolling the editor and the
/// preview to the same place.
#[derive(Clone, Debug, PartialEq)]
struct Anchor {
    top: f64,
    bottom: f64,
    source: Range<usize>,
}

/// A laid-out document, made on the worker thread.
pub struct PreviewLayout {
    width: u16,
    scale: f32,
    height: f64,
    items: Vec<Painted>,
    /// In document order, with tops and sources only going up.
    anchors: Vec<Anchor>,
    links: Vec<(Rect, String)>,
}

impl PreviewLayout {
    /// The surface width and display scale this was laid out for.
    pub fn size(&self) -> (u16, f32) {
        (self.width, self.scale)
    }

    /// Total height in physical pixels.
    pub fn height(&self) -> f64 {
        self.height
    }

    /// The link under a point in document coordinates, if any.
    fn link_at(&self, x: f64, y: f64) -> Option<&str> {
        self.links
            .iter()
            .find(|(rect, _)| rect.contains((x, y)))
            .map(|(_, url)| url.as_str())
    }

    /// The source position shown at document height `y`, interpolated
    /// within a block and across the gaps between blocks.
    pub fn position_at(&self, y: f64) -> f64 {
        let anchors = &self.anchors;
        let Some(first) = anchors.first() else {
            return 0.0;
        };
        if y <= first.top {
            return 0.0;
        }
        let index = anchors.partition_point(|anchor| anchor.top <= y) - 1;
        let anchor = &anchors[index];
        if y < anchor.bottom {
            let fraction = (y - anchor.top) / (anchor.bottom - anchor.top).max(1.0);
            return anchor.source.start as f64 + fraction * anchor.source.len() as f64;
        }
        match anchors.get(index + 1) {
            Some(next) => {
                let fraction = (y - anchor.bottom) / (next.top - anchor.bottom).max(1.0);
                let (from, to) = (anchor.source.end as f64, next.source.start as f64);
                from + fraction.clamp(0.0, 1.0) * (to - from).max(0.0)
            }
            None => anchor.source.end as f64,
        }
    }

    /// The document height at which a source position is shown: the
    /// reverse of [`Self::position_at`].
    pub fn y_of(&self, position: f64) -> f64 {
        let anchors = &self.anchors;
        let Some(first) = anchors.first() else {
            return 0.0;
        };
        if position <= first.source.start as f64 {
            let fraction = position / (first.source.start as f64).max(1.0);
            return fraction * first.top;
        }
        let index = anchors.partition_point(|anchor| anchor.source.start as f64 <= position) - 1;
        let anchor = &anchors[index];
        if position < anchor.source.end as f64 {
            let fraction =
                (position - anchor.source.start as f64) / anchor.source.len().max(1) as f64;
            return anchor.top + fraction * (anchor.bottom - anchor.top);
        }
        match anchors.get(index + 1) {
            Some(next) => {
                let gap = (next.source.start as f64 - anchor.source.end as f64).max(1.0);
                let fraction = ((position - anchor.source.end as f64) / gap).clamp(0.0, 1.0);
                anchor.bottom + fraction * (next.top - anchor.bottom)
            }
            None => anchor.bottom,
        }
    }
}

/// Lays out documents. Owns its own fonts, so it can live on another thread.
pub struct PreviewBuilder {
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
}

impl Default for PreviewBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Inherited styling while descending into blocks.
#[derive(Clone, Copy)]
struct Context {
    color: Rgba8,
    /// Multiplies the body size: footnotes are smaller.
    size: f32,
    /// Inside a list item of a tight list: smaller gaps.
    tight: bool,
}

/// What a single laid-out text needs to know.
struct TextOptions {
    size: f32,
    color: Rgba8,
    bold: bool,
    mono: bool,
    alignment: parley::Alignment,
}

struct Pass<'a> {
    builder: &'a mut PreviewBuilder,
    style: &'a PreviewStyle,
    scale: f32,
    /// Logical pixels to physical.
    s: f64,
    items: Vec<Painted>,
    anchors: Vec<Anchor>,
    links: Vec<(Rect, String)>,
}

impl PreviewBuilder {
    pub fn new() -> Self {
        let mut font_cx = FontContext::default();
        configure_han_fallback(&mut font_cx);
        Self {
            font_cx,
            layout_cx: LayoutContext::default(),
        }
    }

    /// Lay out `source` for a surface `width` physical pixels wide.
    pub fn build(
        &mut self,
        source: &str,
        width: u16,
        scale: f32,
        style: &PreviewStyle,
    ) -> PreviewLayout {
        let outline = aster_markdown::preview::outline(source);
        self.build_outline(&outline, width, scale, style)
    }

    pub fn build_outline(
        &mut self,
        outline: &Outline,
        width: u16,
        scale: f32,
        style: &PreviewStyle,
    ) -> PreviewLayout {
        let s = f64::from(scale);
        let page = style.page;
        let full = f64::from(width);
        let margin = page.margin * s;
        let available = (full - 2.0 * margin).max(1.0);
        let page_width = page
            .max_width
            .map_or(available, |max| (max * s).min(available));
        let padding = (page.padding * s).min(page_width / 8.0);
        let x = ((full - page_width) / 2.0).floor() + padding;
        let column = (page_width - 2.0 * padding).max(1.0);

        let mut pass = Pass {
            builder: self,
            style,
            scale,
            s,
            items: Vec::new(),
            anchors: Vec::new(),
            links: Vec::new(),
        };
        let context = Context {
            color: style.text,
            size: 1.0,
            tight: false,
        };
        let top = margin + padding;
        let bottom = pass.blocks(&outline.blocks, x, top, column, context);
        PreviewLayout {
            width,
            scale,
            height: bottom + padding + margin,
            items: pass.items,
            anchors: pass.anchors,
            links: pass.links,
        }
    }
}

impl Pass<'_> {
    /// Body size in physical pixels.
    fn em(&self, context: Context) -> f64 {
        f64::from(self.style.font_size * context.size) * self.s
    }

    fn push(&mut self, shape: Piece, top: f64, bottom: f64) {
        self.items.push(Painted { shape, top, bottom });
    }

    fn anchor(&mut self, top: f64, bottom: f64, source: Range<usize>) {
        // Keep the list monotonic: a block nested in an earlier one (a list
        // marker, say) must not step back.
        if let Some(last) = self.anchors.last()
            && (source.start < last.source.start || top < last.top)
        {
            return;
        }
        self.anchors.push(Anchor {
            top,
            bottom,
            source,
        });
    }

    /// Lay out blocks one under the other from `y`; returns the bottom.
    fn blocks(
        &mut self,
        blocks: &[Block],
        x: f64,
        mut y: f64,
        width: f64,
        context: Context,
    ) -> f64 {
        let gap = self.em(context) * if context.tight { 0.35 } else { 0.85 };
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                y += gap;
                if matches!(block.kind, BlockKind::Heading { .. }) {
                    y += self.em(context) * 0.6;
                }
            }
            y = self.block(block, x, y, width, context);
        }
        y
    }

    fn block(&mut self, block: &Block, x: f64, y: f64, width: f64, context: Context) -> f64 {
        let em = self.em(context);
        let s = self.s;
        let style = *self.style;
        let base = f64::from(style.font_size * context.size);
        match &block.kind {
            BlockKind::Paragraph(inline) => {
                let options = TextOptions {
                    size: base as f32,
                    color: context.color,
                    bold: false,
                    mono: false,
                    alignment: parley::Alignment::Start,
                };
                let bottom = self.text(inline, x, y, width, &options);
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Heading { level, text } => {
                let factor =
                    [2.0, 1.6, 1.3, 1.12, 1.0, 0.92][usize::from((*level).clamp(1, 6) - 1)];
                let color = if *level >= 6 {
                    style.secondary
                } else {
                    context.color
                };
                let options = TextOptions {
                    size: (base * factor) as f32,
                    color,
                    bold: true,
                    mono: false,
                    alignment: parley::Alignment::Start,
                };
                let mut bottom = self.text(text, x, y, width, &options);
                if *level <= 2 {
                    let line = (bottom + em * 0.3).round();
                    let thickness = s.round().max(1.0);
                    let rect = Rect::new(x, line, x + width, line + thickness);
                    self.push(
                        Piece::Rect {
                            rect,
                            color: style.line,
                            radius: 0.0,
                        },
                        line,
                        line + thickness,
                    );
                    bottom = line + thickness;
                }
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Code { text, .. } => {
                let pad = em * 0.8;
                let inline = Inline {
                    text: text.clone(),
                    runs: Vec::new(),
                };
                let options = TextOptions {
                    size: (base * 0.9) as f32,
                    color: context.color,
                    bold: false,
                    mono: true,
                    alignment: parley::Alignment::Start,
                };
                let index = self.items.len();
                let text_bottom = self.text(&inline, x + pad, y + pad, width - 2.0 * pad, &options);
                let bottom = text_bottom + pad;
                let rect = Rect::new(x, y, x + width, bottom);
                // The background goes under the text laid out just now.
                self.items.insert(
                    index,
                    Painted {
                        shape: Piece::Rect {
                            rect,
                            color: style.fill,
                            radius: 6.0 * s,
                        },
                        top: y,
                        bottom,
                    },
                );
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Quote(blocks) => {
                let indent = em * 1.1;
                let inner = Context {
                    color: style.secondary,
                    ..context
                };
                let bottom = self.blocks(blocks, x + indent, y, width - indent, inner);
                let bar = (3.0 * s).round();
                let rect = Rect::new(x, y, x + bar, bottom.max(y + em));
                self.push(
                    Piece::Rect {
                        rect,
                        color: style.line,
                        radius: bar / 2.0,
                    },
                    y,
                    bottom,
                );
                bottom.max(y + em)
            }
            BlockKind::List {
                start,
                tight,
                items,
            } => self.list(*start, *tight, items, x, y, width, context),
            BlockKind::Table {
                alignments,
                head,
                rows,
            } => {
                let bottom = self.table(alignments, head, rows, x, y, width, context);
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Rule => {
                let line = (y + em * 0.6).round();
                let thickness = s.round().max(1.0);
                let rect = Rect::new(x, line, x + width, line + thickness);
                self.push(
                    Piece::Rect {
                        rect,
                        color: style.line,
                        radius: 0.0,
                    },
                    line,
                    line + thickness,
                );
                let bottom = y + em * 1.2;
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Image { description } => {
                let height = em * 6.0;
                let rect = Rect::new(x, y, x + width, y + height);
                self.push(
                    Piece::Rect {
                        rect,
                        color: style.fill,
                        radius: 6.0 * s,
                    },
                    y,
                    y + height,
                );
                self.push(
                    Piece::Frame {
                        rect,
                        color: style.line,
                        width: s.max(1.0),
                    },
                    y,
                    y + height,
                );
                let inline = Inline {
                    text: description.clone(),
                    runs: Vec::new(),
                };
                let options = TextOptions {
                    size: (base * 0.9) as f32,
                    color: style.secondary,
                    bold: false,
                    mono: false,
                    alignment: parley::Alignment::Center,
                };
                let layout = self.layout(&inline, width - 2.0 * em, &options);
                let text_height = f64::from(layout.height());
                let text_y = y + (height - text_height) / 2.0;
                self.push(
                    Piece::Text {
                        x: x + em,
                        y: text_y,
                        layout: Box::new(layout),
                    },
                    text_y,
                    text_y + text_height,
                );
                let bottom = y + height;
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Footnote { label, blocks } => {
                let inner = Context {
                    color: style.secondary,
                    size: context.size * 0.88,
                    ..context
                };
                let gutter = self.em(inner) * 2.2;
                let marker = Inline {
                    text: format!("{label}."),
                    runs: Vec::new(),
                };
                let options = TextOptions {
                    size: (base * 0.88) as f32,
                    color: style.secondary,
                    bold: false,
                    mono: false,
                    alignment: parley::Alignment::Start,
                };
                self.text(&marker, x, y, gutter, &options);
                self.blocks(blocks, x + gutter, y, width - gutter, inner)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        start: Option<u64>,
        tight: bool,
        items: &[ListItem],
        x: f64,
        mut y: f64,
        width: f64,
        context: Context,
    ) -> f64 {
        let em = self.em(context);
        let base = self.style.font_size * context.size;
        let gutter = em * if start.is_some() { 1.9 } else { 1.4 };
        let inner = Context { tight, ..context };
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                y += em * if tight { 0.25 } else { 0.6 };
            }
            let line_height = em * 1.6;
            match item.task {
                Some(ticked) => {
                    let side = (em * 0.95).round();
                    let top = y + (line_height - side) / 2.0;
                    let left = x + gutter - side - em * 0.45;
                    let rect = Rect::new(left, top, left + side, top + side);
                    if ticked {
                        self.push(
                            Piece::Rect {
                                rect,
                                color: self.style.accent,
                                radius: 3.0 * self.s,
                            },
                            top,
                            top + side,
                        );
                        self.push(
                            Piece::Check {
                                rect,
                                color: self.style.background,
                                width: (1.8 * self.s).max(1.0),
                            },
                            top,
                            top + side,
                        );
                    } else {
                        self.push(
                            Piece::Frame {
                                rect,
                                color: self.style.tertiary,
                                width: (1.2 * self.s).max(1.0),
                            },
                            top,
                            top + side,
                        );
                    }
                }
                None => {
                    let marker = match start {
                        Some(first) => format!("{}.", first + index as u64),
                        None => "•".to_owned(),
                    };
                    let inline = Inline {
                        text: marker,
                        runs: Vec::new(),
                    };
                    let options = TextOptions {
                        size: base,
                        color: self.style.tertiary,
                        bold: false,
                        mono: false,
                        alignment: parley::Alignment::End,
                    };
                    self.text(&inline, x, y, gutter - em * 0.45, &options);
                }
            }
            let bottom = self.blocks(&item.blocks, x + gutter, y, width - gutter, inner);
            if item.blocks.is_empty() {
                self.anchor(y, y + line_height, item.source.clone());
            }
            y = bottom.max(y + line_height);
        }
        y
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        alignments: &[Alignment],
        head: &[Inline],
        rows: &[Vec<Inline>],
        x: f64,
        y: f64,
        width: f64,
        context: Context,
    ) -> f64 {
        let em = self.em(context);
        let base = self.style.font_size * context.size;
        let columns = alignments
            .len()
            .max(head.len())
            .max(rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let (pad_x, pad_y) = (em * 0.6, em * 0.3);
        let empty = Inline::default();
        let cell =
            |row: &[Inline], column: usize| -> Inline { row.get(column).unwrap_or(&empty).clone() };
        let all_rows: Vec<(bool, &[Inline])> = std::iter::once((true, head))
            .chain(rows.iter().map(|row| (false, row.as_slice())))
            .collect();

        // Column widths: what the content wants, shrunk to fit if needed.
        let mut wanted = vec![em * 3.0; columns];
        for (is_head, row) in &all_rows {
            for (column, want) in wanted.iter_mut().enumerate() {
                let options = TextOptions {
                    size: base,
                    color: self.style.text,
                    bold: *is_head,
                    mono: false,
                    alignment: parley::Alignment::Start,
                };
                let layout = self.layout(&cell(row, column), f64::INFINITY, &options);
                *want = want.max(f64::from(layout.full_width()) + 2.0 * pad_x + 1.0);
            }
        }
        let total: f64 = wanted.iter().sum();
        if total > width {
            let minimum = em * 3.0;
            let flexible: f64 = wanted.iter().map(|w| w - minimum).sum();
            let excess = total - width;
            for want in &mut wanted {
                let share = if flexible > 0.0 {
                    (*want - minimum) / flexible
                } else {
                    0.0
                };
                *want = (*want - excess * share).max(minimum);
            }
        }

        let thickness = self.s.round().max(1.0);
        let line = self.style.line;
        let right = x + wanted.iter().sum::<f64>();
        let mut top = y;
        let mut row_lines = vec![y];
        for (is_head, row) in &all_rows {
            let mut height = em * 1.6;
            let mut laid = Vec::with_capacity(columns);
            for (column, column_width) in wanted.iter().enumerate() {
                let alignment = match alignments.get(column) {
                    Some(Alignment::Center) => parley::Alignment::Center,
                    Some(Alignment::Right) => parley::Alignment::End,
                    _ => parley::Alignment::Start,
                };
                let options = TextOptions {
                    size: base,
                    color: context.color,
                    bold: *is_head,
                    mono: false,
                    alignment,
                };
                let layout = self.layout(&cell(row, column), column_width - 2.0 * pad_x, &options);
                height = height.max(f64::from(layout.height()));
                laid.push(layout);
            }
            let bottom = top + height + 2.0 * pad_y;
            if *is_head {
                let rect = Rect::new(x, top, right, bottom);
                self.push(
                    Piece::Rect {
                        rect,
                        color: self.style.fill,
                        radius: 0.0,
                    },
                    top,
                    bottom,
                );
            }
            let mut left = x;
            for ((column_width, layout), column) in wanted.iter().zip(laid).zip(0..) {
                let row_cell = cell(row, column);
                self.collect_links(&row_cell, &layout, left + pad_x, top + pad_y);
                self.code_backgrounds(&row_cell, &layout, left + pad_x, top + pad_y);
                let text_bottom = top + pad_y + f64::from(layout.height());
                self.push(
                    Piece::Text {
                        x: left + pad_x,
                        y: top + pad_y,
                        layout: Box::new(layout),
                    },
                    top,
                    text_bottom,
                );
                left += column_width;
            }
            top = bottom;
            row_lines.push(top);
        }
        // Grid lines.
        for at in &row_lines {
            let rect = Rect::new(x, *at, right, at + thickness);
            self.push(
                Piece::Rect {
                    rect,
                    color: line,
                    radius: 0.0,
                },
                *at,
                at + thickness,
            );
        }
        let mut left = x;
        for column_width in wanted.iter().map(Some).chain(std::iter::once(None)) {
            let rect = Rect::new(left, y, left + thickness, top + thickness);
            self.push(
                Piece::Rect {
                    rect,
                    color: line,
                    radius: 0.0,
                },
                y,
                top,
            );
            if let Some(w) = column_width {
                left += w;
            }
        }
        top + thickness
    }

    /// Lay out `inline` at `(x, y)` within `width`; returns its bottom.
    fn text(&mut self, inline: &Inline, x: f64, y: f64, width: f64, options: &TextOptions) -> f64 {
        let layout = self.layout(inline, width, options);
        let height = f64::from(layout.height());
        self.collect_links(inline, &layout, x, y);
        self.code_backgrounds(inline, &layout, x, y);
        self.push(
            Piece::Text {
                x,
                y,
                layout: Box::new(layout),
            },
            y,
            y + height,
        );
        y + height
    }

    fn layout(&mut self, inline: &Inline, width: f64, options: &TextOptions) -> Layout<Brush> {
        let style = self.style;
        let builder = &mut *self.builder;
        let text = inline.text.as_str();
        let mut b = builder
            .layout_cx
            .ranged_builder(&mut builder.font_cx, text, self.scale, true);
        if options.mono {
            b.push_default(code_family());
        } else {
            b.push_default(StyleProperty::from(GenericFamily::SystemUi));
        }
        b.push_default(StyleProperty::FontSize(options.size));
        b.push_default(StyleProperty::LineHeight(
            parley::LineHeight::FontSizeRelative(1.6),
        ));
        b.push_default(StyleProperty::Brush(Brush(color(options.color))));
        b.push_default(StyleProperty::Locale(DEFAULT_LANGUAGE.parse().ok()));
        if options.bold {
            b.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        }
        for run in &inline.runs {
            let range = run.range.clone();
            if run.style.strong {
                b.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
            }
            if run.style.emphasis {
                b.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            }
            if run.style.strike {
                b.push(StyleProperty::Strikethrough(true), range.clone());
            }
            if run.style.code {
                b.push(code_family(), range.clone());
                b.push(StyleProperty::FontSize(options.size * 0.9), range.clone());
            }
            if run.style.image {
                b.push(
                    StyleProperty::Brush(Brush(color(style.secondary))),
                    range.clone(),
                );
            }
            if run.link.is_some() {
                b.push(
                    StyleProperty::Brush(Brush(color(style.accent))),
                    range.clone(),
                );
                b.push(StyleProperty::Underline(true), range.clone());
            }
        }
        let mut layout = b.build(text);
        layout.set_alignment(options.alignment, AlignmentOptions::default());
        let max = (width.is_finite()).then_some(width.max(1.0) as f32);
        layout.break_all_lines(max);
        layout
    }

    /// Rectangles of a text range of a laid-out text, in document coordinates.
    fn rects(layout: &Layout<Brush>, range: Range<usize>, x: f64, y: f64) -> Vec<Rect> {
        let selection = Selection::new(
            Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
            Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
        );
        let mut rects = Vec::new();
        selection.geometry_with(layout, |rect, _| {
            rects.push(Rect::new(
                x + rect.x0,
                y + rect.y0,
                x + rect.x1,
                y + rect.y1,
            ));
        });
        rects
    }

    fn collect_links(&mut self, inline: &Inline, layout: &Layout<Brush>, x: f64, y: f64) {
        for run in &inline.runs {
            if let Some(url) = &run.link {
                for rect in Self::rects(layout, run.range.clone(), x, y) {
                    self.links.push((rect, url.clone()));
                }
            }
        }
    }

    fn code_backgrounds(&mut self, inline: &Inline, layout: &Layout<Brush>, x: f64, y: f64) {
        let fill = self.style.fill;
        let radius = 3.0 * self.s;
        for run in &inline.runs {
            if run.style.code || run.style.image {
                for rect in Self::rects(layout, run.range.clone(), x, y) {
                    let pad = rect.height() * 0.1;
                    let rect = Rect::new(
                        rect.x0 - 2.0 * self.s,
                        rect.y0 + pad,
                        rect.x1 + 2.0 * self.s,
                        rect.y1 - pad,
                    );
                    self.push(
                        Piece::Rect {
                            rect,
                            color: fill,
                            radius,
                        },
                        rect.y0,
                        rect.y1,
                    );
                }
            }
        }
    }
}

/// The preview in a window: a laid-out document, a scroll position and a
/// pixel buffer.
pub struct PreviewView {
    layout: Option<Arc<PreviewLayout>>,
    background: Rgba8,
    scroll: f64,
    ctx: RenderContext,
    pixmap: Pixmap,
    resources: Resources,
    width: u16,
    height: u16,
    scale: f32,
    glyphs: Vec<Glyph>,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewView {
    pub fn new() -> Self {
        Self {
            layout: None,
            background: [0xff, 0xff, 0xff, 0xff],
            scroll: 0.0,
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            resources: Resources::new(),
            width: 0,
            height: 0,
            scale: 1.0,
            glyphs: Vec::new(),
        }
    }

    /// Resize the pixel buffer; sizes are physical pixels.
    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height, scale) == (self.width, self.height, self.scale) {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        self.clamp();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    pub fn set_background(&mut self, background: Rgba8) {
        self.background = background;
    }

    /// Show a new layout. The scroll position is kept in pixels; the caller
    /// moves it with [`Self::scroll_to_position`] if it should follow the
    /// source instead.
    pub fn set_layout(&mut self, layout: Arc<PreviewLayout>) {
        self.layout = Some(layout);
        self.clamp();
    }

    pub fn layout(&self) -> Option<&Arc<PreviewLayout>> {
        self.layout.as_ref()
    }

    /// Whether the layout shown was made for another width or scale.
    pub fn is_stale(&self) -> bool {
        self.layout
            .as_ref()
            .is_none_or(|layout| layout.size() != (self.width, self.scale))
    }

    fn max_scroll(&self) -> f64 {
        self.layout.as_ref().map_or(0.0, |layout| {
            (layout.height - f64::from(self.height)).max(0.0)
        })
    }

    fn clamp(&mut self) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Scroll by physical pixels; returns whether the view moved.
    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll;
        self.scroll += delta;
        self.clamp();
        self.scroll != before
    }

    /// Scroll offset and its upper bound, in physical pixels.
    pub fn scroll_range(&self) -> (f64, f64) {
        (self.scroll, self.max_scroll())
    }

    /// The source position at the top edge of the view.
    pub fn top_position(&self) -> f64 {
        self.layout
            .as_ref()
            .map_or(0.0, |layout| layout.position_at(self.scroll))
    }

    /// Scroll so that a source position is at the top edge of the view.
    pub fn scroll_to_position(&mut self, position: f64) {
        if let Some(layout) = &self.layout {
            self.scroll = layout.y_of(position);
            self.clamp();
        }
    }

    /// The link at a point in physical pixels relative to the view.
    pub fn link_at(&self, x: f64, y: f64) -> Option<String> {
        self.layout
            .as_ref()?
            .link_at(x, y + self.scroll)
            .map(str::to_owned)
    }

    /// Draw the visible part and return the pixels, premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        let Self {
            layout,
            background,
            scroll,
            ctx,
            pixmap,
            resources,
            width,
            height,
            scale,
            glyphs: buffer,
        } = self;
        let (w, h) = (f64::from(*width), f64::from(*height));
        ctx.reset();
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(color(*background));
        ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        if let Some(layout) = layout {
            let (top, bottom) = (*scroll, *scroll + h);
            for item in &layout.items {
                if item.bottom < top || item.top > bottom {
                    continue;
                }
                ctx.set_transform(Affine::translate((0.0, -*scroll)));
                draw_shape(
                    ctx,
                    resources,
                    &item.shape,
                    top - item.top_of_text(),
                    h,
                    *scale,
                    buffer,
                );
            }
        }
        ctx.flush();
        ctx.render(&mut *pixmap, resources);
        pixmap
    }

    /// Render and encode as PNG, for tests and `--dump`.
    pub fn encode_png(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width.max(1), self.height.max(1));
        let data = self.render().data().to_vec();
        Pixmap::from_parts(data, width, height)
            .into_png()
            .map_err(|err| err.to_string())
    }
}

impl Painted {
    fn top_of_text(&self) -> f64 {
        match &self.shape {
            Piece::Text { y, .. } => *y,
            _ => self.top,
        }
    }
}

/// Draw one item. `visible_from` is the top of the view relative to the
/// item's own top, so long texts skip lines above the view.
fn draw_shape(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    shape: &Piece,
    visible_from: f64,
    view_height: f64,
    scale: f32,
    buffer: &mut Vec<Glyph>,
) {
    match shape {
        Piece::Rect {
            rect,
            color: c,
            radius,
        } => {
            ctx.set_paint(color(*c));
            if *radius > 0.0 {
                ctx.fill_path(&RoundedRect::from_rect(*rect, *radius).to_path(0.1));
            } else {
                ctx.fill_rect(rect);
            }
        }
        Piece::Frame {
            rect,
            color: c,
            width,
        } => {
            ctx.set_paint(color(*c));
            ctx.set_stroke(Stroke::new(*width));
            let inset = rect.inset(-width / 2.0);
            ctx.stroke_path(&RoundedRect::from_rect(inset, 3.0 * f64::from(scale)).to_path(0.1));
        }
        Piece::Check {
            rect,
            color: c,
            width,
        } => {
            let mut path = BezPath::new();
            let (x0, y0, side) = (rect.x0, rect.y0, rect.width());
            path.move_to((x0 + side * 0.24, y0 + side * 0.52));
            path.line_to((x0 + side * 0.43, y0 + side * 0.72));
            path.line_to((x0 + side * 0.77, y0 + side * 0.3));
            ctx.set_paint(color(*c));
            ctx.set_stroke(Stroke::new(*width));
            ctx.stroke_path(&path);
        }
        Piece::Text { x, y, layout } => {
            let transform = *ctx.transform();
            ctx.set_transform(transform * Affine::translate((*x, *y)));
            for line in layout.lines() {
                let metrics = line.metrics();
                if f64::from(metrics.block_max_coord) < visible_from {
                    continue;
                }
                if f64::from(metrics.block_min_coord) > visible_from + view_height {
                    break;
                }
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                        continue;
                    };
                    let style = glyph_run.style();
                    let run = glyph_run.run();
                    buffer.clear();
                    buffer.extend(glyph_run.positioned_glyphs().map(|glyph| Glyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    }));
                    let paint = GlyphPaint {
                        color: style.brush.0,
                        embolden: false,
                        skew: None,
                    };
                    glyphs::draw(ctx, resources, run, buffer, paint);
                    let x0 = f64::from(glyph_run.offset());
                    let x1 = x0 + f64::from(glyph_run.advance());
                    let font = run.font_metrics();
                    if let Some(underline) = &style.underline {
                        let offset = underline.offset.unwrap_or(font.underline_offset);
                        let size = underline.size.unwrap_or(font.underline_size);
                        glyphs::line(
                            ctx,
                            underline.brush.0,
                            (x0, x1),
                            glyph_run.baseline(),
                            offset,
                            size,
                            scale,
                        );
                    }
                    if let Some(strike) = &style.strikethrough {
                        let offset = strike.offset.unwrap_or(font.strikethrough_offset);
                        let size = strike.size.unwrap_or(font.strikethrough_size);
                        glyphs::line(
                            ctx,
                            strike.brush.0,
                            (x0, x1),
                            glyph_run.baseline(),
                            offset,
                            size,
                            scale,
                        );
                    }
                }
            }
            ctx.set_transform(transform);
        }
    }
}

/// A preview to lay out on the worker thread.
pub struct PreviewJob {
    /// Which document it is for; only the newest job per key is done.
    pub key: u64,
    /// Handed back with the result, so stale results can be told apart.
    pub generation: u64,
    pub source: String,
    pub width: u16,
    pub scale: f32,
    pub style: PreviewStyle,
}

/// A thread that lays out previews. Jobs that arrive while it is busy
/// replace older jobs for the same document.
pub struct PreviewWorker {
    jobs: mpsc::Sender<PreviewJob>,
}

impl PreviewWorker {
    /// Start the thread. `done` runs on it with each finished layout.
    pub fn spawn(done: impl Fn(u64, u64, PreviewLayout) + Send + 'static) -> Self {
        let (jobs, inbox) = mpsc::channel::<PreviewJob>();
        std::thread::Builder::new()
            .name("aster-preview".into())
            .spawn(move || {
                let mut builder = PreviewBuilder::new();
                while let Ok(job) = inbox.recv() {
                    let mut latest: HashMap<u64, PreviewJob> = HashMap::new();
                    let mut order = vec![job.key];
                    latest.insert(job.key, job);
                    while let Ok(job) = inbox.try_recv() {
                        if !latest.contains_key(&job.key) {
                            order.push(job.key);
                        }
                        latest.insert(job.key, job);
                    }
                    for key in order {
                        if let Some(job) = latest.remove(&key) {
                            let layout =
                                builder.build(&job.source, job.width, job.scale, &job.style);
                            done(job.key, job.generation, layout);
                        }
                    }
                }
            })
            .expect("cannot start the preview thread");
        Self { jobs }
    }

    pub fn request(&self, job: PreviewJob) {
        // Only fails once the thread is gone, and then there is nobody to tell.
        let _ = self.jobs.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOUR: &str = include_str!("../tests/data/markdown-tour.md");

    fn style() -> PreviewStyle {
        PreviewStyle {
            background: [0xff, 0xff, 0xff, 0xff],
            text: [0x0b, 0x0b, 0x0b, 0xff],
            secondary: [0x5a, 0x5a, 0x58, 0xff],
            tertiary: [0x76, 0x76, 0x74, 0xff],
            accent: [0x7c, 0x5c, 0xe0, 0xff],
            fill: [0xf1, 0xf1, 0xf0, 0xff],
            line: [0xde, 0xde, 0xdd, 0xff],
            font_size: 15.0,
            page: PageLayout {
                margin: 0.0,
                padding: 48.0,
                max_width: Some(890.0),
                min_aspect: None,
            },
        }
    }

    fn view(source: &str, width: u16, height: u16) -> PreviewView {
        let layout = PreviewBuilder::new().build(source, width, 2.0, &style());
        let mut view = PreviewView::new();
        view.resize(width, height, 2.0);
        view.set_layout(Arc::new(layout));
        view
    }

    #[test]
    fn the_tour_lays_out_and_renders() {
        let mut view = view(TOUR, 1600, 1400);
        let layout = view.layout().unwrap().clone();
        assert!(layout.height() > 3000.0, "{}", layout.height());
        assert!(!view.is_stale());
        // Anchors only go down, in the source and on the page.
        assert!(layout.anchors.windows(2).all(|pair| {
            pair[0].source.start <= pair[1].source.start && pair[0].top <= pair[1].top
        }));
        // Set to a folder to look at the result, one image per screen.
        if let Some(folder) = std::env::var_os("ASTER_PREVIEW_PNG") {
            let folder = std::path::PathBuf::from(folder);
            let mut page = 0;
            loop {
                let png = view.encode_png().unwrap();
                std::fs::write(folder.join(format!("tour-{page}.png")), &png).unwrap();
                page += 1;
                if !view.scroll_by(1300.0) {
                    break;
                }
            }
            view.scroll_by(-1.0e9);
        }
        let data = view.render().data_as_u8_slice();
        let (pixels, _) = data.as_chunks::<4>();
        let accent = pixels.iter().filter(|px| **px == style().accent).count();
        assert!(accent > 100, "links are not in the accent colour: {accent}");
    }

    #[test]
    fn links_are_found_where_they_are_drawn() {
        let layout = PreviewBuilder::new().build(
            "[网站](https://example.com) 和 www.example.org",
            1200,
            2.0,
            &style(),
        );
        let urls: Vec<&str> = layout.links.iter().map(|(_, url)| url.as_str()).collect();
        assert_eq!(urls, ["https://example.com", "http://www.example.org"]);
        let (rect, _) = &layout.links[0];
        let (x, y) = (rect.center().x, rect.center().y);
        let mut view = PreviewView::new();
        view.resize(1200, 800, 2.0);
        view.set_layout(Arc::new(layout));
        assert_eq!(view.link_at(x, y).as_deref(), Some("https://example.com"));
        assert_eq!(view.link_at(5.0, 5.0), None);
    }

    #[test]
    fn source_positions_and_heights_map_both_ways() {
        let source: String = (0..120)
            .map(|i| format!("第 {i} 段，有一些文字。\n\n"))
            .collect();
        let mut view = view(&source, 1200, 800);
        let layout = view.layout().unwrap().clone();
        let paragraph_60 = source.find("第 60 段").unwrap() as f64;
        let y = layout.y_of(paragraph_60);
        assert!((layout.position_at(y) - paragraph_60).abs() < 3.0);
        view.scroll_to_position(paragraph_60);
        assert!((view.top_position() - paragraph_60).abs() < 3.0);
        // Monotonic.
        let mut last = 0.0;
        for step in 0..100 {
            let at = layout.y_of(source.len() as f64 * f64::from(step) / 100.0);
            assert!(at >= last);
            last = at;
        }
        assert!(view.scroll_by(1.0e9));
        let (scroll, max) = view.scroll_range();
        assert_eq!(scroll, max);
    }

    #[test]
    fn empty_and_awkward_documents() {
        for source in [
            "",
            "\n\n",
            "```\n未闭合的代码块",
            "| a |\n|---|\n| 1 | 2 | 3 |",
            &"长".repeat(50_000),
        ] {
            let mut view = view(source, 900, 600);
            view.render();
            assert_eq!(view.top_position().max(0.0), view.top_position());
        }
        // Very narrow: nothing panics, text wraps.
        let mut narrow = view(TOUR, 120, 400);
        narrow.render();
    }

    #[test]
    fn the_worker_lays_out_the_newest_job() {
        let (sender, results) = mpsc::channel();
        let worker = PreviewWorker::spawn(move |key, generation, layout| {
            let _ = sender.send((key, generation, layout.height()));
        });
        for generation in 0..5 {
            worker.request(PreviewJob {
                key: 1,
                generation,
                source: format!("# 第 {generation} 版"),
                width: 800,
                scale: 1.0,
                style: style(),
            });
        }
        let mut last = 0;
        while let Ok((key, generation, height)) =
            results.recv_timeout(std::time::Duration::from_secs(10))
        {
            assert_eq!(key, 1);
            assert!(height > 0.0);
            assert!(generation >= last);
            last = generation;
            if generation == 4 {
                break;
            }
        }
        assert_eq!(last, 4);
    }
}
