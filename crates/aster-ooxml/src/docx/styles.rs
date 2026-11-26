//! `styles.xml`: document defaults, paragraph, character, table and
//! numbering styles, and what a paragraph's layers add up to.

use std::collections::HashMap;
use std::sync::Arc;

use aster_writer::{Border, Rgba, StyleKind, StyleSheet, Twips};

use super::dom::Node;
use super::props::{
    Env, Len, ParaPartial, RunProps, Sides, finalize_para, finalize_run, merge_sides, parse_ppr,
    parse_rpr, shading_of,
};
use super::table::{TblBorders, parse_cell_margins, parse_tbl_borders, parse_tc_borders};

/// How far a chain of `basedOn` is followed.
const MAX_CHAIN: usize = 32;

/// The parts of a table style that apply to a cell in some position.
#[derive(Clone, Debug, Default)]
pub struct CondFormat {
    pub para: ParaPartial,
    pub run: RunProps,
    pub borders: TblBorders,
    pub cell_borders: Sides,
    pub margins: [Option<Twips>; 4],
    pub shading: Option<Option<Rgba>>,
}

impl CondFormat {
    pub fn merge(&mut self, upper: &CondFormat) {
        self.para.merge(&upper.para);
        self.run.merge(&upper.run);
        self.borders.merge(&upper.borders);
        merge_sides(&mut self.cell_borders, &upper.cell_borders);
        for (l, u) in self.margins.iter_mut().zip(&upper.margins) {
            if u.is_some() {
                *l = *u;
            }
        }
        if upper.shading.is_some() {
            self.shading = upper.shading;
        }
    }
}

/// Conditional formats, in the order they are applied (later wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Band1Vert,
    Band2Vert,
    Band1Horz,
    Band2Horz,
    LastCol,
    FirstCol,
    LastRow,
    FirstRow,
    NwCell,
    NeCell,
    SwCell,
    SeCell,
}

impl Cond {
    const ALL: [Cond; 12] = [
        Cond::Band1Vert,
        Cond::Band2Vert,
        Cond::Band1Horz,
        Cond::Band2Horz,
        Cond::LastCol,
        Cond::FirstCol,
        Cond::LastRow,
        Cond::FirstRow,
        Cond::NwCell,
        Cond::NeCell,
        Cond::SwCell,
        Cond::SeCell,
    ];

    pub fn bit(self) -> u16 {
        1 << (self as u16)
    }

    fn named(name: &str) -> Option<Option<Cond>> {
        Some(Some(match name {
            "wholeTable" => return Some(None),
            "band1Vert" => Cond::Band1Vert,
            "band2Vert" => Cond::Band2Vert,
            "band1Horz" => Cond::Band1Horz,
            "band2Horz" => Cond::Band2Horz,
            "lastCol" => Cond::LastCol,
            "firstCol" => Cond::FirstCol,
            "lastRow" => Cond::LastRow,
            "firstRow" => Cond::FirstRow,
            "nwCell" => Cond::NwCell,
            "neCell" => Cond::NeCell,
            "swCell" => Cond::SwCell,
            "seCell" => Cond::SeCell,
            _ => return None,
        }))
    }
}

#[derive(Clone, Debug, Default)]
pub struct TableStyleRaw {
    pub whole: CondFormat,
    pub conds: Vec<(Cond, CondFormat)>,
}

pub struct RawStyle {
    pub id: Arc<str>,
    pub name: String,
    pub kind: StyleKind,
    pub based_on: Option<Arc<str>>,
    pub next: Option<Arc<str>>,
    pub default: bool,
    pub hidden: bool,
    pub quick: bool,
    pub priority: u32,
    pub para: ParaPartial,
    pub run: RunProps,
    pub table: Option<TableStyleRaw>,
}

/// A style's own layers and those of the styles it is based on, merged;
/// the document defaults are not in it.
#[derive(Clone, Debug, Default)]
pub struct Chain {
    pub para: ParaPartial,
    pub run: RunProps,
}

#[derive(Default)]
pub struct Styles {
    pub raw: Vec<RawStyle>,
    index: HashMap<Arc<str>, usize>,
    pub defaults_run: RunProps,
    pub defaults_para: ParaPartial,
    pub default_para_style: Option<usize>,
    chains: Vec<Chain>,
}

/// What the list a paragraph is on gives it: `Some(Some((left, first)))`
/// for a level that sets indents, `Some(None)` for one that does not,
/// `None` when there is no such list.
pub type ListLookup<'a> = &'a dyn Fn(u32, u8) -> Option<Option<(Twips, Twips)>>;

impl Styles {
    /// What Word has with no `styles.xml`.
    pub fn builtin(env: &Env) -> Styles {
        let mut styles = Styles::default();
        styles.push(RawStyle {
            id: env.intern("Normal"),
            name: "Normal".to_owned(),
            kind: StyleKind::Paragraph,
            based_on: None,
            next: None,
            default: true,
            hidden: false,
            quick: true,
            priority: 0,
            para: ParaPartial::default(),
            run: RunProps::default(),
            table: None,
        });
        styles.finish();
        styles
    }

    fn push(&mut self, style: RawStyle) {
        self.index.entry(style.id.clone()).or_insert(self.raw.len());
        self.raw.push(style);
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn parse(root: &Node<'_>, env: &Env) -> Styles {
        let mut styles = Styles::default();
        if let Some(defaults) = root.child("docDefaults") {
            if let Some(rpr) = defaults.child("rPrDefault").and_then(|d| d.child("rPr")) {
                styles.defaults_run = parse_rpr(rpr, env);
            }
            if let Some(ppr) = defaults.child("pPrDefault").and_then(|d| d.child("pPr")) {
                styles.defaults_para = parse_ppr(ppr, env).props;
            }
        }
        for node in root.each("style") {
            let Some(id) = node.attr("styleId") else {
                continue;
            };
            let kind = match node.attr("type").as_deref() {
                Some("character") => StyleKind::Character,
                Some("table") => StyleKind::Table,
                Some("numbering") => StyleKind::Numbering,
                _ => StyleKind::Paragraph,
            };
            let mut style = RawStyle {
                id: env.intern(&id),
                name: node
                    .child("name")
                    .and_then(Node::val)
                    .unwrap_or_else(|| id.clone()),
                kind,
                based_on: node.child("basedOn").and_then(Node::val).map(|v| env.intern(&v)),
                next: node.child("next").and_then(Node::val).map(|v| env.intern(&v)),
                default: node.attr("default").is_some_and(|v| matches!(v.trim(), "1" | "true" | "on")),
                hidden: node.toggle("hidden").unwrap_or(false)
                    || node.toggle("semiHidden").unwrap_or(false),
                quick: node.toggle("qFormat").unwrap_or(false),
                priority: node
                    .child("uiPriority")
                    .and_then(|n| n.int("val"))
                    .map_or(99, |v| v.clamp(0, 9_999) as u32),
                para: ParaPartial::default(),
                run: RunProps::default(),
                table: None,
            };
            if let Some(ppr) = node.child("pPr") {
                style.para = parse_ppr(ppr, env).props;
            }
            if let Some(rpr) = node.child("rPr") {
                style.run = parse_rpr(rpr, env);
            }
            if kind == StyleKind::Table {
                style.table = Some(parse_table_style(node, env));
            }
            // A style that sets its own `link` is also a style of the other
            // kind; the model keeps one.
            styles.push(style);
        }
        styles.finish();
        styles
    }

    fn finish(&mut self) {
        self.default_para_style = self
            .raw
            .iter()
            .position(|s| s.default && s.kind == StyleKind::Paragraph);
        self.chains = (0..self.raw.len()).map(|i| self.build_chain(i)).collect();
    }

    /// Indexes of the styles `start` is based on, root first, `start` last.
    pub fn chain_indexes(&self, start: usize) -> Vec<usize> {
        let mut out = vec![start];
        let mut at = start;
        while out.len() < MAX_CHAIN {
            let Some(parent) = self.raw[at].based_on.as_deref().and_then(|id| self.index_of(id))
            else {
                break;
            };
            if out.contains(&parent) {
                break;
            }
            out.push(parent);
            at = parent;
        }
        out.reverse();
        out
    }

    fn build_chain(&self, start: usize) -> Chain {
        let mut chain = Chain::default();
        for at in self.chain_indexes(start) {
            chain.para.merge(&self.raw[at].para);
            chain.run.merge(&self.raw[at].run);
        }
        chain
    }

    /// The chain of the paragraph style `id`, of the default paragraph
    /// style when `id` is `None` or names none, or empty.
    pub fn paragraph_chain(&self, id: Option<&str>) -> Option<(usize, &Chain)> {
        let found = id
            .and_then(|id| self.index_of(id))
            .filter(|&i| self.raw[i].kind == StyleKind::Paragraph)
            .or(self.default_para_style)?;
        Some((found, &self.chains[found]))
    }

    /// The run properties of the character style `id`.
    pub fn character_chain(&self, id: &str) -> Option<&RunProps> {
        let at = self.index_of(id)?;
        Some(&self.chains[at].run)
    }

    pub fn chain(&self, index: usize) -> &Chain {
        &self.chains[index]
    }

    /// A table style's formats for the cell conditions in `mask`, the
    /// styles it is based on first.
    pub fn table_format(&self, style: usize, mask: u16) -> CondFormat {
        let mut out = CondFormat::default();
        for at in self.chain_indexes(style) {
            let Some(table) = &self.raw[at].table else {
                continue;
            };
            out.merge(&table.whole);
            for cond in Cond::ALL {
                if mask & cond.bit() == 0 {
                    continue;
                }
                for (c, format) in &table.conds {
                    if *c == cond {
                        out.merge(format);
                    }
                }
            }
        }
        out
    }

    /// The model's style sheet.
    pub fn sheet(&self, lists: ListLookup<'_>) -> StyleSheet {
        let defaults = finalize_run(&[&self.defaults_run]);
        let default_para = finalize_para(&self.defaults_para, defaults.size, 240);
        let mut styles = Vec::with_capacity(self.raw.len());
        for (i, raw) in self.raw.iter().enumerate() {
            let chain = &self.chains[i];
            let run = finalize_run(&[&self.defaults_run, &chain.run]);
            let mut partial = combine_para(&self.defaults_para, &chain.para, None, lists);
            if raw.kind == StyleKind::Paragraph || raw.kind == StyleKind::Table {
                partial.style = Some(raw.id.clone());
            } else {
                partial.style = None;
            }
            let mut para = finalize_para(&partial, run.size, 240);
            if raw.kind != StyleKind::Paragraph {
                para.style = None;
            }
            styles.push(aster_writer::Style {
                id: raw.id.clone(),
                name: raw.name.clone(),
                kind: raw.kind,
                based_on: raw.based_on.clone(),
                next: raw.next.clone(),
                para,
                run,
                hidden: raw.hidden,
                quick: raw.quick,
                priority: raw.priority,
            });
        }
        StyleSheet {
            styles,
            default_para,
            default_run: defaults,
            default_style: self.default_para_style.map(|i| self.raw[i].id.clone()),
        }
    }
}

/// The paragraph layers added up: `lower` (the document defaults, a table
/// style), the style chain, the list level's indents, and the paragraph's
/// own. The list level's indents sit below direct `ind`; where the list
/// comes from the style, the style's own `ind` wins over them.
pub fn combine_para(
    lower: &ParaPartial,
    chain: &ParaPartial,
    direct: Option<&ParaPartial>,
    lists: ListLookup<'_>,
) -> ParaPartial {
    let direct_list = direct.is_some_and(|d| d.num_id.is_some() || d.ilvl.is_some());
    let num_id = direct
        .and_then(|d| d.num_id)
        .or(chain.num_id)
        .or(lower.num_id)
        .unwrap_or(0);
    let ilvl = direct
        .and_then(|d| d.ilvl)
        .or(chain.ilvl)
        .or(lower.ilvl)
        .unwrap_or(0);
    let level = if num_id > 0 {
        lists(num_id, ilvl.min(8)).flatten()
    } else {
        None
    };
    let indents = level.map(|(left, first)| ParaPartial {
        left: Some(Len {
            twips: Some(left),
            chars: None,
        }),
        first: Some(Len {
            twips: Some(first),
            chars: None,
        }),
        ..ParaPartial::default()
    });
    let mut p = lower.clone();
    if let Some(indents) = indents.as_ref().filter(|_| !direct_list) {
        p.merge(indents);
    }
    p.merge(chain);
    if let Some(indents) = indents.as_ref().filter(|_| direct_list) {
        p.merge(indents);
    }
    if let Some(direct) = direct {
        p.merge(direct);
    }
    p
}

fn parse_cond_format(node: &Node<'_>, env: &Env) -> CondFormat {
    let mut f = CondFormat::default();
    if let Some(ppr) = node.child("pPr") {
        f.para = parse_ppr(ppr, env).props;
    }
    if let Some(rpr) = node.child("rPr") {
        f.run = parse_rpr(rpr, env);
    }
    if let Some(tbl) = node.child("tblPr") {
        if let Some(b) = tbl.child("tblBorders") {
            f.borders = parse_tbl_borders(b, env);
        }
        if let Some(m) = tbl.child("tblCellMar") {
            f.margins = parse_cell_margins(m);
        }
    }
    if let Some(tc) = node.child("tcPr") {
        if let Some(b) = tc.child("tcBorders") {
            f.cell_borders = parse_tc_borders(b, env);
        }
        if let Some(s) = tc.child("shd") {
            f.shading = shading_of(s, env);
        }
        if let Some(m) = tc.child("tcMar") {
            for (slot, value) in f.margins.iter_mut().zip(parse_cell_margins(m)) {
                if value.is_some() {
                    *slot = value;
                }
            }
        }
    }
    f
}

fn parse_table_style(node: &Node<'_>, env: &Env) -> TableStyleRaw {
    let mut out = TableStyleRaw {
        whole: parse_cond_format(node, env),
        conds: Vec::new(),
    };
    for pr in node.each("tblStylePr") {
        let Some(kind) = pr.attr("type").as_deref().and_then(Cond::named) else {
            continue;
        };
        let format = parse_cond_format(pr, env);
        match kind {
            None => out.whole.merge(&format),
            Some(cond) => out.conds.push((cond, format)),
        }
    }
    out
}

/// Sides as the model has them: left, top, right, bottom.
pub fn final_sides(sides: &Sides) -> [Option<Border>; 4] {
    [
        sides[0].flatten(),
        sides[1].flatten(),
        sides[2].flatten(),
        sides[3].flatten(),
    ]
}
