//! DOCX: Word documents into [`aster_writer::Document`].
//!
//! The model is resolved: a paragraph's properties and a run's look hold
//! what document defaults, styles, table styles, list levels and direct
//! formatting add up to, theme fonts and colours included. Every paragraph,
//! run, table, row, cell and section remembers the byte range of its
//! element in the part it came from, so that saving can write back what
//! was not changed as it was.
//!
//! Read: paragraphs and tables (through content controls and the like),
//! runs with their text, tabs, breaks, symbols, pictures, text boxes,
//! note references, ruby, page-number fields and hyperlinks; styles and
//! lists; sections with their headers and footers; footnotes and endnotes;
//! the settings that matter for layout. Kept in place but not shown:
//! bookmarks, comment ranges, field codes, deleted text of tracked changes
//! and every element this version does not know. Not read: macros, math,
//! comments, embedded objects, `altChunk`.

mod body;
mod dom;
mod drawing;
mod numbering;
mod props;
mod section;
mod styles;
mod symbol;
mod table;

use std::collections::HashMap;
use std::sync::Arc;

use aster_writer::{
    Document, Marginals, Note, Notes, PageField, RunLook, Section, Settings, Story, Twips,
};

use self::dom::{Node, flatten};
use self::numbering::{Lists, read_lists};
use self::props::{Env, ParaPartial};
use self::section::grid_pitch;
use self::styles::{CondFormat, Styles};
use crate::drawingml::read_theme;
use crate::error::{Error, Result};
use crate::package::{Kind, Package, rels_name, resolve};
use crate::xml::{Ev, XmlReader};

pub fn read(bytes: Vec<u8>) -> Result<Document> {
    let mut package = Package::open(bytes)?;
    read_package(&mut package)
}

/// A relationship of a part.
struct Rel {
    pub kind: String,
    /// The part it points at, for an internal relationship.
    pub target: Option<String>,
    /// The address, for an external one.
    pub external: Option<String>,
}

/// The state of reading one part.
struct PartState {
    pub rels: HashMap<String, Rel>,
    /// Complex fields open at this point, innermost last.
    pub fields: Vec<Field>,
    pub field_overflow: usize,
    /// The `fldSimple` being read, if it is a page field.
    pub simple: Option<SimpleField>,
    /// The note being read: whether it is an endnote, and its id.
    pub note: Option<(bool, i32)>,
    /// The line pitch of each section's grid, where it has one.
    pub grids: Vec<Option<Twips>>,
    /// The section the next paragraph is in.
    pub section: usize,
    /// Tables and text boxes the reading is inside of.
    pub depth: usize,
    /// What the table style gives the cell being read.
    pub cell: Option<Arc<CondFormat>>,
    /// Section breaks count (the main part only).
    pub sections: bool,
}

impl PartState {
    fn new(rels: HashMap<String, Rel>) -> Self {
        Self {
            rels,
            fields: Vec::new(),
            field_overflow: 0,
            simple: None,
            note: None,
            grids: Vec::new(),
            section: 0,
            depth: 0,
            cell: None,
            sections: false,
        }
    }
}

#[derive(Default)]
struct Field {
    pub instruction: String,
    /// Past the separator: reading the result.
    pub result: bool,
    pub page: Option<PageField>,
    /// The first result run has become the page number.
    pub page_done: bool,
    pub link: Option<Arc<aster_writer::Link>>,
}

struct SimpleField {
    pub field: PageField,
    pub done: bool,
}

/// What a paragraph of some style in some table cell starts from.
struct ParaBase {
    pub look: RunLook,
    /// Document defaults and the table style's.
    pub lower: ParaPartial,
    pub chain: ParaPartial,
}

struct Reader<'p> {
    package: &'p mut Package,
    env: Env,
    styles: Styles,
    lists: Lists,
    images: HashMap<String, Arc<[u8]>>,
    marginals: Vec<Arc<Story>>,
    marginal_index: HashMap<String, usize>,
    /// The headers and footers the last section had.
    inherited: (Marginals, Marginals),
    cell_formats: HashMap<(usize, u16), Arc<CondFormat>>,
    cell_layers: HashMap<(usize, u16), Arc<aster_writer::CellLayer>>,
    bases: HashMap<(usize, usize), Arc<ParaBase>>,
    /// The first thing that went wrong that the file cannot be shown
    /// without.
    error: Option<Error>,
}

/// The relationships of `part`; none if it has no relationships part or
/// that cannot be read.
fn read_rels(package: &mut Package, part: &str) -> HashMap<String, Rel> {
    let name = rels_name(part);
    let mut out = HashMap::new();
    if !package.has(&name) {
        return out;
    }
    let Ok(bytes) = package.read(&name) else {
        return out;
    };
    let mut reader = XmlReader::new(&bytes, &name);
    while let Ok(event) = reader.next() {
        match event {
            Ev::Open(tag) if tag.is("Relationship") => {
                let (Some(id), Some(kind), Some(target)) =
                    (tag.attr("Id"), tag.attr("Type"), tag.attr("Target"))
                else {
                    continue;
                };
                let external = tag
                    .attr("TargetMode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                out.insert(
                    id,
                    Rel {
                        kind: kind.rsplit('/').next().unwrap_or("").to_owned(),
                        target: (!external).then(|| resolve(part, &target)),
                        external: external.then_some(target),
                    },
                );
            }
            Ev::Eof => break,
            _ => {}
        }
    }
    out
}

fn related<'r>(rels: &'r HashMap<String, Rel>, kind: &str) -> Option<&'r str> {
    let mut found: Vec<(&String, &Rel)> = rels.iter().filter(|(_, r)| r.kind == kind).collect();
    found.sort_by(|a, b| a.0.cmp(b.0));
    found.into_iter().find_map(|(_, r)| r.target.as_deref())
}

/// An optional part's bytes: `None` if it is missing or cannot be read.
fn optional(package: &mut Package, name: Option<&str>) -> Option<Vec<u8>> {
    let name = name?;
    if !package.has(name) {
        return None;
    }
    package.read(name).ok()
}

struct Head {
    settings: Settings,
    ea_lang: String,
    lang: Option<String>,
}

fn read_settings(root: &Node<'_>) -> Head {
    let mut settings = Settings::default();
    if let Some(tab) = root.child("defaultTabStop").and_then(|n| n.int("val"))
        && tab > 0
    {
        settings.default_tab = tab.min(31_680) as Twips;
    }
    settings.even_and_odd_headers = root.toggle("evenAndOddHeaders").unwrap_or(false);
    settings.compress_punctuation = root
        .child("characterSpacingControl")
        .and_then(Node::val)
        .is_some_and(|v| v.starts_with("compressPunctuation"));
    let lang = root.child("themeFontLang");
    Head {
        settings,
        ea_lang: lang.and_then(|l| l.attr("eastAsia")).unwrap_or_default(),
        lang: lang.and_then(|l| l.attr("val")),
    }
}

pub fn read_package(package: &mut Package) -> Result<Document> {
    let main = package.expect(Kind::Docx)?;
    let main_rels = read_rels(package, &main);

    let theme = optional(package, related(&main_rels, "theme"))
        .and_then(|bytes| read_theme(&bytes, "theme").ok())
        .unwrap_or_default();
    let settings_bytes = optional(package, related(&main_rels, "settings"));
    let head = settings_bytes
        .as_deref()
        .and_then(|bytes| dom::parse(bytes, "settings").ok())
        .map(|root| read_settings(&root));
    let (mut settings, ea_lang, theme_lang) = match head {
        Some(h) => (h.settings, h.ea_lang, h.lang),
        None => (Settings::default(), String::new(), None),
    };
    let env = Env::new(theme, ea_lang);

    let styles_bytes = optional(package, related(&main_rels, "styles"));
    let styles = styles_bytes
        .as_deref()
        .and_then(|bytes| dom::parse(bytes, "styles").ok())
        .map(|root| Styles::parse(&root, &env))
        .unwrap_or_else(|| Styles::builtin(&env));
    let default_size = props::finalize_run(&[&styles.defaults_run]).size;
    let numbering_bytes = optional(package, related(&main_rels, "numbering"));
    let lists = numbering_bytes
        .as_deref()
        .and_then(|bytes| dom::parse(bytes, "numbering").ok())
        .map(|root| read_lists(&root, &styles, &env, default_size))
        .unwrap_or_default();

    settings.language = styles
        .defaults_run
        .lang
        .clone()
        .or(theme_lang.filter(|l| !l.is_empty()));
    settings.east_asian_language = styles.defaults_run.lang_ea.clone();

    let sheet = styles.sheet(&|id, level| lists.lookup(id, level));
    let mut reader = Reader {
        package,
        env,
        styles,
        lists,
        images: HashMap::new(),
        marginals: Vec::new(),
        marginal_index: HashMap::new(),
        inherited: (Marginals::default(), Marginals::default()),
        cell_formats: HashMap::new(),
        cell_layers: HashMap::new(),
        bases: HashMap::new(),
        error: None,
    };

    let bytes = reader.package.read(&main)?;
    let root = dom::parse(&bytes, &main)?;
    let part_name: Arc<str> = Arc::from(main.as_str());
    let mut part = PartState::new(main_rels);
    part.sections = true;
    let mut blocks = Vec::new();
    let mut last_section = None;
    if let Some(body) = root.child("body") {
        part.grids = scan_grids(body);
        blocks = reader.blocks(&body.children, &mut part);
        if let Some(sect) = body.child("sectPr") {
            last_section = Some(reader.section(sect, &part));
        }
    }
    let last_section = last_section.unwrap_or_else(|| {
        Arc::new(Section {
            page: (12_240, 15_840),
            ..Section::default()
        })
    });

    let footnotes = match related(&part.rels, "footnotes").map(str::to_owned) {
        Some(name) => reader.notes(&name, false),
        None => Notes::default(),
    };
    let endnotes = match related(&part.rels, "endnotes").map(str::to_owned) {
        Some(name) => reader.notes(&name, true),
        None => Notes::default(),
    };

    if let Some(error) = reader.error.take() {
        return Err(error);
    }
    let mut document = Document {
        body: Story {
            blocks,
            part: Some(part_name),
        },
        last_section,
        marginals: reader.marginals,
        footnotes,
        endnotes,
        styles: Arc::new(sheet),
        numbering: Arc::new(reader.lists.numbering),
        settings,
    };
    document.renumber();
    Ok(document)
}

fn is_container(node: &Node<'_>) -> bool {
    matches!(
        node.local(),
        b"sdt" | b"customXml" | b"smartTag" | b"ins" | b"moveTo" | b"dir" | b"bdo"
    )
}

/// The grid pitch of each section of the body, in order, so that
/// paragraphs before the end of a section know it.
fn scan_grids(body: &Node<'_>) -> Vec<Option<Twips>> {
    let mut items = Vec::new();
    flatten(&body.children, &is_container, &mut items);
    let mut out = Vec::new();
    for item in &items {
        if let Some(sect) = item
            .node
            .child("pPr")
            .filter(|_| item.node.is("p"))
            .and_then(|p| p.child("sectPr"))
        {
            out.push(grid_pitch(sect));
        }
    }
    out.push(body.child("sectPr").and_then(grid_pitch));
    out
}

impl Reader<'_> {
    fn fail(&mut self, error: Error) {
        self.error.get_or_insert(error);
    }

    fn part_state(&mut self, name: &str) -> PartState {
        PartState::new(read_rels(self.package, name))
    }

    /// A header or footer part as a story. A part that is missing or
    /// cannot be read is an empty story, so that the section's
    /// references stay as they are.
    fn read_story(&mut self, name: &str) -> Option<Story> {
        let part_name: Arc<str> = Arc::from(name);
        let bytes = match self.package.has(name) {
            true => match self.package.read(name) {
                Ok(bytes) => bytes,
                Err(error @ Error::TooLarge(_)) => {
                    self.fail(error);
                    return None;
                }
                Err(_) => Vec::new(),
            },
            false => return None,
        };
        let mut part = self.part_state(name);
        let blocks = match dom::parse(&bytes, name) {
            Ok(root) => self.blocks(&root.children, &mut part),
            Err(_) => Vec::new(),
        };
        Some(Story {
            blocks,
            part: Some(part_name),
        })
    }

    fn notes(&mut self, name: &str, endnotes: bool) -> Notes {
        let part_name: Arc<str> = Arc::from(name);
        let mut out = Notes {
            notes: Vec::new(),
            part: Some(part_name),
        };
        let Some(bytes) = optional(self.package, Some(name)) else {
            return out;
        };
        let Ok(root) = dom::parse(&bytes, name) else {
            return out;
        };
        let mut part = self.part_state(name);
        for note in root.each(if endnotes { "endnote" } else { "footnote" }) {
            if matches!(
                note.attr("type").as_deref(),
                Some("separator" | "continuationSeparator" | "continuationNotice")
            ) {
                continue;
            }
            let Some(id) = note.int("id") else {
                continue;
            };
            let id = id.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            part.note = Some((endnotes, id));
            part.fields.clear();
            part.field_overflow = 0;
            let blocks = self.blocks(&note.children, &mut part);
            out.notes.push(Note { id, blocks });
        }
        out
    }
}
