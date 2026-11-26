//! Saving headers, footers, footnotes and endnotes.
//!
//! They are spliced the way the body is: a story that is as it was read is
//! not written at all (its part stays byte for byte, compressed bytes and
//! all); one that changed is read again, and its blocks are copied, changed
//! or written where they now are, with the bytes between them as they were.
//! Each part has its own relationships (a link or a picture in a header is
//! related from the header's part), its own saver, and shares what must be
//! unique over the package (part names, picture ids) with the others.
//!
//! **Made since reading.**
//! - A header or footer with no part gets one (`word/headerN.xml`,
//!   `word/footerN.xml`), a relationship from the main part, a content-type
//!   override, and a `headerReference` or `footerReference` as the first
//!   child of the `sectPr` of the section that introduces it (the later
//!   sections that show it have none: the file format makes them follow the
//!   one before). Only that element is put into the `sectPr`; the rest of
//!   its bytes stay.
//! - New footnotes are added to the footnotes part, as `w:footnote` elements
//!   after the others; footnotes that are gone are dropped, element and all.
//!   A file with no footnotes part gets one, with the separator and the
//!   continuation separator Word looks for, a relationship and a content
//!   type. (`settings.xml` is left as it is: it does not list them.)
//!
//! A note's own mark (the number at its start) is written as `footnoteRef`
//! or `endnoteRef`, and a page-number field as a simple field with its last
//! result.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use aster_writer::edit::same_blocks;
use aster_writer::{Block, Document, MarginalKind, MarginalSlot, Note, Section, Story};

use super::{
    Carried, Media, Saver, add_namespace, collect_media, escape_attribute, range_key, root_tag,
};
use crate::error::{Error, Result};
use crate::package::{Package, rels_name};
use crate::xml_edit::{attribute, children, inner, qualified_name};

const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";

/// The key of the references of a last section that has no `sectPr`.
pub(super) const NO_SECTION: (usize, usize) = (usize::MAX, usize::MAX);

const SLOTS: [MarginalSlot; 3] = [
    MarginalSlot::Default,
    MarginalSlot::First,
    MarginalSlot::Even,
];

fn slot_name(slot: MarginalSlot) -> &'static str {
    match slot {
        MarginalSlot::Default => "default",
        MarginalSlot::First => "first",
        MarginalSlot::Even => "even",
    }
}

/// A header or footer part to make.
pub(super) struct NewPart {
    marginal: usize,
    name: String,
    kind: MarginalKind,
}

/// What a save adds to the package for stories.
pub(super) struct Plan {
    parts: Vec<NewPart>,
    /// The name of the footnotes part to make, if the file has none.
    footnotes: Option<String>,
}

/// A name for a part in `dir` starting `stem`, not taken (case does not
/// matter); it is taken now.
fn free_name(taken: &mut HashSet<String>, dir: &str, stem: &str, number_first: bool) -> String {
    let make = |n: usize| {
        if n == 0 && !number_first {
            format!("{dir}{stem}.xml")
        } else {
            format!("{dir}{stem}{}.xml", n.max(1))
        }
    };
    let mut n = usize::from(number_first);
    loop {
        let name = make(n);
        if taken.insert(name.to_ascii_lowercase()) {
            return name;
        }
        n += 1;
    }
}

/// Decide the new parts and give the relationships and section references
/// they need. Called before the main part is written.
pub(super) fn plan(
    main: &str,
    original: &Document,
    document: &Document,
    saver: &mut Saver<'_>,
) -> Plan {
    let dir = main
        .rsplit_once('/')
        .map_or(String::new(), |(dir, _)| format!("{dir}/"));
    let sections = document.sections();
    let mut kinds: HashMap<usize, MarginalKind> = HashMap::new();
    for (_, section) in &sections {
        for slot in SLOTS {
            if let Some(i) = section.headers.get(slot) {
                kinds.entry(i).or_insert(MarginalKind::Header);
            }
            if let Some(i) = section.footers.get(slot) {
                kinds.entry(i).or_insert(MarginalKind::Footer);
            }
        }
    }
    let mut parts = Vec::new();
    let mut rids: HashMap<usize, String> = HashMap::new();
    for (i, story) in document.marginals.iter().enumerate() {
        if story.part.is_some() || i < original.marginals.len() {
            continue;
        }
        let Some(&kind) = kinds.get(&i) else {
            continue;
        };
        let (stem, rel) = match kind {
            MarginalKind::Header => ("header", "header"),
            MarginalKind::Footer => ("footer", "footer"),
        };
        let name = free_name(&mut saver.media.taken, &dir, stem, true);
        let target = name.rsplit('/').next().unwrap_or(&name).to_owned();
        rids.insert(i, saver.rels.add(rel, &target, false));
        parts.push(NewPart {
            marginal: i,
            name,
            kind,
        });
    }
    // The reference goes into the section that has the story and whose
    // predecessor does not: the later sections follow it.
    let (w, r) = (saver.w.clone(), saver.r.clone());
    for (n, (_, section)) in sections.iter().enumerate() {
        // A body with no section properties at all has the last section
        // made up by the reader; its references go into a `sectPr` that is
        // written at the end of the body.
        let key = match &section.source {
            Some(range) => range_key(range),
            None if n + 1 == sections.len() => NO_SECTION,
            None => continue,
        };
        let mut xml = String::new();
        for (kind, slots) in [
            (MarginalKind::Header, &section.headers),
            (MarginalKind::Footer, &section.footers),
        ] {
            for slot in SLOTS {
                let Some(index) = slots.get(slot) else {
                    continue;
                };
                let Some(rid) = rids.get(&index) else {
                    continue;
                };
                let before = n.checked_sub(1).and_then(|p| {
                    let (_, earlier): &(usize, &std::sync::Arc<Section>) = &sections[p];
                    match kind {
                        MarginalKind::Header => earlier.headers.get(slot),
                        MarginalKind::Footer => earlier.footers.get(slot),
                    }
                });
                if before == Some(index) {
                    continue;
                }
                let element = match kind {
                    MarginalKind::Header => "headerReference",
                    MarginalKind::Footer => "footerReference",
                };
                xml.push_str(&format!(
                    "<{w}{element} {w}type=\"{}\" {r}id=\"{rid}\"/>",
                    slot_name(slot)
                ));
            }
        }
        if !xml.is_empty() {
            saver.new_refs.insert(key, xml);
        }
    }
    let footnotes = (original.footnotes.part.is_none() && !document.footnotes.notes.is_empty())
        .then(|| {
            let name = free_name(&mut saver.media.taken, &dir, "footnotes", false);
            let target = name.rsplit('/').next().unwrap_or(&name).to_owned();
            saver.rels.add("footnotes", &target, false);
            name
        });
    Plan { parts, footnotes }
}

/// Put `refs` into the `sectPr` element `xml` as its first children.
pub(super) fn with_references(xml: &str, refs: &str) -> String {
    let (open, _) = inner(xml);
    if xml[..open].ends_with("/>") {
        let qname = qualified_name(xml).to_owned();
        let start = xml[..open - 2].trim_end();
        return format!("{start}>{refs}</{qname}>");
    }
    // After the references there are, so that headers and footers keep the
    // order in which a reader finds them; before everything else.
    let at = children(xml)
        .iter()
        .filter(|c| matches!(c.name.as_str(), "headerReference" | "footerReference"))
        .map(|c| c.end)
        .max()
        .unwrap_or(open);
    format!("{}{refs}{}", &xml[..at], &xml[at..])
}

/// What the whole save needs to hand on between parts.
struct Ctx<'a> {
    package: &'a mut Package,
    main: &'a str,
    original: &'a Document,
    document: &'a Document,
    carried: Carried,
    replaced: &'a mut HashMap<String, Vec<u8>>,
    added: &'a mut Vec<(String, Vec<u8>)>,
    types_edit: &'a mut Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write(
    package: &mut Package,
    main: &str,
    original: &Document,
    document: &Document,
    plan: &Plan,
    carried: Carried,
    replaced: &mut HashMap<String, Vec<u8>>,
    added: &mut Vec<(String, Vec<u8>)>,
    types_edit: &mut Vec<String>,
) -> Result<()> {
    let mut cx = Ctx {
        package,
        main,
        original,
        document,
        carried,
        replaced,
        added,
        types_edit,
    };
    // Headers and footers that were read.
    for (i, model) in document.marginals.iter().enumerate() {
        let Some(orig) = original.marginals.get(i) else {
            continue;
        };
        let Some(part) = orig.part.as_deref() else {
            continue;
        };
        if same_blocks(&model.blocks, &orig.blocks) || !cx.package.has(part) {
            continue;
        }
        cx.splice_story(part, &orig.blocks, &model.blocks)?;
    }
    // Footnotes and endnotes that were read.
    for endnotes in [false, true] {
        let (orig, model) = if endnotes {
            (&original.endnotes, &document.endnotes)
        } else {
            (&original.footnotes, &document.footnotes)
        };
        let Some(part) = orig.part.as_deref() else {
            continue;
        };
        if !cx.package.has(part) || same_notes(&orig.notes, &model.notes) {
            continue;
        }
        cx.splice_notes(part, endnotes, &orig.notes, &model.notes)?;
    }
    // Made since reading.
    for part in &plan.parts {
        let story = &document.marginals[part.marginal];
        cx.new_story(part, story)?;
    }
    if let Some(name) = &plan.footnotes {
        cx.new_footnotes(name)?;
    }
    Ok(())
}

fn same_notes(a: &[Note], b: &[Note]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.id == y.id && same_blocks(&x.blocks, &y.blocks))
}

impl Ctx<'_> {
    fn read_text(&mut self, part: &str) -> Result<String> {
        let bytes = self.package.read(part)?;
        String::from_utf8(bytes).map_err(|_| Error::broken(part, "not UTF-8 text"))
    }

    /// The relationships text of a part (empty if it has none).
    fn rels_of(&mut self, part: &str) -> Result<String> {
        let rels = rels_name(part);
        Ok(if self.package.has(&rels) {
            String::from_utf8_lossy(&self.package.read(&rels)?).into_owned()
        } else {
            super::EMPTY_RELS.to_owned()
        })
    }

    /// A saver for `text` with what is shared taken from the others.
    fn saver<'t>(&self, text: &'t str, orig_blocks: &'t [Block], rels: String) -> Saver<'t>
    where
        Self: 't,
    {
        let mut saver = Saver::new(text, self.original, orig_blocks, self.document, rels);
        saver.media = Media::new(self.main, self.carried.taken.clone());
        saver.next_docpr = saver.next_docpr.max(self.carried.docpr);
        saver
    }

    /// Take over what a saver made: its pictures, its relationships, and
    /// what must stay unique.
    fn finish(
        &mut self,
        saver: &Saver<'_>,
        part: &str,
        text: Option<String>,
        new_part: bool,
    ) -> Result<()> {
        self.carried.taken = saver.media.taken.clone();
        self.carried.docpr = saver.next_docpr;
        collect_media(self.package, &saver.media, self.added, self.types_edit)?;
        let rels = rels_name(part);
        if saver.rels.changed {
            let bytes = saver.rels.text.clone().into_bytes();
            if self.package.has(&rels) {
                self.replaced.insert(rels.to_ascii_lowercase(), bytes);
            } else {
                self.added.push((rels, bytes));
            }
        }
        if let Some(text) = text {
            if new_part {
                self.added.push((part.to_owned(), text.into_bytes()));
            } else {
                self.replaced
                    .insert(part.to_ascii_lowercase(), text.into_bytes());
            }
        }
        Ok(())
    }

    /// A header or footer part that was read and has changed.
    fn splice_story(&mut self, part: &str, orig: &[Block], model: &[Block]) -> Result<()> {
        let text = self.read_text(part)?;
        let rels = self.rels_of(part)?;
        let mut saver = self.saver(&text, orig, rels);
        let new_text = saver.story_text(orig, model);
        let changed = (new_text != text).then_some(new_text);
        self.finish(&saver, part, changed, false)
    }

    /// A header or footer made since reading.
    fn new_story(&mut self, part: &NewPart, story: &Story) -> Result<()> {
        let root = match part.kind {
            MarginalKind::Header => "hdr",
            MarginalKind::Footer => "ftr",
        };
        let skeleton = format!(
            "{DECL}<w:{root} xmlns:w=\"{W_NS}\" xmlns:r=\"{}\"></w:{root}>",
            super::REL_NS
        );
        let mut saver = self.saver(&skeleton, &[], super::EMPTY_RELS.to_owned());
        let text = saver.story_text(&[], &story.blocks);
        let override_type = match part.kind {
            MarginalKind::Header => "header",
            MarginalKind::Footer => "footer",
        };
        self.types_edit.push(format!(
            "<Override PartName=\"/{}\" ContentType=\"{CT}.{override_type}+xml\"/>",
            escape_attribute(&part.name)
        ));
        self.finish(&saver, &part.name, Some(text), true)
    }

    /// The footnotes part of a file that had none.
    fn new_footnotes(&mut self, name: &str) -> Result<()> {
        let separator = |kind: &str, id: i32, element: &str| {
            format!(
                "<w:footnote w:type=\"{kind}\" w:id=\"{id}\"><w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:r><w:{element}/></w:r></w:p></w:footnote>"
            )
        };
        let skeleton = format!(
            "{DECL}<w:footnotes xmlns:w=\"{W_NS}\" xmlns:r=\"{}\">{}{}</w:footnotes>",
            super::REL_NS,
            separator("separator", -1, "separator"),
            separator("continuationSeparator", 0, "continuationSeparator"),
        );
        let mut saver = self.saver(&skeleton, &[], super::EMPTY_RELS.to_owned());
        let text = saver.notes_text(false, &[], &self.document.footnotes.notes);
        self.types_edit.push(format!(
            "<Override PartName=\"/{}\" ContentType=\"{CT}.footnotes+xml\"/>",
            escape_attribute(name)
        ));
        self.finish(&saver, name, Some(text), true)
    }

    /// A footnotes or endnotes part that was read and has changed.
    fn splice_notes(
        &mut self,
        part: &str,
        endnotes: bool,
        orig: &[Note],
        model: &[Note],
    ) -> Result<()> {
        let text = self.read_text(part)?;
        let rels = self.rels_of(part)?;
        let blocks: Vec<Block> = orig.iter().flat_map(|n| n.blocks.iter().cloned()).collect();
        let mut saver = self.saver(&text, &blocks, rels);
        let new_text = saver.notes_text(endnotes, orig, model);
        let changed = (new_text != text).then_some(new_text);
        self.finish(&saver, part, changed, false)
    }
}

impl Saver<'_> {
    /// The start of the root element of the part, its inside, and whether
    /// it is written `<x/>`.
    fn root_region(&self) -> (usize, Range<usize>, bool) {
        let text = self.text;
        let tag = root_tag(text);
        let start = tag.as_ptr() as usize - text.as_ptr() as usize;
        let (open, close) = inner(&text[start..]);
        let empty = open == close && text[start..start + open].ends_with("/>");
        (start, start + open..start + close, empty)
    }

    /// The text of a header or footer part with its blocks `orig` (as read
    /// from it) now `model`.
    pub(super) fn story_text(&mut self, orig: &[Block], model: &[Block]) -> String {
        let text = self.text;
        let (start, region, empty) = self.root_region();
        let out = if empty {
            // `<w:hdr/>`: it needs an end tag to hold anything.
            let qname = qualified_name(&text[start..]).to_owned();
            let open = &text[..region.start - 2].trim_end().to_owned();
            let blocks = self.container(region.end..region.end, orig, model);
            format!("{open}>{blocks}</{qname}>{}", &text[region.end..])
        } else {
            let blocks = self.container(region.clone(), orig, model);
            format!("{}{blocks}{}", &text[..region.start], &text[region.end..])
        };
        self.with_namespaces(out)
    }

    /// The namespaces the blocks written here need declared on the root.
    fn with_namespaces(&self, mut out: String) -> String {
        if self.needs_rel_namespace && self.rels.changed {
            out = add_namespace(&out, "r", super::REL_NS);
        }
        if self.wrote_picture {
            for (prefix, uri) in &self.undeclared {
                out = add_namespace(&out, prefix, uri);
            }
        }
        out
    }

    /// The text of a footnotes (or endnotes) part with its notes `orig` (as
    /// read from it) now `model`: changed notes spliced, gone ones dropped,
    /// new ones added after the others. The separators stay.
    pub(super) fn notes_text(&mut self, endnotes: bool, orig: &[Note], model: &[Note]) -> String {
        let text = self.text;
        let (start, region, empty) = self.root_region();
        let local = if endnotes { "endnote" } else { "footnote" };
        let root = &text[start..];
        let originals: HashMap<i32, &Note> = orig.iter().map(|n| (n.id, n)).collect();
        let models: HashMap<i32, &Note> = model.iter().map(|n| (n.id, n)).collect();
        let mut out = String::new();
        let mut at = 0;
        if !empty {
            for kid in children(root) {
                if kid.name != local {
                    continue;
                }
                let (from, to) = (start + kid.start, start + kid.end);
                let xml = &text[from..to];
                let id_name = format!("{}id", self.w);
                let Some(id) =
                    attribute(xml, &id_name).and_then(|id| id.trim().parse::<i32>().ok())
                else {
                    continue;
                };
                // Separators and the like are not notes.
                let Some(&was) = originals.get(&id) else {
                    continue;
                };
                match models.get(&id) {
                    None => {
                        out.push_str(&text[at..from]);
                        at = to;
                    }
                    Some(now) => {
                        self.in_note = Some((endnotes, id));
                        let (open, close) = inner(xml);
                        if open == close {
                            out.push_str(&text[at..from]);
                            out.push_str(&self.note_element(endnotes, now));
                            at = to;
                        } else {
                            let inside = from + open..from + close;
                            let blocks = self.container(inside.clone(), &was.blocks, &now.blocks);
                            out.push_str(&text[at..inside.start]);
                            out.push_str(&blocks);
                            at = inside.end;
                        }
                    }
                }
            }
        }
        self.in_note = None;
        let added: Vec<&Note> = model
            .iter()
            .filter(|n| !originals.contains_key(&n.id))
            .collect();
        let mut tail = String::new();
        for note in added {
            tail.push_str(&self.note_element(endnotes, note));
        }
        let out = if empty {
            let qname = qualified_name(root).to_owned();
            let open = text[..region.start - 2].trim_end();
            format!("{open}>{tail}</{qname}>{}", &text[region.end..])
        } else {
            out.push_str(&text[at..region.end]);
            out.push_str(&tail);
            out.push_str(&text[region.end..]);
            out
        };
        self.with_namespaces(out)
    }

    /// A note written from the model.
    fn note_element(&mut self, endnotes: bool, note: &Note) -> String {
        let w = self.w.clone();
        let local = if endnotes { "endnote" } else { "footnote" };
        self.in_note = Some((endnotes, note.id));
        let mut out = format!("<{w}{local} {w}id=\"{}\">", note.id);
        for block in &note.blocks {
            out.push_str(&self.block_elsewhere(block));
        }
        out.push_str(&format!("</{w}{local}>"));
        self.in_note = None;
        out
    }
}
