//! Conditional formats: read for display, and kept as the file has them.

mod common;

use std::sync::Arc;

use aster_ooxml::xlsx_save::{Changes, save};
use aster_ooxml::{FormatCodec, Package, Xlsx};
use aster_sheets::cond::{CfvoKind, Compare, IconFamily, Kind, TextTest};
use aster_sheets::structure::Lines;
use aster_sheets::{Cell, CellRef, Value};
use common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

const WORKBOOK: &str = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets></workbook>"#;

const STYLES: &str = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>
<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs>
<dxfs count="3">
<dxf><font><b/><color rgb="FF9C0006"/></font><fill><patternFill><bgColor rgb="FFFFC7CE"/></patternFill></fill></dxf>
<dxf><font><i val="0"/><color theme="1"/></font><fill><patternFill patternType="solid"><fgColor rgb="FF000000"/><bgColor rgb="FFC6EFCE"/></patternFill></fill></dxf>
<dxf><numFmt numFmtId="165" formatCode="0.0%"/><border><bottom style="thin"><color rgb="FF0000FF"/></bottom></border></dxf>
</dxfs>
</styleSheet>"#;

const COND_A: &str = r#"<conditionalFormatting sqref="A1:A10"><cfRule type="cellIs" dxfId="0" priority="1" stopIfTrue="1" operator="between"><formula>2</formula><formula>8</formula></cfRule><cfRule type="expression" dxfId="1" priority="2"><formula>MOD(ROW(),2)=0</formula></cfRule><cfRule type="containsText" dxfId="2" priority="3" operator="containsText" text="x"><formula>NOT(ISERROR(SEARCH("x",A1)))</formula></cfRule></conditionalFormatting>"#;
const COND_B: &str = r#"<conditionalFormatting sqref="B1:B10 D1:D3"><cfRule type="colorScale" priority="4"><colorScale><cfvo type="min"/><cfvo type="percentile" val="50"/><cfvo type="max"/><color rgb="FFF8696B"/><color rgb="FFFFEB84"/><color theme="4"/></colorScale></cfRule><cfRule type="top10" dxfId="0" priority="5" percent="1" rank="20" bottom="1"/><cfRule type="aboveAverage" dxfId="1" priority="6" aboveAverage="0" equalAverage="1" stdDev="2"/></conditionalFormatting>"#;
const COND_C: &str = r#"<conditionalFormatting sqref="C1:C10"><cfRule type="dataBar" priority="7"><dataBar minLength="5" maxLength="80" showValue="0"><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar></cfRule><cfRule type="iconSet" priority="8"><iconSet iconSet="3Arrows" reverse="1"><cfvo type="percent" val="0"/><cfvo type="percent" val="33"/><cfvo type="num" val="5" gte="0"/></iconSet></cfRule><cfRule type="duplicateValues" dxfId="0" priority="9"/><cfRule type="uniqueValues" dxfId="1" priority="10"/><cfRule type="containsBlanks" dxfId="0" priority="11"><formula>LEN(TRIM(C1))=0</formula></cfRule><cfRule type="notContainsErrors" dxfId="0" priority="12"><formula>NOT(ISERROR(C1))</formula></cfRule><cfRule type="timePeriod" dxfId="0" priority="13" timePeriod="today"><formula>FLOOR(C1,1)=TODAY()</formula></cfRule><cfRule type="beginsWith" dxfId="0" priority="14" operator="beginsWith" text="a"><formula>LEFT(C1,1)="a"</formula></cfRule></conditionalFormatting>"#;
const X14: &str = r#"<extLst><ext uri="{78C0D931-6437-407d-A8EE-F0AAD7539E65}" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><x14:conditionalFormattings><x14:conditionalFormatting xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><x14:cfRule type="dataBar" id="{1}"><x14:dataBar minLength="0" maxLength="100" negativeBarColorSameAsPositive="1"><x14:cfvo type="autoMin"/><x14:cfvo type="autoMax"/></x14:dataBar></x14:cfRule><xm:sqref>E1:E5</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst>"#;

fn sheet() -> String {
    let mut rows = String::new();
    for r in 1..=10 {
        rows.push_str(&format!(
            r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c><c r="B{r}"><v>{}</v></c><c r="C{r}"><v>{}</v></c></row>"#,
            r * 3 % 7,
            r % 4
        ));
    }
    format!(
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData>{COND_A}{COND_B}{COND_C}<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>{X14}</worksheet>"#
    )
}

fn workbook() -> Vec<u8> {
    let content_types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    let workbook_rels = rels(&[
        ("rId1", "worksheet", "worksheets/sheet1.xml"),
        ("rId2", "styles", "styles.xml"),
    ]);
    package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/styles.xml", STYLES.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet().as_bytes()),
    ])
}

fn part(bytes: &[u8], name: &str) -> String {
    String::from_utf8(Package::open(bytes.to_vec()).unwrap().read(name).unwrap()).unwrap()
}

#[test]
fn every_kind_of_rule_is_read_with_its_formats() {
    let book = Xlsx.read(workbook()).unwrap();
    let formats = &book.sheets[0].cond_formats;
    assert_eq!(formats.len(), 3, "the x14 block is not read");
    // A1:A10.
    let a = &formats[0];
    assert_eq!(a.ranges[0].to_string(), "A1:A10");
    assert_eq!(a.rules.len(), 3);
    assert!(matches!(
        &a.rules[0].kind,
        Kind::CellIs { op: Compare::Between, first, second: Some(second) }
            if &**first == "2" && &**second == "8"
    ));
    assert!(a.rules[0].stop_if_true && !a.rules[1].stop_if_true);
    let red = a.rules[0].dxf.as_ref().unwrap();
    assert_eq!(red.fill, Some([0xFF, 0xC7, 0xCE, 0xff]));
    assert_eq!(red.font_color, Some([0x9C, 0x00, 0x06, 0xff]));
    assert_eq!(red.bold, Some(true));
    // A solid pattern shows its background colour in a dxf.
    let green = a.rules[1].dxf.as_ref().unwrap();
    assert_eq!(green.fill, Some([0xC6, 0xEF, 0xCE, 0xff]));
    assert_eq!(green.italic, Some(false));
    assert_eq!(
        a.rules[1].kind,
        Kind::Expression(Arc::from("MOD(ROW(),2)=0"))
    );
    assert!(matches!(
        &a.rules[2].kind,
        Kind::Text { test: TextTest::Contains, text } if &**text == "x"
    ));
    let third = a.rules[2].dxf.as_ref().unwrap();
    assert_eq!(third.format.as_ref().unwrap().number(0.256, false), "25.6%");
    assert!(third.borders.bottom.is_some());
    // B1:B10 D1:D3.
    let b = &formats[1];
    assert_eq!(b.ranges.len(), 2);
    let Kind::ColorScale(stops) = &b.rules[0].kind else {
        panic!("{:?}", b.rules[0].kind)
    };
    assert_eq!(stops.len(), 3);
    assert_eq!(stops[1].0.kind, CfvoKind::Percentile);
    assert_eq!(stops[1].0.value, "50");
    assert_eq!(stops[0].1, [0xF8, 0x69, 0x6B, 0xff]);
    // Theme colour 4 is the file's default accent 1.
    assert_eq!(stops[2].1[3], 0xff);
    assert_eq!(
        b.rules[1].kind,
        Kind::Top {
            rank: 20,
            percent: true,
            bottom: true
        }
    );
    assert_eq!(
        b.rules[2].kind,
        Kind::Average {
            above: false,
            equal: true,
            std_dev: 2
        }
    );
    // C1:C10.
    let c = &formats[2];
    let Kind::DataBar {
        min_length,
        max_length,
        show_value,
        color,
        ..
    } = &c.rules[0].kind
    else {
        panic!("{:?}", c.rules[0].kind)
    };
    assert_eq!((*min_length, *max_length, *show_value), (5, 80, false));
    assert_eq!(*color, [0x63, 0x8E, 0xC6, 0xff]);
    let Kind::IconSet {
        set,
        steps,
        reverse,
        show_value,
    } = &c.rules[1].kind
    else {
        panic!("{:?}", c.rules[1].kind)
    };
    assert_eq!((set.family, set.count), (IconFamily::Arrows, 3));
    assert!(*reverse && *show_value);
    assert!(steps[0].gte && !steps[2].gte);
    assert_eq!(c.rules[2].kind, Kind::Duplicates { unique: false });
    assert_eq!(c.rules[3].kind, Kind::Duplicates { unique: true });
    assert_eq!(c.rules[4].kind, Kind::Blanks(true));
    assert_eq!(c.rules[5].kind, Kind::Errors(false));
    // A rule type that is not drawn is kept as a name, not dropped.
    assert_eq!(c.rules[6].kind, Kind::Unsupported("timePeriod".to_owned()));
    assert!(matches!(
        &c.rules[7].kind,
        Kind::Text {
            test: TextTest::Begins,
            ..
        }
    ));
}

#[test]
fn saving_keeps_the_rules_as_written() {
    let source = workbook();
    let book = Xlsx.read(source.clone()).unwrap();
    let saved = save(source.clone(), &book, &Changes::default()).unwrap();
    assert_eq!(part(&saved, "xl/worksheets/sheet1.xml"), sheet());
    assert_eq!(part(&saved, "xl/styles.xml"), STYLES);
    // After an edit of a cell: the cell is written again, the rules,
    // their styles and the newer rules are not touched.
    let mut book = book;
    let mut changes = Changes::default();
    let at = CellRef::parse("A3").unwrap();
    for change in book.set_cells(0, vec![(at, Some(Cell::new(0, Value::Number(77.0))))]) {
        changes.cells.insert((0, change.at));
        changes.edited.insert((0, change.at));
    }
    let saved = save(source, &book, &changes).unwrap();
    let xml = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(xml.contains("<v>77</v>"));
    for kept in [COND_A, COND_B, COND_C, X14] {
        assert!(xml.contains(kept), "{kept}");
    }
    assert_eq!(part(&saved, "xl/styles.xml"), STYLES);
}

#[test]
fn rules_in_the_model_and_in_the_file_move_together() {
    let source = workbook();
    let mut book = Xlsx.read(source.clone()).unwrap();
    assert_eq!(book.sheets[0].structure_blocker.as_deref(), Some("extLst"));
    // The x14 block makes the sheet one rows cannot be inserted in; take
    // it out as an older file would have it.
    let plain = source_without_x14();
    book = Xlsx.read(plain.clone()).unwrap();
    assert_eq!(book.sheets[0].structure_blocker, None);
    let lines = Lines {
        columns: false,
        at: 0,
        count: 2,
        insert: true,
    };
    let mut changes = Changes::default();
    for cell in book.restructure(0, &lines) {
        changes.cells.insert(cell);
        changes.edited.insert(cell);
    }
    changes.formulas_changed = true;
    changes.merges.insert(0);
    changes.structure.push((0, lines));
    let formats = &book.sheets[0].cond_formats;
    assert_eq!(formats[0].ranges[0].to_string(), "A3:A12");
    let saved = save(plain, &book, &changes).unwrap();
    let xml = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(xml.contains(r#"sqref="A3:A12""#), "{xml}");
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(again.sheets[0].cond_formats, book.sheets[0].cond_formats);
}

fn source_without_x14() -> Vec<u8> {
    let sheet = sheet().replace(X14, "");
    let content_types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    let workbook_rels = rels(&[
        ("rId1", "worksheet", "worksheets/sheet1.xml"),
        ("rId2", "styles", "styles.xml"),
    ]);
    package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/styles.xml", STYLES.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

#[test]
fn broken_and_hostile_rules_do_not_fail_the_workbook() {
    // Unknown types, a dxf that is not there, a cfvo that is not one,
    // thousands of rules and an enormous formula.
    let huge = "1+".repeat(20_000);
    let many: String = (0..2_000)
        .map(|i| {
            format!(
                r#"<conditionalFormatting sqref="A{r}"><cfRule type="expression" priority="{i}"><formula>A{r}&gt;1</formula></cfRule></conditionalFormatting>"#,
                r = i % 100 + 1
            )
        })
        .collect();
    let sheet = format!(
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><conditionalFormatting sqref="A1"><cfRule type="nonsense" priority="1"/><cfRule type="cellIs" dxfId="99" priority="2" operator="bogus"><formula>1</formula></cfRule><cfRule type="colorScale" priority="3"><colorScale><cfvo type="weird"/><cfvo type="max"/><color rgb="FF000000"/><color rgb="FFFFFFFF"/></colorScale></cfRule><cfRule type="expression" priority="4"><formula>{huge}1</formula></cfRule><cfRule type="iconSet" priority="5"><iconSet iconSet="9Nothing"><cfvo type="min"/></iconSet></cfRule><cfRule type="top10" priority="6" rank="-5"/></conditionalFormatting><conditionalFormatting sqref="not a range"><cfRule type="expression"><formula>1</formula></cfRule></conditionalFormatting><conditionalFormatting><cfRule type="expression"/></conditionalFormatting>{many}</worksheet>"#
    );
    let content_types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    let workbook_rels = rels(&[("rId1", "worksheet", "worksheets/sheet1.xml")]);
    let bytes = package(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ]);
    let book = Xlsx.read(bytes).unwrap();
    let formats = &book.sheets[0].cond_formats;
    // The first block's six rules are all read, none of them drawable but
    // the last; the blocks without cells are dropped.
    assert_eq!(formats[0].rules.len(), 6);
    assert!(
        formats[0].rules[..5]
            .iter()
            .all(|r| matches!(r.kind, Kind::Unsupported(_)))
    );
    assert!(formats[0].rules[0].dxf.is_none());
    assert_eq!(formats.len(), 1 + 2_000);
}
