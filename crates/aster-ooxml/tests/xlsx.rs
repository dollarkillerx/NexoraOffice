mod common;

use std::time::Instant;

use aster_ooxml::{Error, ErrorKind, FormatCodec, Limits, Package, Xlsx, xlsx};
use aster_sheets::{CellRef, DisplayKind, HAlign, SheetKind, Value};
use common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

const MAIN_CT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";

fn content_types(sheets: usize) -> String {
    let mut out = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="{MAIN_CT}"/>"#
    );
    for n in 1..=sheets {
        out.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
    }
    out.push_str("</Types>");
    out
}

fn at(text: &str) -> CellRef {
    CellRef::parse(text).unwrap()
}

/// Two worksheets, one hidden, and a chart sheet; prefixed element names as
/// some producers write them.
fn sample() -> Vec<u8> {
    let workbook = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:workbook xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <x:workbookPr date1904="0"/>
  <x:bookViews><x:workbookView activeTab="1"/></x:bookViews>
  <x:sheets>
    <x:sheet name="売上 2026" sheetId="1" r:id="rId1"/>
    <x:sheet name="隐藏" sheetId="2" state="hidden" r:id="rId2"/>
    <x:sheet name="グラフ" sheetId="3" r:id="rId3"/>
  </x:sheets>
</x:workbook>"#;
    let workbook_rels = rels(&[
        ("rId1", "worksheet", "worksheets/sheet1.xml"),
        ("rId2", "worksheet", "/xl/worksheets/sheet2.xml"),
        ("rId3", "chartsheet", "chartsheets/sheet1.xml"),
        ("rId4", "sharedStrings", "sharedStrings.xml"),
        ("rId5", "styles", "styles.xml"),
    ]);
    let strings = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="4" uniqueCount="4">
  <si><t>商品名</t></si>
  <si><r><rPr><b/></rPr><t>東京</t></r><r><t xml:space="preserve"> 本店</t></r><rPh sb="0" eb="2"><t>トウキョウ</t></rPh><phoneticPr fontId="1"/></si>
  <si><t>line_x000D_break &amp; more</t></si>
  <si><t>上海浦东</t></si>
</sst>"#;
    let styles = r##"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="1"><numFmt numFmtId="164" formatCode="#,##0.00&quot;円&quot;"/></numFmts>
  <cellStyleXfs count="1"><xf numFmtId="49"/></cellStyleXfs>
  <cellXfs count="4">
    <xf numFmtId="0" fontId="0"/>
    <xf numFmtId="164" applyNumberFormat="1"/>
    <xf numFmtId="14" applyNumberFormat="1"><alignment horizontal="center"/></xf>
    <xf numFmtId="9"/>
  </cellXfs>
</styleSheet>"##;
    let sheet1 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetViews><sheetView workbookViewId="0"><selection activeCell="A1"/></sheetView></sheetViews>
  <sheetFormatPr defaultRowHeight="15"/>
  <cols>
    <col min="1" max="1" width="20.7109375" customWidth="1"/>
    <col min="3" max="4" width="0" hidden="1"/>
  </cols>
  <sheetData>
    <row r="1" ht="30" customHeight="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="E1" t="inlineStr"><is><t>インライン</t></is></c></row>
    <row r="2"><c r="A2" s="1"><v>1234.5</v></c><c r="B2" s="2"><v>46348</v></c><c r="E2" s="3"><v>0.256</v></c></row>
    <row r="3" hidden="1"><c r="A3" t="b"><v>1</v></c><c r="B3" t="e"><v>#DIV/0!</v></c></row>
    <row r="4"><c r="A4"><f>SUM(A2:A3)</f><v>1234.5</v></c><c r="B4"><f>A2*2</f></c><c r="E4" t="str"><f>A1&amp;"!"</f><v>商品名!</v></c></row>
    <row r="5"><c r="A5"><f t="shared" ref="A5:A7" si="0">A2+B$2</f><v>1</v></c></row>
    <row r="6"><c r="A6"><f t="shared" si="0"/><v>2</v></c></row>
    <row r="7"><c r="A7"><f t="shared" si="0"/><v>3</v></c><c r="B7" s="2"/></row>
    <row r="8"><c t="s"><v>3</v></c><c><v>8</v></c></row>
    <row><c r="A9" t="s"><v>99</v></c></row>
  </sheetData>
  <mergeCells count="1"><mergeCell ref="A10:C12"/></mergeCells>
</worksheet>"#;
    let sheet2 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
    package(&[
        ("[Content_Types].xml", content_types(2).as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/sharedStrings.xml", strings.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
        ("xl/worksheets/sheet2.xml", sheet2.as_bytes()),
    ])
}

#[test]
fn a_workbook_with_chinese_and_japanese_content() {
    let workbook = Xlsx.read(sample()).unwrap();
    let names: Vec<&str> = workbook.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["売上 2026", "隐藏", "グラフ"]);
    assert!(workbook.sheets[1].hidden);
    // The chart sheet's part is missing; it is still listed, empty.
    assert_eq!(workbook.sheets[2].cell_count(), 0);
    assert_eq!(workbook.active_sheet, 1);

    let sheet = &workbook.sheets[0];
    assert_eq!(sheet.kind, SheetKind::Worksheet);
    let shown = |cell: &str| {
        workbook
            .display(sheet.cell(at(cell)).unwrap())
            .map(|d| d.text)
            .unwrap_or_default()
    };
    assert_eq!(shown("A1"), "商品名");
    // Rich text runs joined, the furigana left out.
    assert_eq!(shown("B1"), "東京 本店");
    assert_eq!(shown("E1"), "インライン");
    assert_eq!(shown("A2"), "1,234.50円");
    assert_eq!(shown("B2"), "2026/11/22");
    assert_eq!(
        workbook
            .display(sheet.cell(at("B2")).unwrap())
            .unwrap()
            .align,
        HAlign::Center
    );
    assert_eq!(shown("E2"), "26%");
    assert_eq!(shown("A3"), "TRUE");
    assert_eq!(shown("B3"), "#DIV/0!");
    // Formulas show the result saved with the file...
    assert_eq!(shown("A4"), "1234.5");
    assert_eq!(
        workbook.input_text(sheet.cell(at("A4")).unwrap()),
        "=SUM(A2:A3)"
    );
    assert_eq!(shown("E4"), "商品名!");
    // ...and their own text when no result was saved.
    let b4 = workbook.display(sheet.cell(at("B4")).unwrap()).unwrap();
    assert_eq!(
        (b4.text.as_str(), b4.kind),
        ("=A2*2", DisplayKind::Uncomputed)
    );
    // Shared formulas are expanded for each cell.
    assert_eq!(
        workbook.input_text(sheet.cell(at("A6")).unwrap()),
        "=A3+B$2"
    );
    assert_eq!(
        workbook.input_text(sheet.cell(at("A7")).unwrap()),
        "=A4+B$2"
    );
    // A styled but empty cell is not kept.
    assert!(sheet.cell(at("B7")).is_none());
    // Cells without an address follow the one before.
    assert_eq!(shown("A8"), "上海浦东");
    assert_eq!(shown("B8"), "8");
    // A row without a number follows the previous row; a string index past
    // the table is an error value, not a crash.
    assert_eq!(
        sheet.cell(at("A9")).unwrap().value,
        Value::Error("#REF!".into())
    );
    assert_eq!(sheet.merges.len(), 1);
    assert_eq!(sheet.used_range().unwrap().to_string(), "A1:E12");

    // Sizes: 20.71 digits is 145 pixels; hidden columns and rows are 0.
    assert_eq!(sheet.columns.size(0), 145.0);
    assert_eq!(sheet.columns.size(1), 64.0);
    assert_eq!((sheet.columns.size(2), sheet.columns.size(3)), (0.0, 0.0));
    assert_eq!(sheet.row_sizes.size(0), 40.0);
    assert_eq!(sheet.row_sizes.size(1), 20.0);
    assert_eq!(sheet.row_sizes.size(2), 0.0);
}

#[test]
fn line_breaks_and_entities_in_shared_strings() {
    let workbook = Xlsx.read(sample()).unwrap();
    let strings: Vec<String> = workbook.sheets[0]
        .cells_in(aster_sheets::CellRange::parse("A1:Z20").unwrap())
        .filter_map(|(_, cell)| match &cell.value {
            Value::Text(text) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert!(!strings.iter().any(|s| s.contains("_x000D_")));
}

#[test]
fn an_empty_workbook_sheet_opens() {
    let workbook = Xlsx.read(sample()).unwrap();
    let hidden = &workbook.sheets[1];
    assert_eq!(hidden.cell_count(), 0);
    assert_eq!(hidden.used_range(), None);

    // A worksheet part with nothing in it at all, not even `sheetData`.
    let bare = package(&[
        ("[Content_Types].xml", content_types(1).as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        (
            "xl/workbook.xml",
            br#"<workbook xmlns:r="r"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[("rId1", "worksheet", "worksheets/sheet1.xml")]).as_bytes(),
        ),
        ("xl/worksheets/sheet1.xml", b"<worksheet/>"),
    ]);
    let workbook = Xlsx.read(bare).unwrap();
    assert_eq!(workbook.sheets.len(), 1);
    assert_eq!(workbook.sheets[0].cell_count(), 0);
    assert!(workbook.styles.is_empty());
}

#[test]
fn a_large_sheet_reads_in_reasonable_time() {
    // 50,000 rows by 8 columns: 400,000 cells, about 25 MB of XML.
    let rows = 50_000u32;
    let mut sheet = String::with_capacity(30 << 20);
    sheet.push_str(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#);
    for r in 1..=rows {
        sheet.push_str(&format!(r#"<row r="{r}">"#));
        for (c, col) in ["A", "B", "C", "D", "E", "F", "G", "H"].iter().enumerate() {
            if c == 0 {
                sheet.push_str(&format!(r#"<c r="{col}{r}" t="s"><v>{}</v></c>"#, r % 3));
            } else {
                sheet.push_str(&format!(
                    r#"<c r="{col}{r}"><v>{}.5</v></c>"#,
                    r * 10 + c as u32
                ));
            }
        }
        sheet.push_str("</row>");
    }
    // One cell in the very last corner of the sheet.
    sheet.push_str(r#"<row r="1048576"><c r="XFD1048576"><v>1</v></c></row>"#);
    sheet.push_str("</sheetData></worksheet>");
    let strings = r#"<sst><si><t>苹果</t></si><si><t>りんご</t></si><si><t>apple</t></si></sst>"#;
    let bytes = package(&[
        ("[Content_Types].xml", content_types(1).as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        (
            "xl/workbook.xml",
            br#"<workbook xmlns:r="r"><sheets><sheet name="Big" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[
                ("rId1", "worksheet", "worksheets/sheet1.xml"),
                ("rId2", "sharedStrings", "sharedStrings.xml"),
            ])
            .as_bytes(),
        ),
        ("xl/sharedStrings.xml", strings.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ]);
    let started = Instant::now();
    let workbook = xlsx::read(bytes).unwrap();
    let elapsed = started.elapsed();
    let sheet = &workbook.sheets[0];
    assert_eq!(sheet.cell_count(), rows as usize * 8 + 1);
    assert_eq!(sheet.used_range().unwrap().to_string(), "A1:XFD1048576");
    assert_eq!(
        sheet.cell(at("A49999")).unwrap().value,
        Value::Text("りんご".into())
    );
    assert_eq!(sheet.cell(at("H123")).unwrap().value, Value::Number(1237.5));
    // Generous: an unoptimised build on a slow machine. The release build
    // reads this in a fraction of that.
    assert!(elapsed.as_secs() < 20, "took {elapsed:?}");
}

#[test]
fn other_files_are_refused_with_a_reason() {
    assert_eq!(
        Xlsx.read(b"not a zip".to_vec()).unwrap_err().kind(),
        ErrorKind::NotZip
    );
    let mut legacy = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    legacy.resize(4096, 0);
    assert_eq!(Xlsx.read(legacy).unwrap_err(), Error::LegacyOrEncrypted);
    // A zip file that is not an Office document.
    let plain = package(&[("hello.txt", b"hi")]);
    assert_eq!(Xlsx.read(plain).unwrap_err().kind(), ErrorKind::WrongKind);
    // A presentation is not a workbook.
    let pptx = package(&[
        (
            "[Content_Types].xml",
            format!(
                r#"{CONTENT_TYPES_HEAD}<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#
            )
            .as_bytes(),
        ),
        ("_rels/.rels", root_rels("ppt/presentation.xml").as_bytes()),
        ("ppt/presentation.xml", b"<presentation/>"),
    ]);
    let err = Xlsx.read(pptx).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WrongKind);
    assert!(err.to_string().contains("PowerPoint"), "{err}");
    // A workbook part that is not well-formed XML.
    let broken = package(&[
        ("[Content_Types].xml", content_types(0).as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        (
            "xl/workbook.xml",
            b"<workbook><sheets><sheet name=\"a\" r:id=</workbook>",
        ),
    ]);
    assert_eq!(Xlsx.read(broken).unwrap_err().kind(), ErrorKind::Broken);
}

#[test]
fn parts_that_inflate_too_far_are_refused() {
    let zeros = vec![b' '; 8 << 20];
    let bytes = package(&[
        ("[Content_Types].xml", content_types(0).as_bytes()),
        ("big.xml", &zeros),
    ]);
    let mut package = Package::open_with(
        bytes.clone(),
        Limits {
            max_ratio: 100,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        package.read("big.xml").unwrap_err().kind(),
        ErrorKind::TooLarge
    );

    let mut package = Package::open_with(
        bytes,
        Limits {
            max_part: 1 << 20,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        package.read("big.xml").unwrap_err().kind(),
        ErrorKind::TooLarge
    );
    // Part names compare without case.
    assert!(package.has("BIG.XML"));
}

/// Fonts, fills, borders and alignment with theme, tinted and indexed
/// colours; a frozen pane; whole-row and whole-column formats; a picture
/// and a chart on a drawing; the Chinese default font.
fn styled() -> Vec<u8> {
    let workbook = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="样式" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#;
    let workbook_rels = rels(&[
        ("rId1", "worksheet", "worksheets/sheet1.xml"),
        ("rId2", "styles", "styles.xml"),
        ("rId3", "theme", "theme/theme1.xml"),
    ]);
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements>
  <a:clrScheme name="x"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
  <a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1></a:clrScheme>
</a:themeElements></a:theme>"#;
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="3">
    <font><sz val="11"/><color theme="1"/><name val="等线"/></font>
    <font><b/><i val="0"/><u/><strike/><sz val="14"/><color theme="0"/><name val="Arial"/></font>
    <font><sz val="9"/><color indexed="10"/><name val="Calibri"/></font>
  </fonts>
  <fills count="6">
    <fill><patternFill patternType="none"/></fill>
    <fill><patternFill patternType="gray125"/></fill>
    <fill><patternFill patternType="solid"><fgColor theme="4" tint="0.79998168889431442"/><bgColor indexed="64"/></patternFill></fill>
    <fill><patternFill patternType="solid"><fgColor indexed="13"/></patternFill></fill>
    <fill><patternFill patternType="solid"><fgColor rgb="FF7C5CE0"/></patternFill></fill>
    <fill><gradientFill degree="90"><stop position="0"><color rgb="FF00B050"/></stop><stop position="1"><color theme="4"/></stop></gradientFill></fill>
  </fills>
  <borders count="2">
    <border><left/><right/><top/><bottom/><diagonal/></border>
    <border><left style="thin"><color auto="1"/></left><right style="dashed"><color rgb="FFFF0000"/></right><top/><bottom style="double"><color theme="4" tint="-0.249977111117893"/></bottom><diagonal style="thin"/></border>
  </borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="6">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
    <xf numFmtId="0" fontId="1" fillId="2" borderId="1"><alignment horizontal="center" vertical="top" wrapText="1"/></xf>
    <xf numFmtId="0" fontId="2" fillId="3" borderId="0"><alignment horizontal="left" vertical="center" indent="2"/></xf>
    <xf numFmtId="0" fontId="0" fillId="4" borderId="0"/>
    <xf numFmtId="0" fontId="0" fillId="5" borderId="0"/>
    <xf numFmtId="0" fontId="9" fillId="99" borderId="99"/>
  </cellXfs>
  <colors><indexedColors>
    <rgbColor rgb="00000000"/><rgbColor rgb="00FFFFFF"/><rgbColor rgb="00FF0000"/><rgbColor rgb="0000FF00"/>
    <rgbColor rgb="000000FF"/><rgbColor rgb="00FFFF00"/><rgbColor rgb="00FF00FF"/><rgbColor rgb="0000FFFF"/>
    <rgbColor rgb="00000000"/><rgbColor rgb="00FFFFFF"/><rgbColor rgb="00123456"/>
  </indexedColors></colors>
</styleSheet>"#;
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetViews><sheetView showGridLines="0" workbookViewId="0"><pane xSplit="1" ySplit="2" topLeftCell="B3" activePane="bottomRight" state="frozen"/></sheetView></sheetViews>
  <sheetFormatPr defaultRowHeight="14.25"/>
  <cols><col min="1" max="1" width="16.625" customWidth="1"/><col min="3" max="4" width="9" style="3" customWidth="1"/></cols>
  <sheetData>
    <row r="1"><c r="A1" s="1" t="inlineStr"><is><t>标题</t></is></c><c r="B1" s="1"/><c r="E1" s="2"><v>5</v></c><c r="F1" s="5"><v>1</v></c></row>
    <row r="2"><c r="A2" s="4"/><c r="B2" s="0"/></row>
    <row r="5" s="4" customFormat="1"><c r="A5"><v>1</v></c></row>
  </sheetData>
  <drawing r:id="rId1"/>
</worksheet>"#;
    let sheet_rels = rels(&[("rId1", "drawing", "../drawings/drawing1.xml")]);
    let drawing = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <xdr:twoCellAnchor editAs="oneCell">
    <xdr:from><xdr:col>2</xdr:col><xdr:colOff>95250</xdr:colOff><xdr:row>3</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
    <xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>10</xdr:row><xdr:rowOff>190500</xdr:rowOff></xdr:to>
    <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="图片 1"/><xdr:cNvPicPr/></xdr:nvPicPr>
      <xdr:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr/></xdr:pic>
    <xdr:clientData/>
  </xdr:twoCellAnchor>
  <mc:AlternateContent><mc:Choice Requires="a14"><xdr:oneCellAnchor>
    <xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>20</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
    <xdr:ext cx="952500" cy="476250"/>
    <xdr:graphicFrame macro=""><xdr:nvGraphicFramePr><xdr:cNvPr id="3" name="グラフ 2"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>
      <xdr:xfrm/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"/></a:graphic></xdr:graphicFrame>
    <xdr:clientData/></xdr:oneCellAnchor></mc:Choice></mc:AlternateContent>
  <xdr:absoluteAnchor><xdr:pos x="0" y="0"/><xdr:ext cx="9525" cy="9525"/>
    <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="4" name="lost"/></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId9"/></xdr:blipFill></xdr:pic><xdr:clientData/></xdr:absoluteAnchor>
</xdr:wsDr>"#;
    let drawing_rels = rels(&[("rId1", "image", "../media/image1.png")]);
    let png: &[u8] = b"\x89PNG\r\n\x1a\nnot really";
    package(&[
        ("[Content_Types].xml", content_types(1).as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
        ("xl/theme/theme1.xml", theme.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels.as_bytes()),
        ("xl/drawings/drawing1.xml", drawing.as_bytes()),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            drawing_rels.as_bytes(),
        ),
        ("xl/media/image1.png", png),
    ])
}

#[test]
fn styles_colours_panes_and_pictures() {
    use aster_sheets::{LineStyle, Placement, VAlign};

    let workbook = Xlsx.read(styled()).unwrap();
    let sheet = &workbook.sheets[0];
    let style_of = |cell: &str| workbook.style(sheet.style_at(at(cell)));

    let title = style_of("A1");
    assert!(title.font.bold && !title.font.italic && title.font.underline && title.font.strike);
    assert_eq!(title.font.size, 14.0);
    assert_eq!(title.font.name.as_deref(), Some("Arial"));
    // Theme 0 is lt1 (white), not dk1.
    assert_eq!(title.font.color, Some([0xFF, 0xFF, 0xFF, 0xFF]));
    // Accent 1, lighter 80%.
    assert_eq!(title.fill, Some([0xDA, 0xE3, 0xF3, 0xFF]));
    assert_eq!(
        (title.align, title.valign, title.wrap),
        (Some(HAlign::Center), VAlign::Top, true)
    );
    let left = title.borders.left.unwrap();
    assert_eq!((left.style, left.color), (LineStyle::Thin, None));
    assert_eq!(title.borders.right.unwrap().color, Some([0xFF, 0, 0, 0xFF]));
    let bottom = title.borders.bottom.unwrap();
    assert_eq!(bottom.style, LineStyle::Double);
    assert_eq!(bottom.color, Some([0x2F, 0x55, 0x97, 0xFF]));
    assert_eq!(title.borders.top, None);

    // The file's own palette replaces index 10; 13 is past it, so the
    // default palette's yellow.
    let indexed = style_of("E1");
    assert_eq!(indexed.font.color, Some([0x12, 0x34, 0x56, 0xFF]));
    assert_eq!(indexed.fill, Some([0xFF, 0xFF, 0x00, 0xFF]));
    assert_eq!((indexed.indent, indexed.valign), (2, VAlign::Center));
    // A gradient shows its first colour; indices past the lists fall back.
    assert_eq!(style_of("F2").fill, None);
    assert_eq!(workbook.style(4).fill, Some([0x00, 0xB0, 0x50, 0xFF]));
    assert_eq!(style_of("F1").font.size, 11.0);
    assert_eq!(style_of("F1").fill, None);

    // An empty cell with a fill or border is kept, one without is not.
    assert!(sheet.cell(at("B1")).is_some_and(|c| c.is_blank()));
    assert!(sheet.cell(at("A2")).is_some());
    assert!(sheet.cell(at("B2")).is_none());
    assert_eq!(sheet.cell_count(), 4);
    // Whole-column and whole-row formats.
    assert_eq!(style_of("D40").fill, Some([0x7C, 0x5C, 0xE0, 0xFF]));
    assert_eq!(style_of("B40").fill, None);
    assert_eq!(style_of("D5").fill, Some([0x00, 0xB0, 0x50, 0xFF]));

    assert_eq!(sheet.frozen, (2, 1));
    assert!(!sheet.show_grid);
    // 等线 counts columns in 8-pixel digits: 16.625 → 133 pixels.
    assert_eq!(sheet.columns.size(0), 133.0);
    assert_eq!(sheet.columns.size(2), 72.0);

    assert_eq!(sheet.pictures.len(), 3);
    let picture = &sheet.pictures[0];
    assert_eq!(picture.name, "图片 1");
    assert_eq!(&picture.image.as_deref().unwrap()[..4], b"\x89PNG");
    let Placement::TwoCell { from, to } = picture.placement else {
        panic!("{:?}", picture.placement);
    };
    assert_eq!((from.cell, from.dx, from.dy), (at("C4"), 10.0, 0.0));
    assert_eq!((to.cell, to.dy), (at("F11"), 20.0));
    let chart = &sheet.pictures[1];
    assert_eq!(
        (chart.name.as_str(), chart.image.is_none()),
        ("グラフ 2", true)
    );
    assert!(matches!(
        chart.placement,
        Placement::OneCell {
            width: 100.0,
            height: 50.0,
            ..
        }
    ));
    // A picture whose image part is missing keeps its frame.
    assert!(sheet.pictures[2].image.is_none());
}

#[test]
fn the_digit_width_follows_the_normal_font() {
    use aster_sheets::Font;
    use std::sync::Arc;

    let font = |name: &str, size: f32| Font {
        name: Some(Arc::from(name)),
        size,
        ..Font::default()
    };
    assert_eq!(xlsx::digit_width(&Font::default()), 7.0);
    assert_eq!(xlsx::digit_width(&font("Calibri", 11.0)), 7.0);
    assert_eq!(xlsx::digit_width(&font("等线", 11.0)), 8.0);
    assert_eq!(xlsx::digit_width(&font("Yu Gothic", 11.0)), 8.0);
    assert_eq!(xlsx::digit_width(&font("ＭＳ Ｐゴシック", 11.0)), 8.0);
    assert_eq!(xlsx::digit_width(&font("Arial", 10.0)), 7.0);
    assert_eq!(xlsx::digit_width(&font("Calibri", 22.0)), 15.0);
}
