//! Charts: what a chart part says ([`Chart`]) and the numbers it plots
//! ([`Workbook::resolve_chart`]).
//!
//! A chart names the cells it plots (`Sheet1!$B$2:$B$9`); the values are
//! read from the workbook as it is now, so a chart follows its cells. The
//! values the file saved with the chart are only the fallback for a
//! reference that cannot be followed (a sheet that is gone, a formula in
//! place of a range).
//!
//! Everything is bounded: series, points, text, the number of plots. What
//! is over a bound is cut or thinned out, not an error.

use std::sync::Arc;

use crate::address::{CellRange, CellRef};
use crate::format::Format;
use crate::model::{Value, Workbook};
use crate::style::Rgba;
use crate::validation::parse_range;

/// Most series drawn.
pub const MAX_SERIES: usize = 64;
/// Most points one series draws; more are thinned out.
pub const MAX_POINTS: usize = 10_000;
/// Most cells read for one series.
pub const MAX_READ: usize = 200_000;
/// Most cells read for one chart.
pub const MAX_TOTAL_READ: usize = 2_000_000;
/// Longest title, series name or label kept, characters.
pub const MAX_TEXT: usize = 200;
/// Most plots (kinds of chart) in one chart.
pub const MAX_PLOTS: usize = 8;
/// Most category labels kept for drawing.
pub const MAX_LABELS: usize = 500;
/// Most slices of a pie.
pub const MAX_SLICES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    Clustered,
    Stacked,
    Percent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlotKind {
    Bar {
        horizontal: bool,
        grouping: Grouping,
        /// Gap between groups, percent of a bar's width.
        gap: f64,
        /// Overlap of bars of one group, percent.
        overlap: f64,
    },
    Line {
        grouping: Grouping,
    },
    Area {
        grouping: Grouping,
    },
    Pie {
        /// The size of a doughnut's hole, 0 to 0.9 of its radius.
        hole: Option<f64>,
        /// Where the first slice starts, degrees clockwise from the top.
        first_angle: f64,
    },
    Scatter,
}

/// Where a series' numbers or names come from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source {
    /// `Sheet1!$B$2:$B$9`, as the file writes it.
    pub formula: Option<String>,
    /// What the file saved there.
    pub cached: Vec<Value>,
    /// The number format the file saved with them.
    pub format: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MarkerSymbol {
    /// The kind the series number picks.
    #[default]
    Auto,
    None,
    Circle,
    Square,
    Diamond,
    Triangle,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Marker {
    pub symbol: MarkerSymbol,
    /// Pixels across.
    pub size: Option<f64>,
    pub fill: Option<Rgba>,
}

/// What a series' data labels show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Labels {
    pub value: bool,
    pub percent: bool,
    pub category: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: Option<Source>,
    /// The categories; for a scatter chart the x values.
    pub categories: Option<Source>,
    /// The values; for a scatter chart the y values.
    pub values: Option<Source>,
    pub fill: Option<Rgba>,
    pub line: Option<Rgba>,
    /// Pixels.
    pub line_width: Option<f64>,
    pub no_line: bool,
    pub no_fill: bool,
    pub marker: Marker,
    /// Colours of single points (slices of a pie), by index.
    pub point_fills: Vec<(usize, Rgba)>,
    pub smooth: bool,
    pub labels: Labels,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    pub kind: PlotKind,
    pub series: Vec<Series>,
    /// The ids of the axes it is measured on.
    pub axes: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisKind {
    Category,
    Date,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartAxis {
    pub id: u32,
    pub kind: AxisKind,
    pub side: AxisSide,
    pub deleted: bool,
    pub reversed: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub major_unit: Option<f64>,
    pub log_base: Option<f64>,
    pub gridlines: bool,
    pub title: Option<String>,
    /// The number format code, unless it follows the cells.
    pub format: Option<String>,
    /// Tick labels are written (`tickLblPos` is not `none`).
    pub labels: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegendPos {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Chart {
    pub title: Option<String>,
    pub plots: Vec<Plot>,
    pub axes: Vec<ChartAxis>,
    pub legend: Option<LegendPos>,
    pub background: Option<Rgba>,
    pub plot_background: Option<Rgba>,
    /// Hidden rows and columns are not plotted.
    pub visible_only: bool,
    /// The colours series take in turn: the theme's accents.
    pub palette: Vec<Rgba>,
    /// Parts of the chart that are not drawn: unsupported kinds and
    /// elements, by their names in the file.
    pub not_shown: Vec<String>,
}

/// One series, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnSeries {
    /// `None`: the series has no name; the drawing says "Series n".
    pub name: Option<String>,
    pub color: Rgba,
    pub line_color: Rgba,
    pub line_width: f64,
    pub no_line: bool,
    pub no_fill: bool,
    pub marker: Marker,
    pub smooth: bool,
    pub labels: Labels,
    /// Where each point is on its axis: the category's place from 0, or a
    /// scatter chart's x.
    pub xs: Vec<f64>,
    /// The values; `NaN` where there is none.
    pub ys: Vec<f64>,
    /// Pie slices' colours.
    pub slice_colors: Vec<Rgba>,
    /// The series number counting all plots, from 0.
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawnPlot {
    pub kind: PlotKind,
    pub series: Vec<DrawnSeries>,
    /// Measured on the second value axis.
    pub secondary: bool,
    /// How many categories there are (the longest series).
    pub categories: usize,
    /// Category labels by their place; a long list is thinned out.
    pub labels: Vec<(usize, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawnAxis {
    pub kind: AxisKind,
    pub side: AxisSide,
    pub deleted: bool,
    pub reversed: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub major_unit: Option<f64>,
    pub log: bool,
    pub gridlines: bool,
    pub title: Option<String>,
    pub format: Arc<Format>,
    pub labels: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawnChart {
    pub title: Option<String>,
    pub plots: Vec<DrawnPlot>,
    /// The horizontal axis (the vertical one for a horizontal bar chart is
    /// the category axis, still `x`).
    pub x: Option<DrawnAxis>,
    pub y: Option<DrawnAxis>,
    pub y2: Option<DrawnAxis>,
    pub legend: Option<LegendPos>,
    pub background: Option<Rgba>,
    pub plot_background: Option<Rgba>,
    pub not_shown: Vec<String>,
    pub date1904: bool,
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_TEXT - 1).collect();
    out.push('…');
    out
}

/// A reference written as the file has it: one or several areas,
/// `Sheet1!$A$1:$A$3` or `(Sheet1!$A$1:$A$3,Sheet1!$C$1:$C$3)`.
pub fn parse_areas(text: &str) -> Vec<(String, CellRange)> {
    let text = text.trim();
    let inner = text
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or(text);
    let mut parts = Vec::new();
    let (mut depth_quote, mut start) = (false, 0usize);
    for (i, c) in inner.char_indices() {
        match c {
            '\'' => depth_quote = !depth_quote,
            ',' if !depth_quote => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        if parts.len() > 32 {
            break;
        }
    }
    parts.push(&inner[start..]);
    parts
        .into_iter()
        .filter_map(|part| {
            let (sheet, range) = parse_range(part)?;
            Some((sheet?, range))
        })
        .collect()
}

impl Workbook {
    /// The cells a source names, in order, or `None` when it cannot be
    /// followed (a sheet that is not there, text that is no reference).
    /// `budget` is what may still be read for the chart.
    fn source_cells(
        &self,
        source: &Source,
        visible_only: bool,
        budget: &mut usize,
    ) -> Option<Vec<(Value, Option<Arc<Format>>)>> {
        let areas = parse_areas(source.formula.as_deref()?);
        if areas.is_empty() {
            return None;
        }
        let mut out: Vec<(Value, Option<Arc<Format>>)> = Vec::new();
        let limit = MAX_READ.min(*budget);
        for (name, range) in areas {
            let sheet = self.sheets.iter().find(|s| s.name == name).or_else(|| {
                self.sheets
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(&name))
            })?;
            for row in range.first.row..=range.last.row {
                if visible_only && sheet.row_sizes.size(row) == 0.0 {
                    continue;
                }
                for col in range.first.col..=range.last.col {
                    if out.len() >= limit {
                        *budget = budget.saturating_sub(out.len());
                        return Some(out);
                    }
                    if visible_only && sheet.columns.size(col) == 0.0 {
                        continue;
                    }
                    match sheet.cell(CellRef::new(row, col)) {
                        Some(cell) => out.push((
                            cell.value.clone(),
                            self.styles
                                .get(cell.style as usize)
                                .map(|s| s.format.clone()),
                        )),
                        None => out.push((Value::Empty, None)),
                    }
                }
            }
        }
        *budget = budget.saturating_sub(out.len());
        Some(out)
    }

    /// A source's values (with their formats): the cells, or what the file
    /// saved.
    fn source_values(
        &self,
        source: &Source,
        visible_only: bool,
        budget: &mut usize,
    ) -> Vec<(Value, Option<Arc<Format>>)> {
        if let Some(cells) = self.source_cells(source, visible_only, budget) {
            return cells;
        }
        let format = source
            .format
            .as_deref()
            .map(|code| Arc::new(Format::parse(code)));
        source
            .cached
            .iter()
            .take(MAX_READ.min(*budget))
            .map(|v| (v.clone(), format.clone()))
            .collect()
    }

    fn text_of(&self, value: &Value, format: Option<&Format>) -> String {
        match value {
            Value::Empty => String::new(),
            Value::Number(n) => match format {
                Some(format) => format.number(*n, self.date1904),
                None => crate::format::general(*n),
            },
            Value::Text(t) => t.to_string(),
            Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
            Value::Error(e) => e.to_string(),
        }
    }

    /// The chart with the values of its cells in it.
    pub fn resolve_chart(&self, chart: &Chart) -> DrawnChart {
        let mut budget = MAX_TOTAL_READ;
        let mut not_shown = chart.not_shown.clone();
        let mut plots = Vec::new();
        let mut counted = 0usize;
        // The first plot's value axis is the first, any other the second.
        let first_value_axis = chart
            .plots
            .first()
            .and_then(|p| {
                p.axes.iter().find(|id| {
                    chart
                        .axes
                        .iter()
                        .any(|a| a.id == **id && a.kind == AxisKind::Value)
                })
            })
            .copied();
        let mut axis_ids: [Option<u32>; 3] = [None; 3];
        let mut source_format: Option<Arc<Format>> = None;
        for plot in chart.plots.iter().take(MAX_PLOTS) {
            let value_axis = plot
                .axes
                .iter()
                .find(|id| {
                    chart
                        .axes
                        .iter()
                        .any(|a| a.id == **id && a.kind == AxisKind::Value)
                })
                .copied();
            let secondary = value_axis.is_some() && value_axis != first_value_axis;
            let mut series = Vec::new();
            let mut labels: Vec<(usize, String)> = Vec::new();
            let mut categories = 0usize;
            for (i, ser) in plot.series.iter().enumerate() {
                if counted >= MAX_SERIES {
                    if !not_shown.iter().any(|n| n == "series limit") {
                        not_shown.push("series limit".to_owned());
                    }
                    break;
                }
                let index = counted;
                counted += 1;
                let name = ser.name.as_ref().map(|source| {
                    let values = self.source_values(source, false, &mut budget);
                    clip(
                        &values
                            .iter()
                            .map(|(v, f)| self.text_of(v, f.as_deref()))
                            .filter(|t| !t.is_empty())
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                });
                let name = name.filter(|n| !n.is_empty());
                let values = ser
                    .values
                    .as_ref()
                    .map(|s| self.source_values(s, chart.visible_only, &mut budget))
                    .unwrap_or_default();
                if source_format.is_none() {
                    source_format = values.iter().find_map(|(_, f)| f.clone());
                }
                let numbers: Vec<f64> = values
                    .iter()
                    .map(|(v, _)| match v {
                        Value::Number(n) if n.is_finite() => *n,
                        _ => f64::NAN,
                    })
                    .collect();
                let cats = ser
                    .categories
                    .as_ref()
                    .map(|s| self.source_values(s, chart.visible_only, &mut budget))
                    .unwrap_or_default();
                let scatter = matches!(plot.kind, PlotKind::Scatter);
                let positions: Vec<f64> = if scatter {
                    (0..numbers.len())
                        .map(|k| match cats.get(k) {
                            Some((Value::Number(n), _)) if n.is_finite() => *n,
                            // Text x values are plotted as 1, 2, 3 ...
                            _ => (k + 1) as f64,
                        })
                        .collect()
                } else {
                    (0..numbers.len()).map(|k| k as f64).collect()
                };
                if !scatter && cats.len().max(numbers.len()) > categories {
                    categories = cats.len().max(numbers.len());
                    let count = categories;
                    let step = count.div_ceil(MAX_LABELS).max(1);
                    labels = (0..count)
                        .step_by(step)
                        .map(|k| {
                            let text = cats
                                .get(k)
                                .map(|(v, f)| clip(&self.text_of(v, f.as_deref())))
                                .unwrap_or_else(|| (k + 1).to_string());
                            (k, text)
                        })
                        .collect();
                }
                let stacked = matches!(
                    plot.kind,
                    PlotKind::Bar {
                        grouping: Grouping::Stacked | Grouping::Percent,
                        ..
                    } | PlotKind::Line {
                        grouping: Grouping::Stacked | Grouping::Percent
                    } | PlotKind::Area { .. }
                );
                let (xs, ys) = thin(
                    &positions,
                    &numbers,
                    matches!(plot.kind, PlotKind::Line { .. } | PlotKind::Area { .. }) && !stacked,
                );
                let (xs, ys) = if matches!(plot.kind, PlotKind::Pie { .. }) && xs.len() > MAX_SLICES
                {
                    (xs[..MAX_SLICES].to_vec(), ys[..MAX_SLICES].to_vec())
                } else {
                    (xs, ys)
                };
                let color = ser
                    .fill
                    .or(ser.line)
                    .unwrap_or_else(|| palette(&chart.palette, index));
                let slice_colors = (0..xs.len())
                    .map(|k| {
                        ser.point_fills
                            .iter()
                            .find(|(at, _)| *at == k)
                            .map_or_else(|| palette(&chart.palette, k), |(_, c)| *c)
                    })
                    .collect();
                let _ = i;
                series.push(DrawnSeries {
                    name,
                    color,
                    line_color: ser.line.unwrap_or(color),
                    line_width: ser.line_width.unwrap_or(2.25),
                    no_line: ser.no_line,
                    no_fill: ser.no_fill,
                    marker: ser.marker,
                    smooth: ser.smooth,
                    labels: ser.labels,
                    xs,
                    ys,
                    slice_colors,
                    index,
                });
            }
            for (slot, id) in plot.axes.iter().take(3).enumerate() {
                axis_ids[slot].get_or_insert(*id);
            }
            plots.push(DrawnPlot {
                kind: plot.kind,
                series,
                secondary,
                categories,
                labels,
            });
        }
        let format_of = |axis: &ChartAxis| {
            Arc::new(match &axis.format {
                Some(code) => Format::parse(code),
                None => source_format
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(Format::general),
            })
        };
        let drawn = |axis: &ChartAxis| DrawnAxis {
            kind: axis.kind,
            side: axis.side,
            deleted: axis.deleted,
            reversed: axis.reversed,
            min: axis.min,
            max: axis.max,
            major_unit: axis.major_unit,
            log: axis.log_base.is_some(),
            gridlines: axis.gridlines,
            title: axis.title.clone(),
            format: format_of(axis),
            labels: axis.labels,
        };
        // Which axes the first plot is measured on.
        let first = chart.plots.first();
        let pick = |kinds: &[AxisKind]| {
            first.and_then(|plot| {
                plot.axes
                    .iter()
                    .filter_map(|id| chart.axes.iter().find(|a| a.id == *id))
                    .find(|a| kinds.contains(&a.kind))
            })
        };
        let x = pick(&[AxisKind::Category, AxisKind::Date]).or_else(|| {
            // A scatter chart has two value axes: the first is x.
            first.and_then(|plot| {
                plot.axes
                    .first()
                    .and_then(|id| chart.axes.iter().find(|a| a.id == *id))
            })
        });
        let scatter = matches!(first.map(|p| p.kind), Some(PlotKind::Scatter));
        let y = if scatter {
            first.and_then(|plot| {
                plot.axes
                    .get(1)
                    .and_then(|id| chart.axes.iter().find(|a| a.id == *id))
            })
        } else {
            pick(&[AxisKind::Value])
        };
        let y2 = chart.plots.iter().skip(1).find_map(|plot| {
            let axis = plot
                .axes
                .iter()
                .filter_map(|id| chart.axes.iter().find(|a| a.id == *id))
                .find(|a| a.kind == AxisKind::Value)?;
            (Some(axis.id) != y.map(|a| a.id)).then_some(axis)
        });
        DrawnChart {
            title: match chart.title.as_deref() {
                // A title with no text of its own is the name of the series.
                Some("") => plots
                    .first()
                    .and_then(|p: &DrawnPlot| p.series.first())
                    .and_then(|s| s.name.clone()),
                other => other.map(clip),
            },
            plots,
            x: x.map(drawn),
            y: y.map(drawn),
            y2: y2.map(drawn),
            legend: chart.legend,
            background: chart.background,
            plot_background: chart.plot_background,
            not_shown,
            date1904: self.date1904,
        }
    }
}

/// The colour of series `index`: the palette in turn, then darker and
/// lighter turns of it.
pub fn palette(colors: &[Rgba], index: usize) -> Rgba {
    const FALLBACK: [Rgba; 6] = [
        [0x44, 0x72, 0xc4, 0xff],
        [0xed, 0x7d, 0x31, 0xff],
        [0xa5, 0xa5, 0xa5, 0xff],
        [0xff, 0xc0, 0x00, 0xff],
        [0x5b, 0x9b, 0xd5, 0xff],
        [0x70, 0xad, 0x47, 0xff],
    ];
    let colors = if colors.is_empty() {
        &FALLBACK[..]
    } else {
        colors
    };
    let base = colors[index % colors.len()];
    let turn = index / colors.len();
    let mix = |to: f64, share: f64| {
        let channel = |i: usize| {
            (f64::from(base[i]) * (1.0 - share) + to * share)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        [channel(0), channel(1), channel(2), 0xff]
    };
    match turn {
        0 => base,
        t if t % 2 == 1 => mix(0.0, 0.35),
        _ => mix(255.0, 0.4),
    }
}

/// At most [`MAX_POINTS`] of the points, in order. A line keeps the
/// lowest and highest of each stretch so spikes survive; the others take
/// every nth.
fn thin(xs: &[f64], ys: &[f64], keep_extremes: bool) -> (Vec<f64>, Vec<f64>) {
    let n = xs.len().min(ys.len());
    if n <= MAX_POINTS {
        return (xs[..n].to_vec(), ys[..n].to_vec());
    }
    if !keep_extremes {
        let step = n.div_ceil(MAX_POINTS);
        let picked: Vec<usize> = (0..n).step_by(step).collect();
        return (
            picked.iter().map(|&i| xs[i]).collect(),
            picked.iter().map(|&i| ys[i]).collect(),
        );
    }
    let buckets = MAX_POINTS / 2;
    let (mut out_x, mut out_y) = (
        Vec::with_capacity(MAX_POINTS),
        Vec::with_capacity(MAX_POINTS),
    );
    for b in 0..buckets {
        let (from, to) = (b * n / buckets, ((b + 1) * n / buckets).min(n));
        let mut low: Option<usize> = None;
        let mut high: Option<usize> = None;
        for i in from..to {
            if ys[i].is_nan() {
                continue;
            }
            if low.is_none_or(|l| ys[i] < ys[l]) {
                low = Some(i);
            }
            if high.is_none_or(|h| ys[i] > ys[h]) {
                high = Some(i);
            }
        }
        match (low, high) {
            (Some(l), Some(h)) => {
                let (first, second) = if l <= h { (l, h) } else { (h, l) };
                out_x.push(xs[first]);
                out_y.push(ys[first]);
                if second != first {
                    out_x.push(xs[second]);
                    out_y.push(ys[second]);
                }
            }
            // A stretch of gaps stays one.
            _ if from < to => {
                out_x.push(xs[from]);
                out_y.push(f64::NAN);
            }
            _ => {}
        }
    }
    (out_x, out_y)
}

/// A chart's plot of one workbook cell range, for tests and tools: the
/// sheet's own range of numbers.
pub fn cell_source(formula: &str) -> Source {
    Source {
        formula: Some(formula.to_owned()),
        ..Source::default()
    }
}

impl Chart {
    /// With sheet `from`'s name in its references changed to `to`, or, with
    /// `None`, those references made `#REF!`.
    pub fn retarget(&mut self, from: &str, to: Option<&str>) {
        let mut sources: Vec<&mut Source> = Vec::new();
        for plot in &mut self.plots {
            for series in &mut plot.series {
                sources.extend(series.name.as_mut());
                sources.extend(series.categories.as_mut());
                sources.extend(series.values.as_mut());
            }
        }
        for source in sources {
            if let Some(formula) = &source.formula {
                source.formula = Some(crate::sheets::retarget(formula, from, to));
            }
        }
    }

    /// The cell ranges the chart plots, for finding out whether it is over
    /// some rows or columns.
    pub fn references(&self) -> Vec<(String, CellRange)> {
        let mut out = Vec::new();
        for plot in &self.plots {
            for series in &plot.series {
                for source in [&series.name, &series.categories, &series.values]
                    .into_iter()
                    .flatten()
                {
                    if let Some(formula) = &source.formula {
                        out.extend(parse_areas(formula));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, SheetKind};

    fn book() -> Workbook {
        let mut sheet = SheetBuilder::new("売上", SheetKind::Worksheet);
        for (row, (name, a, b)) in [
            ("一月", 10.0, 3.0),
            ("二月", 20.0, 6.0),
            ("三月", 30.0, 9.0),
        ]
        .into_iter()
        .enumerate()
        {
            let row = row as u32 + 1;
            sheet.push(row, Cell::new(0, Value::Text(name.into())));
            sheet.push(row, Cell::new(1, Value::Number(a)));
            sheet.push(row, Cell::new(2, Value::Number(b)));
        }
        sheet.push(0, Cell::new(1, Value::Text("甲".into())));
        sheet.push(0, Cell::new(2, Value::Text("乙".into())));
        Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        }
    }

    fn series(values: &str) -> Series {
        Series {
            name: Some(cell_source("売上!$B$1")),
            categories: Some(cell_source("売上!$A$2:$A$4")),
            values: Some(cell_source(values)),
            ..Series::default()
        }
    }

    fn chart(series: Vec<Series>) -> Chart {
        Chart {
            plots: vec![Plot {
                kind: PlotKind::Bar {
                    horizontal: false,
                    grouping: Grouping::Clustered,
                    gap: 150.0,
                    overlap: 0.0,
                },
                series,
                axes: vec![1, 2],
            }],
            axes: vec![
                ChartAxis {
                    id: 1,
                    kind: AxisKind::Category,
                    side: AxisSide::Bottom,
                    deleted: false,
                    reversed: false,
                    min: None,
                    max: None,
                    major_unit: None,
                    log_base: None,
                    gridlines: false,
                    title: None,
                    format: None,
                    labels: true,
                },
                ChartAxis {
                    id: 2,
                    kind: AxisKind::Value,
                    side: AxisSide::Left,
                    deleted: false,
                    reversed: false,
                    min: None,
                    max: None,
                    major_unit: None,
                    log_base: None,
                    gridlines: true,
                    title: None,
                    format: None,
                    labels: true,
                },
            ],
            visible_only: true,
            ..Chart::default()
        }
    }

    #[test]
    fn values_come_from_the_cells_and_follow_them() {
        let mut workbook = book();
        let chart = chart(vec![series("売上!$B$2:$B$4")]);
        let drawn = workbook.resolve_chart(&chart);
        let plot = &drawn.plots[0];
        assert_eq!(plot.series[0].ys, [10.0, 20.0, 30.0]);
        assert_eq!(plot.series[0].name.as_deref(), Some("甲"));
        assert_eq!(plot.labels[1], (1, "二月".to_owned()));
        assert_eq!(plot.categories, 3);
        workbook.set_cells(
            0,
            vec![(CellRef::new(2, 1), Some(Cell::new(1, Value::Number(99.0))))],
        );
        let again = workbook.resolve_chart(&chart);
        assert_eq!(again.plots[0].series[0].ys, [10.0, 99.0, 30.0]);
    }

    #[test]
    fn the_saved_values_stand_in_for_a_reference_that_cannot_be_followed() {
        let workbook = book();
        let mut ser = series("Gone!$B$2:$B$4");
        ser.values = Some(Source {
            formula: Some("Gone!$B$2:$B$4".into()),
            cached: vec![Value::Number(1.0), Value::Empty, Value::Number(3.0)],
            format: None,
        });
        let drawn = workbook.resolve_chart(&chart(vec![ser]));
        let ys = &drawn.plots[0].series[0].ys;
        assert_eq!(ys[0], 1.0);
        assert!(ys[1].is_nan());
        assert_eq!(ys[2], 3.0);
    }

    #[test]
    fn hidden_rows_are_not_plotted_unless_the_chart_says_so() {
        let mut workbook = book();
        workbook.sheets[0].row_sizes.set(2, 3, 0.0);
        let mut c = chart(vec![series("売上!$B$2:$B$4")]);
        assert_eq!(
            workbook.resolve_chart(&c).plots[0].series[0].ys,
            [10.0, 30.0]
        );
        c.visible_only = false;
        assert_eq!(
            workbook.resolve_chart(&c).plots[0].series[0].ys,
            [10.0, 20.0, 30.0]
        );
    }

    #[test]
    fn several_areas_make_one_series() {
        let workbook = book();
        let c = chart(vec![series("(売上!$B$2:$B$3,売上!$B$4)")]);
        assert_eq!(
            workbook.resolve_chart(&c).plots[0].series[0].ys,
            [10.0, 20.0, 30.0]
        );
        assert_eq!(parse_areas("'a,b'!A1:A2,c!B1").len(), 2);
    }

    #[test]
    fn too_many_series_and_points_are_cut_or_thinned() {
        let workbook = book();
        let many: Vec<Series> = (0..100).map(|_| series("売上!$B$2:$B$4")).collect();
        let drawn = workbook.resolve_chart(&chart(many));
        assert_eq!(drawn.plots[0].series.len(), MAX_SERIES);
        assert!(drawn.not_shown.contains(&"series limit".to_owned()));
        // A hundred thousand points, with a spike, in a line.
        let mut sheet = SheetBuilder::new("大", SheetKind::Worksheet);
        for row in 0..100_000u32 {
            let v = if row == 54_321 {
                1e6
            } else {
                f64::from(row % 100)
            };
            sheet.push(row, Cell::new(0, Value::Number(v)));
        }
        let big = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        let mut c = chart(vec![Series {
            values: Some(cell_source("大!$A$1:$A$100000")),
            ..Series::default()
        }]);
        c.plots[0].kind = PlotKind::Line {
            grouping: Grouping::Clustered,
        };
        let drawn = big.resolve_chart(&c);
        let ser = &drawn.plots[0].series[0];
        assert!(ser.ys.len() <= MAX_POINTS);
        assert_eq!(ser.xs.len(), ser.ys.len());
        assert!(ser.ys.contains(&1e6), "the spike survives");
        assert!(ser.xs.windows(2).all(|w| w[0] <= w[1]));
        // Bars take every nth instead.
        c.plots[0].kind = PlotKind::Bar {
            horizontal: false,
            grouping: Grouping::Clustered,
            gap: 150.0,
            overlap: 0.0,
        };
        let drawn = big.resolve_chart(&c);
        assert!(drawn.plots[0].series[0].ys.len() <= MAX_POINTS);
        assert!(drawn.plots[0].labels.len() <= MAX_LABELS);
    }

    #[test]
    fn the_budget_for_reading_cells_is_shared_by_a_chart() {
        let mut sheet = SheetBuilder::new("大", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Number(1.0)));
        let big = Workbook {
            sheets: vec![sheet.finish()],
            ..Workbook::default()
        };
        // 64 series of a million cells each: bounded by the chart's budget.
        let series: Vec<Series> = (0..64)
            .map(|_| Series {
                values: Some(cell_source("大!$A$1:$A$1000000")),
                ..Series::default()
            })
            .collect();
        let drawn = big.resolve_chart(&chart(series));
        let points: usize = drawn.plots[0].series.iter().map(|s| s.ys.len()).sum();
        assert!(points <= MAX_TOTAL_READ);
    }

    #[test]
    fn renaming_a_sheet_changes_the_references() {
        let mut c = chart(vec![series("売上!$B$2:$B$4")]);
        c.retarget("売上", Some("Sales 2026"));
        assert_eq!(
            c.plots[0].series[0]
                .values
                .as_ref()
                .unwrap()
                .formula
                .as_deref(),
            Some("'Sales 2026'!$B$2:$B$4")
        );
        assert_eq!(c.references().len(), 3);
    }

    #[test]
    fn palette_cycles_and_varies() {
        let colors = [[1, 2, 3, 255], [4, 5, 6, 255]];
        assert_eq!(palette(&colors, 1), colors[1]);
        assert_ne!(palette(&colors, 2), colors[0]);
        assert_ne!(palette(&colors, 4), palette(&colors, 2));
        assert_eq!(palette(&[], 0)[3], 255);
    }
}
