//! PPTX: presentations into [`aster_slides::Presentation`].
//!
//! Read: the slide size and order, hidden slides, and on each slide its
//! shapes in drawing order: text shapes and their paragraphs (size, bold,
//! italic, underline, alignment, level, bullets), the outline geometry of
//! autoshapes and connectors, and the frames of pictures, tables, charts and
//! other graphics. Groups are flattened with their transforms applied.
//!
//! A slide inherits from its layout and the layout's master: placeholder
//! positions, text sizes, bullets and body insets come from there when the
//! slide does not set them, and the shapes of the layout and master that are
//! not placeholders (logos, rules) are drawn behind the slide's own unless
//! the slide or layout turns that off.
//!
//! Colours go through the master's theme and colour map: backgrounds, shape
//! fills and outlines (their own or the theme style they refer to), text
//! colours and theme fonts. Pictures are read with their cropping, tables
//! with their cells, merges, fills and borders (their own or the table
//! style's), and each slide's speaker notes as plain text. Animations,
//! charts, effects and 3-D are not read.

use std::collections::HashMap;
use std::sync::Arc;

use aster_slides::{
    Align, Anchor, DEFAULT_TEXT_POINTS, Fill, Frame, Geometry, Layout, LevelLook, Levels as Looks,
    List, Outline, ParaSource, Paragraph, Picture, Placeholder as SlidePlaceholder, Presentation,
    Role, Run, RunSource, Shape, ShapeSource, Slide, SlideLook, SlideSource, Table, TableCell,
    TableRow, TextBody,
};

use crate::drawingml::{
    ColorContext, ColorMap, ColorSpec, FillSpec, LineSpec, Theme, is_color, is_fill, read_color,
    read_color_in, read_fill, read_line, read_theme,
};
use crate::error::Result;
use crate::package::{Kind, Package};
use crate::xml::{Ev, Tag, XmlReader, each_child};

/// A line the file shows but does not size: 0.75 points.
const DEFAULT_LINE: i64 = 9_525;

pub fn read(bytes: Vec<u8>) -> Result<Presentation> {
    let mut package = Package::open(bytes)?;
    read_package(&mut package)
}

pub fn read_package(package: &mut Package) -> Result<Presentation> {
    let main = package.expect(Kind::Pptx)?;
    let main_rels = package.relationships(&main)?;
    let rels: HashMap<String, Option<String>> = main_rels
        .iter()
        .map(|rel| (rel.id.clone(), rel.target.clone()))
        .collect();
    let bytes = package.read(&main)?;
    let head = presentation(&bytes, &main)?;
    let mut table_styles = builtin_table_styles();
    if let Some(part) = main_rels
        .iter()
        .find(|rel| rel.kind == "tableStyles")
        .and_then(|rel| rel.target.clone())
        .filter(|part| package.has(part))
    {
        // A table style list that does not parse only loses table styles.
        if let Ok(styles) = read_table_styles(&package.read(&part)?, &part) {
            table_styles.extend(styles);
        }
    }

    let mut images = Images::default();
    let mut layouts: HashMap<String, Option<Template>> = HashMap::new();
    let mut masters: HashMap<String, Option<Master>> = HashMap::new();
    // The masters the presentation lists, then those its slides use, and
    // their layouts in the order each master lists them.
    let mut master_parts: Vec<String> = head
        .master_ids
        .iter()
        .filter_map(|id| rels.get(id).cloned().flatten())
        .collect();
    let mut read_slides = Vec::with_capacity(head.slide_ids.len());
    for id in &head.slide_ids {
        let Some(Some(part)) = rels.get(id) else {
            continue;
        };
        if !package.has(part) {
            continue;
        }
        let bytes = package.read(part)?;
        let mut slide = template(&bytes, part)?;
        images.attach(package, part, &mut slide)?;
        let layout_part = linked(package, part, "slideLayout")?;
        load_layout(package, &mut images, &mut layouts, layout_part.as_deref())?;
        let master_part = match &layout_part {
            Some(layout_part) => linked(package, layout_part, "slideMaster")?,
            None => None,
        };
        if let Some(master_part) = &master_part
            && !master_parts.contains(master_part)
        {
            master_parts.push(master_part.clone());
        }
        let notes = match linked(package, part, "notesSlide")? {
            // Notes that do not parse are left out rather than failing the
            // slide.
            Some(notes) if package.has(&notes) => {
                notes_text(&package.read(&notes)?, &notes).ok().flatten()
            }
            _ => None,
        };
        read_slides.push((part.clone(), slide, layout_part, master_part, notes));
    }
    let mut layout_order: Vec<String> = Vec::new();
    for master_part in &master_parts {
        if masters.contains_key(master_part) {
            continue;
        }
        let parsed = match package.has(master_part) {
            true => {
                let mut parsed = master(&package.read(master_part)?, master_part)?;
                images.attach(package, master_part, &mut parsed.template)?;
                if let Some(theme_part) = linked(package, master_part, "theme")?
                    && package.has(&theme_part)
                {
                    parsed.theme =
                        read_theme(&package.read(&theme_part)?, &theme_part).unwrap_or_default();
                }
                let targets: HashMap<String, String> = package
                    .relationships(master_part)?
                    .into_iter()
                    .filter_map(|rel| Some((rel.id, rel.target?)))
                    .collect();
                for id in &parsed.layout_ids {
                    if let Some(target) = targets.get(id)
                        && !layout_order.contains(target)
                    {
                        layout_order.push(target.clone());
                    }
                }
                Some(parsed)
            }
            false => None,
        };
        masters.insert(master_part.clone(), parsed);
    }
    for part in &layout_order {
        load_layout(package, &mut images, &mut layouts, Some(part))?;
    }
    let language = east_asian_language(read_slides.iter().map(|s| &s.1));
    let language_tag = language.as_deref().unwrap_or("en");
    let context_for = |master| Context {
        master,
        defaults: &head.default_style,
        table_styles: &table_styles,
        language: language_tag,
    };
    let mut out_layouts = Vec::new();
    for part in &layout_order {
        let Some(Some(layout)) = layouts.get(part) else {
            continue;
        };
        let master_part = linked(package, part, "slideMaster")?;
        let master = master_part.as_ref().and_then(|p| masters.get(p)?.as_ref());
        let mut slide = resolve_slide(&blank_on(layout), Some(layout), &context_for(master));
        slide.layout = Some(out_layouts.len());
        out_layouts.push(Layout {
            name: layout.name.clone(),
            part: part.clone(),
            slide,
        });
    }
    let mut slides = Vec::with_capacity(read_slides.len());
    for (part, slide, layout_part, master_part, notes) in read_slides {
        let layout = layout_part.as_ref().and_then(|p| layouts.get(p)?.as_ref());
        let master = master_part.as_ref().and_then(|p| masters.get(p)?.as_ref());
        let mut resolved = resolve_slide(&slide, layout, &context_for(master));
        resolved.notes = notes.clone();
        resolved.layout = layout_part
            .as_ref()
            .and_then(|p| layout_order.iter().position(|l| l == p));
        let shapes = resolved
            .shapes
            .iter()
            .filter_map(|shape| Some(shape.source.as_ref()?.index))
            .collect();
        resolved.source = Some(Arc::new(SlideSource {
            part,
            shapes,
            notes,
            copy: false,
        }));
        slides.push(resolved);
    }
    Ok(Presentation {
        size: head.size,
        slides,
        layouts: out_layouts,
        east_asian_language: language,
    })
}

/// Read a layout once.
fn load_layout(
    package: &mut Package,
    images: &mut Images,
    layouts: &mut HashMap<String, Option<Template>>,
    part: Option<&str>,
) -> Result<()> {
    let Some(part) = part else {
        return Ok(());
    };
    if layouts.contains_key(part) {
        return Ok(());
    }
    let parsed = match package.has(part) {
        true => {
            let mut layout = template(&package.read(part)?, part)?;
            images.attach(package, part, &mut layout)?;
            Some(layout)
        }
        false => None,
    };
    layouts.insert(part.to_owned(), parsed);
    Ok(())
}

/// A new slide on `layout`: an empty placeholder for each of the layout's,
/// except the date, footer and slide number, as PowerPoint makes it.
fn blank_on(layout: &Template) -> Template {
    let shapes = layout
        .shapes
        .iter()
        .filter_map(|shape| {
            let ph = shape.placeholder.clone()?;
            if matches!(ph.kind.as_str(), "dt" | "ftr" | "sldNum") {
                return None;
            }
            let mut raw = RawShape::new(Role::Shape);
            raw.placeholder = Some(ph);
            raw.body = Some(RawBody {
                paragraphs: vec![RawParagraph::default()],
                ..RawBody::default()
            });
            Some(raw)
        })
        .collect();
    Template {
        shapes,
        show_master_shapes: true,
        ..Template::default()
    }
}

/// The East Asian language most text of the slides is marked with.
fn east_asian_language<'a>(slides: impl Iterator<Item = &'a Template>) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for slide in slides {
        for shape in &slide.shapes {
            let Some(body) = &shape.body else { continue };
            for paragraph in &body.paragraphs {
                for run in &paragraph.runs {
                    for language in [&run.language, &run.alt_language].into_iter().flatten() {
                        let lower = language.to_ascii_lowercase();
                        if ["ja", "zh", "ko"].iter().any(|p| lower.starts_with(p)) {
                            *counts.entry(language.clone()).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(language, _)| language)
}

/// Picture bytes by part name, read once and shared by every slide that
/// shows them.
#[derive(Default)]
struct Images(HashMap<String, Arc<[u8]>>);

impl Images {
    /// Read the pictures a template's shapes and background refer to.
    fn attach(&mut self, package: &mut Package, part: &str, template: &mut Template) -> Result<()> {
        let wanted = template
            .shapes
            .iter()
            .flat_map(|shape| [shape.blip.as_ref().map(|b| &b.0), shape.fill_picture()])
            .chain([template.background_picture()])
            .flatten()
            .next()
            .is_some();
        if !wanted {
            return Ok(());
        }
        let rels: HashMap<String, String> = package
            .relationships(part)?
            .into_iter()
            .filter_map(|rel| Some((rel.id, rel.target?)))
            .collect();
        let mut load = |id: &str| -> Result<Option<Arc<[u8]>>> {
            let Some(target) = rels.get(id) else {
                return Ok(None);
            };
            if let Some(found) = self.0.get(target) {
                return Ok(Some(found.clone()));
            }
            if !package.has(target) {
                return Ok(None);
            }
            let bytes: Arc<[u8]> = Arc::from(package.read(target)?);
            self.0.insert(target.clone(), bytes.clone());
            Ok(Some(bytes))
        };
        for shape in &mut template.shapes {
            if let Some((id, _)) = &shape.blip {
                shape.image = load(id)?;
            }
            if let Some(id) = shape.fill_picture().cloned() {
                shape.fill_image = load(&id)?;
            }
        }
        if let Some(id) = template.background_picture().cloned() {
            template.background_image = load(&id)?;
        }
        Ok(())
    }
}

/// The part `source` points at with a relationship of `kind`.
fn linked(package: &mut Package, source: &str, kind: &str) -> Result<Option<String>> {
    Ok(package
        .relationships(source)?
        .into_iter()
        .find(|rel| rel.kind == kind)
        .and_then(|rel| rel.target))
}

struct Head {
    size: (i64, i64),
    slide_ids: Vec<String>,
    master_ids: Vec<String>,
    default_style: Levels,
}

fn presentation(bytes: &[u8], part: &str) -> Result<Head> {
    let mut reader = XmlReader::new(bytes, part);
    let mut head = Head {
        size: aster_slides::DEFAULT_SIZE,
        slide_ids: Vec::new(),
        master_ids: Vec::new(),
        default_style: Levels::default(),
    };
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("sldSz") => {
                let (cx, cy) = (tag.attr_i64("cx"), tag.attr_i64("cy"));
                if let (Some(cx), Some(cy)) = (cx, cy)
                    && cx > 0
                    && cy > 0
                {
                    head.size = (cx, cy);
                }
            }
            Ev::Open(tag) if tag.is("sldId") => {
                if let Some(id) = rel_id(&tag) {
                    head.slide_ids.push(id);
                }
            }
            Ev::Open(tag) if tag.is("sldMasterId") => {
                if let Some(id) = rel_id(&tag) {
                    head.master_ids.push(id);
                }
            }
            Ev::Open(tag) if tag.is("defaultTextStyle") => {
                head.default_style = levels(&mut reader)?;
            }
            Ev::Open(tag) if tag.is("notesSz") || tag.is("extLst") => reader.skip()?,
            Ev::Eof => return Ok(head),
            _ => {}
        }
    }
}

/// `r:id` of a `sldId`. Its own `id` attribute is a number; the relationship
/// id is the one in the relationships namespace, which also has the local
/// name `id`.
fn rel_id(tag: &Tag<'_>) -> Option<String> {
    tag.attrs("id")
        .into_iter()
        .find(|id| !id.is_empty() && !id.bytes().all(|b| b.is_ascii_digit()))
}

/// Text properties of one outline level.
#[derive(Clone, Debug, Default, PartialEq)]
struct Level {
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<ColorSpec>,
    latin: Option<String>,
    east_asian: Option<String>,
    align: Option<Align>,
    /// `Some(None)` is an explicit "no bullet".
    bullet: Option<Option<Bullet>>,
    margin: Option<i64>,
    indent: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Bullet {
    Char(String),
    /// The scheme and the first number.
    Number(String, u32),
}

impl Bullet {
    fn list(&self) -> List {
        match self {
            Bullet::Char(c) => List::Char(c.clone()),
            Bullet::Number(scheme, start) => List::Number(scheme.clone(), *start),
        }
    }
}

type Levels = [Level; 9];

#[derive(Clone, Debug, Default, PartialEq)]
struct BodyProps {
    insets: [Option<i64>; 4],
    anchor: Option<Anchor>,
    wrap: Option<bool>,
    font_scale: Option<f32>,
    /// `spAutoFit`: the shape fits its text.
    grow: Option<bool>,
}

#[derive(Clone, Debug, Default)]
struct RawRun {
    text: String,
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: bool,
    strike: bool,
    color: Option<ColorSpec>,
    latin: Option<String>,
    east_asian: Option<String>,
    /// `lang`, which picks the theme's East Asian font, and `altLang`.
    language: Option<String>,
    alt_language: Option<String>,
    /// The run element and its properties element as written.
    xml: String,
    props_xml: String,
}

#[derive(Clone, Debug, Default)]
struct RawParagraph {
    level: u8,
    props: Level,
    runs: Vec<RawRun>,
    /// The end mark's properties.
    end: Option<RawRun>,
    /// The paragraph, its `pPr` and its `endParaRPr` as written.
    xml: String,
    props_xml: String,
    end_xml: String,
}

#[derive(Clone, Debug, Default)]
struct RawBody {
    props: BodyProps,
    list: Levels,
    paragraphs: Vec<RawParagraph>,
}

#[derive(Clone, Debug, PartialEq)]
struct Placeholder {
    kind: String,
    idx: Option<u32>,
}

impl Placeholder {
    /// The family a placeholder type belongs to when matched on a master.
    fn family(&self) -> &str {
        match self.kind.as_str() {
            "title" | "ctrTitle" => "title",
            "dt" => "dt",
            "ftr" => "ftr",
            "sldNum" => "sldNum",
            _ => "body",
        }
    }
}

/// A theme style a shape refers to: its index and the colour that stands
/// in for `phClr`.
#[derive(Clone, Debug, PartialEq)]
struct StyleRef {
    index: i64,
    color: Option<ColorSpec>,
}

#[derive(Clone, Debug)]
struct RawShape {
    frame: Option<Frame>,
    rotation: f64,
    flip_h: bool,
    flip_v: bool,
    geometry: Option<Geometry>,
    role: Role,
    placeholder: Option<Placeholder>,
    fill: Option<FillSpec>,
    line: Option<LineSpec>,
    fill_ref: Option<StyleRef>,
    line_ref: Option<StyleRef>,
    /// The text colour of the shape's style.
    font_color: Option<ColorSpec>,
    body: Option<RawBody>,
    /// A picture's relationship id and crop (left, top, right, bottom).
    blip: Option<(String, [f64; 4])>,
    image: Option<Arc<[u8]>>,
    /// The picture of a picture fill.
    fill_image: Option<Arc<[u8]>>,
    table: Option<RawTable>,
    /// For a shape inside a group: child to slide coordinates.
    group: Option<Transform>,
}

impl RawShape {
    fn new(role: Role) -> Self {
        Self {
            frame: None,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry: None,
            role,
            placeholder: None,
            fill: None,
            line: None,
            fill_ref: None,
            line_ref: None,
            font_color: None,
            body: None,
            blip: None,
            image: None,
            fill_image: None,
            table: None,
            group: None,
        }
    }

    fn fill_picture(&self) -> Option<&String> {
        match &self.fill {
            Some(FillSpec::Picture(id)) => Some(id),
            _ => None,
        }
    }
}

/// A background as written: a fill, or a theme background style.
#[derive(Clone, Debug)]
enum Background {
    Fill(FillSpec),
    Ref(StyleRef),
}

/// A slide, layout or master: its shapes and whether the shapes of the
/// templates behind it show through.
#[derive(Debug, Default)]
struct Template {
    /// `cSld`'s `name`: a layout's name.
    name: String,
    shapes: Vec<RawShape>,
    show_master_shapes: bool,
    hidden: bool,
    background: Option<Background>,
    background_image: Option<Arc<[u8]>>,
    /// A master's `clrMap`, or a layout's or slide's override of it.
    color_map: Option<ColorMap>,
}

impl Template {
    fn background_picture(&self) -> Option<&String> {
        match &self.background {
            Some(Background::Fill(FillSpec::Picture(id))) => Some(id),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct Master {
    template: Template,
    /// Relationship ids of its layouts, in order.
    layout_ids: Vec<String>,
    theme: Theme,
    title: Levels,
    body: Levels,
    other: Levels,
}

fn template(bytes: &[u8], part: &str) -> Result<Template> {
    let mut reader = XmlReader::new(bytes, part);
    let mut out = Template {
        show_master_shapes: true,
        ..Template::default()
    };
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("sld") || tag.is("sldLayout") || tag.is("sldMaster") => {
                out.show_master_shapes = tag.attr_bool("showMasterSp").unwrap_or(true);
                out.hidden = tag.attr_bool("show") == Some(false);
            }
            Ev::Open(tag) if tag.is("cSld") => {
                out.name = tag.attr("name").unwrap_or_default();
            }
            Ev::Open(tag) if tag.is("spTree") => {
                shape_tree(&mut reader, Transform::IDENTITY, false, &mut out.shapes)?;
            }
            Ev::Open(tag) if tag.is("bg") => {
                each_child(&mut reader, |reader, child| {
                    if child.is("bgPr") {
                        each_child(reader, |reader, fill| {
                            if is_fill(&fill) {
                                if let Some(fill) = read_fill(reader, &fill)? {
                                    out.background = Some(Background::Fill(fill));
                                }
                                Ok(())
                            } else {
                                reader.skip()
                            }
                        })
                    } else if child.is("bgRef") {
                        let index = child.attr_i64("idx").unwrap_or(0);
                        let color = read_color_in(reader)?;
                        out.background = Some(Background::Ref(StyleRef { index, color }));
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            Ev::Open(tag) if tag.is("clrMap") || tag.is("overrideClrMapping") => {
                out.color_map = Some(ColorMap::from_tag(&tag));
                reader.skip()?;
            }
            Ev::Open(tag) if tag.is("txStyles") || tag.is("extLst") || tag.is("timing") => {
                reader.skip()?
            }
            Ev::Eof => return Ok(out),
            _ => {}
        }
    }
}

fn master(bytes: &[u8], part: &str) -> Result<Master> {
    let template = template(bytes, part)?;
    let mut out = Master {
        template,
        ..Master::default()
    };
    let mut reader = XmlReader::new(bytes, part);
    while let Some(list) = reader.find("sldLayoutIdLst")? {
        let _ = list;
        each_child(&mut reader, |reader, tag| {
            if tag.is("sldLayoutId")
                && let Some(id) = rel_id(&tag)
            {
                out.layout_ids.push(id);
            }
            reader.skip()
        })?;
    }
    let mut reader = XmlReader::new(bytes, part);
    while let Some(_styles) = reader.find("txStyles")? {
        each_child(&mut reader, |reader, tag| {
            if tag.is("titleStyle") {
                out.title = levels(reader)?;
            } else if tag.is("bodyStyle") {
                out.body = levels(reader)?;
            } else if tag.is("otherStyle") {
                out.other = levels(reader)?;
            } else {
                reader.skip()?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Maps child coordinates of a group to slide coordinates.
#[derive(Clone, Copy, Debug)]
struct Transform {
    scale_x: f64,
    scale_y: f64,
    dx: f64,
    dy: f64,
}

impl Transform {
    const IDENTITY: Transform = Transform {
        scale_x: 1.0,
        scale_y: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    fn apply(&self, frame: Frame) -> Frame {
        Frame::new(
            (self.dx + frame.x as f64 * self.scale_x).round() as i64,
            (self.dy + frame.y as f64 * self.scale_y).round() as i64,
            (frame.width as f64 * self.scale_x).round() as i64,
            (frame.height as f64 * self.scale_y).round() as i64,
        )
    }

    /// This transform after `inner`.
    fn then(&self, inner: Transform) -> Transform {
        Transform {
            scale_x: self.scale_x * inner.scale_x,
            scale_y: self.scale_y * inner.scale_y,
            dx: self.dx + inner.dx * self.scale_x,
            dy: self.dy + inner.dy * self.scale_y,
        }
    }
}

/// Group and shape transforms as written: `off`, `ext`, and for groups the
/// child space `chOff`, `chExt`.
#[derive(Clone, Copy, Debug, Default)]
struct Xfrm {
    off: Option<(i64, i64)>,
    ext: Option<(i64, i64)>,
    ch_off: Option<(i64, i64)>,
    ch_ext: Option<(i64, i64)>,
    rotation: f64,
    flip_h: bool,
    flip_v: bool,
}

impl Xfrm {
    fn frame(&self) -> Option<Frame> {
        let (x, y) = self.off?;
        let (w, h) = self.ext.unwrap_or((0, 0));
        Some(Frame::new(x, y, w.max(0), h.max(0)))
    }

    fn group(&self) -> Transform {
        let (Some((x, y)), Some((w, h))) = (self.off, self.ext) else {
            return Transform::IDENTITY;
        };
        let (cx, cy) = self.ch_off.unwrap_or((x, y));
        let (cw, ch) = self.ch_ext.unwrap_or((w, h));
        let scale_x = if cw > 0 { w as f64 / cw as f64 } else { 1.0 };
        let scale_y = if ch > 0 { h as f64 / ch as f64 } else { 1.0 };
        Transform {
            scale_x,
            scale_y,
            dx: x as f64 - cx as f64 * scale_x,
            dy: y as f64 - cy as f64 * scale_y,
        }
    }
}

fn read_xfrm(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<Xfrm> {
    let mut xfrm = Xfrm {
        rotation: tag.attr_f64("rot").map_or(0.0, |r| r / 60_000.0),
        flip_h: tag.attr_bool("flipH").unwrap_or(false),
        flip_v: tag.attr_bool("flipV").unwrap_or(false),
        ..Xfrm::default()
    };
    each_child(reader, |reader, child| {
        let point = |a: &str, b: &str| Some((child.attr_i64(a)?, child.attr_i64(b)?));
        if child.is("off") {
            xfrm.off = point("x", "y");
        } else if child.is("ext") {
            xfrm.ext = point("cx", "cy");
        } else if child.is("chOff") {
            xfrm.ch_off = point("x", "y");
        } else if child.is("chExt") {
            xfrm.ch_ext = point("cx", "cy");
        }
        reader.skip()
    })?;
    Ok(xfrm)
}

/// After the `spTree` or `grpSp` open: the shapes inside, flattened.
/// `nested` inside a group.
fn shape_tree(
    reader: &mut XmlReader<'_>,
    transform: Transform,
    nested: bool,
    out: &mut Vec<RawShape>,
) -> Result<()> {
    let mut group = Transform::IDENTITY;
    each_child(reader, |reader, tag| {
        let local = tag.local().to_vec();
        match local.as_slice() {
            b"grpSpPr" => {
                each_child(reader, |reader, child| {
                    if child.is("xfrm") {
                        group = read_xfrm(reader, &child)?.group();
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
            }
            b"grpSp" => shape_tree(reader, transform.then(group), true, out)?,
            b"sp" | b"cxnSp" | b"pic" | b"graphicFrame" => {
                let role = match local.as_slice() {
                    b"pic" => Role::Picture,
                    b"graphicFrame" => Role::Graphic,
                    _ => Role::Shape,
                };
                let mut shape = shape(reader, role)?;
                if local == b"cxnSp" && shape.geometry.is_none() {
                    shape.geometry = Some(Geometry::Line);
                }
                let full = transform.then(group);
                shape.frame = shape.frame.map(|frame| full.apply(frame));
                if nested {
                    shape.group = Some(full);
                }
                out.push(shape);
            }
            b"AlternateContent" => {
                // The first choice is read as if it were in the tree; the
                // fallback repeats it for older readers.
                let mut done = false;
                let inner = transform.then(group);
                each_child(reader, |reader, child| {
                    if child.is("Choice") && !done {
                        done = true;
                        shape_tree(reader, inner, nested, out)
                    } else {
                        reader.skip()
                    }
                })?;
            }
            _ => reader.skip()?,
        }
        Ok(())
    })
}

/// After a shape's open (`sp`, `cxnSp`, `pic`, `graphicFrame`).
fn shape(reader: &mut XmlReader<'_>, role: Role) -> Result<RawShape> {
    let mut shape = RawShape::new(role);
    each_child(reader, |reader, tag| {
        match tag.local() {
            b"nvSpPr" | b"nvCxnSpPr" | b"nvPicPr" | b"nvGraphicFramePr" => {
                non_visual(reader, &mut shape)?
            }
            b"spPr" => shape_props(reader, &mut shape)?,
            // A graphic frame's transform sits outside `spPr`.
            b"xfrm" => {
                let xfrm = read_xfrm(reader, &tag)?;
                shape.frame = xfrm.frame();
            }
            b"graphic" => {
                each_child(reader, |reader, data| {
                    if data.is("graphicData") {
                        let uri = data.attr("uri").unwrap_or_default();
                        shape.role = if uri.ends_with("/table") {
                            Role::Table
                        } else if uri.ends_with("/chart") || uri.contains("chartex") {
                            Role::Chart
                        } else {
                            Role::Graphic
                        };
                        if shape.role == Role::Table {
                            return each_child(reader, |reader, child| {
                                if child.is("tbl") {
                                    shape.table = Some(table(reader)?);
                                    Ok(())
                                } else {
                                    reader.skip()
                                }
                            });
                        }
                    }
                    reader.skip()
                })?;
            }
            b"blipFill" => {
                let mut crop = [0.0; 4];
                let mut embed = None;
                each_child(reader, |reader, child| {
                    if child.is("blip") {
                        embed = child.attr("embed");
                    } else if child.is("srcRect") {
                        let share = |name: &str| {
                            (child.attr_f64(name).unwrap_or(0.0) / 100_000.0).clamp(-1.0, 1.0)
                        };
                        crop = [share("l"), share("t"), share("r"), share("b")];
                    }
                    reader.skip()
                })?;
                shape.blip = embed.map(|id| (id, crop));
            }
            b"style" => {
                each_child(reader, |reader, child| {
                    let reference = || StyleRef {
                        index: child.attr_i64("idx").unwrap_or(0),
                        color: None,
                    };
                    match child.local() {
                        b"lnRef" => {
                            let mut style = reference();
                            style.color = read_color_in(reader)?;
                            shape.line_ref = Some(style);
                            Ok(())
                        }
                        b"fillRef" => {
                            let mut style = reference();
                            style.color = read_color_in(reader)?;
                            shape.fill_ref = Some(style);
                            Ok(())
                        }
                        b"fontRef" => {
                            shape.font_color = read_color_in(reader)?;
                            Ok(())
                        }
                        _ => reader.skip(),
                    }
                })?;
            }
            b"txBody" => shape.body = Some(text_body(reader)?),
            _ => reader.skip()?,
        }
        Ok(())
    })?;
    Ok(shape)
}

fn non_visual(reader: &mut XmlReader<'_>, shape: &mut RawShape) -> Result<()> {
    each_child(reader, |reader, tag| {
        match tag.local() {
            b"nvPr" => {
                each_child(reader, |reader, child| {
                    if child.is("ph") {
                        shape.placeholder = Some(Placeholder {
                            kind: child.attr("type").unwrap_or_else(|| "obj".to_owned()),
                            idx: child.attr_i64("idx").and_then(|i| u32::try_from(i).ok()),
                        });
                    }
                    reader.skip()
                })?;
            }
            _ => reader.skip()?,
        }
        Ok(())
    })
}

fn shape_props(reader: &mut XmlReader<'_>, shape: &mut RawShape) -> Result<()> {
    each_child(reader, |reader, tag| {
        match tag.local() {
            b"xfrm" => {
                let xfrm = read_xfrm(reader, &tag)?;
                shape.frame = xfrm.frame();
                shape.rotation = xfrm.rotation;
                shape.flip_h = xfrm.flip_h;
                shape.flip_v = xfrm.flip_v;
            }
            b"prstGeom" => {
                let preset = tag.attr("prst").unwrap_or_default();
                shape.geometry = Some(match preset.as_str() {
                    "rect" => Geometry::Rect,
                    "roundRect" => Geometry::RoundRect,
                    "ellipse" => Geometry::Ellipse,
                    "line" | "straightConnector1" => Geometry::Line,
                    p if p.contains("Connector") => Geometry::Line,
                    other => Geometry::Other(other.to_owned()),
                });
                reader.skip()?;
            }
            b"custGeom" => {
                shape.geometry = Some(Geometry::Other("custom".to_owned()));
                reader.skip()?;
            }
            b"ln" => shape.line = Some(read_line(reader, &tag)?),
            _ if is_fill(&tag) => {
                if let Some(fill) = read_fill(reader, &tag)? {
                    shape.fill = Some(fill);
                }
            }
            _ => reader.skip()?,
        }
        Ok(())
    })
}

fn text_body(reader: &mut XmlReader<'_>) -> Result<RawBody> {
    let mut body = RawBody::default();
    each_child(reader, |reader, tag| {
        match tag.local() {
            b"bodyPr" => {
                let props = &mut body.props;
                props.insets = [
                    tag.attr_i64("lIns"),
                    tag.attr_i64("tIns"),
                    tag.attr_i64("rIns"),
                    tag.attr_i64("bIns"),
                ];
                props.anchor = match tag.attr("anchor").as_deref() {
                    Some("t") => Some(Anchor::Top),
                    Some("ctr") => Some(Anchor::Middle),
                    Some("b") => Some(Anchor::Bottom),
                    _ => None,
                };
                props.wrap = tag.attr("wrap").map(|wrap| wrap != "none");
                each_child(reader, |reader, child| {
                    if child.is("spAutoFit") {
                        props.grow = Some(true);
                    } else if child.is("noAutofit") || child.is("normAutofit") {
                        props.grow = Some(false);
                    }
                    if child.is("normAutofit") {
                        props.font_scale = Some(
                            child
                                .attr_f64("fontScale")
                                .map_or(1.0, |scale| (scale / 100_000.0) as f32)
                                .clamp(0.01, 1.0),
                        );
                    }
                    reader.skip()
                })?;
            }
            b"lstStyle" => body.list = levels(reader)?,
            b"p" => body.paragraphs.push(paragraph(reader)?),
            _ => reader.skip()?,
        }
        Ok(())
    })?;
    Ok(body)
}

/// After the open of `lstStyle`, a master's `titleStyle` and the like.
fn levels(reader: &mut XmlReader<'_>) -> Result<Levels> {
    let mut out = Levels::default();
    each_child(reader, |reader, tag| {
        let name = tag.local();
        let level = name
            .strip_prefix(b"lvl")
            .and_then(|rest| rest.strip_suffix(b"pPr"))
            .and_then(|digit| std::str::from_utf8(digit).ok()?.parse::<usize>().ok())
            .filter(|n| (1..=9).contains(n));
        match level {
            Some(n) => out[n - 1] = level_props(reader, &tag)?,
            None => reader.skip()?,
        }
        Ok(())
    })?;
    Ok(out)
}

/// After the open of a `lvlNpPr` or a paragraph's `pPr`.
fn level_props(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<Level> {
    let mut level = Level {
        align: match tag.attr("algn").as_deref() {
            Some("l") => Some(Align::Left),
            Some("ctr") => Some(Align::Center),
            Some("r") => Some(Align::Right),
            Some("just" | "dist" | "justLow" | "thaiDist") => Some(Align::Justify),
            _ => None,
        },
        margin: tag.attr_i64("marL"),
        indent: tag.attr_i64("indent"),
        ..Level::default()
    };
    each_child(reader, |reader, child| {
        match child.local() {
            b"defRPr" => {
                let run = run_props(reader, &child)?;
                level.size = run.size;
                level.bold = run.bold;
                level.italic = run.italic;
                level.color = run.color;
                level.latin = run.latin;
                level.east_asian = run.east_asian;
                return Ok(());
            }
            b"buNone" => level.bullet = Some(None),
            b"buChar" => {
                level.bullet = Some(
                    child
                        .attr("char")
                        .filter(|c| !c.is_empty())
                        .map(Bullet::Char),
                )
            }
            b"buAutoNum" => {
                level.bullet = Some(Some(Bullet::Number(
                    child
                        .attr("type")
                        .unwrap_or_else(|| "arabicPeriod".to_owned()),
                    child
                        .attr_i64("startAt")
                        .map_or(1, |n| n.clamp(1, 32_767) as u32),
                )))
            }
            _ => {}
        }
        reader.skip()
    })?;
    Ok(level)
}

/// After the open of `rPr`, `defRPr` or `endParaRPr`: the run properties,
/// with the colour and fonts inside.
fn run_props(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<RawRun> {
    let mut run = RawRun {
        text: String::new(),
        size: tag
            .attr_f64("sz")
            .map(|sz| (sz / 100.0) as f32)
            .filter(|&sz| sz > 0.0 && sz < 4_000.0),
        bold: tag.attr_bool("b"),
        italic: tag.attr_bool("i"),
        underline: tag.attr("u").is_some_and(|u| u != "none"),
        strike: tag.attr("strike").is_some_and(|s| s != "noStrike"),
        language: tag.attr("lang"),
        alt_language: tag.attr("altLang"),
        ..RawRun::default()
    };
    each_child(reader, |reader, child| match child.local() {
        b"solidFill" => {
            run.color = read_color_in(reader)?;
            Ok(())
        }
        b"gradFill" => {
            if let Some(FillSpec::Gradient(stops, _)) = read_fill(reader, &child)? {
                run.color = stops.into_iter().next().map(|(_, color)| color);
            }
            Ok(())
        }
        b"latin" => {
            run.latin = child.attr("typeface").filter(|face| !face.is_empty());
            reader.skip()
        }
        b"ea" => {
            run.east_asian = child.attr("typeface").filter(|face| !face.is_empty());
            reader.skip()
        }
        _ => reader.skip(),
    })?;
    Ok(run)
}

fn paragraph(reader: &mut XmlReader<'_>) -> Result<RawParagraph> {
    let start = reader.opened_at();
    let mut out = RawParagraph::default();
    each_child(reader, |reader, tag| {
        let at = reader.opened_at();
        match tag.local() {
            b"pPr" => {
                out.level = tag.attr_i64("lvl").unwrap_or(0).clamp(0, 8) as u8;
                out.props = level_props(reader, &tag)?;
                out.props_xml = reader.slice(at, reader.position());
            }
            b"r" | b"fld" | b"br" => {
                let is_break = tag.is("br");
                let mut run = RawRun::default();
                let mut props_xml = String::new();
                each_child(reader, |reader, child| {
                    let child_at = reader.opened_at();
                    if child.is("rPr") {
                        run = RawRun {
                            text: std::mem::take(&mut run.text),
                            ..run_props(reader, &child)?
                        };
                        props_xml = reader.slice(child_at, reader.position());
                        Ok(())
                    } else if child.is("t") {
                        run.text.push_str(&reader.text()?);
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
                if is_break {
                    run.text = "\n".to_owned();
                }
                // Vertical tabs are PowerPoint's soft line breaks.
                run.text = run.text.replace('\u{b}', "\n");
                run.xml = reader.slice(at, reader.position());
                run.props_xml = props_xml;
                out.runs.push(run);
            }
            b"endParaRPr" => {
                let mut end = run_props(reader, &tag)?;
                end.props_xml = reader.slice(at, reader.position());
                out.end_xml = end.props_xml.clone();
                out.end = Some(end);
            }
            _ => reader.skip()?,
        }
        Ok(())
    })?;
    out.xml = reader.slice(start, reader.position());
    Ok(out)
}

/// A table as written, before styles and colours are applied.
#[derive(Clone, Debug, Default)]
struct RawTable {
    style_id: Option<String>,
    first_row: bool,
    last_row: bool,
    first_col: bool,
    last_col: bool,
    band_row: bool,
    band_col: bool,
    columns: Vec<i64>,
    rows: Vec<(i64, Vec<RawCell>)>,
}

#[derive(Clone, Debug, Default)]
struct RawCell {
    body: Option<RawBody>,
    span: (u32, u32),
    merged: bool,
    fill: Option<FillSpec>,
    /// Left, top, right, bottom.
    borders: [Option<LineSpec>; 4],
    /// Left, top, right, bottom, EMU.
    margins: [Option<i64>; 4],
    anchor: Option<Anchor>,
}

/// After the open of `a:tbl`.
fn table(reader: &mut XmlReader<'_>) -> Result<RawTable> {
    let mut out = RawTable::default();
    each_child(reader, |reader, tag| match tag.local() {
        b"tblPr" => {
            let flag = |name: &str| tag.attr_bool(name).unwrap_or(false);
            out.first_row = flag("firstRow");
            out.last_row = flag("lastRow");
            out.first_col = flag("firstCol");
            out.last_col = flag("lastCol");
            out.band_row = flag("bandRow");
            out.band_col = flag("bandCol");
            each_child(reader, |reader, child| {
                if child.is("tableStyleId") {
                    out.style_id = Some(reader.text()?.trim().to_owned());
                    Ok(())
                } else {
                    reader.skip()
                }
            })
        }
        b"tblGrid" => each_child(reader, |reader, column| {
            if column.is("gridCol") {
                out.columns.push(column.attr_i64("w").unwrap_or(0).max(0));
            }
            reader.skip()
        }),
        b"tr" => {
            let height = tag.attr_i64("h").unwrap_or(0).max(0);
            let mut cells = Vec::new();
            each_child(reader, |reader, cell| {
                if cell.is("tc") {
                    cells.push(table_cell(reader, &cell)?);
                    Ok(())
                } else {
                    reader.skip()
                }
            })?;
            out.rows.push((height, cells));
            Ok(())
        }
        _ => reader.skip(),
    })?;
    Ok(out)
}

fn table_cell(reader: &mut XmlReader<'_>, tag: &Tag<'_>) -> Result<RawCell> {
    let span = |name: &str| tag.attr_i64(name).unwrap_or(1).clamp(1, 1_000) as u32;
    let mut cell = RawCell {
        span: (span("gridSpan"), span("rowSpan")),
        merged: tag.attr_bool("hMerge").unwrap_or(false)
            || tag.attr_bool("vMerge").unwrap_or(false),
        ..RawCell::default()
    };
    each_child(reader, |reader, child| match child.local() {
        b"txBody" => {
            cell.body = Some(text_body(reader)?);
            Ok(())
        }
        b"tcPr" => {
            cell.margins = [
                child.attr_i64("marL"),
                child.attr_i64("marT"),
                child.attr_i64("marR"),
                child.attr_i64("marB"),
            ];
            cell.anchor = match child.attr("anchor").as_deref() {
                Some("t") => Some(Anchor::Top),
                Some("ctr") => Some(Anchor::Middle),
                Some("b") => Some(Anchor::Bottom),
                _ => None,
            };
            each_child(reader, |reader, property| {
                let side = match property.local() {
                    b"lnL" => Some(0),
                    b"lnT" => Some(1),
                    b"lnR" => Some(2),
                    b"lnB" => Some(3),
                    _ => None,
                };
                if let Some(side) = side {
                    cell.borders[side] = Some(read_line(reader, &property)?);
                    Ok(())
                } else if is_fill(&property) {
                    if let Some(fill) = read_fill(reader, &property)? {
                        cell.fill = Some(fill);
                    }
                    Ok(())
                } else {
                    reader.skip()
                }
            })
        }
        _ => reader.skip(),
    })?;
    Ok(cell)
}

/// One part of a table style (`wholeTbl`, `firstRow`, `band1H` ...).
#[derive(Clone, Debug, Default)]
struct TablePart {
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<ColorSpec>,
    fill: Option<FillSpec>,
    /// Left, top, right, bottom, then the lines between cells: inside
    /// horizontal and inside vertical.
    borders: [Option<LineSpec>; 6],
}

type TableStyle = HashMap<String, TablePart>;

/// The cells a table style part covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Table,
    Row,
    Column,
}

/// The table styles of `tableStyles.xml` by their ids.
fn read_table_styles(bytes: &[u8], part: &str) -> Result<HashMap<String, TableStyle>> {
    let mut reader = XmlReader::new(bytes, part);
    let mut styles = HashMap::new();
    loop {
        match reader.next()? {
            Ev::Open(tag) if tag.is("tblStyle") => {
                let id = tag.attr("styleId").unwrap_or_default();
                let mut style = TableStyle::new();
                each_child(&mut reader, |reader, part| {
                    let name = String::from_utf8_lossy(part.local()).into_owned();
                    style.insert(name, table_part(reader)?);
                    Ok(())
                })?;
                styles.insert(id, style);
            }
            Ev::Eof => return Ok(styles),
            _ => {}
        }
    }
}

fn table_part(reader: &mut XmlReader<'_>) -> Result<TablePart> {
    let mut part = TablePart::default();
    each_child(reader, |reader, child| match child.local() {
        b"tcTxStyle" => {
            part.bold = child.attr("b").map(|b| b == "on");
            part.italic = child.attr("i").map(|i| i == "on");
            each_child(reader, |reader, inner| {
                if is_color(&inner) {
                    part.color = read_color(reader, &inner)?;
                    Ok(())
                } else if inner.is("fontRef") && part.color.is_none() {
                    part.color = read_color_in(reader)?;
                    Ok(())
                } else {
                    reader.skip()
                }
            })
        }
        b"tcStyle" => each_child(reader, |reader, inner| match inner.local() {
            b"tcBdr" => each_child(reader, |reader, side| {
                let slot = match side.local() {
                    b"left" => 0,
                    b"top" => 1,
                    b"right" => 2,
                    b"bottom" => 3,
                    b"insideH" => 4,
                    b"insideV" => 5,
                    _ => return reader.skip(),
                };
                each_child(reader, |reader, line| {
                    if line.is("ln") {
                        part.borders[slot] = Some(read_line(reader, &line)?);
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })
            }),
            b"fill" => each_child(reader, |reader, fill| {
                if is_fill(&fill) {
                    part.fill = read_fill(reader, &fill)?;
                    Ok(())
                } else {
                    reader.skip()
                }
            }),
            _ => reader.skip(),
        }),
        _ => reader.skip(),
    })?;
    Ok(part)
}

/// PowerPoint's default table style, "Medium Style 2 – Accent 1". Files
/// that use it usually leave it out of `tableStyles.xml`, since every copy
/// of PowerPoint knows it.
const MEDIUM_STYLE_2_ACCENT_1: &str = r#"<a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<a:tblStyle styleId="{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}" styleName="Medium Style 2 - Accent 1">
 <a:wholeTbl><a:tcTxStyle><a:fontRef idx="minor"><a:prstClr val="black"/></a:fontRef><a:schemeClr val="dk1"/></a:tcTxStyle>
  <a:tcStyle><a:tcBdr>
   <a:left><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:left>
   <a:right><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:right>
   <a:top><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:top>
   <a:bottom><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:bottom>
   <a:insideH><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:insideH>
   <a:insideV><a:ln w="12700"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:insideV>
  </a:tcBdr><a:fill><a:solidFill><a:schemeClr val="accent1"><a:tint val="20000"/></a:schemeClr></a:solidFill></a:fill></a:tcStyle></a:wholeTbl>
 <a:band1H><a:tcStyle><a:tcBdr/><a:fill><a:solidFill><a:schemeClr val="accent1"><a:tint val="40000"/></a:schemeClr></a:solidFill></a:fill></a:tcStyle></a:band1H>
 <a:band2H><a:tcStyle><a:tcBdr/></a:tcStyle></a:band2H>
 <a:band1V><a:tcStyle><a:tcBdr/><a:fill><a:solidFill><a:schemeClr val="accent1"><a:tint val="40000"/></a:schemeClr></a:solidFill></a:fill></a:tcStyle></a:band1V>
 <a:band2V><a:tcStyle><a:tcBdr/></a:tcStyle></a:band2V>
 <a:lastCol><a:tcTxStyle b="on"><a:fontRef idx="minor"><a:prstClr val="black"/></a:fontRef><a:schemeClr val="lt1"/></a:tcTxStyle><a:tcStyle><a:tcBdr/><a:fill><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fill></a:tcStyle></a:lastCol>
 <a:firstCol><a:tcTxStyle b="on"><a:fontRef idx="minor"><a:prstClr val="black"/></a:fontRef><a:schemeClr val="lt1"/></a:tcTxStyle><a:tcStyle><a:tcBdr/><a:fill><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fill></a:tcStyle></a:firstCol>
 <a:lastRow><a:tcTxStyle b="on"><a:fontRef idx="minor"><a:prstClr val="black"/></a:fontRef><a:schemeClr val="lt1"/></a:tcTxStyle><a:tcStyle><a:tcBdr><a:top><a:ln w="38100"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:top></a:tcBdr><a:fill><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fill></a:tcStyle></a:lastRow>
 <a:firstRow><a:tcTxStyle b="on"><a:fontRef idx="minor"><a:prstClr val="black"/></a:fontRef><a:schemeClr val="lt1"/></a:tcTxStyle><a:tcStyle><a:tcBdr><a:bottom><a:ln w="38100"><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></a:bottom></a:tcBdr><a:fill><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></a:fill></a:tcStyle></a:firstRow>
</a:tblStyle></a:tblStyleLst>"#;

fn builtin_table_styles() -> HashMap<String, TableStyle> {
    read_table_styles(MEDIUM_STYLE_2_ACCENT_1.as_bytes(), "built-in table style")
        .expect("the built-in table style parses")
}

/// The text of a notes slide's notes placeholder, one paragraph per line.
fn notes_text(bytes: &[u8], part: &str) -> Result<Option<String>> {
    let notes = template(bytes, part)?;
    let text = notes
        .shapes
        .iter()
        .filter(|shape| {
            shape
                .placeholder
                .as_ref()
                .is_some_and(|ph| ph.kind == "body")
        })
        .filter_map(|shape| shape.body.as_ref())
        .flat_map(|body| &body.paragraphs)
        .map(|paragraph| {
            paragraph
                .runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim_end().to_owned();
    Ok((!text.trim().is_empty()).then_some(text))
}

/// What every shape of a slide is resolved against.
struct Context<'a> {
    master: Option<&'a Master>,
    defaults: &'a Levels,
    table_styles: &'a HashMap<String, TableStyle>,
    /// The East Asian language for text not marked with one.
    language: &'a str,
}

/// Where a slide placeholder's inherited properties come from.
struct Sources<'a> {
    layout: Option<&'a RawShape>,
    master: Option<&'a RawShape>,
    /// The master's text style for this kind of shape.
    style: Option<&'a Levels>,
}

fn find_on_layout<'a>(layout: &'a Template, ph: &Placeholder) -> Option<&'a RawShape> {
    let placeholders = || {
        layout
            .shapes
            .iter()
            .filter_map(|s| s.placeholder.as_ref().map(|p| (s, p)))
    };
    if let Some(idx) = ph.idx.filter(|&i| i != 0)
        && let Some((shape, _)) = placeholders().find(|(_, p)| p.idx == Some(idx))
    {
        return Some(shape);
    }
    placeholders()
        .find(|(_, p)| p.kind == ph.kind)
        .or_else(|| {
            placeholders().find(|(_, p)| p.family() == ph.family() && ph.family() != "body")
        })
        .map(|(shape, _)| shape)
}

fn find_on_master<'a>(master: &'a Master, ph: &Placeholder) -> Option<&'a RawShape> {
    master.template.shapes.iter().find(|s| {
        s.placeholder
            .as_ref()
            .is_some_and(|p| p.family() == ph.family())
    })
}

/// Colours of one slide: its master's theme through the colour map the
/// slide, its layout or its master sets.
struct Colors<'a> {
    theme: &'a Theme,
    map: ColorMap,
}

impl Colors<'_> {
    fn context(&self, placeholder: Option<[u8; 4]>) -> ColorContext<'_> {
        ColorContext {
            theme: self.theme,
            map: &self.map,
            placeholder,
        }
    }

    fn color(&self, spec: &ColorSpec) -> [u8; 4] {
        self.context(None).resolve(spec)
    }

    /// A fill as drawn; `None` for no fill.
    fn fill(
        &self,
        spec: &FillSpec,
        placeholder: Option<[u8; 4]>,
        image: Option<&Arc<[u8]>>,
    ) -> Option<Fill> {
        let context = self.context(placeholder);
        match spec {
            FillSpec::None | FillSpec::Group => None,
            FillSpec::Solid(color) => Some(Fill::Solid(context.resolve(color))),
            FillSpec::Gradient(stops, angle) => Some(Fill::Gradient {
                stops: stops
                    .iter()
                    .map(|(at, color)| (*at as f32, context.resolve(color)))
                    .collect(),
                angle: *angle,
            }),
            FillSpec::Pattern(fg, bg) => fg
                .as_ref()
                .or(bg.as_ref())
                .map(|c| Fill::Solid(context.resolve(c))),
            FillSpec::Picture(_) => image.map(|image| {
                Fill::Picture(Picture {
                    image: image.clone(),
                    crop: [0.0; 4],
                })
            }),
        }
    }

    /// A theme fill style by its reference.
    fn styled_fill(&self, reference: &StyleRef) -> Option<Fill> {
        let placeholder = reference.color.as_ref().map(|c| self.color(c));
        let spec = match reference.index {
            i @ 1..=999 => self.theme.fills.get(i as usize - 1),
            i if i > 1000 => self.theme.background_fills.get(i as usize - 1001),
            _ => None,
        };
        match spec {
            Some(spec) => self.fill(spec, placeholder, None),
            // A theme without fill styles: the reference colour itself.
            None if reference.index > 0 => placeholder.map(Fill::Solid),
            None => None,
        }
    }

    /// An outline from a shape's own line over its style's.
    fn line(&self, own: Option<&LineSpec>, reference: Option<&StyleRef>) -> Option<Outline> {
        let styled = reference.filter(|r| r.index > 0).map(|r| {
            (
                self.theme.lines.get(r.index as usize - 1),
                r.color.as_ref().map(|c| self.color(c)),
            )
        });
        let width = own
            .and_then(|l| l.width)
            .or_else(|| styled.and_then(|(line, _)| line?.width))
            .unwrap_or(DEFAULT_LINE);
        let placeholder = styled.and_then(|(_, color)| color);
        let fill = match own.and_then(|l| l.fill.as_ref()) {
            Some(fill) => Some(fill.clone()),
            None => match styled {
                Some((Some(line), _)) => line.fill.clone(),
                Some((None, color)) => color.map(|c| FillSpec::Solid(ColorSpec::rgb(c))),
                None => None,
            },
        }?;
        let color = self.fill(&fill, placeholder, None)?.main_color()?;
        Some(Outline {
            color,
            width: width.max(0),
        })
    }
}

fn colors<'a>(
    slide: &Template,
    layout: Option<&Template>,
    master: Option<&'a Master>,
    fallback: &'a Theme,
) -> Colors<'a> {
    let map = slide
        .color_map
        .clone()
        .or_else(|| layout.and_then(|l| l.color_map.clone()))
        .or_else(|| master.and_then(|m| m.template.color_map.clone()))
        .unwrap_or_default();
    Colors {
        theme: master.map_or(fallback, |m| &m.theme),
        map,
    }
}

fn resolve_slide(slide: &Template, layout: Option<&Template>, context: &Context<'_>) -> Slide {
    let fallback = Theme::default();
    let master = context.master;
    let colors = colors(slide, layout, master, &fallback);
    let mut shapes = Vec::new();
    let layout_shows_master = layout.is_none_or(|l| l.show_master_shapes);
    let other = master.map(|m| &m.other);
    if slide.show_master_shapes {
        if layout_shows_master && let Some(master) = master {
            for raw in master
                .template
                .shapes
                .iter()
                .filter(|s| s.placeholder.is_none())
            {
                let sources = Sources {
                    layout: None,
                    master: None,
                    style: other,
                };
                if let Some(mut shape) = resolve_shape(raw, &sources, context, &colors, None) {
                    shape.inherited = true;
                    shapes.push(shape);
                }
            }
        }
        if let Some(layout) = layout {
            for raw in layout.shapes.iter().filter(|s| s.placeholder.is_none()) {
                let sources = Sources {
                    layout: None,
                    master: None,
                    style: other,
                };
                if let Some(mut shape) = resolve_shape(raw, &sources, context, &colors, None) {
                    shape.inherited = true;
                    shapes.push(shape);
                }
            }
        }
    }
    for (index, raw) in slide.shapes.iter().enumerate() {
        let sources = match &raw.placeholder {
            Some(ph) => {
                let from_layout = layout.and_then(|l| find_on_layout(l, ph));
                let family_ph = from_layout
                    .and_then(|s| s.placeholder.as_ref())
                    .unwrap_or(ph);
                Sources {
                    layout: from_layout,
                    master: master.and_then(|m| find_on_master(m, family_ph)),
                    style: master.map(|m| match family_ph.family() {
                        "title" => &m.title,
                        "body" => &m.body,
                        _ => &m.other,
                    }),
                }
            }
            None => Sources {
                layout: None,
                master: None,
                style: other,
            },
        };
        if let Some(shape) = resolve_shape(raw, &sources, context, &colors, Some(index as u32)) {
            shapes.push(shape);
        }
    }
    // The slide's background, else its layout's, else its master's.
    let templates = [Some(slide), layout, master.map(|m| &m.template)];
    let background = templates.iter().flatten().find_map(|template| {
        let background = template.background.as_ref()?;
        Some(match background {
            Background::Fill(spec) => colors.fill(spec, None, template.background_image.as_ref()),
            Background::Ref(reference) => colors.styled_fill(reference),
        })
    });
    // What a new shape or text box on this slide looks like.
    let text = text_levels(
        &[],
        &TextDefaults {
            style: other,
            defaults: context.defaults,
            color: None,
            table: None,
        },
        &colors,
        context.language,
    );
    let accent = ColorSpec::scheme("accent1");
    let mut accent_line = accent.clone();
    accent_line
        .modifiers
        .push(crate::drawingml::Modifier::Shade(0.15));
    let look = SlideLook {
        accent: colors.color(&accent),
        accent_line: colors.color(&accent_line),
        on_accent: colors.color(&ColorSpec::scheme("lt1")),
        text: Arc::new(text),
    };
    Slide {
        shapes,
        hidden: slide.hidden,
        background: background.flatten(),
        notes: None,
        source: None,
        layout: None,
        look: Arc::new(look),
    }
}

/// `index`: the shape's place among the slide part's shapes, for a shape of
/// the slide itself; `None` for one of its layout or master.
fn resolve_shape(
    raw: &RawShape,
    sources: &Sources<'_>,
    context: &Context<'_>,
    colors: &Colors<'_>,
    index: Option<u32>,
) -> Option<Shape> {
    let chain = [Some(raw), sources.layout, sources.master];
    let frame = chain.iter().flatten().find_map(|s| s.frame)?;
    let geometry = chain
        .iter()
        .flatten()
        .find_map(|s| s.geometry.clone())
        .unwrap_or(Geometry::Rect);
    let role = match (&raw.placeholder, raw.role) {
        (Some(ph), _) => match ph.family() {
            "title" => Role::Title,
            "body" => Role::Body,
            _ => Role::Footer,
        },
        (None, role) => role,
    };
    // Fill and outline: the shape's own, else the theme style it refers
    // to, looking through the placeholders it inherits from.
    let fill = chain.iter().flatten().find_map(|s| {
        if let Some(spec) = &s.fill {
            return Some(colors.fill(spec, None, s.fill_image.as_ref()));
        }
        s.fill_ref.as_ref().map(|r| colors.styled_fill(r))
    });
    let fill = fill.flatten();
    let line_spec = chain.iter().flatten().find_map(|s| s.line.as_ref());
    let line_ref = chain.iter().flatten().find_map(|s| s.line_ref.as_ref());
    let line = match role {
        Role::Shape | Role::Title | Role::Body | Role::Footer | Role::Picture => {
            colors.line(line_spec, line_ref)
        }
        _ => None,
    };
    let font_color = chain
        .iter()
        .flatten()
        .find_map(|s| s.font_color.as_ref())
        .map(|c| colors.color(c));
    let text = raw.body.as_ref().and_then(|body| {
        let bodies: Vec<&RawBody> = chain
            .iter()
            .skip(1)
            .flatten()
            .filter_map(|s| s.body.as_ref())
            .collect();
        resolve_body(
            body,
            &bodies,
            &TextDefaults {
                style: sources.style,
                defaults: context.defaults,
                color: font_color,
                table: None,
            },
            colors,
            context.language,
            index.is_some(),
        )
    });
    let picture = match (&raw.blip, &raw.image) {
        (Some((_, crop)), Some(image)) => Some(Picture {
            image: image.clone(),
            crop: *crop,
        }),
        _ => None,
    };
    let table = raw
        .table
        .as_ref()
        .map(|table| resolve_table(table, context, colors));
    let source = index.map(|index| ShapeSource {
        index,
        copy: false,
        frame,
        group: raw.group.map(|t| [t.scale_x, t.scale_y, t.dx, t.dy]),
        paragraphs: text
            .as_ref()
            .map(|body| {
                body.paragraphs
                    .iter()
                    .filter_map(|p| p.source.clone())
                    .collect()
            })
            .unwrap_or_default(),
    });
    Some(Shape {
        frame,
        rotation: raw.rotation,
        flip_h: raw.flip_h,
        flip_v: raw.flip_v,
        geometry,
        role,
        fill,
        line,
        text,
        picture,
        table,
        inherited: false,
        placeholder: raw.placeholder.as_ref().map(|ph| SlidePlaceholder {
            kind: ph.kind.clone(),
            idx: ph.idx,
        }),
        source,
    })
}

/// What a text body falls back on.
struct TextDefaults<'a> {
    /// The master's text style for this kind of shape.
    style: Option<&'a Levels>,
    defaults: &'a Levels,
    /// The shape style's text colour.
    color: Option<[u8; 4]>,
    /// A table style's text.
    table: Option<TableText>,
}

/// Text formatting a table style gives its cells: bold, italic, colour.
type TableText = (Option<bool>, Option<bool>, Option<[u8; 4]>);

/// What each outline level of a body inherits: everything but a
/// paragraph's own properties, most specific first (the body's list style,
/// the placeholders it inherits from, a table style's text, the shape
/// style's text colour, the master's and the presentation's text styles).
fn text_levels(
    inherited: &[&Levels],
    fallback: &TextDefaults<'_>,
    colors: &Colors<'_>,
    language: &str,
) -> Looks {
    let (table_bold, table_italic, table_color) = fallback.table.unwrap_or_default();
    std::array::from_fn(|lvl| {
        let own: Vec<&Level> = inherited.iter().map(|list| &list[lvl]).collect();
        let mut levels = own.clone();
        if let Some(style) = fallback.style {
            levels.push(&style[lvl]);
        }
        levels.push(&fallback.defaults[lvl]);
        let face = |name: Option<String>, fallback_name: &str, language: &str| {
            name.and_then(|name| colors.theme.typeface(&name, language))
                .or_else(|| colors.theme.typeface(fallback_name, language))
        };
        LevelLook {
            size: levels
                .iter()
                .find_map(|l| l.size)
                .unwrap_or(DEFAULT_TEXT_POINTS),
            bold: own
                .iter()
                .find_map(|l| l.bold)
                .or(table_bold)
                .or_else(|| levels.iter().find_map(|l| l.bold))
                .unwrap_or(false),
            italic: own
                .iter()
                .find_map(|l| l.italic)
                .or(table_italic)
                .or_else(|| levels.iter().find_map(|l| l.italic))
                .unwrap_or(false),
            // A shape style's or table style's text colour wins over the
            // master and presentation text styles, not over the shape's own.
            color: own
                .iter()
                .find_map(|l| l.color.as_ref())
                .map(|c| colors.color(c))
                .or(table_color)
                .or(fallback.color)
                .or_else(|| {
                    levels
                        .iter()
                        .find_map(|l| l.color.as_ref())
                        .map(|c| colors.color(c))
                }),
            font: face(levels.iter().find_map(|l| l.latin.clone()), "+mn-lt", "en"),
            east_asian_font: face(
                levels.iter().find_map(|l| l.east_asian.clone()),
                "+mn-ea",
                language,
            ),
            align: levels.iter().find_map(|l| l.align).unwrap_or_default(),
            list: levels
                .iter()
                .find_map(|l| l.bullet.clone())
                .flatten()
                .map(|b| b.list()),
            margin: levels.iter().find_map(|l| l.margin).unwrap_or(0).max(0),
            indent: levels.iter().find_map(|l| l.indent).unwrap_or(0),
        }
    })
}

/// `keep_empty`: an empty body stays (a placeholder of the slide, to type
/// into); otherwise it is dropped, as PowerPoint shows an empty
/// placeholder's prompt only while editing.
#[allow(clippy::too_many_arguments)]
fn resolve_body(
    body: &RawBody,
    inherited: &[&RawBody],
    fallback: &TextDefaults<'_>,
    colors: &Colors<'_>,
    language: &str,
    keep_empty: bool,
) -> Option<TextBody> {
    if !keep_empty
        && body
            .paragraphs
            .iter()
            .all(|p| p.runs.iter().all(|r| r.text.is_empty()))
    {
        return None;
    }
    let props: Vec<&BodyProps> = std::iter::once(&body.props)
        .chain(inherited.iter().map(|b| &b.props))
        .collect();
    let mut out = TextBody::default();
    for (i, inset) in out.insets.iter_mut().enumerate() {
        if let Some(value) = props.iter().find_map(|p| p.insets[i]) {
            *inset = value.max(0);
        }
    }
    if let Some(anchor) = props.iter().find_map(|p| p.anchor) {
        out.anchor = anchor;
    }
    if let Some(wrap) = props.iter().find_map(|p| p.wrap) {
        out.wrap = wrap;
    }
    out.grow = props.iter().find_map(|p| p.grow).unwrap_or(false);
    out.font_scale = body.props.font_scale.unwrap_or(1.0);
    let lists: Vec<&Levels> = std::iter::once(&body.list)
        .chain(inherited.iter().map(|b| &b.list))
        .collect();
    let levels = text_levels(&lists, fallback, colors, language);

    for raw in &body.paragraphs {
        let lvl = usize::from(raw.level);
        let look = &levels[lvl];
        let own = &raw.props;
        let size = own.size.unwrap_or(look.size);
        let bold = own.bold.unwrap_or(look.bold);
        let italic = own.italic.unwrap_or(look.italic);
        let color = own.color.as_ref().map(|c| colors.color(c)).or(look.color);
        // Font names through the whole chain, picked per run's language.
        let mut chain: Vec<&Level> = vec![own];
        chain.extend(lists.iter().map(|list| &list[lvl]));
        if let Some(style) = fallback.style {
            chain.push(&style[lvl]);
        }
        chain.push(&fallback.defaults[lvl]);
        let latin = chain.iter().find_map(|l| l.latin.clone());
        let east_asian = chain.iter().find_map(|l| l.east_asian.clone());
        let make_run = |run: &RawRun| {
            let language = run.language.as_deref().unwrap_or("en");
            let face =
                |name: Option<&String>| name.and_then(|name| colors.theme.typeface(name, language));
            Run {
                text: run.text.clone(),
                size: run.size.unwrap_or(size),
                bold: run.bold.unwrap_or(bold),
                italic: run.italic.unwrap_or(italic),
                underline: run.underline,
                strike: run.strike,
                color: run.color.as_ref().map(|c| colors.color(c)).or(color),
                font: face(run.latin.as_ref().or(latin.as_ref()))
                    .or_else(|| colors.theme.typeface("+mn-lt", language)),
                east_asian_font: face(run.east_asian.as_ref().or(east_asian.as_ref()))
                    .or_else(|| colors.theme.typeface("+mn-ea", language)),
                source: None,
            }
        };
        let runs: Vec<(Run, &RawRun)> = raw
            .runs
            .iter()
            .filter(|run| !run.text.is_empty())
            .map(|run| (make_run(run), run))
            .collect();
        let end_raw = raw.end.clone().unwrap_or_else(|| RawRun {
            language: None,
            ..RawRun::default()
        });
        let mut end = make_run(&end_raw);
        end.text.clear();
        let end_size = end_raw.size.unwrap_or(size);
        let mut paragraph = Paragraph {
            runs: runs.iter().map(|(run, _)| run.clone()).collect(),
            align: own.align.unwrap_or(look.align),
            level: raw.level,
            bullet: None,
            margin: own.margin.map_or(look.margin, |m| m.max(0)),
            indent: own.indent.unwrap_or(look.indent),
            end_size,
            list: match &own.bullet {
                Some(bullet) => bullet.as_ref().map(Bullet::list),
                None => look.list.clone(),
            },
            end: end.clone(),
            source: None,
        };
        let original = paragraph.clone();
        for (run, (_, raw_run)) in paragraph.runs.iter_mut().zip(&runs) {
            run.source = Some(Arc::new(RunSource {
                xml: raw_run.xml.clone(),
                props: raw_run.props_xml.clone(),
                original: run.clone(),
                own_size: raw_run.size.is_some(),
                own_bold: raw_run.bold.is_some(),
                own_italic: raw_run.italic.is_some(),
                own_color: raw_run.color.is_some(),
            }));
        }
        paragraph.end.source = Some(aster_slides::edit::end_source(&raw.end_xml, &end));
        paragraph.source = Some(Arc::new(ParaSource {
            xml: raw.xml.clone(),
            props: raw.props_xml.clone(),
            end: raw.end_xml.clone(),
            original,
            own_align: own.align.is_some(),
            own_list: own.bullet.is_some(),
            own_margin: own.margin.is_some(),
            own_indent: own.indent.is_some(),
        }));
        out.paragraphs.push(paragraph);
    }
    out.levels = Some(Arc::new(levels));
    out.renumber();
    Some(out)
}

/// Apply a table's style and its cells' own formatting.
fn resolve_table(raw: &RawTable, context: &Context<'_>, colors: &Colors<'_>) -> Table {
    let style = raw
        .style_id
        .as_ref()
        .and_then(|id| context.table_styles.get(id));
    let rows = raw.rows.len();
    let cols = raw.columns.len();
    let mut out = Table {
        columns: raw.columns.clone(),
        rows: Vec::with_capacity(rows),
    };
    let master_other = context.master.map(|m| &m.other);
    for (r, (height, cells)) in raw.rows.iter().enumerate() {
        let mut row = TableRow {
            height: *height,
            cells: Vec::with_capacity(cols),
        };
        for c in 0..cols {
            let Some(cell) = cells.get(c) else {
                row.cells.push(TableCell::default());
                continue;
            };
            // The style's parts that apply to this cell, later ones winning,
            // with the shape of the area each covers: the table, a row or
            // a column.
            let mut parts: Vec<(&TablePart, Region)> = Vec::new();
            if let Some(style) = style {
                let mut add = |name: &str, region: Region| {
                    if let Some(part) = style.get(name) {
                        parts.push((part, region));
                    }
                };
                add("wholeTbl", Region::Table);
                let body_row = r >= usize::from(raw.first_row) && !(raw.last_row && r + 1 == rows);
                if raw.band_row && body_row {
                    let band = r - usize::from(raw.first_row);
                    add(if band % 2 == 0 { "band1H" } else { "band2H" }, Region::Row);
                }
                let body_col = c >= usize::from(raw.first_col) && !(raw.last_col && c + 1 == cols);
                if raw.band_col && body_col {
                    let band = c - usize::from(raw.first_col);
                    add(
                        if band % 2 == 0 { "band1V" } else { "band2V" },
                        Region::Column,
                    );
                }
                if raw.first_col && c == 0 {
                    add("firstCol", Region::Column);
                }
                if raw.last_col && c + 1 == cols {
                    add("lastCol", Region::Column);
                }
                if raw.last_row && r + 1 == rows {
                    add("lastRow", Region::Row);
                }
                if raw.first_row && r == 0 {
                    add("firstRow", Region::Row);
                }
            }
            let fill = match &cell.fill {
                Some(spec) => colors.fill(spec, None, None),
                None => parts
                    .iter()
                    .rev()
                    .find_map(|(p, _)| p.fill.as_ref())
                    .and_then(|spec| colors.fill(spec, None, None)),
            };
            // Edges: the cell's own, else the style's: a part's outer line
            // where the cell's edge is on the edge of the part's area, its
            // inside line elsewhere.
            let edges = [
                c == 0,
                r == 0,
                c + cell.span.0 as usize >= cols,
                r + cell.span.1 as usize >= rows,
            ];
            let styled: [Option<&LineSpec>; 4] = std::array::from_fn(|side| {
                parts.iter().rev().find_map(|(part, region)| {
                    let horizontal = side % 2 == 1;
                    let outer = match region {
                        Region::Table => edges[side],
                        Region::Row => horizontal || edges[side],
                        Region::Column => !horizontal || edges[side],
                    };
                    let slot = match (outer, horizontal) {
                        (true, _) => side,
                        (false, true) => 4,
                        (false, false) => 5,
                    };
                    part.borders[slot].as_ref()
                })
            });
            let mut borders = [None; 4];
            for side in 0..4 {
                let spec = cell.borders[side].as_ref().or(styled[side]);
                borders[side] = spec.and_then(|spec| colors.line(Some(spec), None));
            }
            let text_style = (
                parts.iter().rev().find_map(|(p, _)| p.bold),
                parts.iter().rev().find_map(|(p, _)| p.italic),
                parts
                    .iter()
                    .rev()
                    .find_map(|(p, _)| p.color.as_ref())
                    .map(|c| colors.color(c)),
            );
            let text = cell.body.as_ref().and_then(|body| {
                let mut body = body.clone();
                // Table cells' own default margins: 0.1 inch left and right,
                // 0.05 top and bottom.
                for (i, margin) in cell.margins.iter().enumerate() {
                    if margin.is_some() {
                        body.props.insets[i] = *margin;
                    }
                }
                if body.props.anchor.is_none() {
                    body.props.anchor = cell.anchor;
                }
                resolve_body(
                    &body,
                    &[],
                    &TextDefaults {
                        style: master_other,
                        defaults: context.defaults,
                        color: None,
                        table: Some(text_style),
                    },
                    colors,
                    context.language,
                    false,
                )
            });
            row.cells.push(TableCell {
                text,
                fill,
                borders,
                span: cell.span,
                merged: cell.merged,
            });
        }
        out.rows.push(row);
    }
    out
}
