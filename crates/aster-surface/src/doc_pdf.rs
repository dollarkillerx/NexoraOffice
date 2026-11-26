//! A laid-out Word document as a PDF that looks like the page on screen.
//!
//! [`export_doc_pdf`] takes the display lists that [`aster_layout`] made and
//! writes every item the way [`crate::doc_paint::DocPainter`] draws it, one
//! PDF page per layout page and each with its own size, so documents that
//! mix portrait and landscape sections come out right. Text is real text, in
//! the fonts the layout shaped it with (embedded, subsetted, with a
//! ToUnicode map, so that it can be selected, copied and searched); the
//! rest is paths and images.
//!
//! What each item becomes:
//!
//! - `Fill` (shading, highlights, cell backgrounds): a filled rectangle.
//! - `Rule`: single and double rules as filled bars, dotted and dashed
//!   ones as one dashed stroke; the same dash lengths as on screen.
//! - `Text`: one text object per run of glyphs in one face and colour:
//!   made-up bold as an outline in the text colour, made-up italics as a
//!   shear about the baseline, small capitals, superscripts and subscripts,
//!   letter spacing and horizontal stretch as the layout placed them.
//!   Underlines and strikethroughs (single, double, thick, dotted, dashed,
//!   wavy) follow the text as paths.
//! - `Leader` (the dots, hyphens or line that fill a tab): marks on the
//!   same grid as on screen, as one dashed stroke.
//! - `Picture`: JPEG files are embedded as they are, other formats are
//!   decoded the way the screen decodes them (at most 3072 pixels on the
//!   longer side) and stored as RGBA. Cropping is a clip of the rectangle;
//!   a picture that cannot be decoded is the grey placeholder.
//! - `Frame` (a text box's fill and outline), `Placeholder` (a drawing this
//!   version does not draw: a grey box with a cross) and `PushClip` /
//!   `PopClip` (a text box's content is cut to it): as on screen.
//!
//! Headers, footers, footnotes and list labels are ordinary items of a page
//! and need nothing of their own.
//!
//! The PDF also has the outline of the document as bookmarks (each heading
//! at its page and height, nested by level) and link annotations over
//! linked text of the body that point to an address on the web or to an
//! e-mail address.
//!
//! Where the PDF differs from the screen:
//!
//! - Lines never get thinner than half a point (on screen: one pixel).
//! - Dashes, dots and leaders are strokes, not rows of rectangles.
//! - A JPEG that is damaged past its header is embedded as it is, and
//!   viewers show what they can of it; the screen shows the placeholder.
//! - Links to a bookmark inside the document (`#name`) are left out: the
//!   layout does not know where bookmarks are. Links in headers, footers,
//!   notes and text boxes are left out too: the layout answers for the
//!   body's text only.
//! - The size of the page and the content are in points, as laid out; there
//!   is no page content outside the paper (the screen clips to the page,
//!   so does the PDF's media box).
//!
//! The output depends on the input only: no creation date, no random
//! identifiers. Nothing here keeps global state, and [`export_doc_pdf`] can
//! run on any thread.

use std::collections::HashMap;
use std::sync::Arc;

use aster_layout::{
    FrameItem, Item, Layout, LeaderItem, LinkRect, Page, PictureItem, Rect as PageRect, RuleStyle,
};
use aster_text::{Decoration, Leader, LineKind};
use aster_writer::Document;
use krilla::action::{Action, LinkAction};
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::destination::XyzDestination;
use krilla::geom::{PathBuilder, Point, Rect as PdfRect, Size, Transform};
use krilla::image::Image;
use krilla::metadata::Metadata;
use krilla::page::PageSettings;
use krilla::paint::{FillRule, StrokeDash};
use krilla::surface::Surface;
use krilla::{Data, Document as PdfDocument, SerializeSettings};
use vello_cpu::kurbo::Rect;

use crate::pdf_common::{Fonts, bookmarks, clusters, draw_runs, fill, path, stroke};

const PLACEHOLDER_FILL: [u8; 4] = [0xee, 0xee, 0xee, 0xff];
const PLACEHOLDER_LINE: [u8; 4] = [0x99, 0x99, 0x99, 0xff];
/// Pictures that are not JPEG are shrunk to this many pixels on the longer
/// side before they are stored.
const MAX_PICTURE_SIDE: u32 = 3072;
/// The thinnest line, in points.
const HAIRLINE: f64 = 0.5;

/// What the document says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocPdfInfo {
    pub title: Option<String>,
    /// A BCP 47 tag, such as `en-US` or `zh-CN`.
    pub language: Option<String>,
}

/// Export `layout` as a PDF. `document` is the one the layout was made from:
/// it tells where the hyperlinks are.
pub fn export_doc_pdf(
    layout: &Layout,
    document: &Document,
    info: &DocPdfInfo,
) -> Result<Vec<u8>, String> {
    export(layout, document, info, true)
}

/// Lay `document` out afresh and export it: for a worker thread that has
/// the document (cloning one is cheap) but not the view's layout. The same
/// shaping fonts as the view's, so the pages are the ones on screen.
pub fn export_document(document: &Document, info: &DocPdfInfo) -> Result<Vec<u8>, String> {
    let mut shaper = aster_text::Shaper::new();
    shaper.set_han_fallbacks(&crate::text_surface::HAN_FALLBACK_FAMILIES);
    let mut layouter = aster_layout::Layouter::with_shaper(shaper);
    let layout = layouter.layout(document);
    export_doc_pdf(&layout, document, info)
}

/// Like [`export_doc_pdf`]; without compression the content streams, the
/// ToUnicode maps and the document information can be read in the bytes.
fn export(
    layout: &Layout,
    document: &Document,
    info: &DocPdfInfo,
    compress: bool,
) -> Result<Vec<u8>, String> {
    let settings = SerializeSettings {
        compress_content_streams: compress,
        enable_tagging: false,
        ..SerializeSettings::default()
    };
    let mut pdf = PdfDocument::new_with(settings);
    let mut writer = Writer::default();
    let mut links: Vec<Vec<LinkRect>> = vec![Vec::new(); layout.pages.len()];
    for link in layout.links(document) {
        if let Some(page) = links.get_mut(link.page) {
            page.push(link);
        }
    }
    for (index, page) in layout.pages.iter().enumerate() {
        let settings = PageSettings::from_wh(page.width.max(1.0), page.height.max(1.0))
            .ok_or("the page has no size")?;
        let mut pdf_page = pdf.start_page_with(settings);
        let mut surface = pdf_page.surface();
        writer.page(&mut surface, page);
        surface.finish();
        for link in &links[index] {
            if let Some(annotation) = annotation(link) {
                pdf_page.add_annotation(annotation);
            }
        }
        pdf_page.finish();
    }
    let mut metadata = Metadata::new().creator("Aster Office".into());
    if let Some(title) = info.title.as_deref().filter(|t| !t.trim().is_empty()) {
        metadata = metadata.title(title.to_owned());
    }
    if let Some(language) = info.language.as_deref().filter(|l| !l.trim().is_empty()) {
        metadata = metadata.language(language.to_owned());
    }
    pdf.set_metadata(metadata);
    pdf.set_outline(outline(layout));
    pdf.finish()
        .map_err(|err| format!("cannot write the PDF: {err:?}"))
}

/// The headings as bookmarks, nested by level. A heading that skips a level
/// (a level 3 straight under a level 1) hangs from the nearest one above.
fn outline(layout: &Layout) -> krilla::outline::Outline {
    let entries: Vec<(u8, String, XyzDestination)> = layout
        .outline()
        .iter()
        .filter(|entry| !entry.text.trim().is_empty() && entry.page < layout.pages.len())
        .map(|entry| {
            // The place of the heading; the top of its page when the
            // layout cannot say.
            let top = layout
                .caret(&entry.pos)
                .filter(|caret| caret.page == entry.page)
                .map_or(0.0, |caret| caret.top);
            let at = Point::from_xy(0.0, top.max(0.0));
            (
                entry.level.saturating_add(1),
                entry.text.split_whitespace().collect::<Vec<_>>().join(" "),
                XyzDestination::new(entry.page, at),
            )
        })
        .collect();
    bookmarks(&entries)
}

/// A link over linked text, if it goes anywhere a PDF viewer can follow.
fn annotation(link: &LinkRect) -> Option<Annotation> {
    if !aster_platform::links::is_openable(&link.target) {
        return None;
    }
    let area = PdfRect::from_ltrb(link.rect.x0, link.rect.y0, link.rect.x1, link.rect.y1)?;
    let target = Target::Action(Action::Link(LinkAction::new(link.target.clone())));
    Some(Annotation::new_link(
        LinkAnnotation::new(area, target),
        None,
    ))
}

/// What the pages share: fonts and pictures, each embedded once.
#[derive(Default)]
struct Writer {
    fonts: Fonts,
    /// Pictures by the address of their bytes (the layout keeps them alive
    /// for the whole export); `None` for one that cannot be shown.
    images: HashMap<usize, Option<Image>>,
}

fn rect_of(rect: &PageRect) -> Rect {
    Rect::new(
        f64::from(rect.x0),
        f64::from(rect.y0),
        f64::from(rect.x1),
        f64::from(rect.y1),
    )
}

impl Writer {
    fn page(&mut self, surface: &mut Surface<'_>, page: &Page) {
        // How many clips are open: a `PopClip` without a `PushClip` is
        // ignored, and clips left open are closed.
        let mut clips = 0usize;
        for item in &page.items {
            match item {
                Item::PushClip(rect) => {
                    if let Some(shape) = path(&rect_of(rect)) {
                        surface.push_clip_path(&shape, &FillRule::NonZero);
                        clips += 1;
                    }
                }
                Item::PopClip => {
                    if clips > 0 {
                        surface.pop();
                        clips -= 1;
                    }
                }
                item => self.item(surface, item),
            }
        }
        for _ in 0..clips {
            surface.pop();
        }
    }

    fn item(&mut self, surface: &mut Surface<'_>, item: &Item) {
        match item {
            Item::Fill { rect, color } => fill_rect(surface, &rect_of(rect), *color),
            Item::Rule {
                from,
                to,
                width,
                color,
                style,
            } => rule(surface, *from, *to, *width, *color, *style),
            Item::Text(text) => self.text(surface, text),
            Item::Leader(leader) => leader_marks(surface, leader),
            Item::Picture(picture) => self.picture(surface, picture),
            Item::Frame(frame) => frame_of(surface, frame),
            Item::Placeholder { rect, .. } => placeholder(surface, &rect_of(rect)),
            Item::PushClip(_) | Item::PopClip => {}
        }
    }

    fn text(&mut self, surface: &mut Surface<'_>, item: &aster_layout::TextItem) {
        let paragraph = &item.paragraph;
        let ranges = paragraph.line_ranges();
        let lines = paragraph.lines();
        let end = item.lines.end.min(lines.len());
        let (x, y) = (f64::from(item.x), f64::from(item.y));
        for index in item.lines.start.min(end)..end {
            let line = &lines[index];
            let middle = line.top + line.height / 2.0;
            let runs = paragraph.draw_runs_in(index..index + 1);
            let clusters = clusters(paragraph, &runs, middle, ranges[index].clone());
            draw_runs(
                surface,
                &mut self.fonts,
                paragraph.text(),
                &runs,
                &clusters,
                x,
                y,
            );
        }
        for decoration in paragraph.decorations_in(item.lines.clone(), 0.5) {
            decorate(surface, &decoration, x, y);
        }
    }

    fn picture(&mut self, surface: &mut Surface<'_>, picture: &PictureItem) {
        let rect = rect_of(&picture.rect);
        let image = self
            .images
            .entry(picture.image.as_ptr() as usize)
            .or_insert_with(|| image_of(&picture.image))
            .clone();
        let Some(image) = image else {
            placeholder(surface, &rect);
            return;
        };
        let [left, top, right, bottom] = picture.crop;
        // The whole picture, of which `rect` shows the part left uncropped.
        let kept_w = (1.0 - left - right).max(1e-3);
        let kept_h = (1.0 - top - bottom).max(1e-3);
        let (w, h) = (rect.width() / kept_w, rect.height() / kept_h);
        let (x, y) = (rect.x0 - left * w, rect.y0 - top * h);
        let (Some(clip), Some(size)) = (path(&rect), Size::from_wh(w as f32, h as f32)) else {
            return;
        };
        surface.push_clip_path(&clip, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(x as f32, y as f32));
        surface.draw_image(image, size);
        surface.pop();
        surface.pop();
    }
}

/// A picture's bytes as a PDF image: a JPEG as it is, anything else decoded.
fn image_of(bytes: &Arc<[u8]>) -> Option<Image> {
    if aster_image::sniff(bytes) == aster_image::Kind::Jpeg {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes.clone());
        if let Ok(image) = Image::from_jpeg(Data::from(data), true) {
            return Some(image);
        }
    }
    let decoded = aster_image::decode(bytes, MAX_PICTURE_SIDE).ok()?;
    Some(Image::from_rgba8(
        decoded.pixels,
        decoded.width,
        decoded.height,
    ))
}

fn fill_rect(surface: &mut Surface<'_>, rect: &Rect, color: [u8; 4]) {
    if let Some(shape) = path(rect) {
        surface.set_fill(Some(fill(color)));
        surface.set_stroke(None);
        surface.draw_path(&shape);
    }
}

/// A straight stroke from `a` to `b`, dashed when `dash` is `(on, off)`.
fn line(
    surface: &mut Surface<'_>,
    a: (f64, f64),
    b: (f64, f64),
    width: f64,
    color: [u8; 4],
    dash: Option<(f64, f64)>,
) {
    let mut builder = PathBuilder::new();
    builder.move_to(a.0 as f32, a.1 as f32);
    builder.line_to(b.0 as f32, b.1 as f32);
    let Some(shape) = builder.finish() else {
        return;
    };
    let mut pen = stroke(color, width);
    pen.dash = dash.map(|(on, off)| StrokeDash {
        array: vec![on as f32, off as f32],
        offset: 0.0,
    });
    surface.set_fill(None);
    surface.set_stroke(Some(pen));
    surface.draw_path(&shape);
    surface.set_stroke(None);
}

fn rule(
    surface: &mut Surface<'_>,
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
    color: [u8; 4],
    style: RuleStyle,
) {
    let w = f64::from(width).max(HAIRLINE);
    let (x0, y0) = (f64::from(from.0), f64::from(from.1));
    let (x1, y1) = (f64::from(to.0), f64::from(to.1));
    let horizontal = (y1 - y0).abs() < (x1 - x0).abs();
    let (a, b) = if horizontal {
        (x0.min(x1), x0.max(x1))
    } else {
        (y0.min(y1), y0.max(y1))
    };
    // One thin bar `offset` away from the line, across it.
    let bar = |surface: &mut Surface<'_>, offset: f64, thickness: f64| {
        let r = if horizontal {
            Rect::new(
                a,
                y0 + offset - thickness / 2.0,
                b,
                y0 + offset + thickness / 2.0,
            )
        } else {
            Rect::new(
                x0 + offset - thickness / 2.0,
                a,
                x0 + offset + thickness / 2.0,
                b,
            )
        };
        fill_rect(surface, &r, color);
    };
    match style {
        RuleStyle::Single => bar(surface, 0.0, w),
        RuleStyle::Double => {
            let t = (w / 3.0).max(HAIRLINE / 2.0);
            bar(surface, -t, t);
            bar(surface, t, t);
        }
        RuleStyle::Dotted | RuleStyle::Dashed => {
            let dash = if style == RuleStyle::Dotted {
                (w, w * 1.5)
            } else {
                (w * 4.0, w * 2.0)
            };
            let (p, q) = if horizontal {
                ((a, y0), (b, y0))
            } else {
                ((x0, a), (x0, b))
            };
            line(surface, p, q, w, color, Some(dash));
        }
    }
}

fn leader_marks(surface: &mut Surface<'_>, leader: &LeaderItem) {
    let size = f64::from(leader.size);
    let (x0, x1) = (f64::from(leader.x0), f64::from(leader.x1));
    let baseline = f64::from(leader.baseline);
    let thickness = (size * 0.07).max(HAIRLINE);
    let (pitch, mark, y) = match leader.kind {
        Leader::Dots => (size * 0.33, thickness, baseline - thickness),
        Leader::MiddleDots => (size * 0.33, thickness, baseline - size * 0.28),
        Leader::Hyphens => (size * 0.5, size * 0.3, baseline - size * 0.28),
        Leader::Underscore => (x1 - x0, x1 - x0, baseline + thickness),
        Leader::None => return,
    };
    if pitch <= 0.0 {
        return;
    }
    // Marks sit on a grid anchored to the right end, as Word's do, so that
    // leaders of neighbouring lines line up: from the last one to the left
    // as far as there is room.
    let last = x1 - mark - pitch * 0.5;
    let first = x0 + pitch * 0.25;
    if last >= first {
        let count = ((last - first) / pitch).floor() + 1.0;
        let from = last - (count - 1.0) * pitch;
        if count < 2.0 || pitch - mark < 0.01 {
            fill_rect(
                surface,
                &Rect::new(last, y, last + mark, y + thickness),
                leader.color,
            );
        } else {
            let middle = y + thickness / 2.0;
            line(
                surface,
                (from, middle),
                (last + mark, middle),
                thickness,
                leader.color,
                Some((mark, pitch - mark)),
            );
        }
    }
    if leader.kind == Leader::Underscore {
        fill_rect(surface, &Rect::new(x0, y, x1, y + thickness), leader.color);
    }
}

/// An underline or strikethrough of a paragraph whose origin is at
/// `(ox, oy)`.
fn decorate(surface: &mut Surface<'_>, line_: &Decoration, ox: f64, oy: f64) {
    let (x0, x1) = (ox + f64::from(line_.x0), ox + f64::from(line_.x1));
    let (y, t) = (oy + f64::from(line_.y), f64::from(line_.thickness));
    if x1 <= x0 {
        return;
    }
    match line_.kind {
        LineKind::Solid => fill_rect(surface, &Rect::new(x0, y, x1, y + t), line_.color),
        LineKind::Dotted | LineKind::Dashed => {
            let dash = if line_.kind == LineKind::Dotted {
                (t, t)
            } else {
                (t * 4.0, t * 2.0)
            };
            line(
                surface,
                (x0, y + t / 2.0),
                (x1, y + t / 2.0),
                t,
                line_.color,
                Some(dash),
            );
        }
        LineKind::Wave => {
            let (step, amp) = (t * 2.0, t * 1.2);
            let middle = y + t / 2.0;
            let mut builder = PathBuilder::new();
            builder.move_to(x0 as f32, middle as f32);
            let mut at = x0;
            let mut up = false;
            while at < x1 {
                at = (at + step).min(x1);
                up = !up;
                let off = if up { -amp } else { amp };
                builder.line_to(at as f32, (middle + off) as f32);
            }
            if let Some(shape) = builder.finish() {
                surface.set_fill(None);
                surface.set_stroke(Some(stroke(line_.color, t * 0.8)));
                surface.draw_path(&shape);
                surface.set_stroke(None);
            }
        }
    }
}

fn frame_of(surface: &mut Surface<'_>, frame: &FrameItem) {
    let rect = rect_of(&frame.rect);
    if let Some(color) = frame.fill {
        fill_rect(surface, &rect, color);
    }
    if let Some((width, color)) = frame.line
        && let Some(shape) = path(&rect)
    {
        surface.set_fill(None);
        surface.set_stroke(Some(stroke(color, f64::from(width).max(HAIRLINE))));
        surface.draw_path(&shape);
        surface.set_stroke(None);
    }
}

fn placeholder(surface: &mut Surface<'_>, rect: &Rect) {
    fill_rect(surface, rect, PLACEHOLDER_FILL);
    if let Some(shape) = path(rect) {
        surface.set_fill(None);
        surface.set_stroke(Some(stroke(PLACEHOLDER_LINE, 0.75)));
        surface.draw_path(&shape);
        surface.set_stroke(None);
    }
    for (a, b) in [
        ((rect.x0, rect.y0), (rect.x1, rect.y1)),
        ((rect.x0, rect.y1), (rect.x1, rect.y0)),
    ] {
        line(surface, a, b, 0.75, PLACEHOLDER_LINE, None);
    }
}
