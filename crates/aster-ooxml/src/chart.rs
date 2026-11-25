//! Chart parts (`xl/charts/chartN.xml`): the kinds of chart, series, axes,
//! title and legend, for display. The part itself is saved as it was.
//!
//! Read: bar and column (clustered, stacked, percent stacked), line,
//! area, pie, doughnut, scatter; series names, categories and values with
//! the cells they refer to and the values saved with them; series fills,
//! lines and markers, the colours of single slices; axes with scale,
//! number format, gridlines, titles; title and legend position.
//!
//! What is not drawn is named in [`Chart::not_shown`] instead of dropped
//! without a word: other kinds of chart (radar, stock, surface, bubble
//! and the second chart of a pie of pie), the third dimension, trend
//! lines, error bars, data tables, and logarithmic axes.
//!
//! Everything read is bounded: parts over [`MAX_PART`] bytes are not read,
//! and series, points, text and nesting are cut where they run past what
//! a drawing can use.

use aster_sheets::chart::{
    AxisKind, AxisSide, Chart, ChartAxis, Grouping, Labels, LegendPos, MAX_PLOTS, MAX_READ,
    MAX_SERIES, MAX_TEXT, MarkerSymbol, Plot, PlotKind, Series, Source,
};
use aster_sheets::{Rgba, Value};

use crate::drawingml::{
    ColorContext, ColorMap, ColorSpec, FillSpec, Theme, is_fill, read_fill, read_line,
};
use crate::error::Result;
use crate::xml::{Ev, Tag, XmlReader, each_child};

/// The largest chart part read, bytes.
pub const MAX_PART: usize = 32 * 1024 * 1024;
/// Most points kept of the values saved with a series.
const MAX_CACHED: usize = MAX_READ;
/// Most points read for all the series of a chart.
const MAX_TOTAL_CACHED: usize = 2_000_000;

/// The chart a part describes. A part that cannot be read, or is too
/// large, gives an empty chart that says so.
pub fn read_chart(bytes: &[u8], part: &str, theme: &Theme) -> Chart {
    let palette: Vec<Rgba> = (1..=6)
        .map(|n| theme.color(&format!("accent{n}")))
        .collect();
    if bytes.len() > MAX_PART {
        return Chart {
            palette,
            not_shown: vec!["chart too large".to_owned()],
            ..Chart::default()
        };
    }
    let mut reader = Parser {
        theme,
        map: ColorMap::default(),
        total_cached: 0,
        not_shown: Vec::new(),
    };
    let mut chart = reader.parse(bytes, part).unwrap_or_else(|_| Chart {
        not_shown: vec!["chart part".to_owned()],
        ..Chart::default()
    });
    chart.palette = palette;
    for name in reader.not_shown {
        if !chart.not_shown.contains(&name) {
            chart.not_shown.push(name);
        }
    }
    chart
}

struct Parser<'a> {
    theme: &'a Theme,
    map: ColorMap,
    total_cached: usize,
    not_shown: Vec<String>,
}

fn clip(text: String) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text;
    }
    let mut out: String = text.chars().take(MAX_TEXT - 1).collect();
    out.push('…');
    out
}

/// `<c:v val="..."/>`-style elements with a `val`.
fn val(tag: &Tag<'_>) -> Option<String> {
    tag.attr("val")
}

impl Parser<'_> {
    fn note(&mut self, what: &str) {
        if !self.not_shown.iter().any(|n| n == what) {
            self.not_shown.push(what.to_owned());
        }
    }

    fn resolve(&self, spec: &ColorSpec) -> Rgba {
        ColorContext {
            theme: self.theme,
            map: &self.map,
            placeholder: None,
        }
        .resolve(spec)
    }

    fn parse(&mut self, bytes: &[u8], part: &str) -> Result<Chart> {
        let mut reader = XmlReader::new(bytes, part);
        let mut chart = Chart {
            visible_only: true,
            ..Chart::default()
        };
        if reader.find("chartSpace")?.is_some() {
            each_child(&mut reader, |reader, child| match child.local() {
                b"chart" => self.read_chart_element(reader, &mut chart),
                b"spPr" => {
                    let (fill, _, _, _) = self.read_shape(reader)?;
                    chart.background = fill.flatten();
                    Ok(())
                }
                _ => reader.skip(),
            })?;
        }
        Ok(chart)
    }

    fn read_chart_element(&mut self, reader: &mut XmlReader<'_>, chart: &mut Chart) -> Result<()> {
        each_child(reader, |reader, child| match child.local() {
            b"title" => {
                chart.title = Some(clip(self.read_title(reader)?));
                Ok(())
            }
            b"autoTitleDeleted" => {
                if child.attr_bool("val").unwrap_or(true) && chart.title.is_none() {
                    chart.title = None;
                }
                reader.skip()
            }
            b"plotVisOnly" => {
                chart.visible_only = child.attr_bool("val").unwrap_or(true);
                reader.skip()
            }
            b"legend" => {
                chart.legend = Some(LegendPos::Right);
                each_child(reader, |reader, item| {
                    if item.is("legendPos") {
                        chart.legend = Some(match val(&item).as_deref() {
                            Some("b") => LegendPos::Bottom,
                            Some("t") => LegendPos::Top,
                            Some("l") => LegendPos::Left,
                            _ => LegendPos::Right,
                        });
                    }
                    reader.skip()
                })
            }
            b"plotArea" => self.read_plot_area(reader, chart),
            b"view3D" => {
                self.note("3-D view");
                reader.skip()
            }
            _ => reader.skip(),
        })
    }

    /// The text of a title: its rich text runs, or the text of the cell it
    /// is linked to as saved. Empty for the automatic one (the series
    /// name).
    fn read_title(&mut self, reader: &mut XmlReader<'_>) -> Result<String> {
        let mut text = String::new();
        each_child(reader, |reader, child| {
            if child.is("tx") {
                text = self.read_text_body(reader)?;
                Ok(())
            } else {
                reader.skip()
            }
        })?;
        Ok(text)
    }

    /// Rich text (`<c:rich>`), or cached text of a reference, as one
    /// line: paragraphs joined by a space.
    fn read_text_body(&mut self, reader: &mut XmlReader<'_>) -> Result<String> {
        let mut out = String::new();
        let mut paragraph = false;
        let mut depth = 1usize;
        loop {
            match reader.next()? {
                Ev::Open(tag) => {
                    depth += 1;
                    if tag.is("p") {
                        if paragraph && !out.is_empty() && !out.ends_with(' ') {
                            out.push(' ');
                        }
                        paragraph = true;
                    } else if tag.is("t") || tag.is("v") {
                        depth -= 1;
                        let text = reader.text()?;
                        if out.chars().count() < MAX_TEXT {
                            out.push_str(&crate::xml::decode_escapes(&text));
                        }
                    }
                }
                Ev::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out.trim().to_owned());
                    }
                }
                Ev::Text(_) => {}
                Ev::Eof => return Ok(out),
            }
        }
    }

    /// `<c:spPr>`: fill (`Some(None)` for none, `None` when not stated),
    /// line colour and width in pixels, and whether the line is off.
    #[allow(clippy::type_complexity)]
    fn read_shape(
        &mut self,
        reader: &mut XmlReader<'_>,
    ) -> Result<(Option<Option<Rgba>>, Option<Rgba>, Option<f64>, bool)> {
        let mut fill = None;
        let mut line = None;
        let mut width = None;
        let mut no_line = false;
        each_child(reader, |reader, child| {
            if is_fill(&child) {
                fill = match read_fill(reader, &child)? {
                    Some(FillSpec::None) => Some(None),
                    Some(FillSpec::Solid(color)) => Some(Some(self.resolve(&color))),
                    Some(FillSpec::Gradient(stops, _)) => {
                        stops.first().map(|(_, c)| Some(self.resolve(c)))
                    }
                    Some(FillSpec::Pattern(fg, bg)) => fg.or(bg).map(|c| Some(self.resolve(&c))),
                    _ => None,
                };
                Ok(())
            } else if child.is("ln") {
                let spec = read_line(reader, &child)?;
                width = spec.width.map(|w| (w as f64 / 9_525.0).clamp(0.5, 12.0));
                match spec.fill {
                    Some(FillSpec::None) => no_line = true,
                    Some(FillSpec::Solid(color)) => line = Some(self.resolve(&color)),
                    Some(FillSpec::Gradient(stops, _)) => {
                        line = stops.first().map(|(_, c)| self.resolve(c));
                    }
                    _ => {}
                }
                Ok(())
            } else {
                reader.skip()
            }
        })?;
        Ok((fill, line, width, no_line))
    }

    fn read_plot_area(&mut self, reader: &mut XmlReader<'_>, chart: &mut Chart) -> Result<()> {
        each_child(reader, |reader, child| {
            let name = String::from_utf8_lossy(child.local()).into_owned();
            match name.as_str() {
                "catAx" | "dateAx" | "valAx" => {
                    if chart.axes.len() < 8 {
                        chart.axes.push(self.read_axis(reader, &name)?);
                        Ok(())
                    } else {
                        reader.skip()
                    }
                }
                "serAx" => {
                    self.note("series axis");
                    reader.skip()
                }
                "spPr" => {
                    let (fill, _, _, _) = self.read_shape(reader)?;
                    chart.plot_background = fill.flatten();
                    Ok(())
                }
                "dTable" => {
                    self.note("data table");
                    reader.skip()
                }
                n if n.ends_with("Chart") => {
                    if chart.plots.len() >= MAX_PLOTS {
                        self.note("plot limit");
                        return reader.skip();
                    }
                    if let Some(plot) = self.read_plot(reader, n)? {
                        chart.plots.push(plot);
                    }
                    Ok(())
                }
                _ => reader.skip(),
            }
        })
    }

    fn read_plot(&mut self, reader: &mut XmlReader<'_>, name: &str) -> Result<Option<Plot>> {
        let three_d = name.ends_with("3DChart");
        let base = name.trim_end_matches("3DChart").trim_end_matches("Chart");
        let kind = match base {
            "bar" | "line" | "area" | "pie" | "doughnut" | "scatter" => base,
            "ofPie" => {
                self.note("pie of pie");
                "pie"
            }
            "bubble" => {
                self.note("bubble chart");
                "scatter"
            }
            other => {
                self.note(&format!("{other} chart"));
                reader.skip()?;
                return Ok(None);
            }
        };
        if three_d {
            self.note("3-D");
        }
        let mut horizontal = false;
        let mut grouping = Grouping::Clustered;
        let mut gap = 150.0;
        let mut overlap = 0.0;
        let mut hole = None;
        let mut first_angle = 0.0;
        let mut series: Vec<Series> = Vec::new();
        let mut axes: Vec<u32> = Vec::new();
        each_child(reader, |reader, child| {
            match child.local() {
                b"barDir" => horizontal = val(&child).as_deref() == Some("bar"),
                b"grouping" => {
                    grouping = match val(&child).as_deref() {
                        Some("stacked") => Grouping::Stacked,
                        Some("percentStacked") => Grouping::Percent,
                        _ => Grouping::Clustered,
                    }
                }
                b"gapWidth" => gap = child.attr_f64("val").unwrap_or(150.0).clamp(0.0, 500.0),
                b"overlap" => overlap = child.attr_f64("val").unwrap_or(0.0).clamp(-100.0, 100.0),
                b"holeSize" => {
                    hole = Some(child.attr_f64("val").unwrap_or(50.0).clamp(10.0, 90.0) / 100.0)
                }
                b"firstSliceAng" => {
                    first_angle = child.attr_f64("val").unwrap_or(0.0).rem_euclid(360.0)
                }
                b"axId" => {
                    if let Some(id) = child.attr_i64("val").and_then(|v| u32::try_from(v).ok()) {
                        axes.push(id);
                    }
                }
                b"ser" => {
                    if series.len() < MAX_SERIES {
                        series.push(self.read_series(reader, kind)?);
                    } else {
                        self.note("series limit");
                        return reader.skip();
                    }
                    return Ok(());
                }
                b"varyColors" | b"dLbls" | b"marker" | b"smooth" | b"scatterStyle"
                | b"bubbleScale" | b"showNegBubbles" | b"sizeRepresents" | b"serLines"
                | b"dropLines" | b"hiLowLines" | b"upDownBars" | b"gapDepth" | b"shape"
                | b"ofPieType" | b"splitType" | b"splitPos" | b"secondPieSize" => {}
                _ => {}
            }
            reader.skip()
        })?;
        let kind = match kind {
            "bar" => PlotKind::Bar {
                horizontal,
                grouping,
                gap,
                overlap,
            },
            "line" => PlotKind::Line { grouping },
            "area" => PlotKind::Area { grouping },
            "pie" => PlotKind::Pie {
                hole: None,
                first_angle,
            },
            "doughnut" => PlotKind::Pie {
                hole: Some(hole.unwrap_or(0.5)),
                first_angle,
            },
            _ => PlotKind::Scatter,
        };
        // Line charts mark their points unless the chart says no; scatter
        // charts follow `scatterStyle`. Series carry their own say.
        Ok(Some(Plot { kind, series, axes }))
    }

    fn read_series(&mut self, reader: &mut XmlReader<'_>, kind: &str) -> Result<Series> {
        let mut series = Series::default();
        // A line chart's points are marked by default; `<c:marker val=0>`
        // at the chart level and `<c:symbol val="none">` here turn it off.
        series.marker.symbol = if kind == "line" || kind == "scatter" {
            MarkerSymbol::Auto
        } else {
            MarkerSymbol::None
        };
        each_child(reader, |reader, child| match child.local() {
            b"tx" => {
                series.name = self.read_source(reader)?;
                Ok(())
            }
            b"cat" | b"xVal" => {
                series.categories = self.read_source(reader)?;
                Ok(())
            }
            b"val" | b"yVal" => {
                series.values = self.read_source(reader)?;
                Ok(())
            }
            b"spPr" => {
                let (fill, line, width, no_line) = self.read_shape(reader)?;
                match fill {
                    Some(Some(color)) => series.fill = Some(color),
                    Some(None) => series.no_fill = true,
                    None => {}
                }
                series.line = line;
                series.line_width = width;
                series.no_line = no_line;
                Ok(())
            }
            b"marker" => each_child(reader, |reader, item| {
                match item.local() {
                    b"symbol" => {
                        series.marker.symbol = match val(&item).as_deref() {
                            Some("none") => MarkerSymbol::None,
                            Some("circle") => MarkerSymbol::Circle,
                            Some("square") => MarkerSymbol::Square,
                            Some("diamond") => MarkerSymbol::Diamond,
                            Some("triangle") => MarkerSymbol::Triangle,
                            _ => MarkerSymbol::Auto,
                        }
                    }
                    b"size" => {
                        series.marker.size = item
                            .attr_f64("val")
                            .map(|s| (s * 96.0 / 72.0).clamp(2.0, 40.0))
                    }
                    b"spPr" => {
                        let (fill, line, _, _) = self.read_shape(reader)?;
                        series.marker.fill = fill.flatten().or(line);
                        return Ok(());
                    }
                    _ => {}
                }
                reader.skip()
            }),
            b"dPt" => {
                let mut index = None;
                let mut color = None;
                each_child(reader, |reader, item| match item.local() {
                    b"idx" => {
                        index = item.attr_i64("val").and_then(|i| usize::try_from(i).ok());
                        reader.skip()
                    }
                    b"spPr" => {
                        let (fill, line, _, _) = self.read_shape(reader)?;
                        color = fill.flatten().or(line);
                        Ok(())
                    }
                    _ => reader.skip(),
                })?;
                if let (Some(index), Some(color)) = (index, color)
                    && index < aster_sheets::chart::MAX_SLICES
                    && series.point_fills.len() < aster_sheets::chart::MAX_SLICES
                {
                    series.point_fills.push((index, color));
                }
                Ok(())
            }
            b"smooth" => {
                series.smooth = child.attr_bool("val").unwrap_or(true);
                reader.skip()
            }
            b"dLbls" => {
                let mut labels = Labels::default();
                each_child(reader, |reader, item| {
                    let on = item.attr_bool("val").unwrap_or(true);
                    match item.local() {
                        b"showVal" => labels.value = on,
                        b"showPercent" => labels.percent = on,
                        b"showCatName" => labels.category = on,
                        _ => {}
                    }
                    reader.skip()
                })?;
                series.labels = labels;
                Ok(())
            }
            b"trendline" => {
                self.note("trend line");
                reader.skip()
            }
            b"errBars" => {
                self.note("error bars");
                reader.skip()
            }
            _ => reader.skip(),
        })?;
        Ok(series)
    }

    /// A reference or literal: `strRef`, `numRef`, `multiLvlStrRef`,
    /// `strLit`, `numLit`, or a plain `<c:v>` (a series name).
    fn read_source(&mut self, reader: &mut XmlReader<'_>) -> Result<Option<Source>> {
        let mut source: Option<Source> = None;
        each_child(reader, |reader, child| match child.local() {
            b"strRef" | b"numRef" | b"multiLvlStrRef" | b"strLit" | b"numLit" => {
                let multi = child.is("multiLvlStrRef");
                let numeric = child.is("numRef") || child.is("numLit");
                let mut found = Source::default();
                let mut formula = None;
                let mut first_level = true;
                each_child(reader, |reader, item| match item.local() {
                    b"f" => {
                        formula = Some(reader.text()?);
                        Ok(())
                    }
                    b"numCache" | b"strCache" | b"multiLvlStrCache" => {
                        self.read_cache(reader, numeric, &mut found, &mut first_level)
                    }
                    _ => self.read_literal_item(reader, &item, numeric, &mut found),
                })?;
                found.formula = if multi {
                    None
                } else {
                    formula.filter(|f| f.chars().count() <= 4_096)
                };
                source = Some(found);
                Ok(())
            }
            b"v" => {
                let text = reader.text()?;
                source = Some(Source {
                    cached: vec![Value::Text(clip(text).into())],
                    ..Source::default()
                });
                Ok(())
            }
            _ => reader.skip(),
        })?;
        Ok(source)
    }

    /// An element of a literal: `formatCode`, `ptCount`, `pt`.
    fn read_literal_item(
        &mut self,
        reader: &mut XmlReader<'_>,
        item: &Tag<'_>,
        numeric: bool,
        into: &mut Source,
    ) -> Result<()> {
        match item.local() {
            b"formatCode" => {
                into.format = Some(clip(reader.text()?));
                Ok(())
            }
            b"pt" => {
                let at = item.attr_i64("idx").and_then(|i| usize::try_from(i).ok());
                let mut text = String::new();
                each_child(reader, |reader, v| {
                    if v.is("v") {
                        text = reader.text()?;
                        Ok(())
                    } else {
                        reader.skip()
                    }
                })?;
                if let Some(at) = at
                    && at < MAX_CACHED
                    && self.total_cached < MAX_TOTAL_CACHED
                {
                    if into.cached.len() <= at {
                        self.total_cached += at + 1 - into.cached.len();
                        into.cached.resize(at + 1, Value::Empty);
                    }
                    into.cached[at] = if numeric {
                        match text.trim().parse::<f64>() {
                            Ok(n) if n.is_finite() => Value::Number(n),
                            _ => Value::Empty,
                        }
                    } else {
                        Value::Text(clip(crate::xml::decode_escapes(&text).into_owned()).into())
                    };
                }
                Ok(())
            }
            _ => reader.skip(),
        }
    }

    /// `numCache`, `strCache` or `multiLvlStrCache`; of the last the first
    /// level (the innermost one, next to the values).
    fn read_cache(
        &mut self,
        reader: &mut XmlReader<'_>,
        numeric: bool,
        into: &mut Source,
        first_level: &mut bool,
    ) -> Result<()> {
        each_child(reader, |reader, item| {
            if item.is("lvl") {
                if !*first_level {
                    return reader.skip();
                }
                *first_level = false;
                each_child(reader, |reader, inner| {
                    self.read_literal_item(reader, &inner, numeric, into)
                })
            } else {
                self.read_literal_item(reader, &item, numeric, into)
            }
        })
    }

    fn read_axis(&mut self, reader: &mut XmlReader<'_>, name: &str) -> Result<ChartAxis> {
        let mut axis = ChartAxis {
            id: 0,
            kind: match name {
                "valAx" => AxisKind::Value,
                "dateAx" => AxisKind::Date,
                _ => AxisKind::Category,
            },
            side: if name == "valAx" {
                AxisSide::Left
            } else {
                AxisSide::Bottom
            },
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
        };
        each_child(reader, |reader, child| match child.local() {
            b"axId" => {
                axis.id = child
                    .attr_i64("val")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0);
                reader.skip()
            }
            b"delete" => {
                axis.deleted = child.attr_bool("val").unwrap_or(true);
                reader.skip()
            }
            b"axPos" => {
                axis.side = match val(&child).as_deref() {
                    Some("l") => AxisSide::Left,
                    Some("r") => AxisSide::Right,
                    Some("t") => AxisSide::Top,
                    _ => AxisSide::Bottom,
                };
                reader.skip()
            }
            b"scaling" => {
                each_child(reader, |reader, item| {
                    match item.local() {
                        b"orientation" => axis.reversed = val(&item).as_deref() == Some("maxMin"),
                        b"min" => axis.min = item.attr_f64("val"),
                        b"max" => axis.max = item.attr_f64("val"),
                        b"logBase" => axis.log_base = item.attr_f64("val").filter(|b| *b > 1.0),
                        _ => {}
                    }
                    reader.skip()
                })?;
                if axis.log_base.take().is_some() {
                    self.note("logarithmic axis");
                }
                Ok(())
            }
            b"majorGridlines" => {
                axis.gridlines = true;
                reader.skip()
            }
            b"majorUnit" => {
                axis.major_unit = child.attr_f64("val").filter(|u| *u > 0.0);
                reader.skip()
            }
            b"numFmt" => {
                if !child.attr_bool("sourceLinked").unwrap_or(false) {
                    axis.format = child.attr("formatCode").map(clip);
                }
                reader.skip()
            }
            b"tickLblPos" => {
                axis.labels = val(&child).as_deref() != Some("none");
                reader.skip()
            }
            b"title" => {
                let text = self.read_title(reader)?;
                axis.title = (!text.is_empty()).then(|| clip(text));
                Ok(())
            }
            _ => reader.skip(),
        })?;
        Ok(axis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(xml: &str) -> Chart {
        read_chart(xml.as_bytes(), "chart1.xml", &Theme::default())
    }

    const BAR: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<c:chart>
 <c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>売上</a:t></a:r><a:r><a:t> 2026</a:t></a:r></a:p></c:rich></c:tx></c:title>
 <c:plotArea>
  <c:barChart><c:barDir val="col"/><c:grouping val="stacked"/><c:gapWidth val="80"/><c:overlap val="100"/>
   <c:ser><c:idx val="0"/><c:order val="0"/>
    <c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>甲</c:v></c:pt></c:strCache></c:strRef></c:tx>
    <c:spPr><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></c:spPr>
    <c:cat><c:strRef><c:f>Sheet1!$A$2:$A$4</c:f><c:strCache><c:ptCount val="3"/><c:pt idx="0"><c:v>一</c:v></c:pt><c:pt idx="2"><c:v>三</c:v></c:pt></c:strCache></c:strRef></c:cat>
    <c:val><c:numRef><c:f>Sheet1!$B$2:$B$4</c:f><c:numCache><c:formatCode>0.0</c:formatCode><c:ptCount val="3"/><c:pt idx="0"><c:v>1.5</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt><c:pt idx="2"><c:v>x</c:v></c:pt></c:numCache></c:numRef></c:val>
   </c:ser>
   <c:ser><c:idx val="1"/><c:order val="1"/><c:val><c:numLit><c:ptCount val="2"/><c:pt idx="0"><c:v>7</c:v></c:pt></c:numLit></c:val><c:trendline/></c:ser>
   <c:axId val="10"/><c:axId val="20"/>
  </c:barChart>
  <c:catAx><c:axId val="10"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="b"/><c:tickLblPos val="nextTo"/></c:catAx>
  <c:valAx><c:axId val="20"/><c:scaling><c:max val="100"/><c:min val="0"/></c:scaling><c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/><c:title><c:tx><c:rich><a:p><a:r><a:t>円</a:t></a:r></a:p></c:rich></c:tx></c:title><c:numFmt formatCode="#,##0" sourceLinked="0"/><c:majorUnit val="25"/></c:valAx>
 </c:plotArea>
 <c:legend><c:legendPos val="b"/></c:legend>
 <c:plotVisOnly val="1"/>
</c:chart>
<c:spPr><a:solidFill><a:srgbClr val="EEEEEE"/></a:solidFill></c:spPr>
</c:chartSpace>"##;

    #[test]
    fn a_stacked_column_chart_is_read_with_its_series_and_axes() {
        let chart = read(BAR);
        assert_eq!(chart.title.as_deref(), Some("売上 2026"));
        assert_eq!(chart.legend, Some(LegendPos::Bottom));
        assert_eq!(chart.background, Some([0xee, 0xee, 0xee, 0xff]));
        assert_eq!(chart.plots.len(), 1);
        let plot = &chart.plots[0];
        assert_eq!(
            plot.kind,
            PlotKind::Bar {
                horizontal: false,
                grouping: Grouping::Stacked,
                gap: 80.0,
                overlap: 100.0
            }
        );
        assert_eq!(plot.axes, [10, 20]);
        let first = &plot.series[0];
        assert_eq!(first.fill, Some([0xff, 0, 0, 0xff]));
        let values = first.values.as_ref().unwrap();
        assert_eq!(values.formula.as_deref(), Some("Sheet1!$B$2:$B$4"));
        assert_eq!(
            values.cached,
            [Value::Number(1.5), Value::Number(2.0), Value::Empty]
        );
        assert_eq!(values.format.as_deref(), Some("0.0"));
        // A point the cache does not have is empty.
        let cats = first.categories.as_ref().unwrap();
        assert_eq!(cats.cached[1], Value::Empty);
        assert_eq!(cats.cached[2], Value::Text("三".into()));
        assert_eq!(
            first.name.as_ref().unwrap().cached,
            [Value::Text("甲".into())]
        );
        // A literal has no formula.
        assert_eq!(plot.series[1].values.as_ref().unwrap().formula, None);
        let axis = &chart.axes[1];
        assert_eq!(axis.kind, AxisKind::Value);
        assert_eq!(
            (axis.min, axis.max, axis.major_unit),
            (Some(0.0), Some(100.0), Some(25.0))
        );
        assert!(axis.gridlines);
        assert_eq!(axis.title.as_deref(), Some("円"));
        assert_eq!(axis.format.as_deref(), Some("#,##0"));
        assert_eq!(chart.axes[0].side, AxisSide::Bottom);
        assert_eq!(chart.not_shown, ["trend line"]);
        assert_eq!(chart.palette.len(), 6);
    }

    #[test]
    fn line_pie_doughnut_area_and_scatter_are_told_apart() {
        let wrap = |inner: &str| {
            format!(
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea>{inner}</c:plotArea></c:chart></c:chartSpace>"#
            )
        };
        let kinds = |inner: &str| {
            read(&wrap(inner))
                .plots
                .iter()
                .map(|p| p.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(
                r#"<c:lineChart><c:grouping val="standard"/><c:ser><c:smooth val="1"/><c:marker><c:symbol val="none"/></c:marker></c:ser></c:lineChart>"#
            ),
            [PlotKind::Line {
                grouping: Grouping::Clustered
            }]
        );
        assert_eq!(
            kinds(r#"<c:pieChart><c:firstSliceAng val="90"/></c:pieChart>"#),
            [PlotKind::Pie {
                hole: None,
                first_angle: 90.0
            }]
        );
        assert_eq!(
            kinds(r#"<c:doughnutChart><c:holeSize val="60"/></c:doughnutChart>"#),
            [PlotKind::Pie {
                hole: Some(0.6),
                first_angle: 0.0
            }]
        );
        assert_eq!(
            kinds(r#"<c:areaChart><c:grouping val="percentStacked"/></c:areaChart>"#),
            [PlotKind::Area {
                grouping: Grouping::Percent
            }]
        );
        assert_eq!(
            kinds(r#"<c:scatterChart><c:scatterStyle val="lineMarker"/></c:scatterChart>"#),
            [PlotKind::Scatter]
        );
        let line = read(&wrap(
            r#"<c:lineChart><c:ser><c:smooth val="1"/><c:marker><c:symbol val="none"/></c:marker></c:ser></c:lineChart>"#,
        ));
        let series = &line.plots[0].series[0];
        assert!(series.smooth);
        assert_eq!(series.marker.symbol, MarkerSymbol::None);
        // A bar chart does not mark points; slices have colours.
        let pie = read(&wrap(
            r#"<c:pieChart><c:ser><c:dPt><c:idx val="1"/><c:spPr><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></c:spPr></c:dPt></c:ser></c:pieChart>"#,
        ));
        assert_eq!(pie.plots[0].series[0].point_fills.len(), 1);
        assert_eq!(pie.plots[0].series[0].point_fills[0].0, 1);
    }

    #[test]
    fn what_is_not_drawn_is_named() {
        let chart = read(
            r#"<c:chartSpace xmlns:c="c"><c:chart><c:view3D/><c:plotArea><c:radarChart/><c:bar3DChart><c:barDir val="bar"/></c:bar3DChart><c:valAx><c:axId val="1"/><c:scaling><c:logBase val="10"/></c:scaling></c:valAx><c:dTable/></c:plotArea></c:chart></c:chartSpace>"#,
        );
        for name in [
            "radar chart",
            "3-D",
            "3-D view",
            "logarithmic axis",
            "data table",
        ] {
            assert!(chart.not_shown.iter().any(|n| n == name), "{name}");
        }
        // The 3-D bar chart is still drawn, flat.
        assert_eq!(chart.plots.len(), 1);
    }

    #[test]
    fn broken_and_hostile_parts_give_a_chart_that_says_so() {
        let theme = Theme::default();
        let chart = read_chart(
            b"<c:chartSpace><c:chart><c:plotArea><c:barChart>",
            "c.xml",
            &theme,
        );
        assert!(chart.not_shown.contains(&"chart part".to_owned()));
        let chart = read_chart(b"not xml at all \xff\xfe", "c.xml", &theme);
        assert!(chart.plots.is_empty());
        assert!(chart.palette.len() == 6);
        // Nested far too deep.
        let deep = format!(
            "<c:chartSpace><c:chart><c:plotArea>{}",
            "<c:x>".repeat(5_000)
        );
        let chart = read_chart(deep.as_bytes(), "c.xml", &theme);
        assert!(chart.not_shown.contains(&"chart part".to_owned()));
        // A point index past anything real does not allocate.
        let chart = read(
            r#"<c:chartSpace><c:chart><c:plotArea><c:lineChart><c:ser><c:val><c:numRef><c:f>A!A1</c:f><c:numCache><c:ptCount val="4000000000"/><c:pt idx="3999999999"><c:v>1</c:v></c:pt><c:pt idx="2"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
        );
        let cached = &chart.plots[0].series[0].values.as_ref().unwrap().cached;
        assert_eq!(cached.len(), 3);
        // A part over the limit is not read.
        let big = vec![b' '; MAX_PART + 1];
        assert!(read_chart(&big, "c.xml", &theme).plots.is_empty());
    }

    #[test]
    fn a_long_text_and_many_series_are_cut() {
        let long = "x".repeat(10_000);
        let series = r#"<c:ser/>"#.repeat(200);
        let chart = read(&format!(
            r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>{long}</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:lineChart>{series}</c:lineChart></c:plotArea></c:chart></c:chartSpace>"#
        ));
        assert!(chart.title.as_ref().unwrap().chars().count() <= MAX_TEXT);
        assert_eq!(chart.plots[0].series.len(), MAX_SERIES);
        assert!(chart.not_shown.contains(&"series limit".to_owned()));
    }
}
