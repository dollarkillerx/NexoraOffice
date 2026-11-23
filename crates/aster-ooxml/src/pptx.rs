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
//! the slide or layout turns that off. Themes, colours, fills, pictures and
//! animations are not read.

use std::collections::HashMap;

use aster_slides::{
    Align, Anchor, DEFAULT_TEXT_POINTS, Frame, Geometry, Paragraph, Presentation, Role, Run, Shape,
    Slide, TextBody,
};

use crate::error::Result;
use crate::package::{Kind, Package};
use crate::xml::{Ev, Tag, XmlReader, each_child};

pub fn read(bytes: Vec<u8>) -> Result<Presentation> {
    let mut package = Package::open(bytes)?;
    read_package(&mut package)
}

pub fn read_package(package: &mut Package) -> Result<Presentation> {
    let main = package.expect(Kind::Pptx)?;
    let rels: HashMap<String, Option<String>> = package
        .relationships(&main)?
        .into_iter()
        .map(|rel| (rel.id, rel.target))
        .collect();
    let bytes = package.read(&main)?;
    let head = presentation(&bytes, &main)?;

    let mut layouts: HashMap<String, Option<Template>> = HashMap::new();
    let mut masters: HashMap<String, Option<Master>> = HashMap::new();
    let mut slides = Vec::with_capacity(head.slide_ids.len());
    for id in &head.slide_ids {
        let Some(Some(part)) = rels.get(id) else {
            continue;
        };
        if !package.has(part) {
            continue;
        }
        let bytes = package.read(part)?;
        let slide = template(&bytes, part)?;
        let layout_part = linked(package, part, "slideLayout")?;
        if let Some(layout_part) = &layout_part
            && !layouts.contains_key(layout_part)
        {
            let parsed = match package.has(layout_part) {
                true => Some(template(&package.read(layout_part)?, layout_part)?),
                false => None,
            };
            layouts.insert(layout_part.clone(), parsed);
        }
        let layout = layout_part.as_ref().and_then(|p| layouts[p].as_ref());
        let master_part = match &layout_part {
            Some(layout_part) => linked(package, layout_part, "slideMaster")?,
            None => None,
        };
        if let Some(master_part) = &master_part
            && !masters.contains_key(master_part)
        {
            let parsed = match package.has(master_part) {
                true => Some(master(&package.read(master_part)?, master_part)?),
                false => None,
            };
            masters.insert(master_part.clone(), parsed);
        }
        let master = master_part.as_ref().and_then(|p| masters[p].as_ref());
        slides.push(resolve_slide(&slide, layout, master, &head.default_style));
    }
    Ok(Presentation {
        size: head.size,
        slides,
    })
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
    default_style: Levels,
}

fn presentation(bytes: &[u8], part: &str) -> Result<Head> {
    let mut reader = XmlReader::new(bytes, part);
    let mut head = Head {
        size: aster_slides::DEFAULT_SIZE,
        slide_ids: Vec::new(),
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
    align: Option<Align>,
    /// `Some(None)` is an explicit "no bullet".
    bullet: Option<Option<Bullet>>,
    margin: Option<i64>,
    indent: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Bullet {
    Char(String),
    Number,
}

type Levels = [Level; 9];

#[derive(Clone, Debug, Default, PartialEq)]
struct BodyProps {
    insets: [Option<i64>; 4],
    anchor: Option<Anchor>,
    wrap: Option<bool>,
    font_scale: Option<f32>,
}

#[derive(Clone, Debug, Default)]
struct RawRun {
    text: String,
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: bool,
}

#[derive(Clone, Debug, Default)]
struct RawParagraph {
    level: u8,
    props: Level,
    runs: Vec<RawRun>,
    end_size: Option<f32>,
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

#[derive(Clone, Debug)]
struct RawShape {
    frame: Option<Frame>,
    rotation: f64,
    flip_h: bool,
    flip_v: bool,
    geometry: Option<Geometry>,
    role: Role,
    placeholder: Option<Placeholder>,
    text_box: bool,
    /// `Some(false)` when the line is explicitly not drawn.
    line: Option<bool>,
    /// The shape style refers to a theme line.
    styled_line: bool,
    body: Option<RawBody>,
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
            text_box: false,
            line: None,
            styled_line: false,
            body: None,
        }
    }
}

/// A slide, layout or master: its shapes and whether the shapes of the
/// templates behind it show through.
#[derive(Debug, Default)]
struct Template {
    shapes: Vec<RawShape>,
    show_master_shapes: bool,
    hidden: bool,
}

#[derive(Debug, Default)]
struct Master {
    template: Template,
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
            Ev::Open(tag) if tag.is("spTree") => {
                shape_tree(&mut reader, Transform::IDENTITY, &mut out.shapes)?;
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
fn shape_tree(
    reader: &mut XmlReader<'_>,
    transform: Transform,
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
            b"grpSp" => shape_tree(reader, transform.then(group), out)?,
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
                        shape_tree(reader, inner, out)
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
                    }
                    reader.skip()
                })?;
            }
            b"style" => {
                each_child(reader, |reader, child| {
                    if child.is("lnRef") {
                        shape.styled_line = child.attr_i64("idx").unwrap_or(0) > 0;
                    }
                    reader.skip()
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
            b"cNvSpPr" => {
                shape.text_box = tag.attr_bool("txBox").unwrap_or(false);
                reader.skip()?;
            }
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
            b"ln" => {
                let mut visible = None;
                each_child(reader, |reader, child| {
                    match child.local() {
                        b"noFill" => visible = Some(false),
                        b"solidFill" | b"gradFill" | b"pattFill" => visible = Some(true),
                        _ => {}
                    }
                    reader.skip()
                })?;
                shape.line = visible.or(shape.line);
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
                let run = run_props(&child);
                level.size = run.size;
                level.bold = run.bold;
                level.italic = run.italic;
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
            b"buAutoNum" => level.bullet = Some(Some(Bullet::Number)),
            _ => {}
        }
        reader.skip()
    })?;
    Ok(level)
}

fn run_props(tag: &Tag<'_>) -> RawRun {
    RawRun {
        text: String::new(),
        size: tag
            .attr_f64("sz")
            .map(|sz| (sz / 100.0) as f32)
            .filter(|&sz| sz > 0.0 && sz < 4_000.0),
        bold: tag.attr_bool("b"),
        italic: tag.attr_bool("i"),
        underline: tag.attr("u").is_some_and(|u| u != "none"),
    }
}

fn paragraph(reader: &mut XmlReader<'_>) -> Result<RawParagraph> {
    let mut out = RawParagraph::default();
    each_child(reader, |reader, tag| {
        match tag.local() {
            b"pPr" => {
                out.level = tag.attr_i64("lvl").unwrap_or(0).clamp(0, 8) as u8;
                out.props = level_props(reader, &tag)?;
            }
            b"r" | b"fld" | b"br" => {
                let is_break = tag.is("br");
                let mut run = RawRun::default();
                each_child(reader, |reader, child| {
                    if child.is("rPr") {
                        run = RawRun {
                            text: std::mem::take(&mut run.text),
                            ..run_props(&child)
                        };
                        reader.skip()
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
                out.runs.push(run);
            }
            b"endParaRPr" => {
                out.end_size = run_props(&tag).size;
                reader.skip()?;
            }
            _ => reader.skip()?,
        }
        Ok(())
    })?;
    Ok(out)
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

fn resolve_slide(
    slide: &Template,
    layout: Option<&Template>,
    master: Option<&Master>,
    defaults: &Levels,
) -> Slide {
    let mut shapes = Vec::new();
    let layout_shows_master = layout.is_none_or(|l| l.show_master_shapes);
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
                    style: Some(&master.other),
                };
                if let Some(mut shape) = resolve_shape(raw, &sources, defaults) {
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
                    style: master.map(|m| &m.other),
                };
                if let Some(mut shape) = resolve_shape(raw, &sources, defaults) {
                    shape.inherited = true;
                    shapes.push(shape);
                }
            }
        }
    }
    for raw in &slide.shapes {
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
                style: master.map(|m| &m.other),
            },
        };
        if let Some(shape) = resolve_shape(raw, &sources, defaults) {
            shapes.push(shape);
        }
    }
    Slide {
        shapes,
        hidden: slide.hidden,
    }
}

fn resolve_shape(raw: &RawShape, sources: &Sources<'_>, defaults: &Levels) -> Option<Shape> {
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
    let outlined = match raw.line {
        Some(visible) => visible,
        None => raw.placeholder.is_none() && !raw.text_box && raw.styled_line,
    } && matches!(role, Role::Shape | Role::Title | Role::Body | Role::Footer);
    let text = raw.body.as_ref().and_then(|body| {
        let bodies: Vec<&RawBody> = chain
            .iter()
            .skip(1)
            .flatten()
            .filter_map(|s| s.body.as_ref())
            .collect();
        resolve_body(body, &bodies, sources.style, defaults)
    });
    Some(Shape {
        frame,
        rotation: raw.rotation,
        flip_h: raw.flip_h,
        flip_v: raw.flip_v,
        geometry,
        role,
        outlined,
        text,
        inherited: false,
    })
}

fn resolve_body(
    body: &RawBody,
    inherited: &[&RawBody],
    style: Option<&Levels>,
    defaults: &Levels,
) -> Option<TextBody> {
    // An empty placeholder: PowerPoint shows its prompt only while editing.
    if body
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
    out.font_scale = body.props.font_scale.unwrap_or(1.0);

    let mut numbers = [0u32; 9];
    for raw in &body.paragraphs {
        let lvl = usize::from(raw.level);
        // Most specific first.
        let mut levels: Vec<&Level> = vec![&raw.props, &body.list[lvl]];
        levels.extend(inherited.iter().map(|b| &b.list[lvl]));
        if let Some(style) = style {
            levels.push(&style[lvl]);
        }
        levels.push(&defaults[lvl]);
        let size = levels
            .iter()
            .find_map(|l| l.size)
            .unwrap_or(DEFAULT_TEXT_POINTS);
        let bold = levels.iter().find_map(|l| l.bold).unwrap_or(false);
        let italic = levels.iter().find_map(|l| l.italic).unwrap_or(false);
        let align = levels.iter().find_map(|l| l.align).unwrap_or_default();
        let bullet = levels.iter().find_map(|l| l.bullet.clone()).flatten();
        let has_text = raw.runs.iter().any(|r| !r.text.trim().is_empty());
        let bullet = match bullet {
            _ if !has_text => None,
            Some(Bullet::Char(c)) => {
                numbers[lvl] = 0;
                Some(c)
            }
            Some(Bullet::Number) => {
                numbers[lvl] += 1;
                Some(format!("{}.", numbers[lvl]))
            }
            None => {
                numbers[lvl] = 0;
                None
            }
        };
        // Deeper levels restart their numbering under a new parent.
        for deeper in numbers.iter_mut().skip(lvl + 1) {
            *deeper = 0;
        }
        out.paragraphs.push(Paragraph {
            runs: raw
                .runs
                .iter()
                .filter(|run| !run.text.is_empty())
                .map(|run| Run {
                    text: run.text.clone(),
                    size: run.size.unwrap_or(size),
                    bold: run.bold.unwrap_or(bold),
                    italic: run.italic.unwrap_or(italic),
                    underline: run.underline,
                })
                .collect(),
            align,
            level: raw.level,
            bullet,
            margin: levels.iter().find_map(|l| l.margin).unwrap_or(0).max(0),
            indent: levels.iter().find_map(|l| l.indent).unwrap_or(0),
            end_size: raw.end_size.unwrap_or(size),
        });
    }
    Some(out)
}
