//! Saving an edited presentation back into its package (ADR 0001).
//!
//! The file as first read is always the starting point, and the model is
//! compared with what was read from it ([`aster_slides::SlideSource`] and
//! the other sources):
//!
//! - parts not touched are copied as they were, compressed bytes and all;
//!   relationship parts and `[Content_Types].xml` included;
//! - in a changed slide only the changed shapes are written again: a
//!   moved or resized shape gets new `off` and `ext` in its transform, a
//!   shape whose text changed gets new paragraphs; inside them a paragraph
//!   or run that did not change is written as it was, and one that did
//!   keeps its properties element with only the changed attributes and
//!   colour replaced. Shapes not changed keep their bytes, wherever they
//!   move in the drawing order;
//! - new shapes, slides, notes and pictures become new elements and parts
//!   with their relationships and content types; a copied slide is a copy
//!   of its part with its own notes;
//! - a deleted slide's part, relationships and notes go, with their
//!   entries in `presentation.xml`, its relationships and the content
//!   types.
//!
//! Since the file as read stays the base, saving again after an undo
//! writes what is on screen, and so does a recovery record.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use aster_slides::{
    Align, Frame, Geometry, LevelLook, Levels, List, Paragraph, Presentation, Role, Run, Shape,
    ShapeSource, Slide, TextBody,
};

use crate::error::{Error, Result};
use crate::package::{Kind, Package, rels_name, resolve};
use crate::xml_edit::{
    Span, append_to_root, attribute, children, edit_children, escape, escape_attribute, find_all,
    free_rel_number, inner, is_bare, prefix_of, remove_attribute, rename, set_attribute, text_of,
};

pub mod blank;

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const CT_PML: &str = "application/vnd.openxmlformats-officedocument.presentationml";
const TYPES: &str = "[Content_Types].xml";

/// A part as this save has it so far.
struct Parts {
    package: Package,
    replaced: HashMap<String, Vec<u8>>,
    removed: HashSet<String>,
    added: Vec<(String, Vec<u8>)>,
}

impl Parts {
    fn key(name: &str) -> String {
        name.trim_start_matches('/').to_ascii_lowercase()
    }

    fn has(&self, name: &str) -> bool {
        let key = Self::key(name);
        !self.removed.contains(&key)
            && (self.package.has(name) || self.added.iter().any(|(n, _)| Self::key(n) == key))
    }

    fn get(&mut self, name: &str) -> Result<Vec<u8>> {
        let key = Self::key(name);
        if let Some(bytes) = self.replaced.get(&key) {
            return Ok(bytes.clone());
        }
        if let Some((_, bytes)) = self.added.iter().find(|(n, _)| Self::key(n) == key) {
            return Ok(bytes.clone());
        }
        self.package.read(name)
    }

    fn put(&mut self, name: &str, bytes: Vec<u8>) {
        let key = Self::key(name);
        if let Some(slot) = self.added.iter_mut().find(|(n, _)| Self::key(n) == key) {
            slot.1 = bytes;
        } else if self.package.has(name) {
            self.replaced.insert(key, bytes);
        } else {
            self.added.push((name.to_owned(), bytes));
        }
    }

    fn remove(&mut self, name: &str) {
        let key = Self::key(name);
        self.replaced.remove(&key);
        self.added.retain(|(n, _)| Self::key(n) != key);
        if self.package.has(name) {
            self.removed.insert(key);
        }
    }

    /// A free part name `{stem}{n}.{extension}`.
    fn free(&self, stem: &str, extension: &str) -> String {
        (1..)
            .map(|n| format!("{stem}{n}.{extension}"))
            .find(|name| !self.has(name) && !self.removed.contains(&Self::key(name)))
            .expect("some number is free")
    }
}

/// Relationship types in this package's spelling (Transitional or Strict).
struct Kinds {
    base: String,
}

impl Kinds {
    fn read(rels: &str) -> Self {
        let base = rels
            .match_indices("Type=\"")
            .find_map(|(at, m)| {
                let rest = &rels[at + m.len()..];
                let value = &rest[..rest.find('"')?];
                let (base, _) = value.rsplit_once('/')?;
                base.contains("officeDocument").then(|| base.to_owned())
            })
            .unwrap_or_else(|| REL.to_owned());
        Self { base }
    }

    fn of(&self, kind: &str) -> String {
        format!("{}/{kind}", self.base)
    }
}

/// The path from one part's folder to another part.
fn relative(from: &str, to: &str) -> String {
    let from_dir: Vec<&str> = from
        .rsplit_once('/')
        .map_or(Vec::new(), |(d, _)| d.split('/').collect());
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<String> = vec!["..".to_owned(); from_dir.len() - common];
    out.extend(to_parts[common..].iter().map(|s| (*s).to_owned()));
    out.join("/")
}

/// Relationships of one part, with new ones added as needed.
struct Rels {
    part: String,
    text: String,
    changed: bool,
}

impl Rels {
    fn load(parts: &mut Parts, source: &str) -> Result<Self> {
        let part = rels_name(source);
        let text = if parts.has(&part) {
            text_of(&parts.get(&part)?).1
        } else {
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_owned()
        };
        Ok(Self {
            part,
            text,
            changed: false,
        })
    }

    /// A new relationship; returns its id.
    fn add(&mut self, kind: &str, target: &str) -> String {
        let id = format!("rId{}", free_rel_number(&self.text));
        let item = format!(
            "<Relationship Id=\"{id}\" Type=\"{}\" Target=\"{}\"/>",
            escape_attribute(kind),
            escape_attribute(target)
        );
        self.text =
            String::from_utf8_lossy(&append_to_root(self.text.as_bytes(), &item)).into_owned();
        self.changed = true;
        id
    }

    /// Take out the relationships whose type ends in `/{kind}`.
    fn remove_kind(&mut self, kind: &str) {
        let suffix = format!("/{kind}\"");
        for span in find_all(&self.text, "Relationship").into_iter().rev() {
            if self.text[span.start..span.end].contains(&suffix) {
                self.text.replace_range(span.start..span.end, "");
                self.changed = true;
            }
        }
    }

    fn remove_id(&mut self, id: &str) {
        for span in find_all(&self.text, "Relationship").into_iter().rev() {
            if attribute(&self.text[span.start..span.end], "Id").as_deref() == Some(id) {
                self.text.replace_range(span.start..span.end, "");
                self.changed = true;
            }
        }
    }

    fn store(self, parts: &mut Parts) {
        if self.changed {
            parts.put(&self.part, self.text.into_bytes());
        }
    }
}

/// `[Content_Types].xml`, edited.
struct Types {
    text: String,
    bom: Vec<u8>,
    changed: bool,
}

impl Types {
    fn add_override(&mut self, part: &str, content_type: &str) {
        let name = format!("/{part}");
        if find_all(&self.text, "Override").iter().any(|span| {
            attribute(&self.text[span.start..span.end], "PartName")
                .is_some_and(|n| n.eq_ignore_ascii_case(&name))
        }) {
            return;
        }
        let item = format!("<Override PartName=\"{name}\" ContentType=\"{content_type}\"/>");
        self.text =
            String::from_utf8_lossy(&append_to_root(self.text.as_bytes(), &item)).into_owned();
        self.changed = true;
    }

    fn remove_override(&mut self, part: &str) {
        let name = format!("/{part}");
        for span in find_all(&self.text, "Override").into_iter().rev() {
            if attribute(&self.text[span.start..span.end], "PartName")
                .is_some_and(|n| n.eq_ignore_ascii_case(&name))
            {
                self.text.replace_range(span.start..span.end, "");
                self.changed = true;
            }
        }
    }

    fn add_default(&mut self, extension: &str, content_type: &str) {
        let has = find_all(&self.text, "Default").iter().any(|span| {
            attribute(&self.text[span.start..span.end], "Extension")
                .is_some_and(|e| e.eq_ignore_ascii_case(extension))
        });
        if !has {
            let item =
                format!("<Default Extension=\"{extension}\" ContentType=\"{content_type}\"/>");
            // Defaults come first in the list.
            let at = find_all(&self.text, "Default")
                .last()
                .map(|s| s.end)
                .or_else(|| find_all(&self.text, "Override").first().map(|s| s.start))
                .unwrap_or_else(|| self.text.rfind("</").unwrap_or(self.text.len()));
            self.text.insert_str(at, &item);
            self.changed = true;
        }
    }
}

/// Pictures inserted here, written once each.
#[derive(Default)]
struct Media {
    written: Vec<(Arc<[u8]>, String)>,
}

impl Media {
    fn part_for(&mut self, parts: &mut Parts, types: &mut Types, image: &Arc<[u8]>) -> String {
        if let Some((_, name)) = self
            .written
            .iter()
            .find(|(bytes, _)| Arc::ptr_eq(bytes, image) || **bytes == **image)
        {
            return name.clone();
        }
        let (extension, content_type) = image_type(image);
        let name = parts.free("ppt/media/image", extension);
        parts.put(&name, image.to_vec());
        types.add_default(extension, content_type);
        self.written.push((image.clone(), name.clone()));
        name
    }
}

fn image_type(bytes: &[u8]) -> (&'static str, &'static str) {
    if bytes.starts_with(b"\x89PNG") {
        ("png", "image/png")
    } else if bytes.starts_with(b"\xFF\xD8") {
        ("jpeg", "image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        ("gif", "image/gif")
    } else {
        ("bin", "application/octet-stream")
    }
}

/// Save `presentation` over `source`, the file it was read from.
pub fn save(source: Vec<u8>, presentation: &Presentation) -> Result<Vec<u8>> {
    let mut package = Package::open(source)?;
    let main = package.expect(Kind::Pptx)?;
    let main_rels = package.relationships(&main)?;
    let mut parts = Parts {
        package,
        replaced: HashMap::new(),
        removed: HashSet::new(),
        added: Vec::new(),
    };
    let (pres_bom, mut pres) = text_of(&parts.get(&main)?);
    let (types_bom, types_text) = text_of(&parts.get(TYPES)?);
    let mut types = Types {
        text: types_text,
        bom: types_bom,
        changed: false,
    };
    let mut pres_rels = Rels::load(&mut parts, &main)?;
    let kinds = Kinds::read(&pres_rels.text);
    let target_of = |id: &str| {
        main_rels
            .iter()
            .find(|rel| rel.id == id)
            .and_then(|rel| rel.target.clone())
    };

    // The slides as the file lists them.
    let list = find_all(&pres, "sldIdLst").into_iter().next();
    let entries: Vec<(String, Option<String>, u32)> = find_all(&pres, "sldId")
        .iter()
        .map(|span| {
            let xml = pres[span.start..span.end].to_owned();
            let rel = rel_id(&xml);
            let number = attribute(&xml, "id")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            (xml, rel.and_then(|r| target_of(&r)), number)
        })
        .collect();
    let kept: HashSet<String> = presentation
        .slides
        .iter()
        .filter_map(|s| {
            s.source
                .as_ref()
                .filter(|s| !s.copy)
                .map(|s| s.part.clone())
        })
        .collect();
    let mut next_number = entries.iter().map(|e| e.2).max().unwrap_or(255).max(255) + 1;
    let ea = presentation.east_asian_language.clone();
    let mut media = Media::default();
    let mut notes_master: Option<String> = main_rels
        .iter()
        .find(|rel| rel.kind == "notesMaster")
        .and_then(|rel| rel.target.clone());

    // Each slide in order: its entry in the list.
    let mut items: Vec<String> = Vec::new();
    for slide in &presentation.slides {
        match slide.source.as_deref() {
            Some(source) if !source.copy => {
                let part = source.part.clone();
                let entry = entries
                    .iter()
                    .find(|e| e.1.as_deref() == Some(part.as_str()))
                    .map(|e| e.0.clone());
                let original = text_of(&parts.get(&part)?);
                let mut rels = Rels::load(&mut parts, &part)?;
                let mut context = Context {
                    presentation,
                    slide,
                    parts: &mut parts,
                    types: &mut types,
                    media: &mut media,
                    rels: &mut rels,
                    kinds: &kinds,
                    part: &part,
                    ea: ea.as_deref(),
                };
                if let Some(written) = write_slide(&original.1, slide, false, &mut context)? {
                    let mut bytes = original.0;
                    bytes.extend_from_slice(written.as_bytes());
                    parts.put(&part, bytes);
                }
                if slide.notes != source.notes {
                    write_notes(
                        &mut parts,
                        &mut types,
                        &mut rels,
                        &kinds,
                        &part,
                        slide.notes.as_deref(),
                        &mut notes_master,
                        &mut pres,
                        &mut pres_rels,
                        ea.as_deref(),
                    )?;
                }
                rels.store(&mut parts);
                match entry {
                    Some(entry) => items.push(entry),
                    None => {
                        // Read but not listed: list it again.
                        let id = pres_rels.add(&kinds.of("slide"), &relative(&main, &part));
                        items.push(new_entry(&pres, next_number, &id));
                        next_number += 1;
                    }
                }
            }
            source => {
                // A new slide, or a copy of one read.
                let part = parts.free(&format!("{}slide", slides_folder(&entries)), "xml");
                let mut rels = Rels {
                    part: rels_name(&part),
                    text: String::new(),
                    changed: true,
                };
                let base = match source {
                    Some(source) => {
                        let (_, text) = text_of(&parts.package.read(&source.part)?);
                        let source_rels = rels_name(&source.part);
                        rels.text = if parts.package.has(&source_rels) {
                            text_of(&parts.package.read(&source_rels)?).1
                        } else {
                            String::new()
                        };
                        rels.remove_kind("notesSlide");
                        text
                    }
                    None => {
                        rels.text = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_owned();
                        let layout = slide
                            .layout
                            .and_then(|l| presentation.layouts.get(l))
                            .map(|l| l.part.clone())
                            .or_else(|| presentation.layouts.first().map(|l| l.part.clone()))
                            .ok_or_else(|| Error::Broken("no slide layout".to_owned()))?;
                        rels.add(&kinds.of("slideLayout"), &relative(&part, &layout));
                        empty_slide()
                    }
                };
                if rels.text.is_empty() {
                    rels.text = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_owned();
                }
                let mut context = Context {
                    presentation,
                    slide,
                    parts: &mut parts,
                    types: &mut types,
                    media: &mut media,
                    rels: &mut rels,
                    kinds: &kinds,
                    part: &part,
                    ea: ea.as_deref(),
                };
                let written =
                    write_slide(&base, slide, true, &mut context)?.unwrap_or(base.clone());
                let mut bytes = XML_DECLARATION.as_bytes().to_vec();
                bytes.extend_from_slice(strip_declaration(&written).as_bytes());
                parts.put(&part, bytes);
                types.add_override(&part, &format!("{CT_PML}.slide+xml"));
                if slide.notes.is_some() {
                    write_notes(
                        &mut parts,
                        &mut types,
                        &mut rels,
                        &kinds,
                        &part,
                        slide.notes.as_deref(),
                        &mut notes_master,
                        &mut pres,
                        &mut pres_rels,
                        ea.as_deref(),
                    )?;
                }
                rels.changed = true;
                rels.store(&mut parts);
                let id = pres_rels.add(&kinds.of("slide"), &relative(&main, &part));
                items.push(new_entry(&pres, next_number, &id));
                next_number += 1;
            }
        }
    }

    // Slides deleted: their parts, notes, relationships and entries.
    for (xml, part, _) in &entries {
        let Some(part) = part else { continue };
        if kept.contains(part) {
            continue;
        }
        let slide_rels = parts.package.relationships(part).unwrap_or_default();
        for rel in slide_rels.iter().filter(|r| r.kind == "notesSlide") {
            if let Some(notes) = &rel.target {
                parts.remove(notes);
                parts.remove(&rels_name(notes));
                types.remove_override(notes);
            }
        }
        parts.remove(part);
        parts.remove(&rels_name(part));
        types.remove_override(part);
        if let Some(id) = rel_id(xml) {
            pres_rels.remove_id(&id);
        }
    }

    // The list of slides, if it changed.
    let old: Vec<&String> = entries.iter().map(|e| &e.0).collect();
    if items.iter().collect::<Vec<_>>() != old {
        let span = find_all(&pres, "sldIdLst").into_iter().next().or(list);
        match span {
            Some(span) => {
                let element = &pres[span.start..span.end];
                let (open, close) = inner(element);
                let new = if items.is_empty() {
                    String::new()
                } else if open == close {
                    format!(
                        "{}>{}</{}>",
                        &element[..open - 2],
                        items.concat(),
                        crate::xml_edit::qualified_name(element)
                    )
                } else {
                    format!(
                        "{}{}{}",
                        &element[..open],
                        items.concat(),
                        &element[close..]
                    )
                };
                pres.replace_range(span.start..span.end, &new);
            }
            None if !items.is_empty() => {
                let p = prefix_of(&pres[pres.find('<').unwrap_or(0)..]);
                let p = root_prefix(&pres).unwrap_or(p);
                let list = format!("<{p}sldIdLst>{}</{p}sldIdLst>", items.concat());
                insert_after_any(
                    &mut pres,
                    &["sldMasterIdLst", "notesMasterIdLst", "handoutMasterIdLst"],
                    &list,
                );
            }
            None => {}
        }
    }
    let pres_bytes = [pres_bom.as_slice(), pres.as_bytes()].concat();
    if pres_bytes != parts.package.read(&main)? {
        parts.put(&main, pres_bytes);
    }
    pres_rels.store(&mut parts);
    if types.changed {
        let bytes = [types.bom.as_slice(), types.text.as_bytes()].concat();
        parts.put(TYPES, bytes);
    }
    let Parts {
        mut package,
        replaced,
        removed,
        added,
    } = parts;
    package.rewrite_adding(&replaced, &removed, &added)
}

const XML_DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn strip_declaration(xml: &str) -> &str {
    match xml.strip_prefix("<?xml") {
        Some(rest) => rest
            .find("?>")
            .map_or(xml, |at| rest[at + 2..].trim_start()),
        None => xml,
    }
}

/// The folder new slides go in: where the first slide is.
fn slides_folder(entries: &[(String, Option<String>, u32)]) -> String {
    entries
        .iter()
        .find_map(|e| e.1.as_ref()?.rsplit_once('/').map(|(d, _)| format!("{d}/")))
        .unwrap_or_else(|| "ppt/slides/".to_owned())
}

/// The prefix the presentationml namespace has at the root.
fn root_prefix(xml: &str) -> Option<String> {
    let pattern = format!("=\"{NS_P}\"");
    let at = xml.find(&pattern)?;
    let declared = &xml[..at];
    let start = declared.rfind("xmlns")?;
    let name = &declared[start..];
    Some(match name.strip_prefix("xmlns:") {
        Some(prefix) => format!("{prefix}:"),
        None => String::new(),
    })
}

/// Insert `xml` after the last of the root's children named in `after`
/// (or as its first child).
fn insert_after_any(text: &mut String, after: &[&str], xml: &str) {
    let root_start = text.find("<p:").or_else(|| {
        text.match_indices('<')
            .find(|(i, _)| !text[*i..].starts_with("<?"))
            .map(|(i, _)| i)
    });
    let Some(root_start) = root_start else {
        return;
    };
    let root = &text[root_start..];
    let spans = children(root);
    let at = spans
        .iter()
        .filter(|s| after.contains(&s.name.as_str()))
        .map(|s| s.end)
        .next_back()
        .unwrap_or_else(|| inner(root).0);
    text.insert_str(root_start + at, xml);
}

/// A `sldId` entry like the file's others.
fn new_entry(pres: &str, number: u32, rel: &str) -> String {
    let p = root_prefix(pres).unwrap_or_else(|| "p:".to_owned());
    format!(
        "<{p}sldId id=\"{number}\" r:id=\"{rel}\" xmlns:r=\"{REL}\"/>",
        p = p
    )
    .replace(&format!(" xmlns:r=\"{REL}\""), &rel_namespace(pres))
}

/// Declare the relationships prefix on new elements only when the root does
/// not.
fn rel_namespace(xml: &str) -> String {
    if xml.contains("xmlns:r=") {
        String::new()
    } else {
        format!(" xmlns:r=\"{REL}\"")
    }
}

/// The relationship id of an element (`r:id`, whatever the prefix).
fn rel_id(xml: &str) -> Option<String> {
    let (open, _) = inner(xml);
    let tag = &xml[..open];
    tag.match_indices(":id=")
        .find_map(|(at, m)| {
            let quote = tag[at + m.len()..].chars().next()?;
            let rest = &tag[at + m.len() + 1..];
            Some(rest[..rest.find(quote)?].to_owned())
        })
        .filter(|v| !v.is_empty())
}

fn empty_slide() -> String {
    format!(
        "{XML_DECLARATION}<p:sld xmlns:a=\"{NS_A}\" xmlns:r=\"{REL}\" xmlns:p=\"{NS_P}\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"
    )
}

// --- Slides ---

struct Context<'a> {
    presentation: &'a Presentation,
    slide: &'a Slide,
    parts: &'a mut Parts,
    types: &'a mut Types,
    media: &'a mut Media,
    rels: &'a mut Rels,
    kinds: &'a Kinds,
    /// The slide's part (new name for a new one).
    part: &'a str,
    ea: Option<&'a str>,
}

/// A shape element found in a slide part, in the reader's order.
#[derive(Clone, Debug)]
struct FileShape {
    start: usize,
    end: usize,
    /// The child of the shape tree it is in (itself, a group, or
    /// `AlternateContent`).
    unit: usize,
}

/// The shape tree of a slide part.
struct Tree {
    /// Where its children are.
    inner: (usize, usize),
    units: Vec<Span>,
    shapes: Vec<FileShape>,
    /// The highest shape id in the part.
    max_id: u32,
}

fn scan(xml: &str) -> Option<Tree> {
    let tree = find_all(xml, "spTree").into_iter().next()?;
    let element = &xml[tree.start..tree.end];
    let (open, close) = inner(element);
    let units: Vec<Span> = children(element)
        .into_iter()
        .map(|s| Span {
            name: s.name,
            start: s.start + tree.start,
            end: s.end + tree.start,
        })
        .collect();
    let mut shapes = Vec::new();
    for (unit, span) in units.iter().enumerate() {
        collect_shapes(xml, span, unit, &mut shapes);
    }
    let max_id = find_all(xml, "cNvPr")
        .iter()
        .filter_map(|s| attribute(&xml[s.start..s.end], "id")?.parse::<u32>().ok())
        .max()
        .unwrap_or(1);
    Some(Tree {
        inner: (tree.start + open, tree.start + close),
        units,
        shapes,
        max_id,
    })
}

/// Shapes in a child of the tree, as the reader flattens them.
fn collect_shapes(xml: &str, span: &Span, unit: usize, out: &mut Vec<FileShape>) {
    match span.name.as_str() {
        "sp" | "cxnSp" | "pic" | "graphicFrame" => out.push(FileShape {
            start: span.start,
            end: span.end,
            unit,
        }),
        "grpSp" => {
            for child in children(&xml[span.start..span.end]) {
                let child = Span {
                    name: child.name,
                    start: child.start + span.start,
                    end: child.end + span.start,
                };
                collect_shapes(xml, &child, unit, out);
            }
        }
        "AlternateContent" => {
            if let Some(choice) = children(&xml[span.start..span.end])
                .into_iter()
                .find(|c| c.name == "Choice")
            {
                for child in children(&xml[span.start + choice.start..span.start + choice.end]) {
                    let child = Span {
                        name: child.name,
                        start: child.start + span.start + choice.start,
                        end: child.end + span.start + choice.start,
                    };
                    collect_shapes(xml, &child, unit, out);
                }
            }
        }
        _ => {}
    }
}

/// Whether a shape read from the file differs from how it was read.
fn shape_changed(shape: &Shape, source: &ShapeSource) -> bool {
    source.copy || shape.frame != source.frame || text_changed(shape, source)
}

fn text_changed(shape: &Shape, source: &ShapeSource) -> bool {
    let paragraphs: &[Paragraph] = shape.text.as_ref().map_or(&[], |b| &b.paragraphs);
    if paragraphs.len() != source.paragraphs.len() {
        // A body made here for a shape that had none, with nothing typed.
        return !(source.paragraphs.is_empty() && paragraphs.iter().all(Paragraph::is_empty));
    }
    paragraphs
        .iter()
        .zip(&source.paragraphs)
        .any(|(p, s)| match &p.source {
            Some(own) => !Arc::ptr_eq(own, s) || !p.same_as(&s.original) || !run_ends_same(p),
            None => true,
        })
}

/// The end mark looks as read.
fn run_ends_same(p: &Paragraph) -> bool {
    match &p.end.source {
        Some(source) => p.end.same_look(&source.original),
        None => true,
    }
}

/// What goes in the tree: a shape as read, a copy of one, or a new one.
enum Item<'a> {
    File(usize, &'a Shape),
    Copy(usize, &'a Shape),
    New(&'a Shape),
}

/// The slide part for `slide`, or `None` when nothing in it changed. `all`
/// writes every shape (a new or copied slide).
fn write_slide(
    xml: &str,
    slide: &Slide,
    all: bool,
    context: &mut Context<'_>,
) -> Result<Option<String>> {
    let Some(tree) = scan(xml) else {
        return Ok(None);
    };
    let own: Vec<&Shape> = slide.shapes.iter().filter(|s| !s.inherited).collect();
    let items: Vec<Item<'_>> = own
        .iter()
        .map(|shape| match &shape.source {
            Some(source) if (source.index as usize) < tree.shapes.len() => {
                if source.copy {
                    Item::Copy(source.index as usize, shape)
                } else {
                    Item::File(source.index as usize, shape)
                }
            }
            _ => Item::New(shape),
        })
        .collect();
    let represented: Vec<usize> = slide
        .source
        .as_ref()
        .map(|s| s.shapes.iter().map(|&i| i as usize).collect())
        .unwrap_or_default();
    let present: BTreeSet<usize> = items
        .iter()
        .filter_map(|item| match item {
            Item::File(i, _) => Some(*i),
            _ => None,
        })
        .collect();
    let deleted: BTreeSet<usize> = represented
        .iter()
        .copied()
        .filter(|i| !present.contains(i))
        .collect();
    let unchanged = !all
        && deleted.is_empty()
        && items.len() == represented.len()
        && items.iter().zip(&represented).all(|(item, &index)| {
            matches!(item, Item::File(i, shape) if *i == index
                && !shape_changed(shape, shape.source.as_ref().expect("from the file")))
        });
    if unchanged {
        return Ok(None);
    }

    let a = drawing_prefix(xml);
    let mut next_id = tree.max_id + 1;
    // Patched elements of shapes read, by file index.
    let mut patched: HashMap<usize, String> = HashMap::new();
    for item in &items {
        if let Item::File(index, shape) = item {
            let source = shape.source.as_ref().expect("from the file");
            if shape_changed(shape, source) {
                let file = &tree.shapes[*index];
                let element = &xml[file.start..file.end];
                patched.insert(
                    *index,
                    patch_shape(element, shape, source, None, &a, context)?,
                );
            }
        }
    }
    // Shapes in each unit.
    let mut in_unit: Vec<Vec<usize>> = vec![Vec::new(); tree.units.len()];
    for (index, shape) in tree.shapes.iter().enumerate() {
        in_unit[shape.unit].push(index);
    }
    let is_single = |unit: usize| {
        in_unit[unit].len() == 1
            && matches!(
                tree.units[unit].name.as_str(),
                "sp" | "cxnSp" | "pic" | "graphicFrame"
            )
    };
    let mut emitted = vec![false; tree.units.len()];
    // A unit is kept in place when none of its shapes are in the model.
    let unrepresented = |unit: usize| in_unit[unit].iter().all(|i| !represented.contains(i));
    let mut out = String::with_capacity(xml.len() + 1024);
    out.push_str(&xml[..tree.inner.0]);
    let mut cursor_unit = 0;
    // Everything before the first shape (the group's own properties).
    let flush = |upto: usize, out: &mut String, emitted: &mut Vec<bool>, cursor: &mut usize| {
        while *cursor < upto {
            let unit = *cursor;
            if !emitted[unit] && unrepresented(unit) {
                let span = &tree.units[unit];
                out.push_str(&xml[span.start..span.end]);
                emitted[unit] = true;
            }
            *cursor += 1;
        }
    };
    let first_shape_unit = tree
        .units
        .iter()
        .position(|u| u.name != "nvGrpSpPr" && u.name != "grpSpPr")
        .unwrap_or(tree.units.len());
    flush(first_shape_unit, &mut out, &mut emitted, &mut cursor_unit);
    for item in &items {
        match item {
            Item::File(index, _) => {
                let unit = tree.shapes[*index].unit;
                if emitted[unit] {
                    continue;
                }
                if unit > cursor_unit {
                    flush(unit, &mut out, &mut emitted, &mut cursor_unit);
                }
                let span = &tree.units[unit];
                if is_single(unit) {
                    match patched.get(index) {
                        Some(new) => out.push_str(new),
                        None => out.push_str(&xml[span.start..span.end]),
                    }
                } else {
                    // A group: its shapes patched or taken out inside it.
                    let mut text = xml[span.start..span.end].to_owned();
                    let mut edits: Vec<(usize, usize, String)> = Vec::new();
                    for &i in &in_unit[unit] {
                        let file = &tree.shapes[i];
                        let (start, end) = (file.start - span.start, file.end - span.start);
                        if deleted.contains(&i) {
                            edits.push((start, end, String::new()));
                        } else if let Some(new) = patched.get(&i) {
                            edits.push((start, end, new.clone()));
                        }
                    }
                    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
                    for (start, end, new) in edits {
                        text.replace_range(start..end, &new);
                    }
                    out.push_str(&text);
                }
                emitted[unit] = true;
            }
            Item::Copy(index, shape) => {
                let file = &tree.shapes[*index];
                let element = &xml[file.start..file.end];
                let source = shape.source.as_ref().expect("from the file");
                let id = next_id;
                next_id += 1;
                out.push_str(&patch_shape(element, shape, source, Some(id), &a, context)?);
            }
            Item::New(shape) => {
                let id = next_id;
                next_id += 1;
                out.push_str(&new_shape(shape, id, &a, context)?);
            }
        }
    }
    // What is left: shapes not in the model, the tree's extensions.
    flush(tree.units.len(), &mut out, &mut emitted, &mut cursor_unit);
    out.push_str(&xml[tree.inner.1..]);
    Ok(Some(out))
}

/// The prefix bound to the DrawingML namespace in a part, with its colon.
fn drawing_prefix(xml: &str) -> String {
    let pattern = format!("=\"{NS_A}\"");
    if let Some(at) = xml.find(&pattern) {
        let declared = &xml[..at];
        if let Some(start) = declared.rfind("xmlns:") {
            return format!("{}:", &declared[start + 6..]);
        }
        return String::new();
    }
    "a:".to_owned()
}

/// A shape element with its new frame, text and (for a copy) id.
fn patch_shape(
    element: &str,
    shape: &Shape,
    source: &ShapeSource,
    new_id: Option<u32>,
    a: &str,
    context: &mut Context<'_>,
) -> Result<String> {
    let mut out = element.to_owned();
    if let Some(id) = new_id
        && let Some(span) = find_all(&out, "cNvPr").into_iter().next()
    {
        let tag = set_attribute(&out[span.start..span.end], "id", &id.to_string());
        out.replace_range(span.start..span.end, &tag);
    }
    // A copy is written at the top of the tree, in slide coordinates.
    let group = if new_id.is_some() { None } else { source.group };
    if shape.frame != source.frame || (new_id.is_some() && source.group.is_some()) {
        out = with_frame(&out, &shape.frame, group, a);
    }
    if text_changed(shape, source)
        && let Some(body) = &shape.text
    {
        let baseline = body.level_looks();
        let paragraphs = paragraphs_xml(body, &baseline, a, context)?;
        out = with_paragraphs(&out, &paragraphs, a, body);
    }
    Ok(out)
}

/// `frame` written into a shape element's transform (child coordinates
/// for a shape in a group).
fn with_frame(element: &str, frame: &Frame, group: Option<[f64; 4]>, a: &str) -> String {
    let (x, y, cx, cy) = match group {
        Some([sx, sy, dx, dy]) => (
            ((frame.x as f64 - dx) / sx).round() as i64,
            ((frame.y as f64 - dy) / sy).round() as i64,
            (frame.width as f64 / sx).round() as i64,
            (frame.height as f64 / sy).round() as i64,
        ),
        None => (frame.x, frame.y, frame.width, frame.height),
    };
    let off_ext = |xfrm: &str, prefix: &str| {
        let mut xfrm = xfrm.to_owned();
        let kids = children(&xfrm);
        let off = format!("<{prefix}off x=\"{x}\" y=\"{y}\"/>");
        let ext = format!("<{prefix}ext cx=\"{cx}\" cy=\"{cy}\"/>");
        match kids.iter().find(|s| s.name == "ext") {
            Some(span) => {
                let new = set_attribute(
                    &set_attribute(&xfrm[span.start..span.end], "cx", &cx.to_string()),
                    "cy",
                    &cy.to_string(),
                );
                xfrm.replace_range(span.start..span.end, &new);
            }
            None => xfrm = edit_children(&xfrm, &[], &ext, &["chOff", "chExt"]),
        }
        let kids = children(&xfrm);
        match kids.iter().find(|s| s.name == "off") {
            Some(span) => {
                let new = set_attribute(
                    &set_attribute(&xfrm[span.start..span.end], "x", &x.to_string()),
                    "y",
                    &y.to_string(),
                );
                xfrm.replace_range(span.start..span.end, &new);
            }
            None => xfrm = edit_children(&xfrm, &[], &off, &["ext", "chOff", "chExt"]),
        }
        xfrm
    };
    let kids = children(element);
    let p = prefix_of(element);
    if crate::xml_edit::qualified_name(element).ends_with("graphicFrame") {
        return match kids.iter().find(|s| s.name == "xfrm") {
            Some(span) => {
                let new = off_ext(&element[span.start..span.end], a);
                format!("{}{new}{}", &element[..span.start], &element[span.end..])
            }
            None => edit_children(
                element,
                &[],
                &format!(
                    "<{p}xfrm>{}</{p}xfrm>",
                    off_ext(&format!("<{p}xfrm/>"), a)
                        .trim_start_matches(&format!("<{p}xfrm>"))
                        .trim_end_matches(&format!("</{p}xfrm>"))
                ),
                &["graphic", "extLst"],
            ),
        };
    }
    let Some(sp_pr) = kids.iter().find(|s| s.name == "spPr") else {
        return element.to_owned();
    };
    let props = &element[sp_pr.start..sp_pr.end];
    let props_kids = children(props);
    let new_props = match props_kids.iter().find(|s| s.name == "xfrm") {
        Some(span) => {
            let new = off_ext(&props[span.start..span.end], a);
            format!("{}{new}{}", &props[..span.start], &props[span.end..])
        }
        None => {
            let xfrm = format!(
                "<{a}xfrm><{a}off x=\"{x}\" y=\"{y}\"/><{a}ext cx=\"{cx}\" cy=\"{cy}\"/></{a}xfrm>"
            );
            let first: Vec<&str> = props_kids.iter().map(|s| s.name.as_str()).collect();
            edit_children(props, &[], &xfrm, &first)
        }
    };
    format!(
        "{}{new_props}{}",
        &element[..sp_pr.start],
        &element[sp_pr.end..]
    )
}

/// A shape element with its paragraphs replaced (a text body added if it
/// had none).
fn with_paragraphs(element: &str, paragraphs: &str, a: &str, body: &TextBody) -> String {
    let kids = children(element);
    let p = prefix_of(element);
    match kids.iter().find(|s| s.name == "txBody") {
        Some(span) => {
            let text_body = &element[span.start..span.end];
            let inner_kids = children(text_body);
            let paras: Vec<&Span> = inner_kids.iter().filter(|s| s.name == "p").collect();
            let new_body = match (paras.first(), paras.last()) {
                (Some(first), Some(last)) => format!(
                    "{}{paragraphs}{}",
                    &text_body[..first.start],
                    &text_body[last.end..]
                ),
                _ => edit_children(text_body, &[], paragraphs, &["extLst"]),
            };
            format!(
                "{}{new_body}{}",
                &element[..span.start],
                &element[span.end..]
            )
        }
        None => {
            let anchor = match body.anchor {
                aster_slides::Anchor::Middle => " anchor=\"ctr\"",
                aster_slides::Anchor::Bottom => " anchor=\"b\"",
                aster_slides::Anchor::Top => "",
            };
            let text_body = format!(
                "<{p}txBody><{a}bodyPr rtlCol=\"0\"{anchor}/><{a}lstStyle/>{paragraphs}</{p}txBody>"
            );
            edit_children(element, &[], &text_body, &["extLst"])
        }
    }
}

// --- Text ---

fn paragraphs_xml(
    body: &TextBody,
    baseline: &Levels,
    a: &str,
    context: &Context<'_>,
) -> Result<String> {
    let mut out = String::new();
    if body.paragraphs.is_empty() {
        out.push_str(&format!(
            "<{a}p><{a}endParaRPr lang=\"{}\"/></{a}p>",
            language_for("", context.ea)
        ));
    }
    for paragraph in &body.paragraphs {
        out.push_str(&paragraph_xml(paragraph, baseline, a, context.ea));
    }
    Ok(out)
}

fn paragraph_xml(paragraph: &Paragraph, baseline: &Levels, a: &str, ea: Option<&str>) -> String {
    if let Some(source) = &paragraph.source
        && !source.xml.is_empty()
        && paragraph.same_as(&source.original)
        && run_ends_same(paragraph)
        && paragraph
            .runs
            .iter()
            .all(|r| r.source.as_ref().is_some_and(|s| !s.xml.is_empty()))
    {
        return source.xml.clone();
    }
    let level = usize::from(paragraph.level.min(8));
    let look = &baseline[level];
    let source = paragraph.source.as_deref();
    let original = source.map(|s| &s.original);
    let mut props = source
        .map(|s| s.props.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| format!("<{a}pPr/>"));
    let level_same = original.is_some_and(|o| o.level == paragraph.level);
    // Attributes: kept as written when unchanged, dropped when the level
    // gives the same, written otherwise.
    let attr =
        |props: &mut String, name: &str, same_as_read: bool, inherits: bool, value: String| {
            if same_as_read && level_same {
                return;
            }
            *props = if inherits {
                remove_attribute(props, name)
            } else {
                set_attribute(props, name, &value)
            };
        };
    if !original.is_some_and(|o| o.level == paragraph.level) {
        props = if paragraph.level == 0 {
            remove_attribute(&props, "lvl")
        } else {
            set_attribute(&props, "lvl", &paragraph.level.to_string())
        };
    }
    attr(
        &mut props,
        "marL",
        original.is_some_and(|o| o.margin == paragraph.margin),
        paragraph.margin == look.margin,
        paragraph.margin.to_string(),
    );
    attr(
        &mut props,
        "indent",
        original.is_some_and(|o| o.indent == paragraph.indent),
        paragraph.indent == look.indent,
        paragraph.indent.to_string(),
    );
    attr(
        &mut props,
        "algn",
        original.is_some_and(|o| o.align == paragraph.align),
        paragraph.align == look.align,
        align_value(paragraph.align).to_owned(),
    );
    let list_same = original.is_some_and(|o| o.list == paragraph.list) && level_same;
    if !list_same {
        let bullets = ["buNone", "buAutoNum", "buChar", "buBlip"];
        let order = ["tabLst", "defRPr", "extLst"];
        props = if paragraph.list == look.list {
            edit_children(&props, &bullets, "", &order)
        } else {
            let new = match &paragraph.list {
                None => format!("<{a}buNone/>"),
                Some(List::Char(c)) => {
                    let font = if children(&props).iter().any(|s| s.name == "buFont") {
                        String::new()
                    } else {
                        format!("<{a}buFont typeface=\"Arial\"/>")
                    };
                    format!("{font}<{a}buChar char=\"{}\"/>", escape_attribute(c))
                }
                Some(List::Number(scheme, start)) => {
                    let start = if *start == 1 {
                        String::new()
                    } else {
                        format!(" startAt=\"{start}\"")
                    };
                    format!(
                        "<{a}buAutoNum type=\"{}\"{start}/>",
                        escape_attribute(scheme)
                    )
                }
            };
            edit_children(&props, &bullets, &new, &order)
        };
    }
    let mut out = format!("<{a}p>");
    if !is_bare(&props) {
        out.push_str(&props);
    }
    for run in &paragraph.runs {
        out.push_str(&run_xml(run, look, a, ea));
    }
    out.push_str(&end_xml(&paragraph.end, look, a, ea));
    out.push_str(&format!("</{a}p>"));
    out
}

fn align_value(align: Align) -> &'static str {
    match align {
        Align::Left => "l",
        Align::Center => "ctr",
        Align::Right => "r",
        Align::Justify => "just",
    }
}

/// A properties element for `run`: `base` (as written, or new) with what
/// differs from `original` (what was read) written in; a property back to
/// what the level gives is dropped instead.
fn run_props(base: &str, run: &Run, original: Option<&Run>, look: &LevelLook, a: &str) -> String {
    let mut props = base.to_owned();
    let changed = |same: bool| original.is_none_or(|_| !same);
    if changed(original.is_some_and(|o| o.size == run.size)) {
        props = if run.size == look.size && original.is_some() {
            remove_attribute(&props, "sz")
        } else if run.size == look.size {
            props
        } else {
            set_attribute(
                &props,
                "sz",
                &((run.size * 100.0).round() as i64).to_string(),
            )
        };
    }
    let flag = |props: String, name: &str, now: bool, then: Option<bool>, inherited: bool| {
        if then == Some(now) || (then.is_none() && now == inherited) {
            return props;
        }
        if now == inherited && then.is_some() && attribute(&props, name).is_some() {
            return remove_attribute(&props, name);
        }
        set_attribute(&props, name, if now { "1" } else { "0" })
    };
    props = flag(props, "b", run.bold, original.map(|o| o.bold), look.bold);
    props = flag(
        props,
        "i",
        run.italic,
        original.map(|o| o.italic),
        look.italic,
    );
    if original.map(|o| o.underline) != Some(run.underline) && (original.is_some() || run.underline)
    {
        props = if run.underline {
            set_attribute(&props, "u", "sng")
        } else if attribute(&props, "u").is_some() {
            set_attribute(&props, "u", "none")
        } else {
            props
        };
    }
    if original.map(|o| o.strike) != Some(run.strike) && (original.is_some() || run.strike) {
        props = set_attribute(
            &props,
            "strike",
            if run.strike { "sngStrike" } else { "noStrike" },
        );
    }
    let color_changed = match original {
        Some(o) => o.color != run.color,
        None => run.color != look.color,
    };
    if color_changed {
        let fills = [
            "solidFill",
            "gradFill",
            "noFill",
            "pattFill",
            "blipFill",
            "grpFill",
        ];
        let after = [
            "effectLst",
            "effectDag",
            "highlight",
            "uLnTx",
            "uLn",
            "uFillTx",
            "uFill",
            "latin",
            "ea",
            "cs",
            "sym",
            "hlinkClick",
            "hlinkMouseOver",
            "rtl",
            "extLst",
        ];
        let fill = match run.color {
            Some(color) if Some(color) != look.color || original.is_some() => format!(
                "<{a}solidFill><{a}srgbClr val=\"{:02X}{:02X}{:02X}\"/></{a}solidFill>",
                color[0], color[1], color[2]
            ),
            _ => String::new(),
        };
        props = edit_children(&props, &fills, &fill, &after);
    }
    props
}

fn run_xml(run: &Run, look: &LevelLook, a: &str, ea: Option<&str>) -> String {
    let source = run.source.as_deref();
    if let Some(source) = source
        && !source.xml.is_empty()
        && source.original.text == run.text
        && run.same_look(&source.original)
    {
        return source.xml.clone();
    }
    let base = match source.map(|s| s.props.as_str()).filter(|p| !p.is_empty()) {
        Some(props) => {
            let mut props = rename(props, "rPr");
            // Text typed into an empty paragraph: marked with its own
            // language when it is Japanese, Chinese or Korean.
            let wanted = language_for(&run.text, ea);
            let east_asian = ["ja", "zh", "ko"].iter().any(|l| wanted.starts_with(l));
            let marked = attribute(&props, "lang").unwrap_or_default();
            if source.is_some_and(|s| s.xml.is_empty())
                && east_asian
                && !marked.starts_with(&wanted[..2])
            {
                props = set_attribute(&props, "lang", &wanted);
            }
            props
        }
        None => format!(
            "<{a}rPr lang=\"{}\" dirty=\"0\"/>",
            language_for(&run.text, ea)
        ),
    };
    let props = run_props(&base, run, source.map(|s| &s.original), look, a);
    let props = if is_bare(&props) {
        String::new()
    } else {
        props
    };
    // A field (a slide number, a date) stays a field while it has no breaks.
    let field = source
        .filter(|s| {
            crate::xml_edit::qualified_name(&s.xml).ends_with("fld") && !run.text.contains('\n')
        })
        .map(|s| {
            s.xml[..inner(&s.xml).0]
                .trim_end_matches("/>")
                .trim_end_matches('>')
                .to_owned()
        });
    let mut out = String::new();
    for (i, piece) in run.text.split('\n').enumerate() {
        if i > 0 {
            if props.is_empty() {
                out.push_str(&format!("<{a}br/>"));
            } else {
                out.push_str(&format!("<{a}br>{props}</{a}br>"));
            }
        }
        if piece.is_empty() {
            continue;
        }
        let t = format!("<{a}t>{}</{a}t>", escape(piece));
        match &field {
            Some(open) => {
                let name = crate::xml_edit::qualified_name(open).to_owned();
                out.push_str(&format!("{open}>{props}{t}</{name}>"));
            }
            None => out.push_str(&format!("<{a}r>{props}{t}</{a}r>")),
        }
    }
    out
}

fn end_xml(end: &Run, look: &LevelLook, a: &str, ea: Option<&str>) -> String {
    let source = end.source.as_deref();
    match source {
        Some(source) if !source.props.is_empty() && end.same_look(&source.original) => {
            return rename(&source.props, "endParaRPr");
        }
        _ => {}
    }
    let base = match source.map(|s| s.props.as_str()).filter(|p| !p.is_empty()) {
        Some(props) => rename(props, "endParaRPr"),
        None => format!(
            "<{a}endParaRPr lang=\"{}\" dirty=\"0\"/>",
            language_for("", ea)
        ),
    };
    let original = source.map(|s| &s.original);
    run_props(&base, end, original, look, a)
}

/// The language a new run is marked with: by its script, else the
/// presentation's.
fn language_for(text: &str, ea: Option<&str>) -> String {
    let has = |range: std::ops::RangeInclusive<char>| text.chars().any(|c| range.contains(&c));
    if has('\u{3040}'..='\u{30FF}') {
        "ja-JP".to_owned()
    } else if has('\u{AC00}'..='\u{D7AF}') {
        "ko-KR".to_owned()
    } else if has('\u{4E00}'..='\u{9FFF}') {
        ea.filter(|l| l.starts_with("zh") || l.starts_with("ja"))
            .unwrap_or("zh-CN")
            .to_owned()
    } else if text.is_empty() {
        ea.unwrap_or("en-US").to_owned()
    } else {
        "en-US".to_owned()
    }
}

// --- New shapes ---

fn frame_xml(frame: &Frame, a: &str) -> String {
    format!(
        "<{a}xfrm><{a}off x=\"{}\" y=\"{}\"/><{a}ext cx=\"{}\" cy=\"{}\"/></{a}xfrm>",
        frame.x, frame.y, frame.width, frame.height
    )
}

/// A new shape's element.
fn new_shape(shape: &Shape, id: u32, a: &str, context: &mut Context<'_>) -> Result<String> {
    let p = "p:";
    let look = context.slide.look.clone();
    let name_number = id.saturating_sub(1);
    if shape.role == Role::Picture
        && let Some(picture) = &shape.picture
    {
        let media = context
            .media
            .part_for(context.parts, context.types, &picture.image);
        let rel = context
            .rels
            .add(&context.kinds.of("image"), &relative(context.part, &media));
        return Ok(format!(
            "<{p}pic><{p}nvPicPr><{p}cNvPr id=\"{id}\" name=\"Picture {name_number}\"/><{p}cNvPicPr><{a}picLocks noChangeAspect=\"1\"/></{p}cNvPicPr><{p}nvPr/></{p}nvPicPr><{p}blipFill><{a}blip xmlns:r=\"{REL}\" r:embed=\"{rel}\"/><{a}stretch><{a}fillRect/></{a}stretch></{p}blipFill><{p}spPr>{}<{a}prstGeom prst=\"rect\"><{a}avLst/></{a}prstGeom></{p}spPr></{p}pic>",
            frame_xml(&shape.frame, a)
        ));
    }
    if shape.geometry == Geometry::Line {
        return Ok(format!(
            "<{p}cxnSp><{p}nvCxnSpPr><{p}cNvPr id=\"{id}\" name=\"Straight Connector {name_number}\"/><{p}cNvCxnSpPr/><{p}nvPr/></{p}nvCxnSpPr><{p}spPr>{}<{a}prstGeom prst=\"line\"><{a}avLst/></{a}prstGeom><{a}ln w=\"12700\"><{a}solidFill><{a}schemeClr val=\"accent1\"/></{a}solidFill></{a}ln></{p}spPr></{p}cxnSp>",
            frame_xml(&shape.frame, a)
        ));
    }
    let empty = TextBody::default();
    let body = shape.text.as_ref().unwrap_or(&empty);
    if let Some(placeholder) = &shape.placeholder {
        // A placeholder of a new slide: where the layout puts it unless moved.
        let layout_frame = context
            .slide
            .layout
            .and_then(|l| context.presentation.layouts.get(l))
            .and_then(|layout| {
                layout
                    .slide
                    .shapes
                    .iter()
                    .find(|s| s.placeholder.as_ref() == Some(placeholder))
                    .map(|s| s.frame)
            });
        let sp_pr = if layout_frame == Some(shape.frame) {
            format!("<{p}spPr/>")
        } else {
            format!("<{p}spPr>{}</{p}spPr>", frame_xml(&shape.frame, a))
        };
        let kind = if placeholder.kind == "obj" {
            String::new()
        } else {
            format!(" type=\"{}\"", escape_attribute(&placeholder.kind))
        };
        let idx = placeholder
            .idx
            .map_or(String::new(), |i| format!(" idx=\"{i}\""));
        let name = match placeholder.kind.as_str() {
            "title" | "ctrTitle" => "Title",
            "subTitle" => "Subtitle",
            "body" | "obj" => "Content Placeholder",
            "pic" => "Picture Placeholder",
            _ => "Placeholder",
        };
        let baseline = body.level_looks();
        let paragraphs = paragraphs_xml(body, &baseline, a, context)?;
        return Ok(format!(
            "<{p}sp><{p}nvSpPr><{p}cNvPr id=\"{id}\" name=\"{name} {name_number}\"/><{p}cNvSpPr><{a}spLocks noGrp=\"1\"/></{p}cNvSpPr><{p}nvPr><{p}ph{kind}{idx}/></{p}nvPr></{p}nvSpPr>{sp_pr}<{p}txBody><{a}bodyPr/><{a}lstStyle/>{paragraphs}</{p}txBody></{p}sp>"
        ));
    }
    let text_box = shape.fill.is_none() && shape.line.is_none();
    if text_box {
        let baseline = look.text.clone();
        let paragraphs = paragraphs_xml(body, &baseline, a, context)?;
        let fit = if body.grow {
            format!("<{a}spAutoFit/>")
        } else {
            String::new()
        };
        return Ok(format!(
            "<{p}sp><{p}nvSpPr><{p}cNvPr id=\"{id}\" name=\"TextBox {name_number}\"/><{p}cNvSpPr txBox=\"1\"/><{p}nvPr/></{p}nvSpPr><{p}spPr>{}<{a}prstGeom prst=\"rect\"><{a}avLst/></{a}prstGeom><{a}noFill/></{p}spPr><{p}txBody><{a}bodyPr wrap=\"square\" rtlCol=\"0\">{fit}</{a}bodyPr><{a}lstStyle/>{paragraphs}</{p}txBody></{p}sp>",
            frame_xml(&shape.frame, a)
        ));
    }
    let (preset, name) = match &shape.geometry {
        Geometry::RoundRect => ("roundRect", "Rounded Rectangle"),
        Geometry::Ellipse => ("ellipse", "Oval"),
        _ => ("rect", "Rectangle"),
    };
    // What a shape's text inherits: the text styles in the shape style's
    // colour, left aligned.
    let mut baseline = (*look.text).clone();
    for level in &mut baseline {
        level.color = Some(look.on_accent);
    }
    let paragraphs = if body.is_empty() && body.paragraphs.len() <= 1 {
        format!(
            "<{a}p><{a}pPr algn=\"ctr\"/><{a}endParaRPr lang=\"{}\" dirty=\"0\"/></{a}p>",
            language_for("", context.ea)
        )
    } else {
        paragraphs_xml(body, &baseline, a, context)?
    };
    Ok(format!(
        "<{p}sp><{p}nvSpPr><{p}cNvPr id=\"{id}\" name=\"{name} {name_number}\"/><{p}cNvSpPr/><{p}nvPr/></{p}nvSpPr><{p}spPr>{}<{a}prstGeom prst=\"{preset}\"><{a}avLst/></{a}prstGeom></{p}spPr><{p}style><{a}lnRef idx=\"2\"><{a}schemeClr val=\"accent1\"><{a}shade val=\"15000\"/></{a}schemeClr></{a}lnRef><{a}fillRef idx=\"1\"><{a}schemeClr val=\"accent1\"/></{a}fillRef><{a}effectRef idx=\"0\"><{a}schemeClr val=\"accent1\"/></{a}effectRef><{a}fontRef idx=\"minor\"><{a}schemeClr val=\"lt1\"/></{a}fontRef></{p}style><{p}txBody><{a}bodyPr rtlCol=\"0\" anchor=\"ctr\"/><{a}lstStyle/>{paragraphs}</{p}txBody></{p}sp>",
        frame_xml(&shape.frame, a)
    ))
}

// --- Notes ---

#[allow(clippy::too_many_arguments)]
fn write_notes(
    parts: &mut Parts,
    types: &mut Types,
    slide_rels: &mut Rels,
    kinds: &Kinds,
    slide_part: &str,
    notes: Option<&str>,
    notes_master: &mut Option<String>,
    pres: &mut String,
    pres_rels: &mut Rels,
    ea: Option<&str>,
) -> Result<()> {
    let existing = find_all(&slide_rels.text, "Relationship")
        .into_iter()
        .map(|s| slide_rels.text[s.start..s.end].to_owned())
        .find(|r| r.contains("/notesSlide\""))
        .and_then(|r| attribute(&r, "Target"))
        .map(|target| resolve(slide_part, &target))
        .filter(|part| parts.has(part));
    let lines: Vec<&str> = notes.map_or(Vec::new(), |n| n.split('\n').collect());
    let paragraphs = |a: &str, props: Option<String>| -> String {
        let mut out = String::new();
        for line in &lines {
            if line.is_empty() {
                out.push_str(&format!(
                    "<{a}p><{a}endParaRPr lang=\"{}\"/></{a}p>",
                    language_for("", ea)
                ));
                continue;
            }
            let props = props.clone().unwrap_or_else(|| {
                format!("<{a}rPr lang=\"{}\" dirty=\"0\"/>", language_for(line, ea))
            });
            out.push_str(&format!(
                "<{a}p><{a}r>{props}<{a}t>{}</{a}t></{a}r></{a}p>",
                escape(line)
            ));
        }
        if out.is_empty() {
            out.push_str(&format!(
                "<{a}p><{a}endParaRPr lang=\"{}\"/></{a}p>",
                language_for("", ea)
            ));
        }
        out
    };
    if let Some(part) = existing {
        let (bom, mut xml) = text_of(&parts.get(&part)?);
        let a = drawing_prefix(&xml);
        // The body placeholder's text.
        let Some(body) = find_all(&xml, "sp").into_iter().find(|span| {
            let sp = &xml[span.start..span.end];
            find_all(sp, "ph").first().is_some_and(|ph| {
                attribute(&sp[ph.start..ph.end], "type").as_deref() == Some("body")
            })
        }) else {
            return Ok(());
        };
        let sp = xml[body.start..body.end].to_owned();
        let props = find_all(&sp, "rPr")
            .first()
            .map(|s| sp[s.start..s.end].to_owned());
        let empty = TextBody::default();
        let new = with_paragraphs(&sp, &paragraphs(&a, props), &a, &empty);
        xml.replace_range(body.start..body.end, &new);
        parts.put(&part, [bom.as_slice(), xml.as_bytes()].concat());
        return Ok(());
    }
    if notes.is_none() {
        return Ok(());
    }
    let master = match notes_master.clone() {
        Some(master) if parts.has(&master) => master,
        _ => {
            let master = add_notes_master(parts, types, kinds, pres, pres_rels)?;
            *notes_master = Some(master.clone());
            master
        }
    };
    let part = parts.free("ppt/notesSlides/notesSlide", "xml");
    let xml = format!(
        "{XML_DECLARATION}<p:notes xmlns:a=\"{NS_A}\" xmlns:r=\"{REL}\" xmlns:p=\"{NS_P}\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Slide Image Placeholder 1\"/><p:cNvSpPr><a:spLocks noGrp=\"1\" noRot=\"1\" noChangeAspect=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"sldImg\"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Notes Placeholder 2\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>",
        paragraphs("a:", None)
    );
    parts.put(&part, xml.into_bytes());
    types.add_override(&part, &format!("{CT_PML}.notesSlide+xml"));
    let mut rels = Rels {
        part: rels_name(&part),
        text: "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_owned(),
        changed: true,
    };
    rels.add(&kinds.of("notesMaster"), &relative(&part, &master));
    rels.add(&kinds.of("slide"), &relative(&part, slide_part));
    rels.store(parts);
    slide_rels.add(&kinds.of("notesSlide"), &relative(slide_part, &part));
    Ok(())
}

/// A notes master for a presentation that has none, with its own theme.
fn add_notes_master(
    parts: &mut Parts,
    types: &mut Types,
    kinds: &Kinds,
    pres: &mut String,
    pres_rels: &mut Rels,
) -> Result<String> {
    let part = parts.free("ppt/notesMasters/notesMaster", "xml");
    let theme = parts.free("ppt/theme/theme", "xml");
    parts.put(&part, blank::NOTES_MASTER.as_bytes().to_vec());
    parts.put(&theme, blank::theme("Office Theme").into_bytes());
    let mut rels = Rels {
        part: rels_name(&part),
        text: "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_owned(),
        changed: true,
    };
    rels.add(&kinds.of("theme"), &relative(&part, &theme));
    rels.store(parts);
    types.add_override(&part, &format!("{CT_PML}.notesMaster+xml"));
    types.add_override(
        &theme,
        "application/vnd.openxmlformats-officedocument.theme+xml",
    );
    let id = pres_rels.add(
        &kinds.of("notesMaster"),
        &relative("ppt/presentation.xml", &part),
    );
    let p = root_prefix(pres).unwrap_or_else(|| "p:".to_owned());
    let list = format!(
        "<{p}notesMasterIdLst><{p}notesMasterId r:id=\"{id}\"{}/></{p}notesMasterIdLst>",
        rel_namespace(pres)
    );
    insert_after_any(pres, &["sldMasterIdLst"], &list);
    Ok(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative("ppt/slides/slide1.xml", "ppt/media/image1.png"),
            "../media/image1.png"
        );
        assert_eq!(
            relative("ppt/presentation.xml", "ppt/slides/slide2.xml"),
            "slides/slide2.xml"
        );
        assert_eq!(
            relative("ppt/notesSlides/notesSlide1.xml", "ppt/slides/slide1.xml"),
            "../slides/slide1.xml"
        );
    }

    #[test]
    fn languages_by_script() {
        assert_eq!(language_for("こんにちは", None), "ja-JP");
        assert_eq!(language_for("你好", None), "zh-CN");
        assert_eq!(language_for("東京", Some("ja-JP")), "ja-JP");
        assert_eq!(language_for("hello", Some("ja-JP")), "en-US");
    }
}
