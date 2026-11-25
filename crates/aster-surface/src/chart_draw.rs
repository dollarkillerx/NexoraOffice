//! Drawing a chart: title, legend, axes with tick labels and gridlines,
//! and the series as bars, lines, areas, slices or points.
//!
//! A chart is drawn once into a pixmap of its own size; the sheet draws
//! that pixmap at the chart's place for as long as the chart's size, data
//! and colours stay the same. All that is drawn is bounded by what
//! [`aster_sheets::chart`] resolved: at most 64 series of 10,000 points,
//! and labels thinned out to what fits.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use aster_sheets::chart::{
    DrawnAxis, DrawnChart, DrawnPlot, DrawnSeries, Grouping, LegendPos, MarkerSymbol, PlotKind,
};
use aster_text::{Align, Family, Paragraph, Shaper, Style as TextStyle};
use vello_cpu::kurbo::{
    Affine, Arc as KArc, BezPath, Cap, Circle, Join, Point, Rect, Shape, Stroke,
};
use vello_cpu::peniko::Color;
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::preview::draw_text;
use crate::sheet_view::lay_out;
use crate::text_surface::Rgba8;

/// The window's colours a chart takes where the file names none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChartStyle {
    pub background: Rgba8,
    pub text: Rgba8,
    pub label: Rgba8,
    pub grid: Rgba8,
}

/// The words a chart writes itself.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChartTexts<'a> {
    /// A series without a name, with `{n}` for its number.
    pub series: &'a str,
    /// The note for what is not drawn.
    pub not_shown: &'a str,
}

/// Most data labels drawn in one chart.
const MAX_DATA_LABELS: usize = 300;
/// Most legend entries.
const MAX_LEGEND: usize = 64;
/// Longest side of the pixmap a chart is drawn in.
pub(crate) const MAX_SIDE: f64 = 4096.0;

fn paint(color: Rgba8) -> Color {
    Color::from_rgba8(color[0], color[1], color[2], color[3])
}

struct Painter<'a> {
    ctx: RenderContext,
    shaper: &'a mut Shaper,
    resources: &'a mut Resources,
    glyphs: Vec<Glyph>,
    scale: f32,
    texts: HashMap<(String, u32, bool, Rgba8), Arc<Paragraph>>,
}

impl Painter<'_> {
    fn s(&self) -> f64 {
        f64::from(self.scale)
    }

    /// A line of text, `size` logical pixels, laid out once.
    fn text(&mut self, text: &str, size: f32, bold: bool, color: Rgba8) -> Arc<Paragraph> {
        let key = (text.to_owned(), size.to_bits(), bold, color);
        if let Some(found) = self.texts.get(&key) {
            return found.clone();
        }
        let paragraph = Arc::new(lay_out(
            self.shaper,
            text,
            TextStyle {
                family: Family::SystemUi,
                size,
                color,
                bold,
                italic: false,
                underline: false,
                strike: false,
                rise: 0.0,
            },
            self.scale,
            Align::Start,
            None,
        ));
        if self.texts.len() > 4_000 {
            self.texts.clear();
        }
        self.texts.insert(key, paragraph.clone());
        paragraph
    }

    fn put(&mut self, paragraph: &Paragraph, x: f64, y: f64) {
        self.put_with(paragraph, Affine::translate((x, y)));
    }

    /// Centred on `(x, y)`, turned a quarter to the left.
    fn put_rotated(&mut self, paragraph: &Paragraph, x: f64, y: f64) {
        let (w, h) = (f64::from(paragraph.width()), f64::from(paragraph.height()));
        self.put_with(
            paragraph,
            Affine::translate((x, y))
                * Affine::rotate(-FRAC_PI_2)
                * Affine::translate((-w / 2.0, -h / 2.0)),
        );
    }

    fn put_with(&mut self, paragraph: &Paragraph, transform: Affine) {
        self.ctx.set_transform(transform);
        draw_text(
            &mut self.ctx,
            self.resources,
            paragraph,
            0.0,
            paragraph.height(),
            self.scale,
            &mut self.glyphs,
        );
        self.ctx.set_transform(Affine::IDENTITY);
    }

    fn fill(&mut self, shape: &impl Shape, color: Rgba8) {
        self.ctx.set_paint(paint(color));
        self.ctx.fill_path(&shape.to_path(0.1));
    }

    fn stroke(&mut self, path: &BezPath, color: Rgba8, width: f64) {
        self.ctx.set_paint(paint(color));
        self.ctx.set_stroke(
            Stroke::new(width.max(1.0))
                .with_caps(Cap::Round)
                .with_join(Join::Round),
        );
        self.ctx.stroke_path(path);
    }

    fn line(&mut self, a: (f64, f64), b: (f64, f64), color: Rgba8, width: f64) {
        let mut path = BezPath::new();
        path.move_to(a);
        path.line_to(b);
        self.ctx.set_paint(paint(color));
        self.ctx.set_stroke(Stroke::new(width.max(1.0)));
        self.ctx.stroke_path(&path);
    }
}

/// Where values lie on an axis.
#[derive(Clone, Copy, Debug)]
struct Scale {
    lo: f64,
    hi: f64,
    step: f64,
    reversed: bool,
}

impl Scale {
    /// 0 to 1 along the axis, from its start.
    fn share(&self, v: f64) -> f64 {
        let t = (v - self.lo) / (self.hi - self.lo);
        if self.reversed { 1.0 - t } else { t }
    }

    fn ticks(&self) -> Vec<f64> {
        let mut out = Vec::new();
        let count = ((self.hi - self.lo) / self.step).round() as usize;
        for i in 0..=count.min(200) {
            out.push(clean(self.lo + self.step * i as f64));
        }
        out
    }
}

/// A value without the noise of repeated addition: 0.30000000000000004 is
/// 0.3.
fn clean(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let digits = 10 - v.abs().log10().floor() as i32;
    if !(0..=300).contains(&digits) {
        return v;
    }
    let factor = 10f64.powi(digits);
    (v * factor).round() / factor
}

/// A scale from the extremes of the data: a step of 1, 2 or 5 times a power
/// of ten, and ends on steps, as Excel chooses when the file says nothing.
/// `from_zero` starts a scale of non-negative data at 0 unless that would
/// squash it.
fn nice_scale(lo: f64, hi: f64, axis: Option<&DrawnAxis>, from_zero: bool) -> Scale {
    let (mut lo, mut hi) = if lo.is_finite() && hi.is_finite() {
        (lo, hi)
    } else {
        (0.0, 1.0)
    };
    // Excel leaves a twentieth of the span above the highest value and
    // below the lowest, unless the scale starts at zero.
    let (mut pad_lo, pad_hi) = ((hi - lo) * 0.05, (hi - lo) * 0.05);
    if from_zero && lo >= 0.0 && lo < hi * 5.0 / 6.0 {
        lo = 0.0;
        pad_lo = 0.0;
    }
    let mut pad_hi = pad_hi;
    if from_zero && hi <= 0.0 && hi > lo * 5.0 / 6.0 {
        hi = 0.0;
        pad_hi = 0.0;
    }
    let (fixed_lo, fixed_hi) = (axis.and_then(|a| a.min), axis.and_then(|a| a.max));
    if lo == hi {
        lo -= 1.0;
        hi += 1.0;
    }
    let span = hi - lo;
    let target = 6.0;
    let mut step = axis.and_then(|a| a.major_unit).unwrap_or_else(|| {
        let raw = span / target;
        let magnitude = 10f64.powf(raw.log10().floor());
        let fraction = raw / magnitude;
        let nice = if fraction <= 1.0 {
            1.0
        } else if fraction <= 2.0 {
            2.0
        } else if fraction <= 5.0 {
            5.0
        } else {
            10.0
        };
        nice * magnitude
    });
    let mut low = fixed_lo.unwrap_or(((lo - pad_lo) / step).floor() * step);
    let mut high = fixed_hi.unwrap_or(((hi + pad_hi) / step).ceil() * step);
    if high <= low {
        high = low + step;
    }
    // A step far smaller than the span would make a thousand ticks.
    if (high - low) / step > 100.0 {
        step = (high - low) / 100.0;
    }
    if !step.is_finite() || step <= 0.0 {
        step = 1.0;
        low = 0.0;
        high = 1.0;
    }
    Scale {
        lo: low,
        hi: high,
        step,
        reversed: axis.is_some_and(|a| a.reversed),
    }
}

/// The lowest and highest value a set of plots reaches, with stacking.
fn extent(plots: &[&DrawnPlot], x_axis: bool) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut see = |v: f64| {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    };
    for plot in plots {
        match plot.kind {
            PlotKind::Scatter => {
                for series in &plot.series {
                    let values = if x_axis { &series.xs } else { &series.ys };
                    values.iter().for_each(|v| see(*v));
                }
            }
            _ if x_axis => {}
            PlotKind::Bar { grouping, .. }
            | PlotKind::Line { grouping }
            | PlotKind::Area { grouping }
                if grouping != Grouping::Clustered =>
            {
                if grouping == Grouping::Percent {
                    see(0.0);
                    see(1.0);
                    continue;
                }
                let len = plot.series.iter().map(|s| s.ys.len()).max().unwrap_or(0);
                for k in 0..len {
                    let (mut up, mut down) = (0.0, 0.0);
                    for series in &plot.series {
                        let v = series.ys.get(k).copied().unwrap_or(f64::NAN);
                        if v.is_finite() {
                            if v >= 0.0 { up += v } else { down += v }
                        }
                    }
                    see(up);
                    see(down);
                }
                see(0.0);
            }
            PlotKind::Bar { .. } | PlotKind::Area { .. } => {
                for series in &plot.series {
                    series.ys.iter().for_each(|v| see(*v));
                }
                see(0.0);
            }
            _ => {
                for series in &plot.series {
                    series.ys.iter().for_each(|v| see(*v));
                }
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

fn marker_path(symbol: MarkerSymbol, at: Point, size: f64) -> BezPath {
    let r = size / 2.0;
    match symbol {
        MarkerSymbol::Square => Rect::new(at.x - r, at.y - r, at.x + r, at.y + r).to_path(0.1),
        MarkerSymbol::Diamond => {
            let mut p = BezPath::new();
            p.move_to((at.x, at.y - r * 1.2));
            p.line_to((at.x + r * 1.2, at.y));
            p.line_to((at.x, at.y + r * 1.2));
            p.line_to((at.x - r * 1.2, at.y));
            p.close_path();
            p
        }
        MarkerSymbol::Triangle => {
            let mut p = BezPath::new();
            p.move_to((at.x, at.y - r * 1.1));
            p.line_to((at.x + r * 1.1, at.y + r * 0.9));
            p.line_to((at.x - r * 1.1, at.y + r * 0.9));
            p.close_path();
            p
        }
        _ => Circle::new(at, r).to_path(0.1),
    }
}

/// A sector of a ring between `inner` and `outer` radii, from `start`
/// radians, `sweep` clockwise.
fn sector(center: Point, outer: f64, inner: f64, start: f64, sweep: f64) -> BezPath {
    let at = |r: f64, a: f64| Point::new(center.x + r * a.cos(), center.y + r * a.sin());
    let mut path = BezPath::new();
    if inner > 0.5 {
        path.move_to(at(inner, start));
        path.line_to(at(outer, start));
    } else {
        path.move_to(center);
        path.line_to(at(outer, start));
    }
    path.extend(KArc::new(center, (outer, outer), start, sweep, 0.0).append_iter(0.1));
    if inner > 0.5 {
        path.line_to(at(inner, start + sweep));
        path.extend(KArc::new(center, (inner, inner), start + sweep, -sweep, 0.0).append_iter(0.1));
    }
    path.close_path();
    path
}

/// A smooth curve through `points` (Catmull-Rom as Béziers).
fn smooth(points: &[Point]) -> BezPath {
    let mut path = BezPath::new();
    let Some(first) = points.first() else {
        return path;
    };
    path.move_to(*first);
    for i in 0..points.len().saturating_sub(1) {
        let p0 = points[i.saturating_sub(1)];
        let (p1, p2) = (points[i], points[i + 1]);
        let p3 = points[(i + 2).min(points.len() - 1)];
        let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
        path.curve_to(c1, c2, p2);
    }
    path
}

fn name_of(series: &DrawnSeries, texts: &ChartTexts<'_>) -> String {
    series
        .name
        .clone()
        .unwrap_or_else(|| texts.series.replace("{n}", &(series.index + 1).to_string()))
}

/// One entry of the legend.
struct Entry {
    text: String,
    color: Rgba8,
    /// A line with a marker, for line and scatter series.
    line: bool,
}

fn legend_entries(chart: &DrawnChart, texts: &ChartTexts<'_>) -> Vec<Entry> {
    let mut out = Vec::new();
    // A pie's legend names its slices.
    if let [plot] = &chart.plots[..]
        && matches!(plot.kind, PlotKind::Pie { .. })
        && let Some(series) = plot.series.first()
    {
        for (k, &x) in series.xs.iter().enumerate().take(MAX_LEGEND) {
            let text = plot
                .labels
                .iter()
                .find(|(at, _)| *at == x as usize)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| (x as usize + 1).to_string());
            out.push(Entry {
                text,
                color: series.slice_colors.get(k).copied().unwrap_or(series.color),
                line: false,
            });
        }
        return out;
    }
    for plot in &chart.plots {
        for series in &plot.series {
            if out.len() >= MAX_LEGEND {
                return out;
            }
            out.push(Entry {
                text: name_of(series, texts),
                color: series.color,
                line: matches!(plot.kind, PlotKind::Line { .. })
                    || (matches!(plot.kind, PlotKind::Scatter) && !series.no_line),
            });
        }
    }
    out
}

/// Draw `chart` into a new pixmap of `width` by `height` physical pixels.
pub(crate) fn render_chart(
    shaper: &mut Shaper,
    resources: &mut Resources,
    chart: &DrawnChart,
    (width, height): (u16, u16),
    scale: f32,
    style: &ChartStyle,
    texts: &ChartTexts<'_>,
) -> Pixmap {
    let (w, h) = (f64::from(width), f64::from(height));
    let mut p = Painter {
        ctx: RenderContext::new(width, height),
        shaper,
        resources,
        glyphs: Vec::new(),
        scale,
        texts: HashMap::new(),
    };
    let s = p.s();
    let background = chart.background.unwrap_or(style.background);
    p.fill(&Rect::new(0.0, 0.0, w, h), background);
    p.ctx.set_paint(paint(style.grid));
    p.ctx.set_stroke(Stroke::new(s.max(1.0)));
    p.ctx
        .stroke_rect(&Rect::new(0.0, 0.0, w, h).inset(-s.max(1.0) / 2.0));
    // Text on a background of the file's own takes the colour that reads
    // on it.
    let on_dark = chart
        .background
        .is_some_and(|c| u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]) < 3 * 110);
    let (text_color, label_color) = if chart.background.is_some() {
        if on_dark {
            ([0xf2; 4], [0xd0, 0xd0, 0xd0, 0xff])
        } else {
            ([0x26, 0x26, 0x26, 0xff], [0x59, 0x59, 0x59, 0xff])
        }
    } else {
        (style.text, style.label)
    };
    let mut area = Rect::new(8.0 * s, 8.0 * s, w - 8.0 * s, h - 8.0 * s);
    if area.width() < 20.0 || area.height() < 20.0 {
        return finish(p, width, height);
    }
    if let Some(title) = &chart.title {
        let title = p.text(title, 14.0, true, text_color);
        let shown = f64::from(title.width()).min(area.width());
        p.put(&title, area.x0 + (area.width() - shown) / 2.0, area.y0);
        area.y0 += f64::from(title.height()) + 6.0 * s;
    }
    // A note for what is not drawn, at the bottom.
    if !chart.not_shown.is_empty() {
        let note = p.text(texts.not_shown, 10.0, false, label_color);
        let y = area.y1 - f64::from(note.height());
        if y > area.y0 + 20.0 * s {
            p.put(&note, area.x0, y);
            area.y1 = y - 4.0 * s;
        }
    }
    let drawable = chart.plots.iter().any(|plot| !plot.series.is_empty());
    if !drawable {
        return finish(p, width, height);
    }
    // Legend.
    if let Some(position) = chart.legend {
        let entries = legend_entries(chart, texts);
        if !entries.is_empty() {
            area = draw_legend(&mut p, entries, position, area, label_color);
        }
    }
    if area.width() < 20.0 || area.height() < 20.0 {
        return finish(p, width, height);
    }
    let pie = chart
        .plots
        .iter()
        .find(|plot| matches!(plot.kind, PlotKind::Pie { .. }));
    if let Some(plot) = pie {
        draw_pie(&mut p, plot, area, background, text_color);
    } else {
        draw_axes_chart(&mut p, chart, area, style, text_color, label_color);
    }
    finish(p, width, height)
}

fn finish(mut p: Painter<'_>, width: u16, height: u16) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height);
    p.ctx.flush();
    p.ctx.render(&mut pixmap, p.resources);
    pixmap
}

fn draw_legend(
    p: &mut Painter<'_>,
    entries: Vec<Entry>,
    position: LegendPos,
    area: Rect,
    color: Rgba8,
) -> Rect {
    let s = p.s();
    let swatch = 10.0 * s;
    let gap = 4.0 * s;
    let spread = 14.0 * s;
    let items: Vec<(Arc<Paragraph>, Rgba8, bool)> = entries
        .into_iter()
        .map(|e| (p.text(&e.text, 11.0, false, color), e.color, e.line))
        .collect();
    let row_h = items
        .iter()
        .map(|(t, _, _)| f64::from(t.height()))
        .fold(swatch, f64::max)
        + 4.0 * s;
    let item_w = |t: &Paragraph| swatch + gap + f64::from(t.width());
    let mark = |p: &mut Painter<'_>, x: f64, y: f64, color: Rgba8, line: bool| {
        if line {
            p.line(
                (x, y + swatch / 2.0),
                (x + swatch, y + swatch / 2.0),
                color,
                2.0 * s,
            );
        } else {
            p.fill(&Rect::new(x, y, x + swatch, y + swatch), color);
        }
    };
    match position {
        LegendPos::Top | LegendPos::Bottom => {
            // Rows that fit the width.
            let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
            let mut used = 0.0;
            for (i, (t, _, _)) in items.iter().enumerate() {
                let need = item_w(t) + if used > 0.0 { spread } else { 0.0 };
                if used + need > area.width() && used > 0.0 {
                    rows.push(Vec::new());
                    used = 0.0;
                }
                used += item_w(t) + if used > 0.0 { spread } else { 0.0 };
                rows.last_mut().expect("a row").push(i);
            }
            let max_rows = ((area.height() / 3.0) / row_h).floor().max(1.0) as usize;
            rows.truncate(max_rows);
            let total_h = row_h * rows.len() as f64;
            let top = if position == LegendPos::Top {
                area.y0
            } else {
                area.y1 - total_h
            };
            for (r, row) in rows.iter().enumerate() {
                let widths: f64 = row.iter().map(|i| item_w(&items[*i].0)).sum::<f64>()
                    + spread * (row.len().saturating_sub(1)) as f64;
                let mut x = area.x0 + ((area.width() - widths) / 2.0).max(0.0);
                let y = top + row_h * r as f64;
                for &i in row {
                    let (t, c, line) = &items[i];
                    mark(p, x, y + (row_h - swatch) / 2.0, *c, *line);
                    p.put(
                        t,
                        x + swatch + gap,
                        y + (row_h - f64::from(t.height())) / 2.0,
                    );
                    x += item_w(t) + spread;
                }
            }
            if position == LegendPos::Top {
                Rect::new(area.x0, area.y0 + total_h + 4.0 * s, area.x1, area.y1)
            } else {
                Rect::new(area.x0, area.y0, area.x1, area.y1 - total_h - 4.0 * s)
            }
        }
        LegendPos::Left | LegendPos::Right => {
            let width = (items.iter().map(|(t, _, _)| item_w(t)).fold(0.0, f64::max))
                .min(area.width() / 3.0);
            let fit = (area.height() / row_h).floor().max(1.0) as usize;
            let shown = items.len().min(fit);
            let top = area.y0 + ((area.height() - row_h * shown as f64) / 2.0).max(0.0);
            let x = if position == LegendPos::Left {
                area.x0
            } else {
                area.x1 - width
            };
            p.ctx
                .push_clip_layer(&Rect::new(x, area.y0, x + width, area.y1).to_path(0.1));
            for (i, (t, c, line)) in items.iter().take(shown).enumerate() {
                let y = top + row_h * i as f64;
                mark(p, x, y + (row_h - swatch) / 2.0, *c, *line);
                p.put(
                    t,
                    x + swatch + gap,
                    y + (row_h - f64::from(t.height())) / 2.0,
                );
            }
            p.ctx.pop_layer();
            if position == LegendPos::Left {
                Rect::new(area.x0 + width + 8.0 * s, area.y0, area.x1, area.y1)
            } else {
                Rect::new(area.x0, area.y0, area.x1 - width - 8.0 * s, area.y1)
            }
        }
    }
}

fn draw_pie(
    p: &mut Painter<'_>,
    plot: &DrawnPlot,
    area: Rect,
    background: Rgba8,
    text_color: Rgba8,
) {
    let s = p.s();
    let PlotKind::Pie { hole, first_angle } = plot.kind else {
        return;
    };
    // Room for labels outside the slices that are too narrow for one.
    let labelled = plot
        .series
        .iter()
        .any(|x| x.labels.value || x.labels.percent || x.labels.category);
    let room = if labelled { 0.8 } else { 1.0 };
    let radius = ((area.width().min(area.height()) / 2.0 - 4.0 * s) * room).max(4.0);
    let center = Point::new((area.x0 + area.x1) / 2.0, (area.y0 + area.y1) / 2.0);
    // A pie has one ring; a doughnut one per series.
    let rings: Vec<&DrawnSeries> = if hole.is_some() {
        plot.series.iter().take(8).collect()
    } else {
        plot.series.iter().take(1).collect()
    };
    let hole_r = radius * hole.unwrap_or(0.0);
    let ring_w = (radius - hole_r) / rings.len().max(1) as f64;
    let mut labels_drawn = 0usize;
    for (ring, series) in rings.iter().enumerate() {
        let outer = radius - ring_w * ring as f64;
        let inner = if hole.is_some() { outer - ring_w } else { 0.0 };
        let values: Vec<f64> = series
            .ys
            .iter()
            .map(|v| if v.is_finite() && *v > 0.0 { *v } else { 0.0 })
            .collect();
        let total: f64 = values.iter().sum();
        if total <= 0.0 {
            continue;
        }
        let mut angle = first_angle.to_radians() - FRAC_PI_2;
        for (k, v) in values.iter().enumerate() {
            let sweep = v / total * TAU;
            if sweep <= 0.0 {
                continue;
            }
            let color = series.slice_colors.get(k).copied().unwrap_or(series.color);
            let shape = sector(center, outer, inner, angle, sweep.min(TAU - 1e-6));
            p.fill(&shape, color);
            if values.len() > 1 {
                p.stroke(&shape, background, s.max(1.0));
            }
            // A label at the middle of the slice.
            let wants = series.labels.value || series.labels.percent || series.labels.category;
            if wants && labels_drawn < MAX_DATA_LABELS {
                let mut parts: Vec<String> = Vec::new();
                if series.labels.category
                    && let Some((_, name)) = plot
                        .labels
                        .iter()
                        .find(|(at, _)| *at == series.xs[k] as usize)
                {
                    parts.push(name.clone());
                }
                if series.labels.value {
                    parts.push(aster_sheets::format::general(*v));
                }
                if series.labels.percent {
                    parts.push(format!("{:.0}%", v / total * 100.0));
                }
                let middle = angle + sweep / 2.0;
                // Inside a wide slice or a ring; outside a narrow one.
                let inside = inner > 0.0 || sweep > 0.5;
                let r = if inside {
                    (outer + inner) / 2.0 * if inner > 0.0 { 1.0 } else { 0.68 }
                } else {
                    outer + 6.0 * s
                };
                let at = Point::new(center.x + r * middle.cos(), center.y + r * middle.sin());
                let on = if !inside {
                    text_color
                } else if luminance(color) > 0.5 {
                    [0x26, 0x26, 0x26, 0xff]
                } else {
                    [0xff; 4]
                };
                let label = p.text(&parts.join(" "), 11.0, false, on);
                let (lw, lh) = (f64::from(label.width()), f64::from(label.height()));
                let x = if inside {
                    at.x - lw / 2.0
                } else if middle.cos() >= 0.0 {
                    at.x
                } else {
                    at.x - lw
                };
                p.put(&label, x, at.y - lh / 2.0);
                labels_drawn += 1;
            }
            angle += sweep;
        }
    }
}

fn luminance(c: Rgba8) -> f64 {
    (0.299 * f64::from(c[0]) + 0.587 * f64::from(c[1]) + 0.114 * f64::from(c[2])) / 255.0
}

/// Everything but the pie: axes and what is plotted over them.
fn draw_axes_chart(
    p: &mut Painter<'_>,
    chart: &DrawnChart,
    area: Rect,
    style: &ChartStyle,
    text_color: Rgba8,
    label_color: Rgba8,
) {
    let s = p.s();
    let primary: Vec<&DrawnPlot> = chart.plots.iter().filter(|plot| !plot.secondary).collect();
    let secondary: Vec<&DrawnPlot> = chart.plots.iter().filter(|plot| plot.secondary).collect();
    let first = primary.first().copied().or(chart.plots.first());
    let Some(first) = first else {
        return;
    };
    let scatter = matches!(first.kind, PlotKind::Scatter);
    let horizontal = matches!(
        first.kind,
        PlotKind::Bar {
            horizontal: true,
            ..
        }
    );
    let categories = chart
        .plots
        .iter()
        .filter(|plot| !matches!(plot.kind, PlotKind::Scatter))
        .map(|plot| plot.categories)
        .max()
        .unwrap_or(0)
        .max(1);
    let percent = primary.iter().any(|plot| {
        matches!(
            plot.kind,
            PlotKind::Bar {
                grouping: Grouping::Percent,
                ..
            } | PlotKind::Line {
                grouping: Grouping::Percent
            } | PlotKind::Area {
                grouping: Grouping::Percent
            }
        )
    });
    // Scales of the value axes (and of x for a scatter chart).
    let (ylo, yhi) = extent(&primary, false).unwrap_or((0.0, 1.0));
    let mut y_scale = nice_scale(ylo, yhi, chart.y.as_ref(), true);
    if percent {
        // A percent stack always runs from nothing to the whole, unless
        // the file says otherwise.
        let axis = chart.y.as_ref();
        y_scale = Scale {
            lo: axis.and_then(|a| a.min).unwrap_or(0.0),
            hi: axis.and_then(|a| a.max).unwrap_or(1.0),
            step: axis.and_then(|a| a.major_unit).unwrap_or(0.1),
            reversed: axis.is_some_and(|a| a.reversed),
        };
        if y_scale.hi <= y_scale.lo
            || y_scale.step <= 0.0
            || (y_scale.hi - y_scale.lo) / y_scale.step > 100.0
        {
            y_scale = nice_scale(ylo, yhi, None, true);
        }
    }
    let x_scale = scatter.then(|| {
        let (lo, hi) = extent(&primary, true).unwrap_or((0.0, 1.0));
        nice_scale(lo, hi, chart.x.as_ref(), true)
    });
    let y2_scale = (!secondary.is_empty()).then(|| {
        let (lo, hi) = extent(&secondary, false).unwrap_or((0.0, 1.0));
        nice_scale(lo, hi, chart.y2.as_ref(), true)
    });
    let general = Arc::new(aster_sheets::Format::general());
    let value_format = |axis: &Option<DrawnAxis>, percent: bool| -> Arc<aster_sheets::Format> {
        match axis {
            Some(a) if percent && a.format.code().eq_ignore_ascii_case("general") => {
                Arc::new(aster_sheets::Format::parse("0%"))
            }
            Some(a) => a.format.clone(),
            None if percent => Arc::new(aster_sheets::Format::parse("0%")),
            None => general.clone(),
        }
    };
    let y_format = value_format(&chart.y, percent);
    let x_format = value_format(&chart.x, false);
    let y2_format = value_format(&chart.y2, false);
    let show = |axis: &Option<DrawnAxis>| axis.as_ref().is_none_or(|a| !a.deleted && a.labels);
    let axis_deleted = |axis: &Option<DrawnAxis>| axis.as_ref().is_some_and(|a| a.deleted);

    // The value axis is vertical (left), the category axis horizontal
    // (bottom); a bar chart on its side swaps them. A scatter chart has a
    // value axis each way.
    let label_size = 11.0;
    let tick_text = |p: &mut Painter<'_>,
                     scale: &Scale,
                     format: &aster_sheets::Format,
                     date: bool|
     -> Vec<(f64, Arc<Paragraph>)> {
        scale
            .ticks()
            .into_iter()
            .map(|v| {
                (
                    v,
                    p.text(&format.number(v, date), label_size, false, label_color),
                )
            })
            .collect()
    };
    let y_ticks = tick_text(p, &y_scale, &y_format, chart.date1904);
    let x_ticks = x_scale
        .as_ref()
        .map(|sc| tick_text(p, sc, &x_format, chart.date1904));
    let y2_ticks = y2_scale
        .as_ref()
        .map(|sc| tick_text(p, sc, &y2_format, chart.date1904));
    let widest = |ticks: &[(f64, Arc<Paragraph>)]| {
        ticks
            .iter()
            .map(|(_, t)| f64::from(t.width()))
            .fold(0.0, f64::max)
    };
    let tall = |ticks: &[(f64, Arc<Paragraph>)]| {
        ticks
            .iter()
            .map(|(_, t)| f64::from(t.height()))
            .fold(0.0, f64::max)
    };
    // Category labels.
    let cat_labels: Vec<(usize, Arc<Paragraph>)> = if scatter {
        Vec::new()
    } else {
        first
            .labels
            .iter()
            .chain(chart.plots.iter().flat_map(|plot| plot.labels.iter()))
            .take(aster_sheets::chart::MAX_LABELS)
            .map(|(k, text)| (*k, p.text(text, label_size, false, label_color)))
            .collect()
    };
    let cat_h = cat_labels.iter().map(|(_, t)| f64::from(t.height())).fold(
        f64::from(p.text("0", label_size, false, label_color).height()),
        f64::max,
    );
    let cat_w = cat_labels
        .iter()
        .map(|(_, t)| f64::from(t.width()))
        .fold(0.0, f64::max);
    let title_of = |axis: &Option<DrawnAxis>| axis.as_ref().and_then(|a| a.title.clone());
    let (vertical_axis_title, horizontal_axis_title) = if scatter {
        (title_of(&chart.y), title_of(&chart.x))
    } else if horizontal {
        (title_of(&chart.x), title_of(&chart.y))
    } else {
        (title_of(&chart.y), title_of(&chart.x))
    };
    let v_title = vertical_axis_title.map(|t| p.text(&t, 11.0, true, text_color));
    let h_title = horizontal_axis_title.map(|t| p.text(&t, 11.0, true, text_color));
    let y2_title = chart
        .y2
        .as_ref()
        .and_then(|a| a.title.clone())
        .map(|t| p.text(&t, 11.0, true, text_color));
    let gap = 4.0 * s;

    // Space the labels and titles need.
    let mut left = 0.0;
    let mut bottom = 0.0;
    let mut right = 0.0;
    if let Some(t) = &v_title {
        left += f64::from(t.height()) + gap * 2.0;
    }
    if let Some(t) = &h_title {
        bottom += f64::from(t.height()) + gap * 2.0;
    }
    if let Some(t) = &y2_title {
        right += f64::from(t.height()) + gap * 2.0;
    }
    if horizontal {
        if show(&chart.x) {
            left += cat_w.min(area.width() / 3.0) + gap * 2.0;
        }
        if show(&chart.y) {
            bottom += tall(&y_ticks) + gap * 2.0;
        }
    } else if scatter {
        if show(&chart.y) {
            left += widest(&y_ticks) + gap * 2.0;
        }
        if show(&chart.x)
            && let Some(ticks) = &x_ticks
        {
            bottom += tall(ticks) + gap * 2.0;
        }
    } else {
        if show(&chart.y) {
            left += widest(&y_ticks) + gap * 2.0;
        }
        if show(&chart.x) {
            bottom += cat_h + gap * 2.0;
        }
    }
    if let Some(ticks) = &y2_ticks
        && show(&chart.y2)
    {
        right += widest(ticks) + gap * 2.0;
    }
    let plot = Rect::new(
        area.x0 + left,
        area.y0 + 6.0 * s,
        (area.x1 - right - 6.0 * s).max(area.x0 + left + 1.0),
        (area.y1 - bottom).max(area.y0 + 6.0 * s + 1.0),
    );
    if plot.width() < 12.0 * s || plot.height() < 12.0 * s {
        return;
    }
    if let Some(fill) = chart.plot_background {
        p.fill(&plot, fill);
    }

    // Maps.
    let value_pos = |scale: &Scale, v: f64| -> f64 {
        let t = scale.share(v);
        if horizontal {
            plot.x0 + t * plot.width()
        } else {
            plot.y1 - t * plot.height()
        }
    };
    let slot = if horizontal {
        plot.height() / categories as f64
    } else {
        plot.width() / categories as f64
    };
    let category_reversed = chart.x.as_ref().is_some_and(|a| a.reversed) && !scatter;
    let category_center = |x: f64| -> f64 {
        let along = (x + 0.5) * slot;
        if horizontal {
            // The first category is at the bottom, as in Excel.
            if category_reversed {
                plot.y0 + along
            } else {
                plot.y1 - along
            }
        } else if category_reversed {
            plot.x1 - along
        } else {
            plot.x0 + along
        }
    };

    // Gridlines and value labels.
    let grid_axis = &chart.y;
    let gridlines = grid_axis.as_ref().is_none_or(|a| a.gridlines) && !axis_deleted(grid_axis);
    for (v, label) in &y_ticks {
        let pos = value_pos(&y_scale, *v);
        if horizontal {
            if gridlines {
                p.line((pos, plot.y0), (pos, plot.y1), style.grid, s.max(1.0));
            }
            if show(&chart.y) {
                p.put(label, pos - f64::from(label.width()) / 2.0, plot.y1 + gap);
            }
        } else {
            if gridlines {
                p.line((plot.x0, pos), (plot.x1, pos), style.grid, s.max(1.0));
            }
            if show(&chart.y) {
                p.put(
                    label,
                    plot.x0 - gap - f64::from(label.width()),
                    pos - f64::from(label.height()) / 2.0,
                );
            }
        }
    }
    if let (Some(scale), Some(ticks)) = (&x_scale, &x_ticks) {
        let vertical_grid = chart.x.as_ref().is_some_and(|a| a.gridlines);
        for (v, label) in ticks {
            let x = plot.x0 + scale.share(*v) * plot.width();
            if vertical_grid {
                p.line((x, plot.y0), (x, plot.y1), style.grid, s.max(1.0));
            }
            if show(&chart.x) {
                p.put(label, x - f64::from(label.width()) / 2.0, plot.y1 + gap);
            }
        }
    }
    if let (Some(scale), Some(ticks)) = (&y2_scale, &y2_ticks)
        && show(&chart.y2)
    {
        for (v, label) in ticks {
            let y = plot.y1 - scale.share(*v) * plot.height();
            p.put(label, plot.x1 + gap, y - f64::from(label.height()) / 2.0);
        }
    }

    // Category labels, thinned out so that none overlaps.
    if !scatter && show(&chart.x) {
        let stride_along = if horizontal {
            f64::from(p.text("0", label_size, false, label_color).height()) * 1.2
        } else {
            cat_w * 1.15 + 4.0 * s
        };
        let step = (stride_along / slot.max(1.0)).ceil().max(1.0) as usize;
        let mut last_drawn: Option<usize> = None;
        for (k, label) in &cat_labels {
            // Labels are every nth category counted from the first shown.
            if let Some(last) = last_drawn
                && k.saturating_sub(last) < step
            {
                continue;
            }
            last_drawn = Some(*k);
            let c = category_center(*k as f64);
            if horizontal {
                let x = (plot.x0 - gap - f64::from(label.width())).max(area.x0);
                p.put(label, x, c - f64::from(label.height()) / 2.0);
            } else {
                let width = f64::from(label.width());
                let x = (c - width / 2.0).clamp(area.x0, (area.x1 - width).max(area.x0));
                p.put(label, x, plot.y1 + gap);
            }
        }
    }

    // The data.
    p.ctx.push_clip_layer(&plot.inset(-1.0).to_path(0.1));
    let mut labels_left = MAX_DATA_LABELS;
    for plot_data in &chart.plots {
        let (scale, format) = if plot_data.secondary {
            (y2_scale.unwrap_or(y_scale), &y2_format)
        } else {
            (y_scale, &y_format)
        };
        draw_plot(
            p,
            plot_data,
            &PlotGeometry {
                plot,
                scale,
                x_scale: x_scale.as_ref(),
                horizontal,
                slot,
                categories,
                category_reversed,
            },
            format,
            chart.date1904,
            &mut labels_left,
            text_color,
        );
    }
    p.ctx.pop_layer();

    // Axis lines: the category axis where the values cross zero.
    let axis_color = label_color;
    if horizontal {
        let zero = value_pos(&y_scale, 0.0f64.clamp(y_scale.lo, y_scale.hi));
        if !axis_deleted(&chart.x) {
            p.line((zero, plot.y0), (zero, plot.y1), axis_color, s.max(1.0));
        }
    } else if !scatter {
        let zero = value_pos(&y_scale, 0.0f64.clamp(y_scale.lo, y_scale.hi));
        if !axis_deleted(&chart.x) {
            p.line((plot.x0, zero), (plot.x1, zero), axis_color, s.max(1.0));
        }
        if !axis_deleted(&chart.y) {
            p.line(
                (plot.x0, plot.y0),
                (plot.x0, plot.y1),
                style.grid,
                s.max(1.0),
            );
        }
    } else {
        p.line(
            (plot.x0, plot.y1),
            (plot.x1, plot.y1),
            axis_color,
            s.max(1.0),
        );
        p.line(
            (plot.x0, plot.y0),
            (plot.x0, plot.y1),
            axis_color,
            s.max(1.0),
        );
    }

    // Axis titles.
    if let Some(t) = &v_title {
        p.put_rotated(
            t,
            area.x0 + f64::from(t.height()) / 2.0 + gap / 2.0,
            (plot.y0 + plot.y1) / 2.0,
        );
    }
    if let Some(t) = &h_title {
        p.put(
            t,
            (plot.x0 + plot.x1 - f64::from(t.width())) / 2.0,
            area.y1 - f64::from(t.height()) - gap / 2.0,
        );
    }
    if let Some(t) = &y2_title {
        p.put_rotated(
            t,
            area.x1 - f64::from(t.height()) / 2.0 - gap / 2.0,
            (plot.y0 + plot.y1) / 2.0,
        );
    }
}

/// Where a plot's points go.
struct PlotGeometry<'a> {
    plot: Rect,
    scale: Scale,
    x_scale: Option<&'a Scale>,
    horizontal: bool,
    slot: f64,
    categories: usize,
    category_reversed: bool,
}

impl PlotGeometry<'_> {
    fn value(&self, v: f64) -> f64 {
        let t = self.scale.share(v);
        if self.horizontal {
            self.plot.x0 + t * self.plot.width()
        } else {
            self.plot.y1 - t * self.plot.height()
        }
    }

    fn category(&self, x: f64) -> f64 {
        let along = (x + 0.5) * self.slot;
        if self.horizontal {
            if self.category_reversed {
                self.plot.y0 + along
            } else {
                self.plot.y1 - along
            }
        } else if self.category_reversed {
            self.plot.x1 - along
        } else {
            self.plot.x0 + along
        }
    }

    /// A point of a line, area or scatter series.
    fn point(&self, x: f64, y: f64, scatter: bool, edge: bool) -> Point {
        if scatter {
            let sx = self.x_scale.map_or(0.0, |sc| sc.share(x));
            return Point::new(self.plot.x0 + sx * self.plot.width(), self.value(y));
        }
        let cx = if edge && self.categories > 1 {
            // An area starts at the axis, not half a slot in.
            let t = x / (self.categories - 1) as f64;
            if self.category_reversed {
                self.plot.x1 - t * self.plot.width()
            } else {
                self.plot.x0 + t * self.plot.width()
            }
        } else {
            self.category(x)
        };
        Point::new(cx, self.value(y))
    }
}

fn draw_plot(
    p: &mut Painter<'_>,
    plot: &DrawnPlot,
    g: &PlotGeometry<'_>,
    format: &aster_sheets::Format,
    date1904: bool,
    labels_left: &mut usize,
    text_color: Rgba8,
) {
    let s = p.s();
    match plot.kind {
        PlotKind::Bar {
            grouping,
            gap,
            overlap,
            ..
        } => {
            let stacked = grouping != Grouping::Clustered;
            let k = if stacked { 1 } else { plot.series.len().max(1) } as f64;
            let (gap, ov) = (gap / 100.0, overlap / 100.0);
            let bar = g.slot / (k - (k - 1.0) * ov + gap);
            let group = bar * (k - (k - 1.0) * ov);
            let len = plot.series.iter().map(|x| x.ys.len()).max().unwrap_or(0);
            // Totals for a percent stack.
            let totals: Vec<f64> = if grouping == Grouping::Percent {
                (0..len)
                    .map(|i| {
                        plot.series
                            .iter()
                            .map(|x| {
                                x.ys.get(i)
                                    .copied()
                                    .filter(|v| v.is_finite())
                                    .unwrap_or(0.0)
                                    .abs()
                            })
                            .sum()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut up = vec![0.0f64; if stacked { len } else { 0 }];
            let mut down = vec![0.0f64; if stacked { len } else { 0 }];
            for (j, series) in plot.series.iter().enumerate() {
                for (i, (&x, &raw)) in series.xs.iter().zip(&series.ys).enumerate() {
                    if !raw.is_finite() || series.no_fill {
                        continue;
                    }
                    let v = if grouping == Grouping::Percent {
                        let t = totals.get(i).copied().unwrap_or(0.0);
                        if t > 0.0 { raw / t } else { 0.0 }
                    } else {
                        raw
                    };
                    let (from, to) = if stacked {
                        let slot_total = if v >= 0.0 { &mut up[i] } else { &mut down[i] };
                        let from = *slot_total;
                        *slot_total += v;
                        (from, *slot_total)
                    } else {
                        (0.0f64.clamp(g.scale.lo, g.scale.hi), v)
                    };
                    let centre = g.category(x);
                    let offset = if stacked {
                        -bar / 2.0
                    } else {
                        -group / 2.0 + bar * (1.0 - ov) * j as f64
                    };
                    let (a, b) = (g.value(from), g.value(to));
                    let rect = if g.horizontal {
                        // Categories run up the page.
                        let dir = if g.category_reversed { 1.0 } else { -1.0 };
                        let (y0, y1) = (centre + dir * offset, centre + dir * (offset + bar));
                        Rect::new(a.min(b), y0.min(y1), a.max(b), y0.max(y1))
                    } else {
                        let dir = if g.category_reversed { -1.0 } else { 1.0 };
                        let (x0, x1) = (centre + dir * offset, centre + dir * (offset + bar));
                        Rect::new(x0.min(x1), a.min(b), x0.max(x1), a.max(b))
                    };
                    if rect.width() <= 0.0 && rect.height() <= 0.0 {
                        continue;
                    }
                    p.fill(&rect, series.color);
                    if series.labels.value && *labels_left > 0 {
                        *labels_left -= 1;
                        let label = p.text(&format.number(raw, date1904), 10.0, false, text_color);
                        let (lw, lh) = (f64::from(label.width()), f64::from(label.height()));
                        let at = if stacked {
                            (rect.center().x - lw / 2.0, rect.center().y - lh / 2.0)
                        } else if g.horizontal {
                            (rect.x1 + 2.0 * s, rect.center().y - lh / 2.0)
                        } else {
                            (rect.center().x - lw / 2.0, rect.y0 - lh - 1.0 * s)
                        };
                        p.put(&label, at.0, at.1);
                    }
                }
            }
        }
        PlotKind::Line { grouping } | PlotKind::Area { grouping } => {
            let area = matches!(plot.kind, PlotKind::Area { .. });
            let stacked = grouping != Grouping::Clustered;
            let len = plot.series.iter().map(|x| x.ys.len()).max().unwrap_or(0);
            let totals: Vec<f64> = if grouping == Grouping::Percent {
                (0..len)
                    .map(|i| {
                        plot.series
                            .iter()
                            .map(|x| {
                                x.ys.get(i)
                                    .copied()
                                    .filter(|v| v.is_finite())
                                    .unwrap_or(0.0)
                                    .abs()
                            })
                            .sum()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // Cumulative values before each series, when stacked.
            let mut below = vec![0.0f64; len];
            // Areas are drawn from the back: the last series first.
            let order: Vec<usize> = if area && !stacked {
                (0..plot.series.len()).rev().collect()
            } else {
                (0..plot.series.len()).collect()
            };
            let mut stacks: Vec<Vec<f64>> = Vec::new();
            if stacked {
                for series in &plot.series {
                    let mut tops = Vec::with_capacity(len);
                    for i in 0..len {
                        let raw = series
                            .ys
                            .get(i)
                            .copied()
                            .filter(|v| v.is_finite())
                            .unwrap_or(0.0);
                        let v = if grouping == Grouping::Percent {
                            let t = totals[i];
                            if t > 0.0 { raw / t } else { 0.0 }
                        } else {
                            raw
                        };
                        below[i] += v;
                        tops.push(below[i]);
                    }
                    stacks.push(tops);
                }
            }
            let draw_order: Vec<usize> = if area && stacked {
                (0..plot.series.len()).rev().collect()
            } else {
                order
            };
            for j in draw_order {
                let series = &plot.series[j];
                let points: Vec<(f64, f64)> = series
                    .xs
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        let y = if stacked {
                            if series.ys.get(i).is_some_and(|v| v.is_finite()) || area {
                                stacks[j].get(i).copied().unwrap_or(f64::NAN)
                            } else {
                                f64::NAN
                            }
                        } else {
                            series.ys.get(i).copied().unwrap_or(f64::NAN)
                        };
                        (x, y)
                    })
                    .collect();
                if area {
                    let base_of = |i: usize| -> f64 {
                        if stacked && j > 0 {
                            stacks[j - 1].get(i).copied().unwrap_or(0.0)
                        } else {
                            0.0f64.clamp(g.scale.lo, g.scale.hi)
                        }
                    };
                    // Runs without a gap.
                    let mut run: Vec<usize> = Vec::new();
                    let flush = |p: &mut Painter<'_>, run: &mut Vec<usize>| {
                        if run.len() >= 2 {
                            let mut path = BezPath::new();
                            let top: Vec<Point> = run
                                .iter()
                                .map(|&i| g.point(points[i].0, points[i].1, false, true))
                                .collect();
                            path.move_to(top[0]);
                            for pt in &top[1..] {
                                path.line_to(*pt);
                            }
                            for &i in run.iter().rev() {
                                path.line_to(g.point(points[i].0, base_of(i), false, true));
                            }
                            path.close_path();
                            p.fill(&path, series.color);
                        }
                        run.clear();
                    };
                    for (i, (_, y)) in points.iter().enumerate() {
                        if y.is_finite() {
                            run.push(i);
                        } else {
                            flush(p, &mut run);
                        }
                    }
                    flush(p, &mut run);
                } else {
                    draw_line_series(p, series, g, &points, false, plot_index(plot, series), s);
                }
                if series.labels.value && !area && *labels_left > 0 {
                    for (i, (x, y)) in points.iter().enumerate() {
                        if !y.is_finite() || *labels_left == 0 {
                            continue;
                        }
                        *labels_left -= 1;
                        let raw = series.ys.get(i).copied().unwrap_or(*y);
                        let at = g.point(*x, *y, false, false);
                        let label = p.text(&format.number(raw, date1904), 10.0, false, text_color);
                        p.put(
                            &label,
                            at.x - f64::from(label.width()) / 2.0,
                            at.y - f64::from(label.height()) - 4.0 * s,
                        );
                    }
                }
            }
        }
        PlotKind::Scatter => {
            for series in &plot.series {
                let points: Vec<(f64, f64)> = series
                    .xs
                    .iter()
                    .copied()
                    .zip(series.ys.iter().copied())
                    .collect();
                draw_line_series(p, series, g, &points, true, plot_index(plot, series), s);
            }
        }
        PlotKind::Pie { .. } => {}
    }
}

/// The series' place among its plot's, for the marker kind that "auto"
/// picks.
fn plot_index(plot: &DrawnPlot, series: &DrawnSeries) -> usize {
    plot.series
        .iter()
        .position(|x| x.index == series.index)
        .unwrap_or(0)
}

fn draw_line_series(
    p: &mut Painter<'_>,
    series: &DrawnSeries,
    g: &PlotGeometry<'_>,
    points: &[(f64, f64)],
    scatter: bool,
    place: usize,
    s: f64,
) {
    let converted: Vec<Option<Point>> = points
        .iter()
        .map(|&(x, y)| (x.is_finite() && y.is_finite()).then(|| g.point(x, y, scatter, false)))
        .collect();
    if !series.no_line {
        let width = series.line_width * s;
        let mut run: Vec<Point> = Vec::new();
        let flush = |p: &mut Painter<'_>, run: &mut Vec<Point>| {
            if run.len() >= 2 {
                let path = if series.smooth && run.len() < 2_000 {
                    smooth(run)
                } else {
                    let mut path = BezPath::new();
                    path.move_to(run[0]);
                    for pt in &run[1..] {
                        path.line_to(*pt);
                    }
                    path
                };
                p.stroke(&path, series.line_color, width);
            }
            run.clear();
        };
        for pt in &converted {
            match pt {
                Some(pt) => run.push(*pt),
                None => flush(p, &mut run),
            }
        }
        flush(p, &mut run);
    }
    let symbol = match series.marker.symbol {
        MarkerSymbol::None => return,
        MarkerSymbol::Auto => [
            MarkerSymbol::Circle,
            MarkerSymbol::Square,
            MarkerSymbol::Diamond,
            MarkerSymbol::Triangle,
        ][place % 4],
        other => other,
    };
    // Thousands of points do not need thousands of markers.
    if converted.len() > 400 {
        return;
    }
    let size = series.marker.size.unwrap_or(7.0) * s;
    let fill = series.marker.fill.unwrap_or(series.color);
    for pt in converted.iter().flatten() {
        p.fill(&marker_path(symbol, *pt, size), fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_sheets::chart::AxisKind;

    #[test]
    fn scales_pick_round_steps() {
        let sc = nice_scale(0.0, 87.0, None, true);
        assert_eq!((sc.lo, sc.hi, sc.step), (0.0, 100.0, 20.0));
        let sc = nice_scale(3.2, 3.9, None, true);
        assert!(sc.lo <= 3.2 && sc.hi >= 3.9 && sc.step > 0.0);
        // Data far from zero does not start at it.
        let sc = nice_scale(1000.0, 1050.0, None, true);
        assert!(sc.lo > 900.0);
        let flat = nice_scale(5.0, 5.0, None, true);
        assert!(flat.hi > flat.lo);
        // Hostile units and bounds do not make a thousand ticks.
        let axis = DrawnAxis {
            kind: AxisKind::Value,
            side: aster_sheets::chart::AxisSide::Left,
            deleted: false,
            reversed: false,
            min: Some(0.0),
            max: Some(1e9),
            major_unit: Some(1e-9),
            log: false,
            gridlines: true,
            title: None,
            format: Arc::new(aster_sheets::Format::general()),
            labels: true,
        };
        let sc = nice_scale(0.0, 1.0, Some(&axis), true);
        assert!(sc.ticks().len() <= 202);
        let nan = nice_scale(f64::NAN, f64::INFINITY, None, true);
        assert!(nan.hi > nan.lo);
        assert_eq!(clean(0.1 + 0.2), 0.3);
    }

    #[test]
    fn ticks_are_clean_numbers() {
        let sc = nice_scale(0.0, 1.0, None, true);
        let ticks = sc.ticks();
        assert_eq!(ticks.first(), Some(&0.0));
        assert!(ticks.contains(&0.2) && ticks.contains(&0.6), "{ticks:?}");
    }
}
