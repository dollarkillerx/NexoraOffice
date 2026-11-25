//! Conditional formats: what a rule makes of a cell when it is drawn.
//!
//! The model keeps the rules as the file states them ([`CondFormat`]); the
//! file itself is saved untouched, so nothing here is written back. A
//! [`Conditional`] cache works out, for the cells in the window, which rules
//! apply and what they do ([`Effect`]): a fill, font and border changes
//! (a `dxf`), a colour from a scale, a data bar or an icon.
//!
//! Rules apply in priority order; a property an earlier rule set is not
//! changed by a later one, and `stopIfTrue` ends the walk. Statistics a
//! rule needs over its cells (the average, the nth largest value, how often
//! a value occurs, the lowest and highest value) are worked out once, on
//! first use, from at most [`MAX_SCAN`] cells, and kept until a cell
//! changes: the cache is thrown away then, and refilled only for the cells
//! on screen.
//!
//! Formulas (the `expression` rule, `cellIs` operands, `formula` values of
//! scales) are evaluated by an [`ExprEngine`] over the calculated values.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::address::{CellRange, CellRef};
use crate::format::{self, Format};
use crate::formula::shift;
use crate::model::{Sheet, Value, Workbook};
use crate::structure::{Lines, adjust};
use crate::style::{Borders, Rgba};

/// Most cells one rule reads to work out its statistics.
pub const MAX_SCAN: usize = 250_000;
/// Most cells of one sheet whose effects are kept.
pub const MAX_CACHED: usize = 60_000;
/// Most cells looked at for one window.
pub const MAX_WINDOW: usize = 40_000;
/// Most rules looked at for one window.
pub const MAX_ACTIVE_RULES: usize = 2_000;
/// Most icons in a set.
pub const MAX_STEPS: usize = 8;

/// Changes to a cell's look: a `dxf`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dxf {
    pub font_color: Option<Rgba>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub fill: Option<Rgba>,
    pub borders: Borders,
    /// A number format of its own.
    pub format: Option<Arc<Format>>,
}

impl Dxf {
    /// What `self` has not set is taken from `other`.
    fn fill_from(&mut self, other: &Dxf) {
        self.font_color = self.font_color.or(other.font_color);
        self.bold = self.bold.or(other.bold);
        self.italic = self.italic.or(other.italic);
        self.underline = self.underline.or(other.underline);
        self.strike = self.strike.or(other.strike);
        self.fill = self.fill.or(other.fill);
        self.borders.left = self.borders.left.or(other.borders.left);
        self.borders.right = self.borders.right.or(other.borders.right);
        self.borders.top = self.borders.top.or(other.borders.top);
        self.borders.bottom = self.borders.bottom.or(other.borders.bottom);
        if self.format.is_none() {
            self.format.clone_from(&other.format);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    Less,
    LessEqual,
    Equal,
    NotEqual,
    GreaterEqual,
    Greater,
    Between,
    NotBetween,
}

impl Compare {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "lessThan" => Self::Less,
            "lessThanOrEqual" => Self::LessEqual,
            "equal" => Self::Equal,
            "notEqual" => Self::NotEqual,
            "greaterThanOrEqual" => Self::GreaterEqual,
            "greaterThan" => Self::Greater,
            "between" => Self::Between,
            "notBetween" => Self::NotBetween,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTest {
    Contains,
    NotContains,
    Begins,
    Ends,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfvoKind {
    Num,
    Percent,
    Percentile,
    Min,
    Max,
    Formula,
}

/// A value a scale, bar or icon set is measured against (`<cfvo>`).
#[derive(Clone, Debug, PartialEq)]
pub struct Cfvo {
    pub kind: CfvoKind,
    pub value: String,
    /// An icon's threshold counts when the value is equal to it.
    pub gte: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconFamily {
    Arrows,
    ArrowsGray,
    TrafficLights,
    /// Traffic lights with a rim (`3TrafficLights2`).
    TrafficLightsRimmed,
    /// Four lights, red to black.
    RedToBlack,
    Signs,
    Symbols,
    /// Symbols on a plain background (`3Symbols2`).
    SymbolsPlain,
    Flags,
    Stars,
    Triangles,
    Quarters,
    Boxes,
    Rating,
}

/// An icon set: which pictures, and how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconSet {
    pub family: IconFamily,
    pub count: u8,
}

impl IconSet {
    /// `3Arrows`, `4TrafficLights`, `5Quarters` ...
    pub fn parse(name: &str) -> Option<Self> {
        let count = name.chars().next()?.to_digit(10)? as u8;
        let family = match name.get(1..)? {
            "Arrows" => IconFamily::Arrows,
            "ArrowsGray" => IconFamily::ArrowsGray,
            "TrafficLights1" | "TrafficLights" => IconFamily::TrafficLights,
            "TrafficLights2" => IconFamily::TrafficLightsRimmed,
            "RedToBlack" => IconFamily::RedToBlack,
            "Signs" => IconFamily::Signs,
            "Symbols" => IconFamily::Symbols,
            "Symbols2" => IconFamily::SymbolsPlain,
            "Flags" => IconFamily::Flags,
            "Stars" => IconFamily::Stars,
            "Triangles" => IconFamily::Triangles,
            "Quarters" => IconFamily::Quarters,
            "Boxes" => IconFamily::Boxes,
            "Rating" => IconFamily::Rating,
            _ => return None,
        };
        (3..=5).contains(&count).then_some(Self { family, count })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    CellIs {
        op: Compare,
        first: Arc<str>,
        second: Option<Arc<str>>,
    },
    Expression(Arc<str>),
    Text {
        test: TextTest,
        text: Arc<str>,
    },
    /// Cells that are empty (or only spaces): `containsBlanks`; with
    /// `false`, the ones that are not.
    Blanks(bool),
    /// Cells that are errors (`containsErrors`), or are not.
    Errors(bool),
    /// Values that occur more than once; with `unique`, once.
    Duplicates {
        unique: bool,
    },
    Top {
        rank: u32,
        percent: bool,
        bottom: bool,
    },
    Average {
        above: bool,
        equal: bool,
        std_dev: u32,
    },
    ColorScale(Vec<(Cfvo, Rgba)>),
    DataBar {
        min: Cfvo,
        max: Cfvo,
        color: Rgba,
        /// Shortest and longest bar, percent of the cell's width.
        min_length: u8,
        max_length: u8,
        show_value: bool,
    },
    IconSet {
        set: IconSet,
        steps: Vec<Cfvo>,
        reverse: bool,
        show_value: bool,
    },
    /// A rule of a type not drawn here, by its `type`.
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub priority: i32,
    pub stop_if_true: bool,
    pub kind: Kind,
    pub dxf: Option<Arc<Dxf>>,
}

/// Rules on some ranges (`<conditionalFormatting sqref>`).
#[derive(Clone, Debug, PartialEq)]
pub struct CondFormat {
    pub ranges: Vec<CellRange>,
    pub rules: Vec<Rule>,
}

impl CondFormat {
    /// The top left corner of all its ranges: where relative references in
    /// its formulas are anchored.
    pub fn anchor(&self) -> CellRef {
        let row = self.ranges.iter().map(|r| r.first.row).min().unwrap_or(0);
        let col = self.ranges.iter().map(|r| r.first.col).min().unwrap_or(0);
        CellRef::new(row, col)
    }

    /// Every formula of its rules, changed by `f`.
    pub fn map_formulas(&mut self, mut f: impl FnMut(&str) -> String) {
        let mut cfvo = |value: &mut Cfvo| {
            if value.kind == CfvoKind::Formula {
                value.value = f(&value.value);
            }
        };
        let mut texts: Vec<&mut Arc<str>> = Vec::new();
        for rule in &mut self.rules {
            match &mut rule.kind {
                Kind::CellIs { first, second, .. } => {
                    texts.push(first);
                    texts.extend(second.as_mut());
                }
                Kind::Expression(formula) => texts.push(formula),
                Kind::ColorScale(stops) => stops.iter_mut().for_each(|(v, _)| cfvo(v)),
                Kind::DataBar { min, max, .. } => {
                    cfvo(min);
                    cfvo(max);
                }
                Kind::IconSet { steps, .. } => steps.iter_mut().for_each(&mut cfvo),
                _ => {}
            }
        }
        // `cfvo` holds `f`; the plain formulas go through a second call.
        drop(cfvo);
        for text in texts {
            *text = Arc::from(f(text));
        }
    }

    /// After rows or columns were inserted or deleted on the sheet `host`
    /// is on: the ranges moved and the formulas followed. `None` when all
    /// of its cells are gone.
    pub fn moved(&self, host: &str, lines: &Lines) -> Option<CondFormat> {
        let ranges: Vec<CellRange> = self
            .ranges
            .iter()
            .filter_map(|range| lines.moved_range(*range))
            .collect();
        if ranges.is_empty() {
            return None;
        }
        let mut moved = CondFormat {
            ranges,
            rules: self.rules.clone(),
        };
        moved.map_formulas(|formula| adjust(formula, Some(host), host, lines));
        Some(moved)
    }
}

/// What the formula engine is asked: the value of a formula, as if it were
/// written in a cell.
pub trait ExprEngine {
    /// `formula` (no `=`) evaluated in cell `at` of sheet `sheet`, over the
    /// values `workbook` holds. `None` when it cannot be worked out.
    fn evaluate(
        &mut self,
        workbook: &Workbook,
        sheet: usize,
        formula: &str,
        at: CellRef,
    ) -> Option<Value>;

    /// A cell changed: what was read from the workbook is out of date.
    fn invalidate(&mut self) {}
}

/// No engine: formulas are not evaluated, so rules that need one do not
/// apply.
pub struct NoEngine;

impl ExprEngine for NoEngine {
    fn evaluate(&mut self, _: &Workbook, _: usize, _: &str, _: CellRef) -> Option<Value> {
        None
    }
}

/// A data bar to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    /// Of the cell's width, 0 to 1.
    pub fraction: f32,
    pub color: Rgba,
}

/// An icon to draw: the index counts from the lowest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icon {
    pub set: IconSet,
    pub index: u8,
}

/// What the rules do to one cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effect {
    pub dxf: Dxf,
    pub bar: Option<Bar>,
    pub icon: Option<Icon>,
    /// The value is not written (`showValue="0"`).
    pub hide_value: bool,
}

impl Effect {
    fn is_empty(&self) -> bool {
        *self == Effect::default()
    }
}

/// Numbers worked out over a rule's cells.
#[derive(Clone, Debug)]
enum Stats {
    /// How often each value occurs.
    Counts(HashMap<String, u32>),
    /// A threshold value; `None` when there is no number to measure.
    Threshold(Option<f64>),
    /// Where a scale's stops, a bar's ends or an icon set's steps lie.
    Points(Vec<f64>),
    None,
}

struct Prepared {
    ranges: Vec<CellRange>,
    anchor: CellRef,
    rule: Rule,
    stats: Option<Stats>,
}

#[derive(Default)]
struct SheetCache {
    /// In priority order.
    rules: Vec<Prepared>,
    cells: HashMap<CellRef, Option<Arc<Effect>>>,
}

/// The effects of conditional formats, kept until a cell changes.
#[derive(Default)]
pub struct Conditional {
    sheets: HashMap<usize, SheetCache>,
}

/// A literal in a rule's formula: a number, a quoted text or a logical
/// value.
fn literal(text: &str) -> Option<Value> {
    let text = text.trim().trim_start_matches('=').trim();
    if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"'))
        && !inner.contains('"')
    {
        return Some(Value::Text(Arc::from(inner)));
    }
    if text.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if text.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    text.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map(Value::Number)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Empty => String::new(),
        Value::Number(n) => format::general(*n),
        Value::Text(t) => t.to_string(),
        Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
        Value::Error(e) => e.to_string(),
    }
}

fn is_blank(value: &Value) -> bool {
    match value {
        Value::Empty => true,
        Value::Text(t) => t.trim().is_empty(),
        _ => false,
    }
}

/// Excel's order of values: numbers, then text (without regard to case),
/// then logical values. An empty cell is 0 or empty text, whichever it is
/// set against. `None` when an error is involved.
fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    let rank = |v: &Value| match v {
        Value::Number(_) => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        _ => 3,
    };
    let (a, b) = match (a, b) {
        (Value::Empty, Value::Text(_)) => (&Value::Text(Arc::from("")), b),
        (Value::Text(_), Value::Empty) => (a, &Value::Text(Arc::from(""))),
        (Value::Empty, Value::Bool(_)) => (&Value::Bool(false), b),
        (Value::Bool(_), Value::Empty) => (a, &Value::Bool(false)),
        (Value::Empty, _) => (&Value::Number(0.0), b),
        (_, Value::Empty) => (a, &Value::Number(0.0)),
        _ => (a, b),
    };
    if matches!(a, Value::Error(_)) || matches!(b, Value::Error(_)) {
        return None;
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::Text(x), Value::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => Some(rank(a).cmp(&rank(b))),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0,
        _ => false,
    }
}

/// A key under which equal values (as Excel counts them) are the same.
fn key(value: &Value) -> Option<String> {
    Some(match value {
        Value::Empty => return None,
        Value::Number(n) => format!("n{}", if *n == 0.0 { 0 } else { n.to_bits() }),
        Value::Text(t) => {
            if t.is_empty() {
                return None;
            }
            format!("t{}", t.to_lowercase())
        }
        Value::Bool(b) => format!("b{}", u8::from(*b)),
        Value::Error(e) => format!("e{e}"),
    })
}

fn lerp(a: Rgba, b: Rgba, t: f64) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let channel = |i: usize| (f64::from(a[i]) * (1.0 - t) + f64::from(b[i]) * t).round() as u8;
    [channel(0), channel(1), channel(2), 0xff]
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let position = (p / 100.0).clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (below, above) = (position.floor() as usize, position.ceil() as usize);
    sorted[below] + (sorted[above] - sorted[below]) * (position - below as f64)
}

impl Conditional {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything: a cell changed.
    pub fn clear(&mut self) {
        self.sheets.clear();
    }

    /// The effects on the cells of `window` of sheet `sheet`, for the cells
    /// that have one.
    pub fn effects(
        &mut self,
        workbook: &Workbook,
        sheet: usize,
        window: CellRange,
        engine: &mut dyn ExprEngine,
    ) -> Vec<(CellRef, Arc<Effect>)> {
        let Some(target) = workbook.sheets.get(sheet) else {
            return Vec::new();
        };
        if target.cond_formats.is_empty() {
            return Vec::new();
        }
        let cache = self
            .sheets
            .entry(sheet)
            .or_insert_with(|| SheetCache::prepare(target));
        // The rules that reach the window.
        let active: Vec<usize> = cache
            .rules
            .iter()
            .enumerate()
            .filter(|(_, p)| p.ranges.iter().any(|r| r.intersects(&window)))
            .map(|(i, _)| i)
            .take(MAX_ACTIVE_RULES)
            .collect();
        if active.is_empty() {
            return Vec::new();
        }
        // The cells of the window some rule covers, row by row.
        let mut covered: HashSet<CellRef> = HashSet::new();
        for &i in &active {
            for range in &cache.rules[i].ranges {
                if !range.intersects(&window) {
                    continue;
                }
                let first = CellRef::new(
                    range.first.row.max(window.first.row),
                    range.first.col.max(window.first.col),
                );
                let last = CellRef::new(
                    range.last.row.min(window.last.row),
                    range.last.col.min(window.last.col),
                );
                'rows: for row in first.row..=last.row {
                    for col in first.col..=last.col {
                        if covered.len() >= MAX_WINDOW {
                            break 'rows;
                        }
                        covered.insert(CellRef::new(row, col));
                    }
                }
            }
        }
        let mut cells: Vec<CellRef> = covered.into_iter().collect();
        cells.sort();
        let mut out = Vec::new();
        for at in cells {
            if let Some(known) = cache.cells.get(&at) {
                if let Some(effect) = known {
                    out.push((at, effect.clone()));
                }
                continue;
            }
            let effect = cache.effect_at(workbook, target, sheet, at, &active, engine);
            if cache.cells.len() >= MAX_CACHED {
                cache.cells.clear();
            }
            cache.cells.insert(at, effect.clone());
            if let Some(effect) = effect {
                out.push((at, effect));
            }
        }
        out
    }
}

impl Prepared {
    /// Every cell of the rule's ranges that is in the file, at most
    /// [`MAX_SCAN`] of them.
    fn scan(&self, sheet: &Sheet, f: &mut dyn FnMut(&Value)) {
        let mut scanned = 0usize;
        for range in &self.ranges {
            for (_, cell) in sheet.cells_in(*range) {
                if scanned >= MAX_SCAN {
                    return;
                }
                scanned += 1;
                f(&cell.value);
            }
        }
    }

    fn numbers(&self, sheet: &Sheet) -> Vec<f64> {
        let mut numbers = Vec::new();
        self.scan(sheet, &mut |v| {
            if let Value::Number(n) = v
                && n.is_finite()
            {
                numbers.push(*n);
            }
        });
        numbers
    }
}

impl SheetCache {
    fn prepare(sheet: &Sheet) -> Self {
        let mut rules: Vec<Prepared> = Vec::new();
        for format in &sheet.cond_formats {
            let anchor = format.anchor();
            for rule in &format.rules {
                rules.push(Prepared {
                    ranges: format.ranges.clone(),
                    anchor,
                    rule: rule.clone(),
                    stats: None,
                });
            }
        }
        // Stable: rules of one priority stay in file order.
        rules.sort_by_key(|p| p.rule.priority);
        Self {
            rules,
            cells: HashMap::new(),
        }
    }

    fn effect_at(
        &mut self,
        workbook: &Workbook,
        sheet: &Sheet,
        index: usize,
        at: CellRef,
        active: &[usize],
        engine: &mut dyn ExprEngine,
    ) -> Option<Arc<Effect>> {
        let mut effect = Effect::default();
        for &i in active {
            if !self.rules[i].ranges.iter().any(|r| r.contains(at)) {
                continue;
            }
            let hit = self.apply(i, workbook, sheet, index, at, &mut effect, engine);
            if hit && self.rules[i].rule.stop_if_true {
                break;
            }
        }
        (!effect.is_empty()).then(|| Arc::new(effect))
    }

    /// Whether rule `i` applies to `at`; if it does, its doing is added to
    /// `effect` where nothing has been set yet.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        i: usize,
        workbook: &Workbook,
        sheet: &Sheet,
        index: usize,
        at: CellRef,
        effect: &mut Effect,
        engine: &mut dyn ExprEngine,
    ) -> bool {
        let value = sheet.cell(at).map_or(Value::Empty, |c| c.value.clone());
        let anchor = self.rules[i].anchor;
        let kind = self.rules[i].rule.kind.clone();
        let matched = match &kind {
            Kind::CellIs { op, first, second } => {
                let a = operand(first, workbook, index, anchor, at, engine);
                let b = second
                    .as_ref()
                    .and_then(|s| operand(s, workbook, index, anchor, at, engine));
                match (a, op) {
                    (None, _) => false,
                    (Some(a), Compare::Between | Compare::NotBetween) => {
                        let Some(b) = b else { return false };
                        let inside = matches!(
                            (compare(&value, &a), compare(&value, &b)),
                            (
                                Some(Ordering::Greater | Ordering::Equal),
                                Some(Ordering::Less | Ordering::Equal)
                            )
                        );
                        let known = compare(&value, &a).is_some() && compare(&value, &b).is_some();
                        known && (inside == (*op == Compare::Between))
                    }
                    (Some(a), op) => match compare(&value, &a) {
                        None => false,
                        Some(order) => match op {
                            Compare::Less => order == Ordering::Less,
                            Compare::LessEqual => order != Ordering::Greater,
                            Compare::Equal => order == Ordering::Equal,
                            Compare::NotEqual => order != Ordering::Equal,
                            Compare::GreaterEqual => order != Ordering::Less,
                            Compare::Greater => order == Ordering::Greater,
                            Compare::Between | Compare::NotBetween => false,
                        },
                    },
                }
            }
            Kind::Expression(formula) => {
                let shifted = shift(
                    formula,
                    i64::from(at.row) - i64::from(anchor.row),
                    i64::from(at.col) - i64::from(anchor.col),
                );
                engine
                    .evaluate(workbook, index, &shifted, at)
                    .is_some_and(|v| truthy(&v))
            }
            Kind::Text { test, text } => {
                let haystack = value_text(&value).to_lowercase();
                let needle = text.to_lowercase();
                match test {
                    TextTest::Contains => haystack.contains(&needle),
                    TextTest::NotContains => !haystack.contains(&needle),
                    TextTest::Begins => haystack.starts_with(&needle),
                    TextTest::Ends => haystack.ends_with(&needle),
                }
            }
            Kind::Blanks(wanted) => is_blank(&value) == *wanted,
            Kind::Errors(wanted) => matches!(value, Value::Error(_)) == *wanted,
            Kind::Duplicates { unique } => {
                let Some(key) = key(&value) else { return false };
                let Stats::Counts(counts) = self.stats(i, workbook, sheet, index, engine) else {
                    return false;
                };
                let count = counts.get(&key).copied().unwrap_or(0);
                if *unique { count == 1 } else { count > 1 }
            }
            Kind::Top { bottom, .. } => {
                let Value::Number(n) = value else {
                    return false;
                };
                let Stats::Threshold(Some(limit)) = self.stats(i, workbook, sheet, index, engine)
                else {
                    return false;
                };
                if *bottom { n <= *limit } else { n >= *limit }
            }
            Kind::Average {
                above,
                equal,
                std_dev,
            } => {
                let Value::Number(n) = value else {
                    return false;
                };
                let Stats::Threshold(Some(limit)) = self.stats(i, workbook, sheet, index, engine)
                else {
                    return false;
                };
                let _ = std_dev;
                match (above, equal) {
                    (true, false) => n > *limit,
                    (true, true) => n >= *limit,
                    (false, false) => n < *limit,
                    (false, true) => n <= *limit,
                }
            }
            Kind::ColorScale(stops) => {
                let Value::Number(n) = value else {
                    return false;
                };
                let Stats::Points(points) = self.stats(i, workbook, sheet, index, engine) else {
                    return false;
                };
                if points.len() != stops.len() || points.is_empty() {
                    return false;
                }
                let color = scale_color(n, points, stops);
                if effect.dxf.fill.is_none() {
                    effect.dxf.fill = Some(color);
                }
                return true;
            }
            Kind::DataBar {
                color,
                min_length,
                max_length,
                show_value,
                ..
            } => {
                let Value::Number(n) = value else {
                    return false;
                };
                let Stats::Points(points) = self.stats(i, workbook, sheet, index, engine) else {
                    return false;
                };
                let [low, high] = points[..] else {
                    return false;
                };
                let (shortest, longest) = (
                    f64::from(*min_length).min(100.0) / 100.0,
                    f64::from(*max_length).min(100.0) / 100.0,
                );
                let share = if high > low {
                    ((n - low) / (high - low)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                if effect.bar.is_none() {
                    effect.bar = Some(Bar {
                        fraction: (shortest + (longest - shortest) * share) as f32,
                        color: *color,
                    });
                    effect.hide_value |= !show_value;
                }
                return true;
            }
            Kind::IconSet {
                set,
                steps,
                reverse,
                show_value,
            } => {
                let Value::Number(n) = value else {
                    return false;
                };
                let Stats::Points(points) = self.stats(i, workbook, sheet, index, engine) else {
                    return false;
                };
                let mut chosen = 0usize;
                for (step, (point, cfvo)) in points.iter().zip(steps).enumerate().skip(1) {
                    let reached = if cfvo.gte { n >= *point } else { n > *point };
                    if reached {
                        chosen = step;
                    }
                }
                let last = usize::from(set.count) - 1;
                let chosen = chosen.min(last);
                let chosen = if *reverse { last - chosen } else { chosen };
                if effect.icon.is_none() {
                    effect.icon = Some(Icon {
                        set: *set,
                        index: chosen as u8,
                    });
                    effect.hide_value |= !show_value;
                }
                return true;
            }
            Kind::Unsupported(_) => false,
        };
        if matched && let Some(dxf) = self.rules[i].rule.dxf.clone() {
            effect.dxf.fill_from(&dxf);
        }
        matched
    }

    /// The statistics of rule `i`, worked out on first use.
    fn stats(
        &mut self,
        i: usize,
        workbook: &Workbook,
        sheet: &Sheet,
        index: usize,
        engine: &mut dyn ExprEngine,
    ) -> &Stats {
        if self.rules[i].stats.is_none() {
            let stats = self.work_out(i, workbook, sheet, index, engine);
            self.rules[i].stats = Some(stats);
        }
        self.rules[i].stats.as_ref().expect("just set")
    }

    fn work_out(
        &self,
        i: usize,
        workbook: &Workbook,
        sheet: &Sheet,
        index: usize,
        engine: &mut dyn ExprEngine,
    ) -> Stats {
        let prepared = &self.rules[i];
        match &prepared.rule.kind {
            Kind::Duplicates { .. } => {
                let mut counts: HashMap<String, u32> = HashMap::new();
                prepared.scan(sheet, &mut |v| {
                    if let Some(key) = key(v) {
                        *counts.entry(key).or_insert(0) += 1;
                    }
                });
                Stats::Counts(counts)
            }
            Kind::Top {
                rank,
                percent,
                bottom,
            } => {
                let mut values = prepared.numbers(sheet);
                if values.is_empty() {
                    return Stats::Threshold(None);
                }
                values.sort_by(|a, b| a.total_cmp(b));
                let count = values.len();
                let wanted = if *percent {
                    ((count as f64 * f64::from(*rank) / 100.0).floor() as usize).max(1)
                } else {
                    (*rank as usize).max(1)
                }
                .min(count);
                Stats::Threshold(Some(if *bottom {
                    values[wanted - 1]
                } else {
                    values[count - wanted]
                }))
            }
            Kind::Average { above, std_dev, .. } => {
                let values = prepared.numbers(sheet);
                if values.is_empty() {
                    return Stats::Threshold(None);
                }
                let n = values.len() as f64;
                let mean = values.iter().sum::<f64>() / n;
                let spread = if *std_dev > 0 && values.len() > 1 {
                    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
                        * f64::from(*std_dev)
                } else {
                    0.0
                };
                Stats::Threshold(Some(if *above { mean + spread } else { mean - spread }))
            }
            Kind::ColorScale(stops) => {
                let values = prepared.numbers(sheet);
                let cfvos: Vec<&Cfvo> = stops.iter().map(|(c, _)| c).collect();
                resolve(&cfvos, values, workbook, index, prepared.anchor, engine)
            }
            Kind::DataBar { min, max, .. } => {
                let values = prepared.numbers(sheet);
                resolve(
                    &[min, max],
                    values,
                    workbook,
                    index,
                    prepared.anchor,
                    engine,
                )
            }
            Kind::IconSet { steps, .. } => {
                let values = prepared.numbers(sheet);
                let cfvos: Vec<&Cfvo> = steps.iter().collect();
                resolve(&cfvos, values, workbook, index, prepared.anchor, engine)
            }
            _ => Stats::None,
        }
    }
}

/// Where each `cfvo` lies, given the numbers of the rule's cells.
fn resolve(
    cfvos: &[&Cfvo],
    mut values: Vec<f64>,
    workbook: &Workbook,
    index: usize,
    anchor: CellRef,
    engine: &mut dyn ExprEngine,
) -> Stats {
    if values.is_empty() {
        return Stats::Points(Vec::new());
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let (low, high) = (values[0], values[values.len() - 1]);
    let points = cfvos
        .iter()
        .map(|cfvo| {
            let number = || {
                cfvo.value
                    .trim()
                    .trim_start_matches('=')
                    .parse::<f64>()
                    .ok()
            };
            match cfvo.kind {
                CfvoKind::Min => low,
                CfvoKind::Max => high,
                CfvoKind::Num => number().unwrap_or(low),
                CfvoKind::Percent => low + (high - low) * number().unwrap_or(0.0) / 100.0,
                CfvoKind::Percentile => percentile(&values, number().unwrap_or(0.0)),
                CfvoKind::Formula => match literal(&cfvo.value) {
                    Some(Value::Number(n)) => n,
                    _ => match engine.evaluate(workbook, index, &cfvo.value, anchor) {
                        Some(Value::Number(n)) => n,
                        _ => low,
                    },
                },
            }
        })
        .collect();
    Stats::Points(points)
}

/// The colour a value gets on a scale whose stops lie at `points`.
fn scale_color(n: f64, points: &[f64], stops: &[(Cfvo, Rgba)]) -> Rgba {
    if n <= points[0] {
        return stops[0].1;
    }
    for pair in 1..points.len() {
        if n <= points[pair] {
            let (from, to) = (points[pair - 1], points[pair]);
            let t = if to > from {
                (n - from) / (to - from)
            } else {
                1.0
            };
            return lerp(stops[pair - 1].1, stops[pair].1, t);
        }
    }
    stops[stops.len() - 1].1
}

/// The value of an operand: a literal, or a formula for the engine.
fn operand(
    text: &str,
    workbook: &Workbook,
    sheet: usize,
    anchor: CellRef,
    at: CellRef,
    engine: &mut dyn ExprEngine,
) -> Option<Value> {
    if let Some(value) = literal(text) {
        return Some(value);
    }
    let shifted = shift(
        text,
        i64::from(at.row) - i64::from(anchor.row),
        i64::from(at.col) - i64::from(anchor.col),
    );
    engine.evaluate(workbook, sheet, &shifted, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, SheetKind};

    /// An engine for the formulas these tests use.
    struct Fake;

    impl ExprEngine for Fake {
        fn evaluate(
            &mut self,
            wb: &Workbook,
            sheet: usize,
            formula: &str,
            at: CellRef,
        ) -> Option<Value> {
            // `MOD(ROW(),2)=0` and `<cell> > <number>` and `<cell>`.
            let formula = formula.replace(' ', "");
            if formula == "MOD(ROW(),2)=0" {
                return Some(Value::Bool((at.row + 1).is_multiple_of(2)));
            }
            if let Some((cell, number)) = formula.split_once('>') {
                let cell = CellRef::parse(&cell.replace('$', ""))?;
                let n: f64 = number.parse().ok()?;
                return match wb.sheets[sheet].cell(cell)?.value {
                    Value::Number(v) => Some(Value::Bool(v > n)),
                    _ => Some(Value::Bool(false)),
                };
            }
            let cell = CellRef::parse(&formula.replace('$', ""))?;
            Some(wb.sheets[sheet].cell(cell)?.value.clone())
        }
    }

    fn dxf(fill: Rgba) -> Option<Arc<Dxf>> {
        Some(Arc::new(Dxf {
            fill: Some(fill),
            ..Dxf::default()
        }))
    }

    const RED: Rgba = [255, 0, 0, 255];
    const GREEN: Rgba = [0, 255, 0, 255];
    const BLUE: Rgba = [0, 0, 255, 255];

    fn rule(kind: Kind) -> Rule {
        Rule {
            priority: 1,
            stop_if_true: false,
            kind,
            dxf: dxf(RED),
        }
    }

    /// A column of values in A1..A{n} with the rules on A1:A{n}.
    fn book(values: &[Value], rules: Vec<Rule>) -> Workbook {
        let mut builder = SheetBuilder::new("S", SheetKind::Worksheet);
        for (row, value) in values.iter().enumerate() {
            if *value != Value::Empty {
                builder.push(row as u32, Cell::new(0, value.clone()));
            }
        }
        let mut sheet = builder.finish();
        sheet.cond_formats.push(CondFormat {
            ranges: vec![CellRange::new(
                CellRef::new(0, 0),
                CellRef::new(values.len() as u32 - 1, 0),
            )],
            rules,
        });
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    fn n(v: f64) -> Value {
        Value::Number(v)
    }

    fn t(s: &str) -> Value {
        Value::Text(Arc::from(s))
    }

    /// The rows (from 0) a rule marks with a red fill.
    fn marked(values: &[Value], rule: Rule) -> Vec<u32> {
        let wb = book(values, vec![rule]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(values.len() as u32, 0));
        Conditional::new()
            .effects(&wb, 0, window, &mut Fake)
            .into_iter()
            .filter(|(_, e)| e.dxf.fill == Some(RED))
            .map(|(at, _)| at.row)
            .collect()
    }

    fn cell_is(op: Compare, a: &str, b: Option<&str>) -> Rule {
        rule(Kind::CellIs {
            op,
            first: Arc::from(a),
            second: b.map(Arc::from),
        })
    }

    #[test]
    fn cell_is_with_every_operator() {
        let v = [n(1.0), n(5.0), n(9.0), t("abc"), Value::Empty];
        assert_eq!(marked(&v, cell_is(Compare::Less, "5", None)), [0, 4]);
        assert_eq!(
            marked(&v, cell_is(Compare::LessEqual, "5", None)),
            [0, 1, 4]
        );
        assert_eq!(marked(&v, cell_is(Compare::Equal, "5", None)), [1]);
        assert_eq!(
            marked(&v, cell_is(Compare::NotEqual, "5", None)),
            [0, 2, 3, 4]
        );
        assert_eq!(
            marked(&v, cell_is(Compare::GreaterEqual, "5", None)),
            [1, 2, 3]
        );
        assert_eq!(marked(&v, cell_is(Compare::Greater, "5", None)), [2, 3]);
        assert_eq!(
            marked(&v, cell_is(Compare::Between, "2", Some("9"))),
            [1, 2]
        );
        assert_eq!(
            marked(&v, cell_is(Compare::NotBetween, "2", Some("9"))),
            [0, 3, 4]
        );
        // Text compares without regard to case.
        assert_eq!(marked(&v, cell_is(Compare::Equal, "\"ABC\"", None)), [3]);
        // A blank cell is 0.
        assert_eq!(marked(&v, cell_is(Compare::Equal, "0", None)), [4]);
    }

    #[test]
    fn cell_is_with_a_formula_operand() {
        let v = [n(1.0), n(5.0), n(9.0)];
        // `$A$2` is read by the engine.
        assert_eq!(marked(&v, cell_is(Compare::Greater, "$A$2", None)), [2]);
    }

    #[test]
    fn expressions_are_relative_to_the_top_left_cell() {
        let v = [n(1.0), n(2.0), n(30.0), n(4.0)];
        // `A1>3` on A1:A4 is `A3>3` for A3.
        assert_eq!(
            marked(&v, rule(Kind::Expression(Arc::from("A1>3")))),
            [2, 3]
        );
        // ROW() is the cell's own.
        assert_eq!(
            marked(&v, rule(Kind::Expression(Arc::from("MOD(ROW(),2)=0")))),
            [1, 3]
        );
        // No engine, no match.
        let wb = book(&v, vec![rule(Kind::Expression(Arc::from("A1>3")))]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(3, 0));
        assert!(
            Conditional::new()
                .effects(&wb, 0, window, &mut NoEngine)
                .is_empty()
        );
    }

    fn text(test: TextTest, s: &str) -> Rule {
        rule(Kind::Text {
            test,
            text: Arc::from(s),
        })
    }

    #[test]
    fn text_rules() {
        let v = [t("Apple pie"), t("banana"), n(15.0), Value::Empty, t("PIE")];
        assert_eq!(marked(&v, text(TextTest::Contains, "pie")), [0, 4]);
        assert_eq!(marked(&v, text(TextTest::NotContains, "pie")), [1, 2, 3]);
        assert_eq!(marked(&v, text(TextTest::Begins, "app")), [0]);
        assert_eq!(marked(&v, text(TextTest::Ends, "PIE")), [0, 4]);
        // A number is searched as it is written.
        assert_eq!(marked(&v, text(TextTest::Contains, "5")), [2]);
    }

    #[test]
    fn blanks_and_errors() {
        let v = [
            t("x"),
            Value::Empty,
            t("  "),
            Value::Error(Arc::from("#N/A")),
            n(0.0),
        ];
        assert_eq!(marked(&v, rule(Kind::Blanks(true))), [1, 2]);
        assert_eq!(marked(&v, rule(Kind::Blanks(false))), [0, 3, 4]);
        assert_eq!(marked(&v, rule(Kind::Errors(true))), [3]);
        assert_eq!(marked(&v, rule(Kind::Errors(false))), [0, 1, 2, 4]);
    }

    #[test]
    fn duplicates_and_unique_values() {
        let v = [t("a"), t("B"), t("A"), n(1.0), n(1.0), Value::Empty, t("c")];
        assert_eq!(
            marked(&v, rule(Kind::Duplicates { unique: false })),
            [0, 2, 3, 4]
        );
        assert_eq!(marked(&v, rule(Kind::Duplicates { unique: true })), [1, 6]);
    }

    #[test]
    fn top_and_bottom() {
        let v = [
            n(5.0),
            n(9.0),
            n(1.0),
            n(9.0),
            n(3.0),
            t("x"),
            n(7.0),
            n(2.0),
            n(8.0),
            n(4.0),
        ];
        let top = |rank, percent, bottom| {
            rule(Kind::Top {
                rank,
                percent,
                bottom,
            })
        };
        // The two highest; the tie at 9 is both.
        assert_eq!(marked(&v, top(2, false, false)), [1, 3]);
        assert_eq!(marked(&v, top(3, false, false)), [1, 3, 8]);
        assert_eq!(marked(&v, top(2, false, true)), [2, 7]);
        // 30 % of nine numbers: two.
        assert_eq!(marked(&v, top(30, true, false)), [1, 3]);
        assert_eq!(marked(&v, top(30, true, true)), [2, 7]);
        // More than there are: all numbers.
        assert_eq!(
            marked(&v, top(50, false, false)),
            [0, 1, 2, 3, 4, 6, 7, 8, 9]
        );
    }

    #[test]
    fn above_and_below_average() {
        let v = [n(1.0), n(2.0), n(3.0), n(4.0), n(5.0)];
        let avg = |above, equal, std_dev| {
            rule(Kind::Average {
                above,
                equal,
                std_dev,
            })
        };
        assert_eq!(marked(&v, avg(true, false, 0)), [3, 4]);
        assert_eq!(marked(&v, avg(true, true, 0)), [2, 3, 4]);
        assert_eq!(marked(&v, avg(false, false, 0)), [0, 1]);
        // Sample deviation is 1.58: above 3 + 1.58 is only 5 (and 4.58).
        assert_eq!(marked(&v, avg(true, false, 1)), [4]);
        assert_eq!(marked(&v, avg(false, false, 1)), [0]);
    }

    #[test]
    fn colour_scales_blend_between_their_stops() {
        let cfvo = |kind, value: &str| Cfvo {
            kind,
            value: value.to_owned(),
            gte: true,
        };
        let v = [n(0.0), n(5.0), n(10.0), t("x")];
        let two = Rule {
            priority: 1,
            stop_if_true: false,
            kind: Kind::ColorScale(vec![
                (cfvo(CfvoKind::Min, ""), [0, 0, 0, 255]),
                (cfvo(CfvoKind::Max, ""), [200, 100, 50, 255]),
            ]),
            dxf: None,
        };
        let wb = book(&v, vec![two]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(3, 0));
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        let fills: Vec<(u32, Option<Rgba>)> =
            found.iter().map(|(a, e)| (a.row, e.dxf.fill)).collect();
        assert_eq!(
            fills,
            [
                (0, Some([0, 0, 0, 255])),
                (1, Some([100, 50, 25, 255])),
                (2, Some([200, 100, 50, 255]))
            ]
        );
        // Three stops, the middle one at the 50th percentile.
        let three = Rule {
            priority: 1,
            stop_if_true: false,
            kind: Kind::ColorScale(vec![
                (cfvo(CfvoKind::Min, ""), RED),
                (cfvo(CfvoKind::Percentile, "50"), GREEN),
                (cfvo(CfvoKind::Max, ""), BLUE),
            ]),
            dxf: None,
        };
        let v = [n(0.0), n(1.0), n(2.0), n(10.0)];
        let wb = book(&v, vec![three]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(3, 0));
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        assert_eq!(found[0].1.dxf.fill, Some(RED));
        // The median is 1.5: the value 1 is two thirds of the way to it.
        assert_eq!(found[1].1.dxf.fill, Some(lerp(RED, GREEN, 1.0 / 1.5)));
        assert_eq!(found[3].1.dxf.fill, Some(BLUE));
    }

    #[test]
    fn data_bars_scale_between_the_shortest_and_longest() {
        let cfvo = |kind| Cfvo {
            kind,
            value: String::new(),
            gte: true,
        };
        let bar = Rule {
            priority: 1,
            stop_if_true: false,
            kind: Kind::DataBar {
                min: cfvo(CfvoKind::Min),
                max: cfvo(CfvoKind::Max),
                color: BLUE,
                min_length: 10,
                max_length: 90,
                show_value: false,
            },
            dxf: None,
        };
        let wb = book(&[n(0.0), n(50.0), n(100.0)], vec![bar]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(2, 0));
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        let lengths: Vec<f32> = found.iter().map(|(_, e)| e.bar.unwrap().fraction).collect();
        assert!((lengths[0] - 0.1).abs() < 1e-6);
        assert!((lengths[1] - 0.5).abs() < 1e-6);
        assert!((lengths[2] - 0.9).abs() < 1e-6);
        assert!(found[0].1.hide_value);
    }

    #[test]
    fn icon_sets_pick_by_threshold() {
        let step = |kind, value: &str, gte| Cfvo {
            kind,
            value: value.to_owned(),
            gte,
        };
        let icons = |reverse| Rule {
            priority: 1,
            stop_if_true: false,
            kind: Kind::IconSet {
                set: IconSet::parse("3Arrows").unwrap(),
                steps: vec![
                    step(CfvoKind::Percent, "0", true),
                    step(CfvoKind::Percent, "33", true),
                    step(CfvoKind::Percent, "67", false),
                ],
                reverse,
                show_value: true,
            },
            dxf: None,
        };
        let values = [n(0.0), n(40.0), n(67.0), n(68.0), n(100.0)];
        let index = |reverse| {
            let wb = book(&values, vec![icons(reverse)]);
            let window = CellRange::new(CellRef::new(0, 0), CellRef::new(4, 0));
            Conditional::new()
                .effects(&wb, 0, window, &mut NoEngine)
                .iter()
                .map(|(_, e)| e.icon.unwrap().index)
                .collect::<Vec<_>>()
        };
        // 67 is not above the 67 % step.
        assert_eq!(index(false), [0, 1, 1, 2, 2]);
        assert_eq!(index(true), [2, 1, 1, 0, 0]);
        assert_eq!(IconSet::parse("5Quarters").unwrap().count, 5);
        assert!(IconSet::parse("3Unknown").is_none());
    }

    #[test]
    fn priority_orders_rules_and_stop_if_true_ends_the_walk() {
        let mut low = cell_is(Compare::Greater, "0", None);
        low.priority = 2;
        low.dxf = dxf(GREEN);
        let mut high = cell_is(Compare::Greater, "5", None);
        high.priority = 1;
        high.dxf = Some(Arc::new(Dxf {
            fill: Some(RED),
            bold: Some(true),
            ..Dxf::default()
        }));
        // A rule written first but of lower priority comes second: the fill
        // of the higher one wins, the lower one adds nothing it set.
        let wb = book(&[n(9.0), n(3.0)], vec![low.clone(), high.clone()]);
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(1, 0));
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        assert_eq!(found[0].1.dxf.fill, Some(RED));
        assert_eq!(found[0].1.dxf.bold, Some(true));
        assert_eq!(found[1].1.dxf.fill, Some(GREEN));
        assert_eq!(found[1].1.dxf.bold, None);
        // Stop at the first.
        high.stop_if_true = true;
        let mut other = low;
        other.dxf = Some(Arc::new(Dxf {
            italic: Some(true),
            ..Dxf::default()
        }));
        let wb = book(&[n(9.0), n(3.0)], vec![other, high]);
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        assert_eq!(found[0].1.dxf.italic, None);
        assert_eq!(found[1].1.dxf.italic, Some(true));
    }

    #[test]
    fn effects_follow_changes_once_the_cache_is_cleared() {
        let mut wb = book(
            &[n(1.0), n(9.0)],
            vec![cell_is(Compare::Greater, "5", None)],
        );
        let window = CellRange::new(CellRef::new(0, 0), CellRef::new(1, 0));
        let mut cache = Conditional::new();
        assert_eq!(cache.effects(&wb, 0, window, &mut NoEngine).len(), 1);
        wb.sheets[0].cell_mut(CellRef::new(0, 0)).unwrap().value = n(7.0);
        cache.clear();
        assert_eq!(cache.effects(&wb, 0, window, &mut NoEngine).len(), 2);
    }

    #[test]
    fn rules_move_with_inserted_and_deleted_rows() {
        let format = CondFormat {
            ranges: vec![CellRange::parse("B3:B8").unwrap()],
            rules: vec![rule(Kind::Expression(Arc::from("$A3>$A$1")))],
        };
        let insert = Lines {
            columns: false,
            at: 0,
            count: 2,
            insert: true,
        };
        let moved = format.moved("S", &insert).unwrap();
        assert_eq!(moved.ranges[0].to_string(), "B5:B10");
        assert_eq!(moved.rules[0].kind, Kind::Expression(Arc::from("$A5>$A$3")));
        // All of the cells deleted: the rules go.
        let delete = Lines {
            columns: false,
            at: 2,
            count: 6,
            insert: false,
        };
        assert!(format.moved("S", &delete).is_none());
    }

    #[test]
    fn a_huge_window_is_bounded() {
        let mut wb = book(&[n(1.0)], vec![cell_is(Compare::Less, "5", None)]);
        wb.sheets[0].cond_formats[0].ranges = vec![CellRange::parse("A1:XFD1048576").unwrap()];
        let window = CellRange::parse("A1:XFD1048576").unwrap();
        let found = Conditional::new().effects(&wb, 0, window, &mut NoEngine);
        assert!(found.len() <= MAX_WINDOW);
    }
}
