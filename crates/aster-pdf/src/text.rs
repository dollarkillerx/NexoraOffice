//! A page's text with the box of each character, for selection and find.
//!
//! The page is interpreted by hayro with a [`Device`] that only listens to
//! `draw_glyph_run`: nothing is drawn. Every glyph gives its characters (from
//! the font's `ToUnicode` map or glyph names, which hayro resolves) and a box
//! worked out from its transform and advance width.
//!
//! hayro cannot say what a glyph means when an embedded CID font has no
//! `ToUnicode` map: `as_unicode` is `None`, and the glyph does not tell which
//! font it came from or how the font's `/Encoding` reads codes. For those
//! glyphs the page's font dictionaries (its resources and those of the forms
//! it draws) are read here, with hayro's own CMap parser: the font is picked
//! by the PostScript name hayro reports for the glyph (or, with none, the
//! fonts of the page that have no `ToUnicode`), the code is turned into a CID
//! with the font's `/Encoding`, and the CID into Unicode with the Adobe
//! `UCS2` table of its character collection. Encodings that are Unicode
//! themselves (`UniGB-UCS2-H`, `UniJIS-UTF16-H` …) use the code as it is.
//! Non-embedded CID fonts never get here: hayro maps them by the UCS2 tables
//! itself.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::font::{Glyph, GlyphRun, PositionedGlyph};
use hayro::hayro_interpret::hayro_cmap::{BfString, CMap, CMapName, CidFamily, WritingMode};
use hayro::hayro_interpret::hayro_syntax::object::dict::keys::{
    DESCENDANT_FONTS, ENCODING, FONT, FONT_DESC, RESOURCES, SUBTYPE, TO_UNICODE, XOBJECT,
};
use hayro::hayro_interpret::hayro_syntax::object::{Dict, Name, Object, ObjectIdentifier, Stream};
use hayro::hayro_interpret::hayro_syntax::page::Resources;
use hayro::hayro_interpret::kurbo::{Affine, BezPath, Rect};
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, SoftMask, TransformExt, interpret_page,
};

use crate::{Document, PageText, TextBox};

/// Where a glyph's box reaches below and above its baseline, per 1000 units.
const DESCENT: f64 = -200.0;
const ASCENT: f64 = 800.0;
/// The advance of a glyph whose font does not say.
const DEFAULT_ADVANCE: f64 = 500.0;
/// How far past the page a box may reach.
const MARGIN: f64 = 100.0;
/// Limits on reading font dictionaries.
const MAX_FONTS: usize = 64;
const MAX_ENTRIES: usize = 1024;
const MAX_FORM_DEPTH: usize = 4;

impl Document {
    /// The text of `page`. `Err` if the parser panicked.
    // The only failure is a caught panic; there is nothing to say about it.
    #[allow(clippy::result_unit_err)]
    pub fn page_text(&self, page: usize) -> Result<PageText, ()> {
        let mut result = PageText {
            page,
            ..PageText::default()
        };
        let pages = self.pdf.pages();
        let Some(page_ref) = pages.get(page) else {
            return Ok(result);
        };
        let max = self.limits.max_text_chars;
        let collected = catch_unwind(AssertUnwindSafe(|| {
            let (width, height) = page_ref.render_dimensions();
            let cache = InterpreterCache::new();
            let transform = page_ref.initial_transform(true).to_kurbo();
            let mut context = Context::new(
                transform,
                Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
                &cache,
                page_ref.xref(),
                self.settings.clone(),
            );
            let mut collector = Collector {
                max,
                width: f64::from(width),
                height: f64::from(height),
                resources: page_ref.resources().clone(),
                fonts: None,
                text: String::new(),
                boxes: Vec::new(),
                chars: 0,
                done: max == 0,
                previous: None,
                last_run: Vec::new(),
            };
            interpret_page(page_ref, &mut context, &mut collector);
            (collector.text, collector.boxes)
        }))
        .map_err(|_| ())?;
        (result.text, result.boxes) = collected;
        Ok(result)
    }
}

/// Where the previous glyph ended, to see how the next one follows.
struct Previous {
    /// Where its advance ends, in page space.
    end: (f64, f64),
    /// Its writing direction as a unit vector.
    direction: (f64, f64),
}

/// Collects text from the glyph runs of one page.
struct Collector<'a> {
    max: usize,
    width: f64,
    height: f64,
    resources: Resources<'a>,
    /// The fonts of the page, read when a glyph needs them.
    fonts: Option<Vec<FontInfo>>,
    text: String,
    boxes: Vec<Option<TextBox>>,
    chars: usize,
    done: bool,
    previous: Option<Previous>,
    /// Codes and transforms of the last run, to skip a stroke that repeats
    /// the fill of the same text.
    last_run: Vec<(u32, [f64; 6])>,
}

impl Collector<'_> {
    fn push(&mut self, c: char, tbox: Option<TextBox>) -> bool {
        if self.chars >= self.max {
            self.done = true;
            return false;
        }
        self.text.push(c);
        self.boxes.push(tbox);
        self.chars += 1;
        true
    }

    /// Put a line break in, in place of spaces that end the line.
    fn newline(&mut self) -> bool {
        while self.text.ends_with(' ') && matches!(self.boxes.last(), Some(None)) {
            self.text.pop();
            self.boxes.pop();
            self.chars -= 1;
        }
        if self.text.is_empty() || self.text.ends_with('\n') {
            return true;
        }
        self.push('\n', None)
    }

    fn space(&mut self) -> bool {
        if self.text.is_empty() || self.text.ends_with([' ', '\n']) {
            return true;
        }
        self.push(' ', None)
    }

    fn glyph(&mut self, glyph: &PositionedGlyph<'_>, base: Affine, vertical: bool) {
        let coeffs = (base * glyph.transform()).as_coeffs();
        if !coeffs.iter().all(|c| c.is_finite()) {
            return;
        }
        let [a, b, c, d, e, f] = coeffs;
        let em = (a * 1000.0).hypot(b * 1000.0);
        if !(em.is_finite() && em > 0.01) {
            return;
        }
        let advance = if vertical {
            1000.0
        } else {
            let g: &Glyph<'_> = glyph;
            let width = match g {
                Glyph::Outline(outline) => outline.advance_width(),
                Glyph::Type3(type3) => type3.advance_width(),
            };
            width
                .map(f64::from)
                .filter(|w| w.is_finite())
                .map_or(DEFAULT_ADVANCE, |w| w.clamp(0.0, 3000.0))
        };
        // The corners of the glyph's box in glyph units: horizontal text
        // stands on its baseline, vertical text hangs from its origin.
        let (x0, y0, x1, y1) = if vertical {
            (-500.0, -1000.0, 500.0, 0.0)
        } else {
            (0.0, DESCENT, advance, ASCENT)
        };
        let map = |x: f64, y: f64| (a * x + c * y + e, b * x + d * y + f);
        let corners = [map(x0, y0), map(x1, y0), map(x0, y1), map(x1, y1)];
        let left = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let right = corners
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let top = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let bottom = corners
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let tbox = self.clamp([left, top, right, bottom]);

        // How this glyph follows the one before.
        let unit = |x: f64, y: f64| {
            let length = x.hypot(y);
            (x / length, y / length)
        };
        let direction = if vertical { unit(-c, -d) } else { unit(a, b) };
        let origin = (e, f);
        let end = if vertical {
            (e - c * 1000.0, f - d * 1000.0)
        } else {
            (e + a * advance, f + b * advance)
        };
        let separator = self.previous.as_ref().map(|previous| {
            let dx = origin.0 - previous.end.0;
            let dy = origin.1 - previous.end.1;
            let along = dx * previous.direction.0 + dy * previous.direction.1;
            let across = -dx * previous.direction.1 + dy * previous.direction.0;
            if across.abs() > 0.5 * em || along < -em {
                '\n'
            } else if along > 0.25 * em {
                ' '
            } else {
                '\0'
            }
        });
        self.previous = Some(Previous { end, direction });

        let chars = self.characters(glyph);
        if chars.is_empty() {
            return;
        }
        match separator {
            Some('\n') if !self.newline() => return,
            Some(' ') if !self.space() => return,
            _ => {}
        }
        let count = chars.len();
        let [l, t, r, bt] = tbox;
        let wide = (r - l) >= (bt - t);
        for (index, ch) in chars.into_iter().enumerate() {
            let part = if count == 1 {
                tbox
            } else {
                let from = index as f32 / count as f32;
                let to = (index + 1) as f32 / count as f32;
                if wide {
                    [l + (r - l) * from, t, l + (r - l) * to, bt]
                } else {
                    [l, t + (bt - t) * from, r, t + (bt - t) * to]
                }
            };
            if !self.push(ch, Some(part)) {
                return;
            }
        }
    }

    /// Keep a box finite and near the page.
    fn clamp(&self, [left, top, right, bottom]: [f64; 4]) -> TextBox {
        let x = |v: f64| v.clamp(-MARGIN, self.width + MARGIN) as f32;
        let y = |v: f64| v.clamp(-MARGIN, self.height + MARGIN) as f32;
        [x(left), y(top), x(right), y(bottom)]
    }

    /// The characters of a glyph, without control characters. A glyph with
    /// no meaning we can find gives U+FFFD.
    fn characters(&mut self, glyph: &PositionedGlyph<'_>) -> Vec<char> {
        let found = match glyph.as_unicode() {
            Some(BfString::Char(c)) => Some(vec![c]),
            Some(BfString::String(s)) => Some(s.chars().collect()),
            None => self.unmapped(glyph),
        };
        let raw = found.unwrap_or_else(|| vec!['\u{FFFD}']);
        raw.into_iter()
            .filter_map(|c| match c {
                '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect()
    }

    /// A glyph without `ToUnicode`, from the font dictionaries of the page.
    fn unmapped(&mut self, glyph: &PositionedGlyph<'_>) -> Option<Vec<char>> {
        let g: &Glyph<'_> = glyph;
        let Glyph::Outline(outline) = g else {
            return None;
        };
        let name = outline
            .font_data()
            .and_then(|data| data.postscript_name)
            .map(|name| strip_subset(&name).to_owned());
        let resources = &self.resources;
        let fonts = self
            .fonts
            .get_or_insert_with(|| read_fonts(resources.clone()));
        let named: Vec<&FontInfo> = match &name {
            Some(name) => fonts
                .iter()
                .filter(|f| f.name.as_deref() == Some(name.as_str()))
                .collect(),
            None => Vec::new(),
        };
        let candidates: Vec<&FontInfo> = if named.is_empty() {
            fonts.iter().filter(|f| !f.to_unicode).collect()
        } else {
            named
        };
        let mut results = candidates.iter().map(|f| f.unicode_of(glyph.char_code()));
        let first = results.next()??;
        // Fonts that disagree tell nothing.
        results
            .all(|other| other.as_ref() == Some(&first))
            .then_some(first)
    }
}

impl<'a> Device<'a> for Collector<'a> {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let signature: Vec<(u32, [f64; 6])> = run
            .glyphs()
            .iter()
            .map(|g| (g.char_code(), (props.transform * g.transform()).as_coeffs()))
            .collect();
        // Fill and stroke of one text come as two runs.
        let repeat = matches!(mode, DrawMode::Stroke(_)) && signature == self.last_run;
        self.last_run = signature;
        if repeat || self.done {
            return;
        }
        let vertical = run.writing_mode() == WritingMode::Vertical;
        for glyph in run.glyphs() {
            if self.done {
                break;
            }
            self.glyph(glyph, props.transform, vertical);
        }
    }
}

fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => name,
    }
}

/// What a composite font's dictionary says about its codes.
struct FontInfo {
    /// `/BaseFont` without a subset prefix.
    name: Option<String>,
    to_unicode: bool,
    /// The codes are Unicode (`UCS2` or `UTF16` encodings).
    unicode_codes: bool,
    /// Identity encodings: the code is the CID.
    identity: bool,
    encoding: Option<CMap>,
    /// CID to Unicode, for collections that have a table.
    ucs2: Option<CMap>,
}

impl FontInfo {
    fn unicode_of(&self, code: u32) -> Option<Vec<char>> {
        if self.unicode_codes {
            return if code > 0xFFFF {
                let units = [(code >> 16) as u16, code as u16];
                let mut decoded = char::decode_utf16(units);
                let c = decoded.next()?.ok()?;
                decoded.next().is_none().then(|| vec![c])
            } else {
                char::from_u32(code).map(|c| vec![c])
            };
        }
        let cid = if self.identity {
            code
        } else {
            let encoding = self.encoding.as_ref()?;
            (1..=4_u8).find_map(|len| encoding.lookup_cid_code(code, len))?
        };
        match self.ucs2.as_ref()?.lookup_bf_string(cid)? {
            BfString::Char(c) => Some(vec![c]),
            BfString::String(s) => Some(s.chars().collect()),
        }
    }
}

/// The composite fonts of `resources` and of the forms below them.
fn read_fonts(resources: Resources<'_>) -> Vec<FontInfo> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut budget = MAX_ENTRIES;
    read_fonts_in(&resources, 0, &mut found, &mut seen, &mut budget);
    found
}

fn read_fonts_in(
    resources: &Resources<'_>,
    depth: usize,
    found: &mut Vec<FontInfo>,
    seen: &mut HashSet<ObjectIdentifier>,
    budget: &mut usize,
) {
    let fonts = resources.fonts.clone();
    for name in fonts.keys().collect::<Vec<Name<'_>>>() {
        if *budget == 0 || found.len() >= MAX_FONTS {
            return;
        }
        *budget -= 1;
        if let Some(dict) = fonts.get::<Dict<'_>>(name.as_ref())
            && let Some(info) = font_info(&dict)
        {
            found.push(info);
        }
    }
    if depth >= MAX_FORM_DEPTH {
        return;
    }
    let forms = resources.x_objects.clone();
    for name in forms.keys().collect::<Vec<Name<'_>>>() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Some(form) = forms.get::<Stream<'_>>(name.as_ref()) else {
            continue;
        };
        if !seen.insert(form.obj_id()) {
            continue;
        }
        if let Some(inner) = form.dict().get::<Dict<'_>>(RESOURCES) {
            read_fonts_in(&Resources::new(inner), depth + 1, found, seen, budget);
        }
    }
}

fn font_info(dict: &Dict<'_>) -> Option<FontInfo> {
    if dict.get::<Name<'_>>(SUBTYPE)?.as_ref() != b"Type0" {
        return None;
    }
    let name = dict
        .get::<Name<'_>>(b"BaseFont")
        .map(|n| strip_subset(n.as_str()).to_owned());
    let mut info = FontInfo {
        name,
        to_unicode: dict.contains_key(TO_UNICODE),
        unicode_codes: false,
        identity: false,
        encoding: None,
        ucs2: None,
    };
    let mut family = None;
    match dict.get::<Object<'_>>(ENCODING)? {
        Object::Name(n) => {
            let bytes: &[u8] = n.as_ref();
            if bytes == b"Identity-H" || bytes == b"Identity-V" {
                info.identity = true;
            } else if bytes.starts_with(b"Uni")
                && (contains(bytes, b"UCS2") || contains(bytes, b"UTF16"))
            {
                info.unicode_codes = true;
            } else if let Some(data) =
                hayro::hayro_interpret::hayro_cmap::load_embedded(CMapName::from_bytes(bytes))
            {
                info.encoding = CMap::parse(data, load);
            }
        }
        Object::Stream(stream) => {
            let data = stream.decoded_data().ok()?;
            info.encoding = CMap::parse(&data, load);
        }
        _ => return None,
    }
    if let Some(encoding) = &info.encoding {
        family = encoding
            .metadata()
            .character_collection
            .as_ref()
            .map(|c| c.family.clone())
            .filter(|f| *f != CidFamily::AdobeIdentity);
    }
    if family.is_none() {
        family = descendant(dict)
            .and_then(|d| d.get::<Dict<'_>>(b"CIDSystemInfo"))
            .and_then(|system| {
                let registry = system
                    .get::<hayro::hayro_interpret::hayro_syntax::object::String<'_>>(b"Registry")?;
                let ordering = system
                    .get::<hayro::hayro_interpret::hayro_syntax::object::String<'_>>(b"Ordering")?;
                Some(CidFamily::from_registry_ordering(
                    registry.as_bytes(),
                    ordering.as_bytes(),
                ))
            });
    }
    if let Some(family) = family
        && let Some(table) = family.ucs2_cmap()
        && let Some(data) = hayro::hayro_interpret::hayro_cmap::load_embedded(table)
    {
        info.ucs2 = CMap::parse(data, load);
    }
    let _ = (FONT, XOBJECT, FONT_DESC);
    Some(info)
}

fn descendant<'a>(dict: &Dict<'a>) -> Option<Dict<'a>> {
    dict.get::<hayro::hayro_interpret::hayro_syntax::object::Array<'a>>(DESCENDANT_FONTS)?
        .iter::<Dict<'a>>()
        .next()
}

fn load(name: CMapName<'_>) -> Option<&'static [u8]> {
    hayro::hayro_interpret::hayro_cmap::load_embedded(name)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Shared by the tests of text, outline and find.
#[cfg(test)]
pub(crate) mod testing {
    /// Open `bytes` on a thread with the child's stack and run `f` on the
    /// document there (the parser's data is not shared between threads).
    pub(crate) fn with_document<T: Send + 'static>(
        bytes: Vec<u8>,
        limits: crate::Limits,
        f: impl FnOnce(&crate::Document) -> T + Send + 'static,
    ) -> T {
        std::thread::Builder::new()
            .stack_size(crate::STACK_BYTES)
            .spawn(move || {
                let document =
                    crate::Document::open(bytes, "", limits).expect("the test file opens");
                f(&document)
            })
            .unwrap()
            .join()
            .expect("the test thread panicked")
    }
}

#[cfg(test)]
mod tests {
    use super::testing::with_document;
    use super::*;
    use crate::Limits;
    use crate::builder::text_page;

    fn text_of(bytes: Vec<u8>) -> PageText {
        with_document(bytes, Limits::default(), |d| d.page_text(0).unwrap())
    }

    fn boxes_of(t: &PageText, from: usize, to: usize) -> Vec<TextBox> {
        t.boxes[from..to].iter().map(|b| b.unwrap()).collect()
    }

    #[test]
    fn order_separators_and_boxes() {
        let t = text_of(text_page(
            "0 0 200 100",
            "",
            b"BT /F1 12 Tf 10 80 Td (Hello) Tj 60 0 Td (World) Tj -60 -20 Td (Next line) Tj ET",
        ));
        assert_eq!(t.text, "Hello World\nNext line");
        assert_eq!(t.boxes.len(), t.text.chars().count());
        // A space and a line break were put in: no box.
        assert_eq!(t.boxes[5], None);
        assert_eq!(t.boxes[11], None);
        let first = boxes_of(&t, 0, 5);
        let second = boxes_of(&t, 6, 11);
        let last_of_first = first[4];
        // The second word starts to the right of the first.
        assert!(second[0][0] > last_of_first[2], "{second:?} {first:?}");
        assert!(first[0][0] >= 9.0 && first[0][0] < 12.0);
        // Page space is top-down: y 80 up is 20 down; 12 pt text.
        assert!(first[0][1] > 5.0 && first[0][1] < 25.0, "{first:?}");
        assert!(first[0][3] > first[0][1] && first[0][3] - first[0][1] < 16.0);
        // The second line is below the first.
        let line2 = boxes_of(&t, 12, 21);
        assert!(line2[0][1] > first[0][3] - 2.0, "{line2:?} {first:?}");
        // Letters follow each other.
        for pair in first.windows(2) {
            assert!(pair[1][0] >= pair[0][0]);
        }
    }

    #[test]
    fn gaps_from_adjustments_become_spaces() {
        let t = text_of(text_page(
            "0 0 200 100",
            "",
            b"BT /F1 12 Tf 10 50 Td [(ab) -1000 (cd) -50 (ef)] TJ ET",
        ));
        assert_eq!(t.text, "ab cdef");
    }

    #[test]
    fn rotated_page_gives_boxes_in_the_shown_space() {
        let bytes = text_page(
            "0 0 200 100",
            "/Rotate 90",
            b"BT /F1 12 Tf 20 30 Td (Hi) Tj ET",
        );
        let (t, size) = with_document(bytes, Limits::default(), |d| {
            (d.page_text(0).unwrap(), d.info().pages[0])
        });
        // Shown 100 wide and 200 tall.
        assert_eq!((size.width, size.height), (100.0, 200.0));
        assert_eq!(t.text, "Hi");
        let [l, top, r, b] = t.boxes[0].unwrap();
        // Turned clockwise: the baseline is along the page's downward side.
        // The point (20, 30) lands at (x = 30, y = 20) when shown.
        assert!(r - l < 12.0 && b - top > 3.0, "{l} {top} {r} {b}");
        let [l2, top2, _, b2] = t.boxes[1].unwrap();
        assert!(top2 >= top, "the next letter is below: {top} {top2}");
        assert!(b2 > b);
        assert!((top - 20.0).abs() < 6.0, "{top}");
        assert!(l > 15.0 && l < 45.0, "{l} {l2}");
        for b in t.boxes.iter().flatten() {
            assert!(b[0] <= b[2] && b[1] <= b[3]);
            assert!(b[2] <= 100.0 + 100.0 && b[3] <= 200.0 + 100.0);
        }
    }

    #[test]
    fn control_characters_are_removed() {
        // Bytes 1, 9 (tab), 10 and 31 among letters of Helvetica: they have
        // no glyph names, so they give nothing or a space, never a control.
        let t = text_of(text_page(
            "0 0 200 100",
            "",
            b"BT /F1 12 Tf 10 50 Td (a\\001b\\011c\\012d\\037e) Tj ET",
        ));
        assert!(
            t.text.chars().all(|c| !c.is_control() || c == '\n'),
            "{:?}",
            t.text
        );
        assert!(t.text.starts_with('a'));
        assert!(t.text.contains('e'));
        assert_eq!(t.boxes.len(), t.text.chars().count());
    }

    #[test]
    fn text_is_cut_at_the_limit() {
        let limits = Limits {
            max_text_chars: 8,
            ..Limits::default()
        };
        let bytes = text_page(
            "0 0 200 100",
            "",
            b"BT /F1 12 Tf 10 50 Td (Hello world and more) Tj ET",
        );
        let t = with_document(bytes, limits, |d| d.page_text(0).unwrap());
        assert_eq!(t.text, "Hello wo");
        assert_eq!(t.boxes.len(), 8);
        let none = with_document(
            text_page("0 0 200 100", "", b"BT /F1 12 Tf (x) Tj ET"),
            Limits {
                max_text_chars: 0,
                ..Limits::default()
            },
            |d| d.page_text(0).unwrap(),
        );
        assert_eq!(none.text, "");
    }

    #[test]
    fn cjk_text_is_read() {
        let t = text_of(text_page(
            "0 0 200 100",
            "",
            b"BT /F2 20 Tf 10 50 Td <4F60597D> Tj ET",
        ));
        assert_eq!(t.text, "\u{4F60}\u{597D}");
        let [l0, _, r0, _] = t.boxes[0].unwrap();
        let [l1, _, _, _] = t.boxes[1].unwrap();
        // Full-width: about 20 points each.
        assert!((r0 - l0 - 20.0).abs() < 1.0, "{l0} {r0}");
        assert!((l1 - r0).abs() < 1.0);
    }

    #[test]
    fn missing_page_and_empty_page() {
        let bytes = text_page("0 0 200 100", "", b"");
        let (a, b) = with_document(bytes, Limits::default(), |d| {
            (d.page_text(0).unwrap(), d.page_text(7).unwrap())
        });
        assert_eq!(a.text, "");
        assert_eq!(b.page, 7);
        assert!(b.boxes.is_empty());
    }

    #[test]
    fn stroke_and_fill_text_is_read_once() {
        let t = text_of(text_page(
            "0 0 200 100",
            "",
            b"BT /F1 12 Tf 2 Tr 10 50 Td (Once) Tj ET",
        ));
        assert_eq!(t.text, "Once");
    }

    #[test]
    fn cid_fonts_without_to_unicode_use_the_ucs2_table() {
        // Embedded CID fonts cannot be built here, so the lookup is tried
        // on the page's font dictionaries: /F3 is Identity-H with the GB1
        // collection (CID 34 is "A", 66 is "a"), /F2 has a Unicode encoding.
        let bytes = text_page("0 0 200 100", "", b"");
        let fonts = with_document(bytes, Limits::default(), |d| {
            let page = &d.pdf.pages()[0];
            let infos = read_fonts(page.resources().clone());
            infos
                .iter()
                .map(|f| (f.unicode_of(34), f.unicode_of(0x4F60), f.identity))
                .collect::<Vec<_>>()
        });
        assert_eq!(fonts.len(), 2);
        let identity = fonts.iter().find(|f| f.2).unwrap();
        assert_eq!(identity.0, Some(vec!['A']));
        let unicode = fonts.iter().find(|f| !f.2).unwrap();
        assert_eq!(unicode.1, Some(vec!['\u{4F60}']));
        assert_eq!(unicode.0, Some(vec!['"']));
    }
}
