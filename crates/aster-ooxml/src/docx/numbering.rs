//! `numbering.xml`: abstract definitions, the lists that instantiate them,
//! and their overrides.

use std::collections::HashMap;

use aster_writer::{Align, List, ListLevel, NumberFormat, Numbering, RunPatch, Suffix, Twips};

use super::dom::Node;
use super::props::{Env, parse_ppr, parse_rpr};
use super::styles::Styles;
use super::symbol;

/// Levels a list has at most.
const MAX_LEVELS: usize = 9;
/// Links from one definition to another (`numStyleLink`) followed.
const MAX_LINKS: usize = 8;

/// The model's lists, and what the reader needs to place paragraphs on
/// them.
#[derive(Default)]
pub struct Lists {
    pub numbering: Numbering,
    /// `numId` to its place in `numbering.lists`.
    index: HashMap<u32, usize>,
    /// The indents of levels that set any, as left and first line.
    indents: HashMap<(u32, u8), (Twips, Twips)>,
}

impl Lists {
    /// See [`super::styles::ListLookup`].
    pub fn lookup(&self, id: u32, level: u8) -> Option<Option<(Twips, Twips)>> {
        self.index.get(&id)?;
        Some(self.indents.get(&(id, level)).copied())
    }
}

type LevelDef = (ListLevel, Option<(Twips, Twips)>);

#[derive(Default)]
struct Abstract {
    levels: Vec<Option<LevelDef>>,
    num_style_link: Option<String>,
}

struct Num {
    id: u32,
    abstract_id: u32,
    overrides: Vec<(usize, Option<u32>, Option<LevelDef>)>,
}

/// The number format a `numFmt` value names.
pub fn number_format(name: &str) -> NumberFormat {
    match name {
        "decimal" => NumberFormat::Decimal,
        "decimalZero" => NumberFormat::DecimalZero,
        "lowerLetter" => NumberFormat::LowerLetter,
        "upperLetter" => NumberFormat::UpperLetter,
        "lowerRoman" => NumberFormat::LowerRoman,
        "upperRoman" => NumberFormat::UpperRoman,
        "bullet" => NumberFormat::Bullet,
        "none" => NumberFormat::None,
        "decimalEnclosedCircle" | "decimalEnclosedCircleChinese" | "decimalEnclosedFullstop" => {
            NumberFormat::DecimalEnclosedCircle
        }
        "decimalFullWidth" | "decimalFullWidth2" => NumberFormat::DecimalFullWidth,
        "chineseCounting"
        | "japaneseCounting"
        | "chineseCountingThousand"
        | "taiwaneseCounting"
        | "taiwaneseCountingThousand"
        | "koreanCounting" => NumberFormat::ChineseCounting,
        "chineseLegalSimplified" | "japaneseLegal" | "koreanLegal" => NumberFormat::ChineseLegal,
        "ideographDigital"
        | "japaneseDigitalTenThousand"
        | "taiwaneseDigital"
        | "koreanDigital" => NumberFormat::IdeographDigital,
        "ideographTraditional" => NumberFormat::IdeographTraditional,
        "ideographZodiac" | "ideographZodiacTraditional" => NumberFormat::IdeographZodiac,
        "aiueoFullWidth" | "aiueo" => NumberFormat::Aiueo,
        "irohaFullWidth" | "iroha" => NumberFormat::Iroha,
        "ordinal" => NumberFormat::Ordinal,
        other => NumberFormat::Other(other.to_owned()),
    }
}

fn parse_level(node: &Node<'_>, env: &Env, size: f32) -> LevelDef {
    let mut level = ListLevel {
        format: NumberFormat::Decimal,
        text: String::new(),
        start: 0,
        restart: None,
        suffix: Suffix::Tab,
        align: Align::Left,
        look: RunPatch::default(),
        indent_left: 0,
        first_line: 0,
        legal: false,
    };
    if let Some(v) = node.child("start").and_then(|n| n.int("val")) {
        level.start = v.clamp(0, 1_000_000) as u32;
    }
    if let Some(v) = node.child("numFmt").and_then(Node::val) {
        level.format = number_format(&v);
    }
    let mut text = node
        .child("lvlText")
        .and_then(Node::val)
        .unwrap_or_default();
    level.suffix = match node.child("suff").and_then(Node::val).as_deref() {
        Some("space") => Suffix::Space,
        Some("nothing") => Suffix::Nothing,
        _ => Suffix::Tab,
    };
    level.align = match node.child("lvlJc").and_then(Node::val).as_deref() {
        Some("center") => Align::Center,
        Some("right" | "end") => Align::Right,
        _ => Align::Left,
    };
    level.legal = node.toggle("isLgl").unwrap_or(false);
    let mut indent = None;
    if let Some(ppr) = node.child("pPr") {
        let p = parse_ppr(ppr, env).props;
        if p.left.is_some() || p.first.is_some() {
            let left = p.left.map_or(0, |l| l.resolve(size));
            let first = p.first.map_or(0, |l| l.resolve(size));
            level.indent_left = left;
            level.first_line = first;
            indent = Some((left, first));
        }
    }
    let mut symbol_font = None;
    if let Some(rpr) = node.child("rPr") {
        let p = parse_rpr(rpr, env);
        symbol_font = p.font.clone().filter(|f| symbol::is_symbol_font(f));
        level.look = RunPatch {
            font: p.font.clone().map(Some),
            east_asian_font: p.east_asian_font.clone().map(Some),
            size: p.size,
            bold: p.bold,
            italic: p.italic,
            underline: p.underline,
            strike: p.strike,
            color: p.color,
            highlight: p.highlight,
            position: p.position,
        };
    }
    if level.format == NumberFormat::Bullet
        && let Some(font) = symbol_font
    {
        // The bullet is a code of the symbol font; show it as Unicode in
        // any font.
        let mapped: String = text
            .chars()
            .map(|c| symbol::to_unicode(&font, c).unwrap_or(c))
            .collect();
        let known = text.chars().all(|c| symbol::to_unicode(&font, c).is_some());
        text = mapped;
        if known {
            level.look.font = None;
        }
    }
    level.text = text;
    (level, indent)
}

/// A list's levels through `numStyleLink`s.
fn levels_of(
    id: u32,
    abstracts: &HashMap<u32, Abstract>,
    nums: &HashMap<u32, usize>,
    num_list: &[Num],
    styles: &Styles,
    depth: usize,
) -> Vec<Option<LevelDef>> {
    let Some(def) = abstracts.get(&id) else {
        return Vec::new();
    };
    if def.levels.iter().any(Option::is_some) || depth >= MAX_LINKS {
        return def.levels.clone();
    }
    let Some(linked) = def
        .num_style_link
        .as_deref()
        .and_then(|style| styles.index_of(style))
        .and_then(|at| styles.chain(at).para.num_id)
        .and_then(|num| nums.get(&num))
        .map(|&at| num_list[at].abstract_id)
    else {
        return def.levels.clone();
    };
    levels_of(linked, abstracts, nums, num_list, styles, depth + 1)
}

pub fn read_lists(root: &Node<'_>, styles: &Styles, env: &Env, size: f32) -> Lists {
    let mut abstracts: HashMap<u32, Abstract> = HashMap::new();
    let mut num_list: Vec<Num> = Vec::new();
    for node in &root.children {
        if node.is("abstractNum") {
            let Some(id) = node.int("abstractNumId") else {
                continue;
            };
            let mut def = Abstract {
                num_style_link: node.child("numStyleLink").and_then(Node::val),
                ..Abstract::default()
            };
            for lvl in node.each("lvl") {
                let at = lvl.int("ilvl").unwrap_or(def.levels.len() as i64);
                if !(0..MAX_LEVELS as i64).contains(&at) {
                    continue;
                }
                let at = at as usize;
                if def.levels.len() <= at {
                    def.levels.resize(at + 1, None);
                }
                def.levels[at] = Some(parse_level(lvl, env, size));
            }
            abstracts
                .entry(id.clamp(0, i64::from(u32::MAX)) as u32)
                .or_insert(def);
        } else if node.is("num") {
            let (Some(id), Some(abs)) = (
                node.int("numId"),
                node.child("abstractNumId").and_then(|n| n.int("val")),
            ) else {
                continue;
            };
            let mut num = Num {
                id: id.clamp(0, i64::from(u32::MAX)) as u32,
                abstract_id: abs.clamp(0, i64::from(u32::MAX)) as u32,
                overrides: Vec::new(),
            };
            for o in node.each("lvlOverride") {
                let Some(at) = o.int("ilvl").filter(|a| (0..MAX_LEVELS as i64).contains(a)) else {
                    continue;
                };
                let start = o
                    .child("startOverride")
                    .and_then(|n| n.int("val"))
                    .map(|v| v.clamp(0, 1_000_000) as u32);
                let whole = o.child("lvl").map(|l| parse_level(l, env, size));
                num.overrides.push((at as usize, start, whole));
            }
            num_list.push(num);
        }
    }
    let mut nums: HashMap<u32, usize> = HashMap::new();
    for (i, num) in num_list.iter().enumerate() {
        nums.entry(num.id).or_insert(i);
    }

    let mut lists = Lists::default();
    for (i, num) in num_list.iter().enumerate() {
        if nums.get(&num.id) != Some(&i) {
            continue;
        }
        let mut levels = levels_of(num.abstract_id, &abstracts, &nums, &num_list, styles, 0);
        for (at, start, whole) in &num.overrides {
            if levels.len() <= *at {
                levels.resize(*at + 1, None);
            }
            if let Some(whole) = whole {
                levels[*at] = Some(whole.clone());
            }
            if let (Some(start), Some((level, _))) = (start, levels[*at].as_mut()) {
                level.restart = Some(*start);
            } else if let Some(start) = start {
                let level = ListLevel {
                    restart: Some(*start),
                    ..ListLevel::default()
                };
                levels[*at] = Some((level, None));
            }
        }
        levels.truncate(MAX_LEVELS);
        let mut out = Vec::with_capacity(levels.len());
        for (at, level) in levels.into_iter().enumerate() {
            match level {
                Some((level, indent)) => {
                    if let Some(indent) = indent {
                        lists.indents.insert((num.id, at as u8), indent);
                    }
                    out.push(level);
                }
                None => out.push(ListLevel::default()),
            }
        }
        lists.index.insert(num.id, lists.numbering.lists.len());
        lists.numbering.lists.push(List {
            id: num.id,
            definition: num.abstract_id,
            levels: out,
        });
    }
    lists
}
