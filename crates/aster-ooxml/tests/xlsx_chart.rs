//! Charts in drawings: read for display, plotting the workbook's cells, and
//! saved as they were.

mod common;

use std::collections::BTreeSet;

use aster_ooxml::xlsx_save::{Changes, save};
use aster_ooxml::{FormatCodec, Package, Xlsx};
use aster_sheets::chart::{AxisKind, Grouping, PlotKind};
use aster_sheets::{Cell, CellRef, Placement, Value};
use common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

const WORKBOOK: &str = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="数据" sheetId="1" r:id="rId1"/><sheet name="Other" sheetId="2" r:id="rId2"/></sheets></workbook>"#;

const SHEET1: &str = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData>
<row r="1"><c r="B1" t="inlineStr"><is><t>金额</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>北</t></is></c><c r="B2"><v>10</v></c></row>
<row r="3"><c r="A3" t="inlineStr"><is><t>南</t></is></c><c r="B3"><v>30</v></c></row>
<row r="4"><c r="A4" t="inlineStr"><is><t>东</t></is></c><c r="B4"><v>20</v></c></row>
</sheetData><drawing r:id="rId1"/></worksheet>"#;

const DRAWING: &str = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
<xdr:twoCellAnchor><xdr:from><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>10</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>16</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
<xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="2" name="图表 1"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId1"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>
<xdr:oneCellAnchor><xdr:from><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>20</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="3000000" cy="2000000"/>
<xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="3" name="图表 2"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId2"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor>
<xdr:oneCellAnchor><xdr:from><xdr:col>12</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>20</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="3000000" cy="2000000"/>
<xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="4" name="图表 3"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId3"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:oneCellAnchor>
</xdr:wsDr>"#;

/// A column chart over the data, with the values saved with it out of date.
const CHART1: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:chart><c:title><c:overlay val="0"/></c:title><c:plotArea><c:layout/>
<c:barChart><c:barDir val="col"/><c:grouping val="clustered"/><c:ser><c:idx val="0"/><c:order val="0"/><c:tx><c:strRef><c:f>数据!$B$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>金额</c:v></c:pt></c:strCache></c:strRef></c:tx>
<c:cat><c:strRef><c:f>数据!$A$2:$A$4</c:f><c:strCache><c:ptCount val="3"/><c:pt idx="0"><c:v>北</c:v></c:pt><c:pt idx="1"><c:v>南</c:v></c:pt><c:pt idx="2"><c:v>东</c:v></c:pt></c:strCache></c:strRef></c:cat>
<c:val><c:numRef><c:f>数据!$B$2:$B$4</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="3"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt><c:pt idx="2"><c:v>3</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
<c:axId val="111"/><c:axId val="222"/></c:barChart>
<c:catAx><c:axId val="111"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="b"/><c:crossAx val="222"/></c:catAx>
<c:valAx><c:axId val="222"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/><c:crossAx val="111"/></c:valAx>
</c:plotArea><c:legend><c:legendPos val="r"/></c:legend><c:plotVisOnly val="1"/></c:chart></c:chartSpace>"##;

const CHART2: &str = r##"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:pieChart><c:ser><c:val><c:numRef><c:f>'Other'!$A$1:$A$2</c:f></c:numRef></c:val></c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"##;

fn source() -> Vec<u8> {
    let content_types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    let workbook_rels = rels(&[
        ("rId1", "worksheet", "worksheets/sheet1.xml"),
        ("rId2", "worksheet", "worksheets/sheet2.xml"),
    ]);
    let sheet_rels = rels(&[("rId1", "drawing", "../drawings/drawing1.xml")]);
    // Chart 1 is used twice, chart 3 does not exist, and chart 2 is a part
    // that is not XML.
    let drawing_rels = rels(&[
        ("rId1", "chart", "../charts/chart1.xml"),
        ("rId2", "chart", "../charts/chart1.xml"),
        ("rId3", "chart", "../charts/missing.xml"),
    ]);
    let other = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>5</v></c></row><row r="2"><c r="A2"><v>7</v></c></row></sheetData></worksheet>"#;
    package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", SHEET1.as_bytes()),
        ("xl/worksheets/sheet2.xml", other.as_bytes()),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels.as_bytes()),
        ("xl/drawings/drawing1.xml", DRAWING.as_bytes()),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            drawing_rels.as_bytes(),
        ),
        ("xl/charts/chart1.xml", CHART1.as_bytes()),
        ("xl/charts/chart2.xml", CHART2.as_bytes()),
    ])
}

fn part(bytes: &[u8], name: &str) -> String {
    String::from_utf8(Package::open(bytes.to_vec()).unwrap().read(name).unwrap()).unwrap()
}

fn differences(before: &[u8], after: &[u8]) -> BTreeSet<String> {
    let mut a = Package::open(before.to_vec()).unwrap();
    let mut b = Package::open(after.to_vec()).unwrap();
    let names: BTreeSet<String> = a.part_names().into_iter().chain(b.part_names()).collect();
    names
        .into_iter()
        .filter(|name| match (a.has(name), b.has(name)) {
            (true, true) => a.read(name).unwrap() != b.read(name).unwrap(),
            _ => true,
        })
        .collect()
}

#[test]
fn a_chart_in_a_drawing_is_read_with_its_anchor_and_plot() {
    let book = Xlsx.read(source()).unwrap();
    let pictures = &book.sheets[0].pictures;
    assert_eq!(pictures.len(), 3);
    assert!(matches!(pictures[0].placement, Placement::TwoCell { .. }));
    assert!(matches!(pictures[1].placement, Placement::OneCell { .. }));
    assert_eq!(pictures[0].name, "图表 1");
    let chart = pictures[0].chart.as_ref().expect("a chart");
    assert_eq!(chart.plots.len(), 1);
    assert_eq!(
        chart.plots[0].kind,
        PlotKind::Bar {
            horizontal: false,
            grouping: Grouping::Clustered,
            gap: 150.0,
            overlap: 0.0
        }
    );
    assert_eq!(chart.axes.len(), 2);
    assert_eq!(chart.axes[1].kind, AxisKind::Value);
    assert!(chart.axes[1].gridlines);
    // The same part shown twice is read once.
    assert!(std::sync::Arc::ptr_eq(
        chart,
        pictures[1].chart.as_ref().unwrap()
    ));
    // A chart whose part is not there is a grey frame, as before.
    assert!(pictures[2].chart.is_none());
}

#[test]
fn the_values_come_from_the_cells_not_from_the_saved_cache() {
    let mut book = Xlsx.read(source()).unwrap();
    let chart = book.sheets[0].pictures[0].chart.clone().unwrap();
    let drawn = book.resolve_chart(&chart);
    let series = &drawn.plots[0].series[0];
    // The file's cache says 1, 2, 3; the cells say 10, 30, 20.
    assert_eq!(series.ys, [10.0, 30.0, 20.0]);
    assert_eq!(series.name.as_deref(), Some("金额"));
    assert_eq!(drawn.plots[0].labels[2], (2, "东".to_owned()));
    // An empty title is the series' name.
    assert_eq!(drawn.title.as_deref(), Some("金额"));
    book.set_cells(
        0,
        vec![(
            CellRef::parse("B3").unwrap(),
            Some(Cell::new(1, Value::Number(99.0))),
        )],
    );
    assert_eq!(
        book.resolve_chart(&chart).plots[0].series[0].ys,
        [10.0, 99.0, 20.0]
    );
}

#[test]
fn saving_keeps_the_chart_parts_as_they_were() {
    let source = source();
    let book = Xlsx.read(source.clone()).unwrap();
    let saved = save(source.clone(), &book, &Changes::default()).unwrap();
    assert!(differences(&source, &saved).is_empty());
    // An edit of a cell, on the chart's own sheet: only the sheet moves.
    let mut book = book;
    let mut changes = Changes::default();
    for change in book.set_cells(
        0,
        vec![(
            CellRef::parse("B3").unwrap(),
            Some(Cell::new(1, Value::Number(99.0))),
        )],
    ) {
        changes.cells.insert((0, change.at));
        changes.edited.insert((0, change.at));
    }
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from(["xl/worksheets/sheet1.xml".to_owned()])
    );
    assert_eq!(part(&saved, "xl/charts/chart1.xml"), CHART1);
    assert_eq!(part(&saved, "xl/drawings/drawing1.xml"), DRAWING);
}

#[test]
fn renaming_the_sheet_a_chart_plots_changes_the_file_and_the_model() {
    let source = source();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    for cell in book.rename_sheet(0, "Q1 data") {
        changes.cells.insert(cell);
        changes.edited.insert(cell);
    }
    changes.formulas_changed = true;
    // The chart in the model follows, and still finds its cells.
    let chart = book.sheets[0].pictures[0].chart.clone().unwrap();
    let formula = chart.plots[0].series[0]
        .values
        .as_ref()
        .unwrap()
        .formula
        .clone()
        .unwrap();
    assert_eq!(formula, "'Q1 data'!$B$2:$B$4");
    assert_eq!(
        book.resolve_chart(&chart).plots[0].series[0].ys,
        [10.0, 30.0, 20.0]
    );
    let saved = save(source, &book, &changes).unwrap();
    let xml = part(&saved, "xl/charts/chart1.xml");
    assert!(xml.contains("<c:f>'Q1 data'!$B$2:$B$4</c:f>"), "{xml}");
    assert!(xml.contains("<c:f>'Q1 data'!$A$2:$A$4</c:f>"), "{xml}");
    // The other chart part is not about it and stays.
    assert_eq!(part(&saved, "xl/charts/chart2.xml"), CHART2);
    let again = Xlsx.read(saved).unwrap();
    let reread = again.sheets[0].pictures[0].chart.clone().unwrap();
    assert_eq!(
        again.resolve_chart(&reread).plots[0].series[0].ys,
        [10.0, 30.0, 20.0]
    );
}

#[test]
fn a_sheet_a_chart_plots_cannot_have_rows_inserted_as_before() {
    let book = Xlsx.read(source()).unwrap();
    assert_eq!(book.sheets[0].structure_blocker.as_deref(), Some("chart"));
}
