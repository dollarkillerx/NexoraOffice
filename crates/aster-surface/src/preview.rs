//! The Markdown preview: the outline from `aster-markdown`, laid out with
//! `aster-text` (Parley's shaping, our own Chinese and Japanese line
//! breaking) and drawn with vello_cpu, with the same fonts as the editor. No
//! browser engine is involved.
//!
//! Text can be selected with the mouse and copied as plain text; links that
//! start with `#` (footnotes and the way back from them) scroll the preview.
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

use aster_markdown::preview::{
    Alignment, Block, BlockKind, Footnote, Inline, Item as ListItem, Outline,
};
use aster_markdown::syntax::normalize_label;
use aster_text::selection::Piece as SelectionPiece;
use aster_text::{
    Align, Family, Paragraph, Rules, Selection, Shaper, Span, Spec, Style as TextStyle, TextPoint,
};
use vello_cpu::kurbo::{Affine, BezPath, Rect, RoundedRect, Shape, Stroke};
use vello_cpu::peniko::{Color, Fill};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::glyphs;
use crate::text_surface::{DEFAULT_LANGUAGE, HAN_FALLBACK_FAMILIES, PageLayout, Rgba8};

/// Code fonts, best first. The generic `monospace` comes out as Courier on
/// macOS, which is hard to read on screen.
const CODE_FONTS: &str = "\"SF Mono\", Menlo, Monaco, Consolas, \"Cascadia Mono\", \"DejaVu Sans Mono\", \"Liberation Mono\", monospace";
/// The marker of an unordered list item.
pub const BULLET: &str = "•";
/// Line height of preview text, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.6;

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
    /// Behind selected text.
    pub selection: Rgba8,
    /// Bars and titles of GitHub's callouts, in the order of
    /// [`aster_markdown::preview::CalloutKind::ALL`].
    pub callouts: [Rgba8; 5],
    /// Their titles, in the interface language.
    pub callout_labels: [&'static str; 5],
    /// Body text size in logical pixels.
    pub font_size: f32,
    pub page: PageLayout,
}

/// Something to draw, at a position in the whole document, in physical pixels.
/// Text is drawn after everything else, over the selection.
enum Piece {
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

/// A laid-out text at its place in the document. The index of one in
/// [`PreviewLayout::texts`] is what a selection calls a block.
struct TextBox {
    x: f64,
    y: f64,
    paragraph: Paragraph,
    /// Takes part in selections; list markers and the like do not.
    selectable: bool,
    /// What separates it from the text before it when copied.
    before: &'static str,
}

impl TextBox {
    fn bottom(&self) -> f64 {
        self.y + f64::from(self.paragraph.height())
    }
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
    /// In reading order.
    texts: Vec<TextBox>,
    /// Where `#name` links go: footnotes and the references to them.
    targets: HashMap<String, f64>,
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

    /// The text position nearest to a point in document coordinates: in the
    /// text the point is level with, or the nearest one above or below.
    pub fn point_at(&self, x: f64, y: f64) -> Option<TextPoint> {
        let mut best: Option<(f64, f64, usize)> = None;
        for (index, text) in self.texts.iter().enumerate() {
            if !text.selectable {
                continue;
            }
            let dy = if y < text.y {
                text.y - y
            } else {
                (y - text.bottom()).max(0.0)
            };
            let right = text.x + f64::from(text.paragraph.width()).max(1.0);
            let dx = if x < text.x {
                text.x - x
            } else {
                (x - right).max(0.0)
            };
            if best.is_none_or(|(by, bx, _)| (dy, dx) < (by, bx)) {
                best = Some((dy, dx, index));
            }
        }
        let (_, _, block) = best?;
        let text = &self.texts[block];
        let offset = text
            .paragraph
            .offset_at((x - text.x) as f32, (y - text.y) as f32);
        Some(TextPoint { block, offset })
    }

    fn pieces(&self) -> Vec<SelectionPiece<'_>> {
        self.texts
            .iter()
            .map(|text| SelectionPiece {
                text: if text.selectable {
                    text.paragraph.text()
                } else {
                    ""
                },
                before: if text.selectable { text.before } else { "" },
            })
            .collect()
    }

    /// The plain text of a selection.
    pub fn selected_text(&self, selection: &Selection) -> String {
        selection.text(&self.pieces())
    }

    /// A selection of everything, if there is any text.
    pub fn select_all(&self) -> Option<Selection> {
        let first = self.texts.iter().position(|text| text.selectable)?;
        let last = self.texts.iter().rposition(|text| text.selectable)?;
        Some(Selection {
            anchor: TextPoint {
                block: first,
                offset: 0,
            },
            focus: TextPoint {
                block: last,
                offset: self.texts[last].paragraph.text().len(),
            },
        })
    }

    /// Where a `#name` link goes, in document coordinates.
    pub fn target(&self, name: &str) -> Option<f64> {
        self.targets.get(name).copied()
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
    shaper: Shaper,
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
    alignment: Align,
    /// Ordinary text; list markers and labels are not.
    selectable: bool,
}

impl TextOptions {
    fn body(size: f32, color: Rgba8) -> Self {
        Self {
            size,
            color,
            bold: false,
            mono: false,
            alignment: Align::Start,
            selectable: true,
        }
    }
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
    texts: Vec<TextBox>,
    targets: HashMap<String, f64>,
    /// Separator before the next selectable text, in copied text.
    before: &'static str,
}

impl PreviewBuilder {
    pub fn new() -> Self {
        let mut shaper = Shaper::new();
        shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
        Self { shaper }
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
            texts: Vec::new(),
            targets: HashMap::new(),
            before: "",
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
            texts: pass.texts,
            targets: pass.targets,
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
                let options = TextOptions::body(base as f32, context.color);
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
                    bold: true,
                    ..TextOptions::body((base * factor) as f32, color)
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
                    mono: true,
                    ..TextOptions::body((base * 0.9) as f32, context.color)
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
                    alignment: Align::Center,
                    ..TextOptions::body((base * 0.9) as f32, style.secondary)
                };
                let paragraph = self.layout(&inline, width - 2.0 * em, &options);
                let text_height = f64::from(paragraph.height());
                let text_y = y + (height - text_height) / 2.0;
                self.add_text(x + em, text_y, paragraph, true);
                let bottom = y + height;
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Callout { kind, blocks } => {
                let tint = style.callouts[kind.index()];
                let indent = em * 1.1;
                let title = Inline {
                    text: style.callout_labels[kind.index()].to_owned(),
                    runs: Vec::new(),
                };
                let options = TextOptions {
                    bold: true,
                    selectable: false,
                    ..TextOptions::body(base as f32, tint)
                };
                let title_bottom = self.text(&title, x + indent, y, width - indent, &options);
                let bottom = self.blocks(
                    blocks,
                    x + indent,
                    title_bottom + em * 0.2,
                    width - indent,
                    context,
                );
                let bar = (3.0 * s).round();
                self.push(
                    Piece::Rect {
                        rect: Rect::new(x, y, x + bar, bottom),
                        color: tint,
                        radius: bar / 2.0,
                    },
                    y,
                    bottom,
                );
                self.anchor(y, bottom, block.source.clone());
                bottom
            }
            BlockKind::Footnotes(notes) => self.footnotes(notes, x, y, width, context),
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
                        None => BULLET.to_owned(),
                    };
                    let inline = Inline {
                        text: marker,
                        runs: Vec::new(),
                    };
                    let options = TextOptions {
                        alignment: Align::End,
                        selectable: false,
                        ..TextOptions::body(base, self.style.tertiary)
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

    /// The footnotes at the end: a short rule, then each note with its
    /// number. Each note is where `#fn:` links go.
    fn footnotes(
        &mut self,
        notes: &[Footnote],
        x: f64,
        y: f64,
        width: f64,
        context: Context,
    ) -> f64 {
        let style = *self.style;
        let em = self.em(context);
        let line = (y + em * 0.3).round();
        let thickness = self.s.round().max(1.0);
        let rule = Rect::new(x, line, x + (width / 3.0).min(em * 12.0), line + thickness);
        self.push(
            Piece::Rect {
                rect: rule,
                color: style.line,
                radius: 0.0,
            },
            line,
            line + thickness,
        );
        let inner = Context {
            color: style.secondary,
            size: context.size * 0.88,
            tight: false,
        };
        let small = self.em(inner);
        let gutter = small * 2.2;
        let mut top = line + thickness + em * 0.8;
        for note in notes {
            if top > line + thickness + em * 0.8 {
                top += small * 0.5;
            }
            self.targets
                .insert(format!("fn:{}", normalize_label(&note.label)), top);
            let marker = Inline {
                text: format!("{}.", note.number),
                runs: Vec::new(),
            };
            let options = TextOptions {
                alignment: Align::End,
                selectable: false,
                ..TextOptions::body(style.font_size * inner.size, style.secondary)
            };
            self.text(&marker, x, top, gutter - small * 0.4, &options);
            let bottom = self.blocks(&note.blocks, x + gutter, top, width - gutter, inner);
            self.anchor(top, bottom, note.source.clone());
            top = bottom.max(top + small * LINE_HEIGHT as f64);
        }
        top
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
                    bold: *is_head,
                    ..TextOptions::body(base, self.style.text)
                };
                let paragraph = self.layout(&cell(row, column), f64::INFINITY, &options);
                *want = want.max(f64::from(paragraph.width()) + 2.0 * pad_x + 1.0);
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
                    Some(Alignment::Center) => Align::Center,
                    Some(Alignment::Right) => Align::End,
                    _ => Align::Start,
                };
                let options = TextOptions {
                    bold: *is_head,
                    alignment,
                    ..TextOptions::body(base, context.color)
                };
                let paragraph =
                    self.layout(&cell(row, column), column_width - 2.0 * pad_x, &options);
                height = height.max(f64::from(paragraph.height()));
                laid.push(paragraph);
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
            for ((column_width, paragraph), column) in wanted.iter().zip(laid).zip(0..) {
                let row_cell = cell(row, column);
                self.collect_links(&row_cell, &paragraph, left + pad_x, top + pad_y);
                self.code_backgrounds(&row_cell, &paragraph, left + pad_x, top + pad_y);
                // Cells of a row copy as tab-separated values.
                if column > 0 {
                    self.before = "\t";
                }
                self.add_text(left + pad_x, top + pad_y, paragraph, true);
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
        let paragraph = self.layout(inline, width, options);
        let bottom = y + f64::from(paragraph.height());
        self.collect_links(inline, &paragraph, x, y);
        self.code_backgrounds(inline, &paragraph, x, y);
        self.add_text(x, y, paragraph, options.selectable);
        bottom
    }

    fn add_text(&mut self, x: f64, y: f64, paragraph: Paragraph, selectable: bool) {
        let before = if selectable {
            std::mem::replace(&mut self.before, "\n")
        } else {
            ""
        };
        self.texts.push(TextBox {
            x,
            y,
            paragraph,
            selectable,
            before,
        });
    }

    fn layout(&mut self, inline: &Inline, width: f64, options: &TextOptions) -> Paragraph {
        let style = self.style;
        let base = TextStyle {
            family: if options.mono {
                Family::List(CODE_FONTS.into())
            } else {
                Family::SystemUi
            },
            size: options.size,
            color: options.color,
            bold: options.bold,
            italic: false,
            underline: false,
            strike: false,
            rise: 0.0,
        };
        let spans: Vec<Span> = inline
            .runs
            .iter()
            .map(|run| {
                let mut look = base.clone();
                look.bold |= run.style.strong;
                look.italic |= run.style.emphasis;
                look.strike |= run.style.strike;
                if run.style.code {
                    look.family = Family::List(CODE_FONTS.into());
                    look.size = options.size * 0.9;
                }
                if run.style.image {
                    look.color = style.secondary;
                }
                if run.link.is_some() {
                    look.color = style.accent;
                    look.underline = true;
                }
                if run.style.footnote {
                    // A raised number, like a superscript.
                    look.size = options.size * 0.72;
                    look.rise = options.size * 0.38;
                    look.underline = false;
                }
                Span {
                    range: run.range.clone(),
                    style: look,
                }
            })
            .collect();
        let language = aster_text::segment::guess_language(&inline.text, DEFAULT_LANGUAGE);
        let spec = Spec {
            text: &inline.text,
            base,
            spans: &spans,
            language,
            width: width.is_finite().then_some(width.max(1.0) as f32),
            scale: self.scale,
            line_height: LINE_HEIGHT,
            align: options.alignment,
            rules: Rules::for_language(language),
        };
        self.builder.shaper.lay_out(&spec)
    }

    /// Rectangles of a text range of a laid-out text, in document coordinates.
    fn rects(paragraph: &Paragraph, range: Range<usize>, x: f64, y: f64) -> Vec<Rect> {
        paragraph
            .rects(range)
            .into_iter()
            .map(|[x0, y0, x1, y1]| {
                Rect::new(
                    x + f64::from(x0),
                    y + f64::from(y0),
                    x + f64::from(x1),
                    y + f64::from(y1),
                )
            })
            .collect()
    }

    fn collect_links(&mut self, inline: &Inline, paragraph: &Paragraph, x: f64, y: f64) {
        for run in &inline.runs {
            if let Some(url) = &run.link {
                let rects = Self::rects(paragraph, run.range.clone(), x, y);
                // The first reference to a footnote is where its note links back to.
                if let (Some(label), Some(first)) = (url.strip_prefix("#fn:"), rects.first()) {
                    self.targets
                        .entry(format!("fnref:{label}"))
                        .or_insert(first.y0);
                }
                for rect in rects {
                    self.links.push((rect, url.clone()));
                }
            }
        }
    }

    fn code_backgrounds(&mut self, inline: &Inline, paragraph: &Paragraph, x: f64, y: f64) {
        let fill = self.style.fill;
        let radius = 3.0 * self.s;
        for run in &inline.runs {
            if run.style.code || run.style.image {
                for rect in Self::rects(paragraph, run.range.clone(), x, y) {
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
    selection: Option<Selection>,
    /// Behind selected text.
    selection_color: Rgba8,
    /// What the pixels show. `None` until the first render.
    drawn: Option<Drawn>,
}

/// What a render depends on, to tell whether the pixels are still right.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawn {
    scroll: f64,
    layout: usize,
    background: Rgba8,
    selection: Option<Selection>,
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
            selection: None,
            selection_color: [0xe4, 0xdc, 0xfa, 0xff],
            drawn: None,
        }
    }

    /// What a render would draw now, to compare with what was drawn.
    fn state(&self) -> Drawn {
        let layout = self
            .layout
            .as_ref()
            .map_or(0, |layout| Arc::as_ptr(layout) as usize);
        Drawn {
            scroll: self.scroll,
            layout,
            background: self.background,
            selection: self.selection,
        }
    }

    /// Forget what was drawn, so the next [`Self::needs_render`] says yes:
    /// for when the host's copy of the pixels was replaced by something else.
    pub fn invalidate(&mut self) {
        self.drawn = None;
    }

    /// Whether [`Self::render`] would draw anything different from last time.
    pub fn needs_render(&self) -> bool {
        self.drawn != Some(self.state())
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
        self.drawn = None;
        self.clamp();
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    pub fn set_background(&mut self, background: Rgba8) {
        self.background = background;
    }

    pub fn set_selection_color(&mut self, color: Rgba8) {
        self.selection_color = color;
    }

    /// Show a new layout. The scroll position is kept in pixels; the caller
    /// moves it with [`Self::scroll_to_position`] if it should follow the
    /// source instead. A selection is dropped: its text may have moved.
    pub fn set_layout(&mut self, layout: Arc<PreviewLayout>) {
        self.layout = Some(layout);
        self.selection = None;
        self.clamp();
    }

    // --- Selection ---

    /// Start a selection at a point in physical pixels relative to the view,
    /// or with `extend` move the end of the current one there.
    pub fn press(&mut self, x: f64, y: f64, extend: bool) {
        let Some(point) = self
            .layout
            .as_ref()
            .and_then(|layout| layout.point_at(x, y + self.scroll))
        else {
            self.selection = None;
            return;
        };
        self.selection = match self.selection {
            Some(selection) if extend => Some(Selection {
                anchor: selection.anchor,
                focus: point,
            }),
            _ => Some(Selection::caret(point)),
        };
    }

    /// Move the end of the selection being dragged.
    pub fn drag(&mut self, x: f64, y: f64) {
        let Some(selection) = self.selection else {
            return;
        };
        if let Some(point) = self
            .layout
            .as_ref()
            .and_then(|layout| layout.point_at(x, y + self.scroll))
        {
            self.selection = Some(Selection {
                anchor: selection.anchor,
                focus: point,
            });
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.layout.as_ref().and_then(|layout| layout.select_all());
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The selected text, if anything is selected.
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection.filter(|selection| !selection.is_empty())?;
        let text = self.layout.as_ref()?.selected_text(&selection);
        (!text.is_empty()).then_some(text)
    }

    // --- Scrolling by keyboard and by link ---

    /// Height of the view in physical pixels.
    pub fn view_height(&self) -> f64 {
        f64::from(self.height)
    }

    /// Scroll to an offset in physical pixels; returns whether the view moved.
    pub fn scroll_to(&mut self, offset: f64) -> bool {
        let before = self.scroll;
        self.scroll = offset;
        self.clamp();
        self.scroll != before
    }

    /// Follow a link inside the document (`#fn:…`, `#fnref:…`): scroll so its
    /// target is a little below the top. Returns false if there is no such
    /// target.
    pub fn follow(&mut self, link: &str) -> bool {
        let Some(name) = link.strip_prefix('#') else {
            return false;
        };
        let Some(target) = self.layout.as_ref().and_then(|layout| layout.target(name)) else {
            return false;
        };
        let margin = 24.0 * f64::from(self.scale);
        self.scroll_to(target - margin);
        true
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
        self.drawn = Some(self.state());
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
            selection,
            selection_color,
            ..
        } = self;
        let (w, h) = (f64::from(*width), f64::from(*height));
        ctx.reset();
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(color(*background));
        ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        if let Some(layout) = layout {
            let (top, bottom) = (*scroll, *scroll + h);
            let view = Affine::translate((0.0, -*scroll));
            for item in &layout.items {
                if item.bottom < top || item.top > bottom {
                    continue;
                }
                ctx.set_transform(view);
                draw_shape(ctx, &item.shape, *scale);
            }
            let visible = layout
                .texts
                .iter()
                .enumerate()
                .filter(|(_, text)| text.bottom() >= top && text.y <= bottom);
            // The selection goes under the text.
            if let Some(selection) = selection.filter(|s| !s.is_empty()) {
                ctx.set_transform(view);
                ctx.set_paint(color(*selection_color));
                for (index, text) in visible.clone() {
                    if !text.selectable {
                        continue;
                    }
                    let Some(range) = selection.range_in(index, text.paragraph.text().len()) else {
                        continue;
                    };
                    for [x0, y0, x1, y1] in text.paragraph.rects(range) {
                        ctx.fill_rect(&Rect::new(
                            text.x + f64::from(x0),
                            text.y + f64::from(y0),
                            text.x + f64::from(x1),
                            text.y + f64::from(y1),
                        ));
                    }
                }
            }
            for (_, text) in visible {
                ctx.set_transform(view * Affine::translate((text.x, text.y)));
                draw_text(
                    ctx,
                    resources,
                    &text.paragraph,
                    (top - text.y) as f32,
                    (bottom - text.y) as f32,
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

/// Draw one shape in the current transform.
fn draw_shape(ctx: &mut RenderContext, shape: &Piece, scale: f32) {
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
    }
}

/// Draw the lines of a paragraph between `top` and `bottom` (relative to the
/// paragraph), in the current transform.
fn draw_text(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    paragraph: &Paragraph,
    top: f32,
    bottom: f32,
    scale: f32,
    buffer: &mut Vec<Glyph>,
) {
    for run in paragraph.draw_runs(top, bottom) {
        buffer.clear();
        buffer.extend(run.glyphs.iter().map(|glyph| Glyph {
            id: glyph.id,
            x: glyph.x,
            y: glyph.y,
        }));
        let face = glyphs::Face {
            font: &run.face.font,
            size: run.face.size,
            coords: &run.face.coords,
            embolden: run.face.embolden,
            skew: run.skew,
        };
        glyphs::draw_face(ctx, resources, &face, color(run.color), buffer);
    }
    for line in paragraph.decorations(top, bottom, scale) {
        ctx.set_paint(color(line.color));
        ctx.fill_rect(&Rect::new(
            f64::from(line.x0),
            f64::from(line.y),
            f64::from(line.x1),
            f64::from(line.y + line.thickness),
        ));
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
            selection: [0xe4, 0xdc, 0xfa, 0xff],
            callouts: [
                [0x09, 0x69, 0xda, 0xff],
                [0x1a, 0x7f, 0x37, 0xff],
                [0x82, 0x50, 0xdf, 0xff],
                [0x9a, 0x67, 0x00, 0xff],
                [0xd1, 0x24, 0x2f, 0xff],
            ],
            callout_labels: ["注意", "提示", "重要", "警告", "当心"],
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
    fn an_unchanged_view_is_not_drawn_again() {
        let mut view = view("# 一\n\n二", 800, 600);
        assert!(view.needs_render());
        view.render();
        assert!(!view.needs_render());
        view.scroll_by(10.0);
        // The document fits: nothing moved.
        assert!(!view.needs_render());
        view.set_background([0, 0, 0, 0xff]);
        assert!(view.needs_render());
        view.render();
        view.resize(700, 600, 2.0);
        assert!(view.needs_render());
        view.render();
        view.invalidate();
        assert!(view.needs_render());
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

    const MIXED: &str = "# 标题\n\n排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。日本語の組版では、行頭に句読点や閉じ括弧（」）、小書きの仮名（ゃ・ゅ・ょ・っ）などを置くことはできません。\n\n- 第一项\n- 第二项\n\n| 甲 | 乙 |\n|---|---|\n| 一 | 二 |\n";

    #[test]
    fn preview_paragraphs_follow_the_line_breaking_rules() {
        for width in [300u16, 420, 560, 700] {
            let layout = PreviewBuilder::new().build(MIXED, width, 1.0, &style());
            let text = &layout.texts[1].paragraph;
            let ranges = text.line_ranges();
            assert!(ranges.len() > 1, "width {width}");
            for (n, range) in ranges.iter().enumerate().skip(1) {
                let first = text.text()[range.clone()].chars().next().unwrap();
                let after_emergency = text.lines()[n - 1].kind == aster_text::BreakKind::Emergency;
                assert!(
                    after_emergency || !"。，、」』）》：；ゃゅょっ”".contains(first),
                    "width {width}: line {n} starts with {first}"
                );
            }
        }
    }

    #[test]
    fn selecting_and_copying() {
        let mut view = view(MIXED, 900, 2000);
        let layout = view.layout().unwrap().clone();
        // Drag from the start of the heading to the middle of the paragraph.
        let heading = &layout.texts[0];
        let paragraph = &layout.texts[1];
        view.press(heading.x + 1.0, heading.y + 4.0, false);
        let line = &paragraph.paragraph.lines()[0];
        view.drag(
            paragraph.x + f64::from(line.width) / 2.0,
            paragraph.y + f64::from(line.top + line.height / 2.0),
        );
        let copied = view.selected_text().unwrap();
        assert!(copied.starts_with("标题\n排版引擎"), "{copied}");
        assert!(copied.len() < paragraph.paragraph.text().len());
        // Select all: list markers are left out, table cells are tab separated.
        view.select_all();
        let all = view.selected_text().unwrap();
        assert!(all.starts_with("标题\n"), "{all}");
        assert!(all.contains("第一项\n第二项"), "{all}");
        assert!(!all.contains('•'), "{all}");
        assert!(all.ends_with("甲\t乙\n一\t二"), "{all}");
        // Drawn: the selection colour appears.
        view.select_all();
        let data = view.render().data_as_u8_slice().to_vec();
        let (pixels, _) = data.as_chunks::<4>();
        assert!(pixels.iter().any(|px| *px == style().selection));
        view.clear_selection();
        assert!(view.selected_text().is_none());
        // A new layout drops the selection.
        view.select_all();
        view.set_layout(Arc::new(PreviewBuilder::new().build(
            "新",
            900,
            2.0,
            &style(),
        )));
        assert!(view.selection().is_none());
    }

    #[test]
    fn footnotes_link_both_ways() {
        let filler: String = (0..60).map(|i| format!("第 {i} 段。\n\n")).collect();
        let source = format!("正文[^注]。\n\n{filler}[^注]: 脚注的内容。\n");
        let mut view = view(&source, 900, 600);
        let layout = view.layout().unwrap().clone();
        let note = layout.target("fn:注").expect("note target");
        let back = layout.target("fnref:注").expect("reference target");
        assert!(note > back + 1000.0);
        // The target ends up just below the top: 24 logical pixels.
        assert!(view.follow("#fn:注"));
        let (scroll, max) = view.scroll_range();
        assert_eq!(scroll, (note - 48.0).min(max));
        assert!(view.follow("#fnref:注"));
        assert_eq!(view.scroll_range().0, (back - 48.0).max(0.0));
        assert!(!view.follow("#nowhere"));
        assert!(!view.follow("https://example.com"));
        let urls: Vec<&str> = layout.links.iter().map(|(_, url)| url.as_str()).collect();
        assert!(urls.contains(&"#fn:注") && urls.contains(&"#fnref:注"));
    }

    #[test]
    fn callouts_have_a_title_in_their_colour() {
        let mut view = view("> [!WARNING]\n> 小心。\n", 900, 400);
        let layout = view.layout().unwrap().clone();
        assert_eq!(layout.texts[0].paragraph.text(), "警告");
        assert!(!layout.texts[0].selectable);
        let data = view.render().data_as_u8_slice().to_vec();
        let (pixels, _) = data.as_chunks::<4>();
        assert!(pixels.iter().any(|px| *px == style().callouts[3]));
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

    /// Made-up italics lean forward: the ink of the top of a glyph sits
    /// further right than the ink of its bottom.
    #[test]
    fn made_up_italics_lean_forward() {
        let layout = PreviewBuilder::new().build("这", 300, 2.0, &style());
        let runs = layout.texts[0].paragraph.draw_runs(0.0, 1000.0);
        let run = &runs[0];
        let lean = |skew: Option<f32>| {
            let mut ctx = RenderContext::new(200, 200);
            let mut resources = Resources::new();
            let mut pixmap = Pixmap::new(200, 200);
            ctx.set_paint(Color::from_rgba8(255, 255, 255, 255));
            ctx.fill_rect(&Rect::new(0.0, 0.0, 200.0, 200.0));
            ctx.set_transform(Affine::translate((60.0, 140.0)));
            let face = glyphs::Face {
                font: &run.face.font,
                size: 100.0,
                coords: &run.face.coords,
                embolden: false,
                skew,
            };
            let glyph = Glyph {
                id: run.glyphs[0].id,
                x: 0.0,
                y: 0.0,
            };
            glyphs::draw_face(&mut ctx, &mut resources, &face, Color::BLACK, &[glyph]);
            ctx.flush();
            ctx.render(&mut pixmap, &mut resources);
            let data = pixmap.data_as_u8_slice();
            let centre = |rows: std::ops::Range<usize>| {
                let (mut sum, mut count) = (0.0, 0.0);
                for y in rows {
                    for x in 0..200 {
                        let ink = 255.0 - f64::from(data[(y * 200 + x) * 4]);
                        sum += ink * x as f64;
                        count += ink;
                    }
                }
                sum / count.max(1.0)
            };
            // Top third of the glyph minus bottom third.
            centre(50..80) - centre(110..140)
        };
        let upright = lean(Some(0.0));
        let slanted = lean(Some(14.0));
        assert!(
            slanted > upright + 5.0,
            "upright {upright}, slanted {slanted}"
        );
    }
}
