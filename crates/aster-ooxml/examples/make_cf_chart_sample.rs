//! Write a small workbook with every kind of conditional format and
//! chart this version draws, for looking at in the app:
//!
//! `cargo run -p aster-ooxml --example make_cf_chart_sample -- sample.xlsx`
//!
//! Two sheets: "条件格式" with the rules in columns, and "图表" with a table
//! and six charts over it (clustered columns, stacked bars on their side,
//! a line with markers, an area, a pie, a scatter chart).

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_TYPE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn text(r: &str, s: &str) -> String {
    format!(r#"<c r="{r}" t="inlineStr"><is><t>{s}</t></is></c>"#)
}

fn num(r: &str, n: f64) -> String {
    format!(r#"<c r="{r}"><v>{n}</v></c>"#)
}

fn rules_sheet() -> String {
    let names = [
        "介于 3-8",
        "色阶",
        "数据条",
        "图标",
        "文字",
        "隔行",
        "前三/平均",
        "空/错误",
    ];
    let mut rows = String::new();
    let letters = ["A", "B", "C", "D", "E", "F", "G", "H"];
    let mut head = String::new();
    for (l, n) in letters.iter().zip(names) {
        head.push_str(&text(&format!("{l}1"), n));
    }
    rows.push_str(&format!(r#"<row r="1">{head}</row>"#));
    let words = [
        "苹果派",
        "香蕉",
        "苹果",
        "橙子",
        "香蕉",
        "苹果酱",
        "葡萄",
        "橙子",
        "桃子",
        "苹果",
        "李子",
        "香蕉",
    ];
    for i in 0..12u32 {
        let r = i + 2;
        let mut cells = String::new();
        cells.push_str(&num(&format!("A{r}"), f64::from(i + 1)));
        cells.push_str(&num(&format!("B{r}"), f64::from((i * 7 + 3) % 13)));
        cells.push_str(&num(&format!("C{r}"), f64::from((i * 5) % 11) * 10.0 + 5.0));
        cells.push_str(&num(&format!("D{r}"), f64::from((i * 3) % 10)));
        cells.push_str(&text(&format!("E{r}"), words[i as usize]));
        cells.push_str(&num(&format!("F{r}"), f64::from(i + 1)));
        cells.push_str(&num(&format!("G{r}"), f64::from((i * 11) % 17)));
        if i == 3 || i == 4 {
            // Left empty.
        } else if i == 7 {
            cells.push_str(r#"<c r="H9" t="e"><v>#N/A</v></c>"#);
        } else {
            cells.push_str(&num(&format!("H{r}"), f64::from(i)));
        }
        rows.push_str(&format!(r#"<row r="{r}">{cells}</row>"#));
    }
    let cond = r#"
<conditionalFormatting sqref="A2:A13"><cfRule type="cellIs" dxfId="0" priority="1" operator="between"><formula>3</formula><formula>8</formula></cfRule></conditionalFormatting>
<conditionalFormatting sqref="B2:B13"><cfRule type="colorScale" priority="2"><colorScale><cfvo type="min"/><cfvo type="percentile" val="50"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FFFFEB84"/><color rgb="FF63BE7B"/></colorScale></cfRule></conditionalFormatting>
<conditionalFormatting sqref="C2:C13"><cfRule type="dataBar" priority="3"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar></cfRule></conditionalFormatting>
<conditionalFormatting sqref="D2:D13"><cfRule type="iconSet" priority="4"><iconSet iconSet="3Arrows"><cfvo type="percent" val="0"/><cfvo type="percent" val="33"/><cfvo type="percent" val="67"/></iconSet></cfRule></conditionalFormatting>
<conditionalFormatting sqref="E2:E13"><cfRule type="containsText" dxfId="1" priority="5" operator="containsText" text="苹果"><formula>NOT(ISERROR(SEARCH("苹果",E2)))</formula></cfRule><cfRule type="duplicateValues" dxfId="2" priority="6"/></conditionalFormatting>
<conditionalFormatting sqref="F2:F13"><cfRule type="expression" dxfId="3" priority="7"><formula>MOD(ROW(),2)=0</formula></cfRule></conditionalFormatting>
<conditionalFormatting sqref="G2:G13"><cfRule type="top10" dxfId="0" priority="8" rank="3"/><cfRule type="aboveAverage" dxfId="1" priority="9"/></conditionalFormatting>
<conditionalFormatting sqref="H2:H13"><cfRule type="containsBlanks" dxfId="3" priority="10"><formula>LEN(TRIM(H2))=0</formula></cfRule><cfRule type="containsErrors" dxfId="0" priority="11"><formula>ISERROR(H2)</formula></cfRule></conditionalFormatting>"#;
    format!(
        r#"<worksheet xmlns="{NS}"><cols><col min="1" max="8" width="12" customWidth="1"/></cols><sheetData>{rows}</sheetData>{cond}</worksheet>"#
    )
}

fn data_sheet() -> String {
    let mut rows = String::from(r#"<row r="1">"#);
    rows.push_str(&text("A1", "月"));
    rows.push_str(&text("B1", "甲"));
    rows.push_str(&text("C1", "乙"));
    rows.push_str("</row>");
    for i in 0..6u32 {
        let r = i + 2;
        rows.push_str(&format!(
            r#"<row r="{r}">{}{}{}</row>"#,
            text(&format!("A{r}"), &format!("{}月", i + 1)),
            num(&format!("B{r}"), f64::from(10 + i * 9 + (i % 2) * 7)),
            num(&format!("C{r}"), f64::from(70 - i * 8))
        ));
    }
    let mut drawing_anchors = String::new();
    for (k, (col, row)) in [(4, 0), (13, 0), (4, 16), (13, 16), (4, 32), (13, 32)]
        .iter()
        .enumerate()
    {
        drawing_anchors.push_str(&format!(
            r#"<xdr:twoCellAnchor><xdr:from><xdr:col>{col}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{row}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>{}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="{}" name="图表 {}"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/></xdr:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId{}"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>"#,
            col + 8,
            row + 15,
            k + 2,
            k + 1,
            k + 1
        ));
    }
    let drawing = format!(
        r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="{REL}" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">{drawing_anchors}</xdr:wsDr>"#
    );
    // The sheet and its drawing are written as two parts by the caller.
    let sheet = format!(
        r#"<worksheet xmlns="{NS}" xmlns:r="{REL}"><sheetData>{rows}</sheetData><drawing r:id="rId1"/></worksheet>"#
    );
    format!("{sheet}\u{0}{drawing}")
}

fn series(index: u32, value_col: &str, extra: &str) -> String {
    format!(
        r#"<c:ser><c:idx val="{index}"/><c:order val="{index}"/><c:tx><c:strRef><c:f>图表!${value_col}$1</c:f></c:strRef></c:tx>{extra}<c:cat><c:strRef><c:f>图表!$A$2:$A$7</c:f></c:strRef></c:cat><c:val><c:numRef><c:f>图表!${value_col}$2:${value_col}$7</c:f></c:numRef></c:val></c:ser>"#
    )
}

const AXES: &str = r#"<c:axId val="1"/><c:axId val="2"/>"#;
const AXIS_ELEMENTS: &str = r#"<c:catAx><c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="b"/><c:crossAx val="2"/></c:catAx><c:valAx><c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/><c:title><c:tx><c:rich><a:p><a:r><a:t>金额</a:t></a:r></a:p></c:rich></c:tx></c:title><c:numFmt formatCode="0" sourceLinked="0"/><c:crossAx val="1"/></c:valAx>"#;

fn chart(title: &str, plot: &str, axes: bool, legend: &str) -> String {
    let axis_elements = if axes { AXIS_ELEMENTS } else { "" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>{title}</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea>{plot}{axis_elements}</c:plotArea><c:legend><c:legendPos val="{legend}"/></c:legend><c:plotVisOnly val="1"/></c:chart></c:chartSpace>"#
    )
}

fn charts() -> Vec<String> {
    let fill = |c: &str| {
        format!(
            r#"<c:spPr><a:solidFill><a:srgbClr val="{c}"/></a:solidFill><a:ln w="28575"><a:solidFill><a:srgbClr val="{c}"/></a:solidFill></a:ln></c:spPr>"#
        )
    };
    let two = |extra_a: &str, extra_b: &str| {
        format!("{}{}", series(0, "B", extra_a), series(1, "C", extra_b))
    };
    let mut out = Vec::new();
    out.push(chart(
        "簇状柱形图",
        &format!(
            r#"<c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>{}{AXES}</c:barChart>"#,
            two("", "")
        ),
        true,
        "b",
    ));
    out.push(chart(
        "堆积条形图",
        &format!(
            r#"<c:barChart><c:barDir val="bar"/><c:grouping val="stacked"/>{}<c:overlap val="100"/>{AXES}</c:barChart>"#,
            two(&fill("C0504D"), &fill("4F81BD"))
        ),
        true,
        "r",
    ));
    out.push(chart(
        "折线图",
        &format!(
            r#"<c:lineChart><c:grouping val="standard"/>{}<c:marker val="1"/>{AXES}</c:lineChart>"#,
            two("", "")
        ),
        true,
        "t",
    ));
    out.push(
        chart(
            "百分比堆积面积图",
            &format!(
                r#"<c:areaChart><c:grouping val="percentStacked"/>{}{AXES}</c:areaChart>"#,
                two("", "")
            ),
            true,
            "b",
        )
        .replace(r#"formatCode="0""#, r#"formatCode="0%""#),
    );
    out.push(chart(
        "饼图",
        &format!(
            r#"<c:pieChart><c:varyColors val="1"/>{}</c:pieChart>"#,
            series(0, "B", r#"<c:dLbls><c:showPercent val="1"/></c:dLbls>"#)
        ),
        false,
        "r",
    ));
    out.push(chart(
        "散点图（甲对乙）",
        &format!(
            r#"<c:scatterChart><c:scatterStyle val="lineMarker"/><c:ser><c:idx val="0"/><c:order val="0"/><c:tx><c:strRef><c:f>图表!$C$1</c:f></c:strRef></c:tx><c:spPr><a:ln w="19050"><a:noFill/></a:ln></c:spPr><c:xVal><c:numRef><c:f>图表!$B$2:$B$7</c:f></c:numRef></c:xVal><c:yVal><c:numRef><c:f>图表!$C$2:$C$7</c:f></c:numRef></c:yVal></c:ser>{AXES}</c:scatterChart>"#
        ),
        true,
        "b",
    )
    .replace("<c:catAx>", "<c:valAx>")
    .replace("</c:catAx>", "</c:valAx>"));
    out
}

const STYLES: &str = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs><dxfs count="4"><dxf><font><b/><color rgb="FF9C0006"/></font><fill><patternFill><bgColor rgb="FFFFC7CE"/></patternFill></fill></dxf><dxf><font><color rgb="FF006100"/></font><fill><patternFill><bgColor rgb="FFC6EFCE"/></patternFill></fill></dxf><dxf><font><i/><color rgb="FF9C5700"/></font><fill><patternFill><bgColor rgb="FFFFEB9C"/></patternFill></fill></dxf><dxf><fill><patternFill><bgColor rgb="FFEDEDED"/></patternFill></fill></dxf></dxfs></styleSheet>"#;

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!("usage: make_cf_chart_sample <out.xlsx>");
        std::process::exit(2);
    };
    let data = data_sheet();
    let (sheet2, drawing) = data.split_once('\u{0}').expect("two parts");
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut put = |name: &str, data: &str| {
        zip.start_file(name, options).expect("start");
        zip.write_all(data.as_bytes()).expect("write");
    };
    let rel = |entries: &[(String, &str, String)]| {
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (id, kind, target) in entries {
            s.push_str(&format!(
                r#"<Relationship Id="{id}" Type="{REL_TYPE}/{kind}" Target="{target}"/>"#
            ));
        }
        s.push_str("</Relationships>");
        s
    };
    let mut content_types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>"#,
    );
    let charts = charts();
    for n in 1..=charts.len() {
        content_types.push_str(&format!(
            r#"<Override PartName="/xl/charts/chart{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>"#
        ));
    }
    content_types.push_str("</Types>");
    put("[Content_Types].xml", &content_types);
    put(
        "_rels/.rels",
        &rel(&[("rId1".into(), "officeDocument", "xl/workbook.xml".into())]),
    );
    put(
        "xl/workbook.xml",
        &format!(
            r#"<workbook xmlns="{NS}" xmlns:r="{REL}"><sheets><sheet name="条件格式" sheetId="1" r:id="rId1"/><sheet name="图表" sheetId="2" r:id="rId2"/></sheets></workbook>"#
        ),
    );
    put(
        "xl/_rels/workbook.xml.rels",
        &rel(&[
            ("rId1".into(), "worksheet", "worksheets/sheet1.xml".into()),
            ("rId2".into(), "worksheet", "worksheets/sheet2.xml".into()),
            ("rId3".into(), "styles", "styles.xml".into()),
        ]),
    );
    put("xl/styles.xml", STYLES);
    put("xl/worksheets/sheet1.xml", &rules_sheet());
    put("xl/worksheets/sheet2.xml", sheet2);
    put(
        "xl/worksheets/_rels/sheet2.xml.rels",
        &rel(&[("rId1".into(), "drawing", "../drawings/drawing1.xml".into())]),
    );
    put("xl/drawings/drawing1.xml", drawing);
    let entries: Vec<(String, &str, String)> = (1..=charts.len())
        .map(|n| {
            (
                format!("rId{n}"),
                "chart",
                format!("../charts/chart{n}.xml"),
            )
        })
        .collect();
    put("xl/drawings/_rels/drawing1.xml.rels", &rel(&entries));
    for (n, xml) in charts.iter().enumerate() {
        put(&format!("xl/charts/chart{}.xml", n + 1), xml);
    }
    let bytes = zip.finish().expect("finish").into_inner();
    std::fs::write(&out, bytes).expect("write file");
    // It must read back.
    let book = aster_ooxml::xlsx::read(std::fs::read(&out).expect("read")).expect("decode");
    println!(
        "{out}: {} conditional formats, {} charts",
        book.sheets[0].cond_formats.len(),
        book.sheets[1]
            .pictures
            .iter()
            .filter(|p| p.chart.is_some())
            .count()
    );
}
