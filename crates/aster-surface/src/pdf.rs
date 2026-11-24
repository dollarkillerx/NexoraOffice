//! A Markdown document as a PDF that looks like the preview.
//!
//! The document is laid out by the preview's own code ([`PreviewBuilder`]),
//! one logical pixel to one point, as wide as the text area of a page. The
//! display list that comes out is then cut into pages and written with
//! krilla: text as real text in the fonts the preview used (embedded,
//! subsetted, with a ToUnicode map so that it can be selected, copied and
//! searched), rules and boxes as paths, pictures as images, web links as
//! link annotations and footnote links as jumps inside the document.
//!
//! Page breaks never cut through a line of text, a table row, a picture or
//! a box unless it is taller than a page, and a heading is never left alone
//! at the bottom of a page: [`paginate`] works on the heights only and is
//! tested on its own.
//!
//! Nothing here keeps global state: [`export_pdf`] loads its own fonts and
//! can run on any thread.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use aster_markdown::preview::{Block, BlockKind, FrontMatterLine, Outline};
use aster_text::{Align, Family, Paragraph, Rules, Spec, Style as TextStyle};
use krilla::action::{Action, LinkAction};
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::destination::{Destination, XyzDestination};
use krilla::geom::{PathBuilder, Point, Rect as PdfRect, Size, Transform};
use krilla::image::Image;
use krilla::metadata::Metadata;
use krilla::num::NormalizedF32;
use krilla::outline::{Outline as Bookmarks, OutlineNode};
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule, Stroke};
use krilla::surface::Surface;
use krilla::text::{Font, GlyphId, KrillaGlyph, Tag};
use krilla::{Data, Document, SerializeSettings};
use vello_cpu::Pixmap;
use vello_cpu::kurbo::{PathEl, Rect, RoundedRect, Shape};

use crate::preview::{Painted, Piece, PreviewBuilder, PreviewLayout, PreviewStyle};
use crate::text_surface::{DEFAULT_LANGUAGE, PageLayout, Rgba8};

/// Outline width of made-up bold, as a fraction of the font size: the same
/// as on screen.
const SYNTHETIC_BOLD: f32 = 0.035;
/// Heights closer than this are the same.
const EPSILON: f64 = 0.01;

/// The size of the paper and the white space around the text, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPage {
    pub width_pt: f32,
    pub height_pt: f32,
    /// On all four sides. Page numbers go in the middle of the bottom one.
    pub margin_pt: f32,
}

impl PdfPage {
    /// ISO A4, 210 × 297 mm, with 20 mm margins.
    pub const A4: PdfPage = PdfPage {
        width_pt: 595.276,
        height_pt: 841.89,
        margin_pt: 56.693,
    };
    /// US Letter, 8.5 × 11 in, with ¾ in margins.
    pub const LETTER: PdfPage = PdfPage {
        width_pt: 612.0,
        height_pt: 792.0,
        margin_pt: 54.0,
    };

    /// Width of the text, in points.
    fn text_width(&self) -> f64 {
        f64::from(self.width_pt - 2.0 * self.margin_pt)
    }

    /// Height of the text on one page, in points.
    fn text_height(&self) -> f64 {
        f64::from(self.height_pt - 2.0 * self.margin_pt)
    }
}

/// Export `source` as a PDF. `base` is the document's folder, for pictures
/// with relative addresses (`None` for a document never saved). The style's
/// colours should be the light ones, as the paper is white; its font size is
/// taken as points. Its page layout is ignored: `page` sets the width.
pub fn export_pdf(
    source: &str,
    base: Option<&Path>,
    style: &PreviewStyle,
    page: PdfPage,
) -> Result<Vec<u8>, String> {
    export(source, base, style, page, true)
}

/// Like [`export_pdf`]; without compression the content streams, the
/// ToUnicode maps and the document information can be read in the bytes.
fn export(
    source: &str,
    base: Option<&Path>,
    style: &PreviewStyle,
    page: PdfPage,
    compress: bool,
) -> Result<Vec<u8>, String> {
    if !(page.text_width() >= 1.0 && page.text_height() >= 1.0 && page.margin_pt >= 0.0) {
        return Err("the margins leave no room on the page".into());
    }
    let mut plan = Plan::new(source, base, style, page);
    let settings = SerializeSettings {
        compress_content_streams: compress,
        enable_tagging: false,
        ..SerializeSettings::default()
    };
    let mut document = Document::new_with(settings);
    let mut writer = Writer::default();
    let (width, height) = (page.width_pt, page.height_pt);
    let margin = f64::from(page.margin_pt);
    let count = plan.pages.len();
    for index in 0..count {
        let (top, bottom) = plan.pages[index];
        let settings = PageSettings::from_wh(width, height).ok_or("the page has no size")?;
        let mut pdf_page = document.start_page_with(settings);
        let mut surface = pdf_page.surface();
        // From the document to this page.
        let shift = Transform::from_translate(page.margin_pt, (margin - top) as f32);
        surface.push_transform(&shift);
        let clip = PdfRect::from_ltrb(-page.margin_pt, top as f32, width, bottom as f32).and_then(
            |rect| {
                let mut builder = PathBuilder::new();
                builder.push_rect(rect);
                builder.finish()
            },
        );
        if let Some(clip) = &clip {
            surface.push_clip_path(clip, &FillRule::NonZero);
        }
        for item in plan.layout.items() {
            if overlaps(item.top, item.bottom, top, bottom) {
                writer.shape(&mut surface, item);
            }
        }
        for text in plan.layout.texts() {
            let ranges = text.paragraph.line_ranges();
            for (line, range) in text.paragraph.lines().iter().zip(ranges) {
                let line_top = text.y + f64::from(line.top);
                let line_bottom = line_top + f64::from(line.height);
                if overlaps(line_top, line_bottom, top, bottom) {
                    writer.line(&mut surface, &text.paragraph, line, range, text.x, text.y);
                }
            }
        }
        if clip.is_some() {
            surface.pop();
        }
        surface.pop();
        // The page number, small, in the middle of the bottom margin.
        let number = plan.number(index + 1);
        if let Some(line) = number.lines().first() {
            let x = (f64::from(width) - f64::from(number.width())) / 2.0;
            let y = f64::from(height) - margin / 2.0 - f64::from(line.height) / 2.0;
            let range = number.line_ranges().swap_remove(0);
            writer.line(&mut surface, &number, line, range, x, y);
        }
        surface.finish();
        for annotation in plan.links(index, margin) {
            pdf_page.add_annotation(annotation);
        }
        pdf_page.finish();
    }
    let mut metadata = Metadata::new().creator("Aster Office".into());
    if let Some(title) = plan.title() {
        metadata = metadata.title(title);
    }
    document.set_metadata(metadata);
    document.set_outline(plan.bookmarks(margin));
    document
        .finish()
        .map_err(|err| format!("cannot write the PDF: {err:?}"))
}

/// Whether `top..bottom` and `from..to` share more than a hair.
fn overlaps(top: f64, bottom: f64, from: f64, to: f64) -> bool {
    bottom > from + EPSILON && top < to - EPSILON
}

/// A heading of the document, for the bookmarks.
struct Heading {
    level: u8,
    title: String,
    source: Range<usize>,
}

/// The document laid out and cut into pages.
struct Plan {
    builder: PreviewBuilder,
    style: PreviewStyle,
    outline: Outline,
    layout: PreviewLayout,
    /// Document heights from and to, one pair per page.
    pages: Vec<(f64, f64)>,
}

impl Plan {
    fn new(source: &str, base: Option<&Path>, style: &PreviewStyle, page: PdfPage) -> Self {
        let mut builder = PreviewBuilder::new();
        builder.set_base(base.map(Path::to_path_buf));
        // The text area is the page: no margin, no padding, no centring.
        let style = PreviewStyle {
            page: PageLayout {
                margin: 0.0,
                padding: 0.0,
                max_width: None,
                min_aspect: None,
            },
            ..*style
        };
        let outline = aster_markdown::preview::outline(source);
        let width = page.text_width().floor().clamp(1.0, f64::from(u16::MAX)) as u16;
        let layout = builder.build_outline(&outline, width, 1.0, &style);
        let height = page.text_height();
        let mut headings = Vec::new();
        collect_headings(&outline.blocks, &mut headings);
        let (keeps, marks) = measure(&layout, f64::from(style.font_size));
        let keeps = keep_with_next(&keeps, &heading_spans(&layout, &headings), height);
        let pages = paginate(&keeps, &marks, height);
        Self {
            builder,
            style,
            outline,
            layout,
            pages,
        }
    }

    /// The page a document height is on.
    fn page_of(&self, y: f64) -> usize {
        self.pages
            .iter()
            .rposition(|(top, _)| *top <= y + EPSILON)
            .unwrap_or(0)
    }

    /// A place in the document as a destination on its page.
    fn destination(&self, y: f64, margin: f64) -> XyzDestination {
        let index = self.page_of(y);
        let top = self.pages.get(index).map_or(0.0, |(top, _)| *top);
        let at = Point::from_xy(0.0, (y - top + margin).max(0.0) as f32);
        XyzDestination::new(index, at)
    }

    /// A page number laid out in the preview's interface font.
    fn number(&mut self, number: usize) -> Paragraph {
        let text = number.to_string();
        let base = TextStyle {
            family: Family::SystemUi,
            size: (self.style.font_size * 0.75).max(7.0),
            color: self.style.secondary,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            rise: 0.0,
        };
        let spec = Spec {
            text: &text,
            base,
            spans: &[],
            language: DEFAULT_LANGUAGE,
            width: None,
            scale: 1.0,
            line_height: 1.2,
            align: Align::Start,
            rules: Rules::for_language(DEFAULT_LANGUAGE),
        };
        self.builder.shaper().lay_out(&spec)
    }

    /// The links on a page: web and e-mail links that the preview would
    /// open, and jumps to footnotes and back.
    fn links(&self, index: usize, margin: f64) -> Vec<Annotation> {
        let (top, bottom) = self.pages[index];
        let mut out = Vec::new();
        for (rect, url) in self.layout.links() {
            if !overlaps(rect.y0, rect.y1, top, bottom) {
                continue;
            }
            let target = if aster_platform::links::is_openable(url) {
                Target::Action(Action::Link(LinkAction::new(url.clone())))
            } else if let Some(y) = url
                .strip_prefix('#')
                .and_then(|name| self.layout.target(name))
            {
                Target::Destination(Destination::Xyz(self.destination(y, margin)))
            } else {
                continue;
            };
            let y0 = rect.y0.max(top) - top + margin;
            let y1 = rect.y1.min(bottom) - top + margin;
            let Some(area) = PdfRect::from_ltrb(
                (rect.x0 + margin) as f32,
                y0 as f32,
                (rect.x1 + margin) as f32,
                y1 as f32,
            ) else {
                continue;
            };
            out.push(Annotation::new_link(
                LinkAnnotation::new(area, target),
                None,
            ));
        }
        out
    }

    /// The title: a `title` field of front matter, or the first heading.
    fn title(&self) -> Option<String> {
        let from_front_matter = self
            .outline
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockKind::FrontMatter(lines) => lines.iter().find_map(|line| match line {
                    FrontMatterLine::Field { key, value }
                        if key.eq_ignore_ascii_case("title") && !value.trim().is_empty() =>
                    {
                        Some(unquote(value.trim()).to_owned())
                    }
                    _ => None,
                }),
                _ => None,
            });
        from_front_matter.or_else(|| {
            let mut headings = Vec::new();
            collect_headings(&self.outline.blocks, &mut headings);
            headings
                .into_iter()
                .map(|heading| heading.title)
                .find(|title| !title.trim().is_empty())
        })
    }

    /// The headings as the PDF's bookmarks, nested by level.
    fn bookmarks(&self, margin: f64) -> Bookmarks {
        let mut headings = Vec::new();
        collect_headings(&self.outline.blocks, &mut headings);
        let anchors = self.layout.anchors();
        let entries: Vec<(u8, String, XyzDestination)> = headings
            .into_iter()
            .filter(|heading| !heading.title.trim().is_empty())
            .filter_map(|heading| {
                let anchor = anchors.iter().find(|a| a.source == heading.source)?;
                Some((
                    heading.level,
                    heading.title,
                    self.destination(anchor.top, margin),
                ))
            })
            .collect();
        let mut bookmarks = Bookmarks::new();
        let mut at = 0;
        for node in nest(&entries, &mut at, 0) {
            bookmarks.push_child(node);
        }
        bookmarks
    }
}

/// Bookmarks below `level`, from `entries[*at..]`, until one at `level` or
/// above.
fn nest(entries: &[(u8, String, XyzDestination)], at: &mut usize, level: u8) -> Vec<OutlineNode> {
    let mut out = Vec::new();
    while let Some((own, title, destination)) = entries.get(*at) {
        if *own <= level {
            break;
        }
        *at += 1;
        let mut node = OutlineNode::new(title.clone(), destination.clone());
        for child in nest(entries, at, *own) {
            node.push_child(child);
        }
        out.push(node);
    }
    out
}

/// A front matter value without the quotes YAML allows around it.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// The headings among `blocks` and the blocks inside them, in document order.
fn collect_headings(blocks: &[Block], out: &mut Vec<Heading>) {
    for block in blocks {
        match &block.kind {
            BlockKind::Heading { level, text } => out.push(Heading {
                level: *level,
                title: text.text.clone(),
                source: block.source.clone(),
            }),
            BlockKind::Quote(blocks) | BlockKind::Callout { blocks, .. } => {
                collect_headings(blocks, out)
            }
            BlockKind::List { items, .. } => {
                for item in items {
                    collect_headings(&item.blocks, out);
                }
            }
            BlockKind::Footnotes(notes) => {
                for note in notes {
                    collect_headings(&note.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// Where the headings ended up, top to bottom.
fn heading_spans(layout: &PreviewLayout, headings: &[Heading]) -> Vec<Keep> {
    let sources: HashSet<&Range<usize>> = headings.iter().map(|h| &h.source).collect();
    layout
        .anchors()
        .iter()
        .filter(|anchor| sources.contains(&anchor.source))
        .map(|anchor| Keep {
            top: anchor.top,
            bottom: anchor.bottom,
        })
        .collect()
}

/// A stretch of the document that a page break should not cut through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Keep {
    pub(crate) top: f64,
    pub(crate) bottom: f64,
}

impl Keep {
    fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// What must stay whole in a laid-out document, and the extent of
/// everything drawn. Whole: each line of text, each picture and box, each
/// row of a table or of front matter (texts side by side, from the grid
/// line above them down to the one below, which belongs to the next row).
fn measure(layout: &PreviewLayout, em: f64) -> (Vec<Keep>, Vec<Keep>) {
    let mut keeps = Vec::new();
    let mut marks = Vec::new();
    for item in layout.items() {
        let span = Keep {
            top: item.top,
            bottom: item.bottom,
        };
        marks.push(span);
        if matches!(
            item.shape,
            Piece::Picture { .. } | Piece::Frame { .. } | Piece::Check { .. }
        ) {
            keeps.push(span);
        }
    }
    // Rows: selectable texts that start at the same height, side by side.
    let mut rows: HashMap<u64, (usize, f64, f64, f64)> = HashMap::new();
    for text in layout.texts() {
        for line in text.paragraph.lines() {
            let top = text.y + f64::from(line.top);
            let span = Keep {
                top,
                bottom: top + f64::from(line.height),
            };
            if span.height() > 0.0 {
                keeps.push(span);
                marks.push(span);
            }
        }
        if text.selectable {
            let right = text.x + f64::from(text.paragraph.width());
            let bottom = text.y + f64::from(text.paragraph.height());
            let row = rows
                .entry(text.y.to_bits())
                .or_insert((0, text.x, right, bottom));
            row.0 += 1;
            row.1 = row.1.min(text.x);
            row.2 = row.2.max(right);
            row.3 = row.3.max(bottom);
        }
    }
    let rules: Vec<&Rect> = layout
        .items()
        .iter()
        .filter_map(|item| match &item.shape {
            Piece::Rect { rect, .. } if rect.height() <= 2.0 => Some(rect),
            _ => None,
        })
        .collect();
    for (bits, (count, left, right, bottom)) in rows {
        if count < 2 {
            continue;
        }
        let mut row = Keep {
            top: f64::from_bits(bits),
            bottom,
        };
        let across = |rect: &&&Rect| rect.x0 <= left + 1.0 && rect.x1 >= right - 1.0;
        if let Some(above) = rules
            .iter()
            .filter(across)
            .filter(|rect| rect.y0 <= row.top && rect.y0 >= row.top - em)
            .map(|rect| rect.y0)
            .reduce(f64::min)
        {
            row.top = above;
        }
        if let Some(below) = rules
            .iter()
            .filter(across)
            .filter(|rect| rect.y0 >= row.bottom - EPSILON && rect.y0 <= row.bottom + em)
            .map(|rect| rect.y0)
            .reduce(f64::min)
        {
            row.bottom = below;
        }
        keeps.push(row);
    }
    keeps.sort_by(|a, b| a.top.total_cmp(&b.top));
    marks.sort_by(|a, b| a.top.total_cmp(&b.top));
    (keeps, marks)
}

/// Adds, for each heading, a stretch from its top to the bottom of the first
/// thing after it, so that a heading is never the last thing on a page. A
/// heading whose stretch is taller than a page is left to itself.
pub(crate) fn keep_with_next(keeps: &[Keep], headings: &[Keep], page: f64) -> Vec<Keep> {
    let mut out = keeps.to_vec();
    for heading in headings {
        let next = keeps
            .iter()
            .filter(|keep| keep.top >= heading.bottom - EPSILON)
            .min_by(|a, b| a.top.total_cmp(&b.top));
        let together = Keep {
            top: heading.top,
            bottom: next.map_or(heading.bottom, |next| next.bottom.max(heading.bottom)),
        };
        if together.height() <= page {
            out.push(together);
        } else if heading.height() <= page {
            out.push(*heading);
        }
    }
    out
}

/// Cut a document into pages `page` high. `keeps` are what a break must not
/// cut through, if they fit on a page; `marks` are everything drawn, so
/// that a page does not start with empty space. Returns the document
/// heights each page shows, from and to.
pub(crate) fn paginate(keeps: &[Keep], marks: &[Keep], page: f64) -> Vec<(f64, f64)> {
    let page = page.max(1.0);
    let Some(start) = marks.iter().map(|m| m.top).reduce(f64::min) else {
        return vec![(0.0, 0.0)];
    };
    let end = marks
        .iter()
        .map(|m| m.bottom)
        .fold(f64::NEG_INFINITY, f64::max);
    let fitting: Vec<&Keep> = keeps.iter().filter(|k| k.height() <= page).collect();
    let mut pages = Vec::new();
    let mut top = start;
    loop {
        if end - top <= page + EPSILON {
            pages.push((top, end));
            break;
        }
        // Move the break up to the top of whatever it would cut, until it
        // cuts nothing that could fit on the next page.
        let mut cut = top + page;
        loop {
            let moved = fitting
                .iter()
                .filter(|k| k.top > top + EPSILON && k.top < cut && k.bottom > cut + EPSILON)
                .map(|k| k.top)
                .fold(cut, f64::min);
            if moved >= cut {
                break;
            }
            cut = moved;
        }
        pages.push((top, cut));
        // The next page starts with the first thing drawn at or after the
        // break, unless something drawn goes on across it.
        let across = marks
            .iter()
            .any(|m| m.top < cut - EPSILON && m.bottom > cut + EPSILON);
        top = if across {
            cut
        } else {
            match marks
                .iter()
                .filter(|m| m.top >= cut - EPSILON)
                .map(|m| m.top)
                .reduce(f64::min)
            {
                Some(next) => next.max(cut),
                None => break,
            }
        };
    }
    pages
}

/// What the pages share: fonts and pictures, each embedded once.
#[derive(Default)]
struct Writer {
    fonts: HashMap<(u64, u32, Vec<i16>), Option<Font>>,
    images: HashMap<usize, Image>,
}

fn paint(color: Rgba8) -> (krilla::paint::Paint, NormalizedF32) {
    let paint = rgb::Color::new(color[0], color[1], color[2]).into();
    let opacity = NormalizedF32::new(f32::from(color[3]) / 255.0).unwrap_or(NormalizedF32::ONE);
    (paint, opacity)
}

fn fill(color: Rgba8) -> Fill {
    let (paint, opacity) = paint(color);
    Fill {
        paint,
        opacity,
        rule: FillRule::NonZero,
    }
}

fn stroke(color: Rgba8, width: f64) -> Stroke {
    let (paint, opacity) = paint(color);
    Stroke {
        paint,
        width: width as f32,
        opacity,
        ..Stroke::default()
    }
}

/// A kurbo shape as a PDF path.
fn path(shape: &impl Shape) -> Option<krilla::geom::Path> {
    let mut builder = PathBuilder::new();
    let f = |value: f64| value as f32;
    for element in shape.path_elements(0.1) {
        match element {
            PathEl::MoveTo(p) => builder.move_to(f(p.x), f(p.y)),
            PathEl::LineTo(p) => builder.line_to(f(p.x), f(p.y)),
            PathEl::QuadTo(a, p) => builder.quad_to(f(a.x), f(a.y), f(p.x), f(p.y)),
            PathEl::CurveTo(a, b, p) => {
                builder.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(p.x), f(p.y))
            }
            PathEl::ClosePath => builder.close(),
        }
    }
    builder.finish()
}

impl Writer {
    /// Draw one of the preview's shapes, in document coordinates.
    fn shape(&mut self, surface: &mut Surface<'_>, item: &Painted) {
        match &item.shape {
            Piece::Rect {
                rect,
                color,
                radius,
            } => {
                let shape = if *radius > 0.0 {
                    path(&RoundedRect::from_rect(*rect, *radius))
                } else {
                    path(rect)
                };
                if let Some(shape) = shape {
                    surface.set_fill(Some(fill(*color)));
                    surface.set_stroke(None);
                    surface.draw_path(&shape);
                }
            }
            Piece::Frame { rect, color, width } => {
                let inset = rect.inset(-width / 2.0);
                if let Some(shape) = path(&RoundedRect::from_rect(inset, 3.0)) {
                    surface.set_fill(None);
                    surface.set_stroke(Some(stroke(*color, *width)));
                    surface.draw_path(&shape);
                }
            }
            Piece::Check { rect, color, width } => {
                let (x0, y0, side) = (rect.x0, rect.y0, rect.width());
                let mut builder = PathBuilder::new();
                builder.move_to((x0 + side * 0.24) as f32, (y0 + side * 0.52) as f32);
                builder.line_to((x0 + side * 0.43) as f32, (y0 + side * 0.72) as f32);
                builder.line_to((x0 + side * 0.77) as f32, (y0 + side * 0.3) as f32);
                if let Some(shape) = builder.finish() {
                    surface.set_fill(None);
                    surface.set_stroke(Some(stroke(*color, *width)));
                    surface.draw_path(&shape);
                }
            }
            Piece::Picture { rect, pixmap } => {
                let image = self
                    .images
                    .entry(Arc::as_ptr(pixmap) as usize)
                    .or_insert_with(|| image(pixmap))
                    .clone();
                if let Some(size) = Size::from_wh(rect.width() as f32, rect.height() as f32) {
                    surface
                        .push_transform(&Transform::from_translate(rect.x0 as f32, rect.y0 as f32));
                    surface.draw_image(image, size);
                    surface.pop();
                }
            }
        }
        surface.set_stroke(None);
    }

    /// The font of a run, once per font file, face and instance.
    fn font(&mut self, face: &aster_text::Face) -> Option<Font> {
        let key = (face.font.data.id(), face.font.index, face.coords.clone());
        self.fonts
            .entry(key)
            .or_insert_with(|| {
                let bytes = face.font.data.clone();
                let variations = variations(bytes.as_ref(), face.font.index, &face.coords);
                let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
                Font::new_variable(Data::from(data), face.font.index, &variations)
            })
            .clone()
    }

    /// Draw one line of a laid-out text whose top left corner is at
    /// `(x, y)`: its glyphs as text, then its underlines and strikethroughs.
    /// `range` is the line's text, without its line break.
    fn line(
        &mut self,
        surface: &mut Surface<'_>,
        paragraph: &Paragraph,
        line: &aster_text::LineBox,
        range: Range<usize>,
        x: f64,
        y: f64,
    ) {
        let middle = line.top + line.height / 2.0;
        let runs = paragraph.draw_runs(middle, middle);
        let clusters = clusters(paragraph, &runs, middle, range);
        let text = paragraph.text();
        let mut next = 0;
        for run in &runs {
            let Some(font) = self.font(run.face) else {
                next += run.glyphs.len();
                continue;
            };
            let Some(first) = run.glyphs.first() else {
                continue;
            };
            let size = run.face.size;
            let glyphs: Vec<KrillaGlyph> = run
                .glyphs
                .iter()
                .enumerate()
                .map(|(index, glyph)| {
                    let advance = run
                        .glyphs
                        .get(index + 1)
                        .map_or(0.0, |after| after.x - glyph.x);
                    KrillaGlyph {
                        glyph_id: GlyphId::new(glyph.id),
                        text_range: clusters[next + index].clone(),
                        x_advance: advance / size,
                        x_offset: 0.0,
                        y_offset: (first.y - glyph.y) / size,
                        y_advance: 0.0,
                        location: None,
                    }
                })
                .collect();
            next += run.glyphs.len();
            let origin = (x + f64::from(first.x), y + f64::from(first.y));
            let skew = run.skew.map(|degrees| degrees.to_radians().tan());
            if let Some(tan) = skew {
                // Lean the tops to the right, about the baseline.
                let baseline = origin.1 as f32;
                surface.push_transform(&Transform::from_row(
                    1.0,
                    0.0,
                    -tan,
                    1.0,
                    tan * baseline,
                    0.0,
                ));
            }
            surface.set_fill(Some(fill(run.color)));
            surface.set_stroke(
                run.face
                    .embolden
                    .then(|| stroke(run.color, f64::from(size * SYNTHETIC_BOLD))),
            );
            surface.draw_glyphs(
                Point::from_xy(origin.0 as f32, origin.1 as f32),
                &glyphs,
                font,
                text,
                size,
                false,
            );
            surface.set_stroke(None);
            if skew.is_some() {
                surface.pop();
            }
        }
        for decoration in paragraph.decorations(middle, middle, 0.5) {
            let rect = Rect::new(
                x + f64::from(decoration.x0),
                y + f64::from(decoration.y),
                x + f64::from(decoration.x1),
                y + f64::from(decoration.y + decoration.thickness),
            );
            if let Some(shape) = path(&rect) {
                surface.set_fill(Some(fill(decoration.color)));
                surface.draw_path(&shape);
            }
        }
    }
}

/// The text each glyph of a line stands for, for the ToUnicode map and for
/// copying: glyphs in drawing order, each given the cluster under its
/// origin. Glyphs whose cluster could not be told apart share one, and
/// clusters without glyphs of their own (a line's trailing space, a bracket
/// drawn into the space of the character before it) join the glyph before
/// them, so that the pieces always add up to the whole line.
fn clusters(
    paragraph: &Paragraph,
    runs: &[aster_text::DrawRun<'_>],
    middle: f32,
    range: Range<usize>,
) -> Vec<Range<usize>> {
    let mut starts: Vec<usize> = Vec::new();
    let mut last = range.start;
    for run in runs {
        let nudge = (run.face.size * 0.05).min(1.0);
        for glyph in &run.glyphs {
            let mut start = paragraph.offset_at(glyph.x + nudge, middle);
            if start < last || start >= range.end {
                start = last;
            }
            if starts.is_empty() {
                start = range.start;
            }
            starts.push(start);
            last = start;
        }
    }
    let mut out = vec![0..0; starts.len()];
    let mut end = range.end;
    for index in (0..starts.len()).rev() {
        if index + 1 < starts.len() && starts[index + 1] != starts[index] {
            end = starts[index + 1];
        }
        out[index] = starts[index]..end.max(starts[index]);
    }
    out
}

/// A decoded picture as an image with straight alpha.
fn image(pixmap: &Pixmap) -> Image {
    let mut data = Vec::with_capacity(pixmap.data().len() * 4);
    for pixel in pixmap.data() {
        let a = pixel.a;
        let straight = |c: u8| {
            if a == 0 {
                0
            } else {
                ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
            }
        };
        data.extend([straight(pixel.r), straight(pixel.g), straight(pixel.b), a]);
    }
    Image::from_rgba8(data, u32::from(pixmap.width()), u32::from(pixmap.height()))
}

// --- Variable fonts ---
//
// Parley hands out the instance of a variable font as normalised
// coordinates, after the font's `avar` mapping; krilla wants the axis values
// a user would pick. The two tables involved are read here directly.

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// A table of the font at `index` in a font file or collection.
fn table<'a>(data: &'a [u8], index: u32, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let start = if data.get(0..4)? == b"ttcf" {
        be32(data, 12 + 4 * index as usize)? as usize
    } else {
        0
    };
    let count = usize::from(be16(data, start + 4)?);
    for record in (0..count).map(|i| start + 12 + 16 * i) {
        if data.get(record..record + 4)? == tag {
            let offset = be32(data, record + 8)? as usize;
            let length = be32(data, record + 12)? as usize;
            return data.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

/// The axis values of an instance given by normalised coordinates, for the
/// axes that are not at their default.
fn variations(data: &[u8], index: u32, coords: &[i16]) -> Vec<(Tag, f32)> {
    if coords.iter().all(|c| *c == 0) {
        return Vec::new();
    }
    let Some(fvar) = table(data, index, b"fvar") else {
        return Vec::new();
    };
    let avar = table(data, index, b"avar");
    let (Some(axes_at), Some(count), Some(axis_size)) =
        (be16(fvar, 4), be16(fvar, 8), be16(fvar, 10))
    else {
        return Vec::new();
    };
    let fixed = |at: usize| be32(fvar, at).map(|v| v as i32 as f32 / 65536.0);
    // The segment maps of `avar`, one per axis, in order.
    let mut maps: Vec<Vec<(f32, f32)>> = Vec::new();
    if let Some(avar) = avar
        && let Some(axes) = be16(avar, 6)
    {
        let mut at = 8;
        for _ in 0..axes {
            let Some(pairs) = be16(avar, at) else { break };
            at += 2;
            let mut map = Vec::with_capacity(usize::from(pairs));
            for _ in 0..pairs {
                let (Some(from), Some(to)) = (be16(avar, at), be16(avar, at + 2)) else {
                    break;
                };
                let f2dot14 = |v: u16| f32::from(v as i16) / 16384.0;
                map.push((f2dot14(from), f2dot14(to)));
                at += 4;
            }
            maps.push(map);
        }
    }
    let mut out = Vec::new();
    for (axis, coord) in coords.iter().enumerate().take(usize::from(count)) {
        if *coord == 0 {
            continue;
        }
        let record = usize::from(axes_at) + axis * usize::from(axis_size);
        let (Some(tag), Some(min), Some(default), Some(max)) = (
            fvar.get(record..record + 4),
            fixed(record + 4),
            fixed(record + 8),
            fixed(record + 12),
        ) else {
            continue;
        };
        let mut value = f32::from(*coord) / 16384.0;
        if let Some(map) = maps.get(axis) {
            value = unmap(map, value);
        }
        let user = if value < 0.0 {
            default + value * (default - min)
        } else {
            default + value * (max - default)
        };
        let tag: [u8; 4] = tag.try_into().unwrap_or(*b"    ");
        out.push((Tag::new(&tag), user));
    }
    out
}

/// The coordinate an `avar` segment map sends to `value`.
fn unmap(map: &[(f32, f32)], value: f32) -> f32 {
    for pair in map.windows(2) {
        let ((from0, to0), (from1, to1)) = (pair[0], pair[1]);
        if value >= to0 && value <= to1 {
            if to1 - to0 <= f32::EPSILON {
                return from0;
            }
            return from0 + (value - to0) * (from1 - from0) / (to1 - to0);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Code colours, as the light theme has them.
    const CODE: [Rgba8; 6] = [
        [0xcf, 0x22, 0x2e, 0xff],
        [0x95, 0x38, 0x00, 0xff],
        [0x66, 0x39, 0xba, 0xff],
        [0x0a, 0x30, 0x69, 0xff],
        [0x05, 0x50, 0xae, 0xff],
        [0x63, 0x6c, 0x76, 0xff],
    ];

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
            code: CODE,
            callout_labels: ["注意", "提示", "重要", "警告", "当心"],
            font_size: 11.0,
            page: PageLayout::default(),
        }
    }

    fn pdf(source: &str, base: Option<&Path>) -> Vec<u8> {
        export(source, base, &style(), PdfPage::A4, false).unwrap()
    }

    /// Set `ASTER_PDF` to a folder to look at the tour as a PDF.
    #[test]
    fn the_tour_exports() {
        let tour = include_str!("../tests/data/markdown-tour.md");
        let bytes = export_pdf(tour, None, &style(), PdfPage::A4).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        if let Some(folder) = std::env::var_os("ASTER_PDF") {
            let folder = std::path::PathBuf::from(folder);
            std::fs::write(folder.join("tour.pdf"), &bytes).unwrap();
            let plain = export(tour, None, &style(), PdfPage::A4, false).unwrap();
            std::fs::write(folder.join("tour-uncompressed.pdf"), plain).unwrap();
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    /// Pages, counted by their dictionaries.
    fn pages(pdf: &[u8]) -> usize {
        count(pdf, b"/Type/Page") - count(pdf, b"/Type/Pages")
    }

    /// Whether a character is in a ToUnicode map, as UTF-16 in hex.
    fn mapped(pdf: &[u8], c: char) -> bool {
        let mut units = [0u16; 2];
        let hex: String = c
            .encode_utf16(&mut units)
            .iter()
            .map(|unit| format!("{unit:04X}"))
            .collect();
        contains(pdf, hex.as_bytes()) || contains(pdf, hex.to_lowercase().as_bytes())
    }

    #[test]
    fn a_small_document_is_a_pdf_with_its_text() {
        let pdf = pdf("# Hello\n\nQuick brown fox.", None);
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(contains(&pdf, b"%%EOF"));
        assert_eq!(pages(&pdf), 1);
        assert!(contains(&pdf, b"/ToUnicode"));
        assert!(contains(&pdf, b"/FontFile"), "fonts are embedded");
        for c in ['Q', 'x', 'H'] {
            assert!(mapped(&pdf, c), "{c} is not in a ToUnicode map");
        }
        assert!(contains(&pdf, b"/Title(Hello)"));
        assert!(!contains(&pdf, b"/CreationDate"));
        // The same input gives the same bytes.
        assert_eq!(pdf, super::tests::pdf("# Hello\n\nQuick brown fox.", None));
    }

    #[test]
    fn chinese_and_japanese_text_is_real_text() {
        let pdf = pdf("中文的段落。\n\n日本語のテキスト。", None);
        assert_eq!(pages(&pdf), 1);
        for c in ['中', '段', '。', '語', 'テ'] {
            assert!(mapped(&pdf, c), "{c} is not in a ToUnicode map");
        }
    }

    #[test]
    fn long_documents_have_pages_and_numbers() {
        let source: String = (0..120)
            .map(|i| format!("Paragraph {i} with some words in it.\n\n"))
            .collect();
        let plan = Plan::new(&source, None, &style(), PdfPage::A4);
        assert!(plan.pages.len() >= 3, "{:?}", plan.pages);
        let pdf = pdf(&source, None);
        assert_eq!(pages(&pdf), plan.pages.len());
        // Pages follow each other and none is taller than the text area.
        for pair in plan.pages.windows(2) {
            assert!(pair[0].1 <= pair[1].0 + EPSILON);
        }
        for (top, bottom) in &plan.pages {
            assert!(bottom - top <= PdfPage::A4.text_height() + EPSILON);
        }
        let letter = export(&source, None, &style(), PdfPage::LETTER, false).unwrap();
        assert!(contains(&letter, b"/MediaBox[0 0 612 792]"));
    }

    #[test]
    fn web_links_are_annotations_and_others_are_not() {
        let pdf = pdf(
            "[site](https://example.com/a) [mail](mailto:a@example.com) \
             [file](file:///etc/passwd) [script](javascript:alert(1))",
            None,
        );
        assert!(contains(&pdf, b"/Subtype/Link"));
        assert!(contains(&pdf, b"(https://example.com/a)"));
        assert!(contains(&pdf, b"(mailto:a@example.com)"));
        assert!(!contains(&pdf, b"/etc/passwd)"));
        assert!(!contains(&pdf, b"javascript"));
        assert_eq!(count(&pdf, b"/URI("), 2);
    }

    #[test]
    fn footnote_links_jump_inside_the_document() {
        let pdf = pdf("See[^1].\n\n[^1]: The note.", None);
        assert!(contains(&pdf, b"/Subtype/Link"));
        assert!(contains(&pdf, b"/XYZ"));
        assert!(!contains(&pdf, b"/URI"));
    }

    #[test]
    fn a_heading_goes_with_the_paragraph_after_it() {
        // Ten paragraphs of 50 points with gaps of 10, a heading of 30 at
        // 600, then a paragraph of three 20-point lines.
        let mut lines = Vec::new();
        let mut marks = Vec::new();
        for i in 0..10 {
            let top = f64::from(i) * 60.0;
            lines.push(Keep {
                top,
                bottom: top + 50.0,
            });
        }
        let heading = Keep {
            top: 600.0,
            bottom: 630.0,
        };
        lines.push(heading);
        for i in 0..3 {
            let top = 640.0 + f64::from(i) * 20.0;
            lines.push(Keep {
                top,
                bottom: top + 20.0,
            });
        }
        marks.extend(lines.iter().copied());
        // Without the heading rule the page would end after the heading.
        assert_eq!(paginate(&lines, &marks, 650.0)[0], (0.0, 640.0));
        let keeps = keep_with_next(&lines, &[heading], 650.0);
        let pages = paginate(&keeps, &marks, 650.0);
        assert_eq!(pages, [(0.0, 600.0), (600.0, 700.0)]);
        // A line is never cut: the break moves up to its top.
        let pages = paginate(&lines, &marks, 655.0);
        assert_eq!(pages[0], (0.0, 640.0));
        // A heading with something taller than a page after it stays put.
        let tall = [
            heading,
            Keep {
                top: 640.0,
                bottom: 2000.0,
            },
        ];
        assert_eq!(keep_with_next(&tall, &[heading], 650.0).len(), 3);
    }

    #[test]
    fn what_is_taller_than_a_page_is_cut() {
        let keeps = [Keep {
            top: 0.0,
            bottom: 1000.0,
        }];
        let pages = paginate(&keeps, &keeps, 400.0);
        assert_eq!(pages, [(0.0, 400.0), (400.0, 800.0), (800.0, 1000.0)]);
        assert_eq!(paginate(&[], &[], 400.0), [(0.0, 0.0)]);
    }

    #[test]
    fn breaks_cut_no_line_and_no_table_row() {
        let mut source = String::from("# Table\n\n| a | b |\n|---|---|\n");
        for i in 0..80 {
            source.push_str(&format!(
                "| row {i} | a longer cell that wraps onto more than one line of the table, \
                 because its text goes on and on and does not stop, {i} |\n"
            ));
        }
        source.push_str("\n## Code\n\n```\n");
        for i in 0..120 {
            source.push_str(&format!("let x{i} = {i};\n"));
        }
        source.push_str("```\n");
        let plan = Plan::new(&source, None, &style(), PdfPage::A4);
        assert!(plan.pages.len() >= 3);
        let (keeps, _) = measure(&plan.layout, 11.0);
        let height = PdfPage::A4.text_height();
        let rows = keeps.iter().filter(|k| k.height() > 30.0).count();
        assert!(rows >= 80, "rows are kept whole: {rows}");
        for (_, bottom) in &plan.pages[..plan.pages.len() - 1] {
            for keep in keeps.iter().filter(|k| k.height() <= height) {
                assert!(
                    !(keep.top < bottom - EPSILON && keep.bottom > bottom + EPSILON),
                    "the break at {bottom} cuts {keep:?}"
                );
            }
        }
        let pdf = pdf(&source, None);
        assert_eq!(pages(&pdf), plan.pages.len());
        assert!(contains(&pdf, b"/Outlines"));
        if let Some(folder) = std::env::var_os("ASTER_PDF") {
            std::fs::write(std::path::Path::new(&folder).join("table.pdf"), &pdf).unwrap();
        }
    }

    #[test]
    fn local_pictures_are_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = crate::md_picture::png_bytes(60, 30, [255, 0, 0, 255]);
        std::fs::write(dir.path().join("红.png"), bytes).unwrap();
        let pdf = pdf("![红色](红.png)", Some(dir.path()));
        assert!(contains(&pdf, b"/Subtype/Image"));
        assert!(contains(&pdf, b"/Width 60"));
        // A web picture stays a box with its description.
        let pdf = super::tests::pdf("![红色](https://example.com/红.png)", Some(dir.path()));
        assert!(!contains(&pdf, b"/Subtype/Image"));
        assert!(mapped(&pdf, '红'));
    }

    #[test]
    fn front_matter_is_a_box_and_gives_the_title() {
        let source = "---\ntitle: \"技术方案\"\nauthor: 某人\n---\n\n# Heading\n\nBody.";
        let plan = Plan::new(source, None, &style(), PdfPage::A4);
        assert_eq!(plan.title().as_deref(), Some("技术方案"));
        // The box under the fields.
        assert!(plan.layout.items().iter().any(|item| matches!(
            item.shape,
            Piece::Rect { color, .. } if color == style().fill
        )));
        let pdf = pdf(source, None);
        assert!(mapped(&pdf, '某'));
        // The document's title, in UTF-16; "Heading" is only a bookmark.
        assert!(contains(&pdf, b"/Title<FEFF6280672F65B96848>"));
        let plan = Plan::new(
            "Intro.\n\n## Second\n\n# First",
            None,
            &style(),
            PdfPage::A4,
        );
        assert_eq!(plan.title().as_deref(), Some("Second"));
    }

    #[test]
    fn code_is_printed_in_its_colours() {
        let source = "```rust\nfn main() {} // 注释\n```\n";
        let plan = Plan::new(source, None, &style(), PdfPage::A4);
        let colors: Vec<Rgba8> = plan
            .layout
            .texts()
            .iter()
            .flat_map(|text| text.paragraph.draw_runs(f32::MIN, f32::MAX))
            .map(|run| run.color)
            .collect();
        // Keyword, function, comment.
        for color in [CODE[0], CODE[2], CODE[5]] {
            assert!(colors.contains(&color), "{color:?} in {colors:?}");
        }
        // The keyword's red, as the page's fill colour.
        let pdf = export(source, None, &style(), PdfPage::A4, false).unwrap();
        assert!(contains(&pdf, b"0.8117647 0.13333334 0.18039216 rg"));
    }

    #[test]
    fn variation_coordinates_go_back_through_avar() {
        let map = [(-1.0, -1.0), (0.0, 0.0), (0.5, 0.8), (1.0, 1.0)];
        assert!((unmap(&map, 0.8) - 0.5).abs() < 1e-6);
        assert!((unmap(&map, 0.9) - 0.75).abs() < 1e-6);
        assert!((unmap(&map, -0.5) + 0.5).abs() < 1e-6);
        assert!(variations(b"not a font", 0, &[16384]).is_empty());
        assert!(variations(b"", 0, &[0]).is_empty());
    }

    #[test]
    fn empty_documents_are_one_page_even_on_another_thread() {
        let style = style();
        let worker = std::thread::spawn(move || {
            export(String::new().as_str(), None, &style, PdfPage::LETTER, false)
        });
        assert_eq!(pages(&worker.join().unwrap().unwrap()), 1);
        let cramped = PdfPage {
            margin_pt: 400.0,
            ..PdfPage::A4
        };
        assert!(export_pdf("x", None, &super::tests::style(), cramped).is_err());
    }
}
