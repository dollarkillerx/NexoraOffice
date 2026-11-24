//! Saving edited workbooks: open, change, save, open again; and every part
//! not changed is the same, byte for byte.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use aster_ooxml::xlsx_save::{Changes, save};
use aster_ooxml::{FormatCodec, Package, Xlsx};
use aster_sheets::{Cell, CellRef, Value, Workbook};
use common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

fn at(text: &str) -> CellRef {
    CellRef::parse(text).unwrap()
}

/// G4's workbook (an Excel file with shared formulas, shared strings, a
/// calculation chain, comments, a table and a drawing), zipped from the
/// corpus folder.
fn g4() -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/g4-ooxml/corpus/xlsx");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let parts: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    package(&parts)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, out);
        } else {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, std::fs::read(&path).unwrap()));
        }
    }
}

/// Parts whose bytes differ, and parts only in one of the two.
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

fn part(bytes: &[u8], name: &str) -> String {
    String::from_utf8(Package::open(bytes.to_vec()).unwrap().read(name).unwrap()).unwrap()
}

/// Change cells on sheet 0 as an edit would, and say so.
fn edit(book: &mut Workbook, changes: &mut Changes, cells: Vec<(&str, Option<Cell>)>) {
    let cells: Vec<(CellRef, Option<Cell>)> = cells.into_iter().map(|(a, c)| (at(a), c)).collect();
    for change in book.set_cells(0, cells) {
        changes.cells.insert((0, change.at));
        changes.edited.insert((0, change.at));
        changes.formulas_changed |= change.touches_formula();
    }
}

fn number(col: u32, n: f64) -> Cell {
    Cell::new(col, Value::Number(n))
}

#[test]
fn nothing_changed_saves_the_same_parts() {
    let source = g4();
    let book = Xlsx.read(source.clone()).unwrap();
    let saved = save(source.clone(), &book, &Changes::default()).unwrap();
    assert!(differences(&source, &saved).is_empty());
}

#[test]
fn a_changed_value_rewrites_only_its_cell_and_the_results_it_changes() {
    let source = g4();
    let original_sheet = part(&source, "xl/worksheets/sheet1.xml");
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    let mut b2 = book.sheets[0].cell(at("B2")).unwrap().clone();
    b2.value = Value::Number(1500.0);
    edit(&mut book, &mut changes, vec![("B2", Some(b2))]);
    // The engine's new result for D2, a shared formula's first cell.
    book.apply_results(&[aster_sheets::Computed {
        sheet: 0,
        at: at("D2"),
        value: Value::Number(4500.0),
    }]);
    changes.cells.insert((0, at("D2")));
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from(["xl/worksheets/sheet1.xml".to_owned()])
    );
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    // Only the two cells differ.
    let expected = original_sheet
        .replace(
            r#"<c r="B2" s="2"><v>1200</v></c>"#,
            r#"<c r="B2" s="2"><v>1500</v></c>"#,
        )
        .replace(
            r#"<c r="D2" s="2"><f t="shared" ref="D2:D4" si="0">B2*C2</f><v>3600</v></c>"#,
            r#"<c r="D2" s="2"><f t="shared" ref="D2:D4" si="0">B2*C2</f><v>4500</v></c>"#,
        );
    assert_ne!(expected, original_sheet);
    assert_eq!(sheet, expected);
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(
        again.sheets[0].cell(at("B2")).unwrap().value,
        Value::Number(1500.0)
    );
    assert_eq!(
        again.sheets[0].cell(at("D3")).unwrap().formula.as_deref(),
        Some("B3*C3")
    );
}

#[test]
fn new_text_formulas_and_rows_and_the_calculation_chain() {
    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    let mut formula = Cell::new(5, Value::Number(15966.0 * 2.0));
    formula.formula = Some(Arc::from("D5*2"));
    edit(
        &mut book,
        &mut changes,
        vec![
            // New text in a row the sheet has, and in a new row past the end.
            (
                "F2",
                Some(Cell::new(5, Value::Text("新しい & <値>".into()))),
            ),
            ("A40", Some(Cell::new(0, Value::Text(" 前后空格 ".into())))),
            ("B40", Some(number(1, 0.1))),
            ("C1", Some(Cell::new(2, Value::Text("数量".into())))),
            ("F5", Some(formula)),
            // A cell cleared.
            ("C3", None),
        ],
    );
    let saved = save(source.clone(), &book, &changes).unwrap();
    // The new formula makes the calculation chain stale: it goes, with its
    // relationship and content type.
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "[Content_Types].xml".to_owned(),
            "xl/_rels/workbook.xml.rels".to_owned(),
            "xl/calcChain.xml".to_owned(),
            "xl/sharedStrings.xml".to_owned(),
            "xl/worksheets/sheet1.xml".to_owned(),
        ])
    );
    assert!(!part(&saved, "[Content_Types].xml").contains("calcChain"));
    assert!(!part(&saved, "xl/_rels/workbook.xml.rels").contains("calcChain"));
    let strings = part(&saved, "xl/sharedStrings.xml");
    assert!(strings.contains("<si><t>新しい &amp; &lt;値&gt;</t></si>"));
    assert!(strings.contains("<si><t xml:space=\"preserve\"> 前后空格 </t></si>"));
    // "数量" was already there: no new string for it.
    assert_eq!(strings.matches("数量").count(), 1);
    let again = Xlsx.read(saved).unwrap();
    let sheet = &again.sheets[0];
    let text = |cell: &str| sheet.cell(at(cell)).map(|c| c.value.clone());
    assert_eq!(text("F2"), Some(Value::Text("新しい & <値>".into())));
    assert_eq!(text("A40"), Some(Value::Text(" 前后空格 ".into())));
    assert_eq!(text("B40"), Some(Value::Number(0.1)));
    assert_eq!(text("C1"), Some(Value::Text("数量".into())));
    assert_eq!(text("C3"), None);
    assert_eq!(
        sheet.cell(at("F5")).unwrap().formula.as_deref(),
        Some("D5*2")
    );
    assert_eq!(text("F5"), Some(Value::Number(31932.0)));
    // Everything else as it was.
    let before = Xlsx.read(source).unwrap();
    assert_eq!(
        sheet.cell(at("D4")).unwrap(),
        before.sheets[0].cell(at("D4")).unwrap()
    );
    assert_eq!(again.sheets[1], before.sheets[1]);
}

#[test]
fn changing_the_first_cell_of_a_shared_formula_writes_out_the_others() {
    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    edit(&mut book, &mut changes, vec![("D2", Some(number(3, 7.0)))]);
    let saved = save(source, &book, &changes).unwrap();
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(!sheet.contains("t=\"shared\""));
    let again = Xlsx.read(saved).unwrap();
    let cell = |c: &str| again.sheets[0].cell(at(c)).unwrap().clone();
    assert_eq!(cell("D2").formula, None);
    assert_eq!(cell("D3").formula.as_deref(), Some("B3*C3"));
    assert_eq!(cell("D4").formula.as_deref(), Some("B4*C4"));
    assert_eq!(cell("D4").value, Value::Number(600.0));
}

/// A workbook without shared strings, with prefixed elements, a byte order
/// mark and an empty <sheetData/>.
fn bare() -> Vec<u8> {
    let workbook = r#"<x:workbook xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><x:sheets><x:sheet name="空" sheetId="1" r:id="rId1"/></x:sheets></x:workbook>"#;
    let sheet = "\u{FEFF}<x:worksheet xmlns:x=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><x:sheetData/></x:worksheet>";
    let types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    package(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[("rId1", "worksheet", "worksheets/sheet1.xml")]).as_bytes(),
        ),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

#[test]
fn a_workbook_without_shared_strings_gets_inline_text() {
    let source = bare();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    edit(
        &mut book,
        &mut changes,
        vec![
            ("B2", Some(Cell::new(1, Value::Text("インライン".into())))),
            ("A1", Some(Cell::new(0, Value::Bool(true)))),
            ("C1", Some(Cell::new(2, Value::Error("#N/A".into())))),
        ],
    );
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from(["xl/worksheets/sheet1.xml".to_owned()])
    );
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(sheet.starts_with('\u{FEFF}'));
    assert!(sheet.contains(
        r#"<x:sheetData><x:row r="1"><x:c r="A1" t="b"><x:v>1</x:v></x:c><x:c r="C1" t="e"><x:v>#N/A</x:v></x:c></x:row><x:row r="2"><x:c r="B2" t="inlineStr"><x:is><x:t>インライン</x:t></x:is></x:c></x:row></x:sheetData>"#
    ));
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(
        again.sheets[0].cell(at("B2")).unwrap().value,
        Value::Text("インライン".into())
    );
    assert_eq!(
        again.sheets[0].cell(at("C1")).unwrap().value,
        Value::Error("#N/A".into())
    );
}

#[test]
fn a_cleared_cell_keeps_its_format() {
    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    let mut blank = Cell::new(1, Value::Empty);
    blank.style = 2;
    edit(&mut book, &mut changes, vec![("B3", Some(blank))]);
    let saved = save(source, &book, &changes).unwrap();
    assert!(part(&saved, "xl/worksheets/sheet1.xml").contains(r#"<c r="B3" s="2"/>"#));
}

#[test]
fn a_blank_workbook_opens_takes_entries_and_saves() {
    let source = aster_ooxml::xlsx_save::blank("Sheet1", "等线");
    let mut book = Xlsx.read(source.clone()).unwrap();
    assert_eq!(book.sheets.len(), 1);
    assert_eq!(book.sheets[0].name, "Sheet1");
    assert_eq!(book.sheets[0].cell_count(), 0);
    // The Chinese default font counts columns in 8-pixel digits.
    assert_eq!(book.sheets[0].columns.size(0), 72.0);
    assert!(book.style(1).format.is_date());
    let mut changes = Changes::default();
    let mut date = number(0, 46350.0);
    date.style = 1;
    edit(
        &mut book,
        &mut changes,
        vec![
            ("A1", Some(date)),
            ("B1", Some(Cell::new(1, Value::Text("表".into())))),
        ],
    );
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "xl/sharedStrings.xml".to_owned(),
            "xl/worksheets/sheet1.xml".to_owned()
        ])
    );
    let again = Xlsx.read(saved.clone()).unwrap();
    let a1 = again.sheets[0].cell(at("A1")).unwrap();
    assert_eq!(again.display(a1).unwrap().text, "2026/11/24");
    assert_eq!(
        again.sheets[0].cell(at("B1")).unwrap().value,
        Value::Text("表".into())
    );
    assert!(
        part(&saved, "xl/sharedStrings.xml")
            .contains(r#"count="1" uniqueCount="1"><si><t>表</t></si></sst>"#)
    );
}

#[test]
fn typed_dates_and_percentages_add_the_formats_they_need() {
    use aster_sheets::input::{Reading, read};

    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let reading = Reading {
        order: aster_sheets::DateOrder::Ymd,
        date1904: false,
        this_year: 2026,
    };
    let mut changes = Changes::default();
    for (cell, text) in [
        ("G2", "2026/11/24"),
        ("G3", "50%"),
        ("G4", "2026/12/1"),
        ("G5", "12.5%"),
    ] {
        let (entry, look) = read(text, &reading);
        let made = book.entry_cell(0, at(cell), entry, look);
        edit(&mut book, &mut changes, vec![(cell, made)]);
    }
    // Two formats added (a date without borders, a whole percentage); the
    // second date reuses the first, 12.5% the book's own 0.00%.
    assert_eq!(book.styles.len(), 7);
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "xl/styles.xml".to_owned(),
            "xl/worksheets/sheet1.xml".to_owned()
        ])
    );
    let styles = part(&saved, "xl/styles.xml");
    let before = part(&source, "xl/styles.xml");
    let added = r#"<xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"><alignment vertical="center"/></xf><xf numFmtId="9" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"><alignment vertical="center"/></xf>"#;
    let expected = before
        .replace(r#"<cellXfs count="5">"#, r#"<cellXfs count="7">"#)
        .replace("  </cellXfs>", &format!("  {added}</cellXfs>"));
    assert_eq!(styles, expected);
    let again = Xlsx.read(saved).unwrap();
    let shown = |c: &str| {
        again
            .display(again.sheets[0].cell(at(c)).unwrap())
            .unwrap()
            .text
    };
    assert_eq!(shown("G2"), "2026/11/24");
    assert_eq!(shown("G4"), "2026/12/1");
    assert_eq!(shown("G3"), "50%");
    assert_eq!(shown("G5"), "12.50%");
    // Saved once, the formats are the file's own: saving again adds none.
    assert_eq!(again.styles.len(), 7);
}

#[test]
fn typed_amounts_add_a_currency_format_once() {
    use aster_sheets::input::{Reading, read};

    let reading = Reading {
        order: aster_sheets::DateOrder::Ymd,
        date1904: false,
        this_year: 2026,
    };
    let type_in = |book: &mut Workbook, changes: &mut Changes, cell: &str, text: &str| {
        let (entry, look) = read(text, &reading);
        let made = book.entry_cell(0, at(cell), entry, look);
        edit(book, changes, vec![(cell, made)]);
    };
    // G4 has "¥"#,##0.00 as format 164, with borders.
    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    type_in(&mut book, &mut changes, "G6", "¥1,200");
    type_in(&mut book, &mut changes, "G7", "¥3,000");
    type_in(&mut book, &mut changes, "G8", "¥5.50");
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "xl/styles.xml".to_owned(),
            "xl/worksheets/sheet1.xml".to_owned()
        ])
    );
    let styles = part(&saved, "xl/styles.xml");
    let before = part(&source, "xl/styles.xml");
    // One new number format; the one with decimals is the file's own 164.
    let expected = before
        .replace(r#"<numFmts count="2">"#, r#"<numFmts count="3">"#)
        .replace(
            "\n  </numFmts>",
            "\n  <numFmt numFmtId=\"166\" formatCode=\"&quot;¥&quot;#,##0\"/></numFmts>",
        )
        .replace(r#"<cellXfs count="5">"#, r#"<cellXfs count="7">"#)
        .replace(
            "  </cellXfs>",
            r#"  <xf numFmtId="166" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"><alignment vertical="center"/></xf><xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"><alignment vertical="center"/></xf></cellXfs>"#,
        );
    assert_eq!(styles, expected);
    let again = Xlsx.read(saved).unwrap();
    let shown = |c: &str| {
        again
            .display(again.sheets[0].cell(at(c)).unwrap())
            .unwrap()
            .text
    };
    assert_eq!(shown("G6"), "¥1,200");
    assert_eq!(shown("G7"), "¥3,000");
    assert_eq!(shown("G8"), "¥5.50");

    // A styles part without <numFmts> gets one at its start.
    let source = aster_ooxml::xlsx_save::blank("Sheet1", "Calibri");
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    type_in(&mut book, &mut changes, "A1", "$2.5");
    let saved = save(source, &book, &changes).unwrap();
    let styles = part(&saved, "xl/styles.xml");
    assert!(styles.contains(r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="&quot;$&quot;#,##0.00"/></numFmts><fonts"#));
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(
        again
            .display(again.sheets[0].cell(at("A1")).unwrap())
            .unwrap()
            .text,
        "$2.50"
    );
}

#[test]
fn formatting_and_merging_write_styles_and_merged_areas() {
    use aster_sheets::restyle::{StyleEdit, YELLOW};
    use aster_sheets::{CellRange, HAlign};

    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    let range = |text: &str| CellRange::parse(text).unwrap();
    for (cells, change) in [
        ("A2:A3", StyleEdit::Bold(true)),
        ("A2", StyleEdit::Size(14.0)),
        ("C2", StyleEdit::Fill(Some(YELLOW))),
        ("G9:H9", StyleEdit::Borders(true)),
        ("C3", StyleEdit::Align(Some(HAlign::Center))),
    ] {
        let cells = book.restyle(0, range(cells), &change);
        let cells = cells
            .into_iter()
            .map(|(at, cell)| (at.to_string(), cell))
            .collect::<Vec<_>>();
        edit(
            &mut book,
            &mut changes,
            cells.iter().map(|(a, c)| (a.as_str(), c.clone())).collect(),
        );
    }
    book.sheets[0].merges = vec![range("F9:G10")];
    book.sheets[0].refresh();
    changes.merges.insert(0);
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "xl/styles.xml".to_owned(),
            "xl/worksheets/sheet1.xml".to_owned()
        ])
    );
    let styles = part(&saved, "xl/styles.xml");
    // A bold copy of font 0, then a 14-point one; a yellow fill; thin borders.
    assert!(styles.contains(r#"<fonts count="5" x14ac:knownFonts="1">"#));
    assert!(styles.contains(r#"<font><b/><sz val="11"/><color theme="1"/><name val="等线"/><family val="2"/><charset val="134"/><scheme val="minor"/></font><font><b/><sz val="14"/><color theme="1"/>"#));
    assert!(styles.contains(r#"<fills count="4">"#));
    assert!(styles.contains(r#"<fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/><bgColor indexed="64"/></patternFill></fill></fills>"#));
    assert!(styles.contains(r#"<borders count="3">"#));
    assert!(
        styles.contains(
            r#"applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf>"#
        )
    );
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(r#"<mergeCells count="1"><mergeCell ref="F9:G10"/></mergeCells>"#));

    let again = Xlsx.read(saved).unwrap();
    let style = |c: &str| again.style(again.sheets[0].style_at(at(c)));
    assert!(style("A2").font.bold && style("A3").font.bold);
    assert_eq!(style("A2").font.size, 14.0);
    assert_eq!(style("A3").font.size, 11.0);
    assert_eq!(style("C2").fill, Some(YELLOW));
    assert!(style("G9").borders.left.is_some() && style("H9").borders.right.is_some());
    assert_eq!(style("C3").align, Some(HAlign::Center));
    // Values stay.
    assert_eq!(
        again.sheets[0].cell(at("A2")).unwrap().value,
        Xlsx.read(g4()).unwrap().sheets[0]
            .cell(at("A2"))
            .unwrap()
            .value
    );
    assert_eq!(again.sheets[0].merges, [range("F9:G10")]);
}

#[test]
fn resized_columns_and_rows_are_written_where_they_belong() {
    let source = g4();
    let mut book = Xlsx.read(source.clone()).unwrap();
    // 等线 counts 8 pixels a digit: 100 pixels is 12.5.
    book.sheets[0].columns.set(2, 3, 100.0);
    book.sheets[0].row_sizes.set(2, 3, 30.0);
    book.sheets[0].row_sizes.set(30, 31, 40.0);
    book.sheets[0].columns.set(9, 10, 80.0);
    let mut changes = Changes::default();
    changes.columns.extend([(0, 2), (0, 9)]);
    changes.rows.extend([(0, 2), (0, 30)]);
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from(["xl/worksheets/sheet1.xml".to_owned()])
    );
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    // The <col> for B:D is split around C, each part keeping its style.
    assert!(sheet.contains(r#"<cols><col min="1" max="1" width="14.625" customWidth="1"/><col min="2" max="2" width="12.625" style="2" customWidth="1"/><col min="3" max="3" style="2" width="12.5" customWidth="1"/><col min="4" max="4" width="12.625" style="2" customWidth="1"/><col min="5" max="5" width="16.625" style="3" customWidth="1"/><col min="10" max="10" width="10" customWidth="1"/></cols>"#), "{sheet}");
    assert!(
        sheet.contains(
            r#"<row r="3" spans="1:5" x14ac:dyDescent="0.2" ht="22.5" customHeight="1">"#
        ),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<row r="31" ht="30" customHeight="1"/>"#));
    let again = Xlsx.read(saved).unwrap();
    let s = &again.sheets[0];
    assert_eq!(s.columns.size(2), 100.0);
    assert_eq!(s.columns.size(1), book.sheets[0].columns.size(1));
    assert_eq!(s.columns.size(9), 80.0);
    assert_eq!(s.row_sizes.size(2), 30.0);
    assert_eq!(s.row_sizes.size(30), 40.0);
    assert_eq!(s.cell(at("C3")), book.sheets[0].cell(at("C3")));
}

/// Two sheets: 数据 with formulas, a merged area, a conditional format,
/// validations and a page break; 汇总 with formulas on 数据; names for a
/// range and 数据's print area.
fn linked() -> Vec<u8> {
    const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let workbook = format!(
        r#"<workbook xmlns="{MAIN}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="数据" sheetId="1" r:id="rId1"/><sheet name="汇总" sheetId="2" r:id="rId2"/></sheets><definedNames><definedName name="区域">数据!$A$2:$A$4</definedName><definedName name="_xlnm.Print_Area" localSheetId="0">数据!$A$1:$C$5</definedName></definedNames></workbook>"#
    );
    let data = format!(
        r#"<worksheet xmlns="{MAIN}"><dimension ref="A1:C5"/><sheetViews><sheetView workbookViewId="0"><selection activeCell="B4" sqref="B4"/></sheetView></sheetViews><cols><col min="2" max="3" width="20" customWidth="1"/></cols><sheetData><row r="1" spans="1:3"><c r="A1"><v>1</v></c><c r="B1"><v>10</v></c></row><row r="2" ht="30" customHeight="1"><c r="A2"><v>2</v></c><c r="C2"><v>20</v></c></row><row r="3"><c r="A3"><v>3</v></c></row><row r="4"><c r="A4"><f>SUM(A1:A3)</f><v>6</v></c><c r="B4" t="str"><f>A3&amp;"x"</f><v>3x</v></c></row><row r="5"><c r="A5"><v>5</v></c></row></sheetData><mergeCells count="1"><mergeCell ref="B3:C3"/></mergeCells><conditionalFormatting sqref="A1:A5"><cfRule type="expression" dxfId="0" priority="1"><formula>A1&gt;$A$4</formula></cfRule></conditionalFormatting><dataValidations count="2"><dataValidation type="whole" sqref="C1:C5"><formula1>0</formula1><formula2>$A$5</formula2></dataValidation><dataValidation type="list" sqref="A2"><formula1>$A$1:$A$3</formula1></dataValidation></dataValidations><rowBreaks count="1" manualBreakCount="1"><brk id="3" max="16383" man="1"/></rowBreaks></worksheet>"#
    );
    let summary = format!(
        r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1"><f>数据!A4*2</f><v>12</v></c><c r="B1"><f>SUM(数据!A1:A5)</f><v>11</v></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="expression" dxfId="0" priority="1"><formula>数据!$A$4&gt;0</formula></cfRule></conditionalFormatting></worksheet>"#
    );
    let types = format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
    );
    package(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[
                ("rId1", "worksheet", "worksheets/sheet1.xml"),
                ("rId2", "worksheet", "worksheets/sheet2.xml"),
            ])
            .as_bytes(),
        ),
        ("xl/worksheets/sheet1.xml", data.as_bytes()),
        ("xl/worksheets/sheet2.xml", summary.as_bytes()),
        ("docProps/app.xml", APP.as_bytes()),
    ])
}

/// Document properties as Excel writes them: the worksheets, then the
/// named ranges.
const APP: &str = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><HeadingPairs><vt:vector size="4" baseType="variant"><vt:variant><vt:lpstr>工作表</vt:lpstr></vt:variant><vt:variant><vt:i4>2</vt:i4></vt:variant><vt:variant><vt:lpstr>命名范围</vt:lpstr></vt:variant><vt:variant><vt:i4>1</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="3" baseType="lpstr"><vt:lpstr>数据</vt:lpstr><vt:lpstr>汇总</vt:lpstr><vt:lpstr>区域</vt:lpstr></vt:vector></TitlesOfParts></Properties>"#;

/// Move lines in the model as the shell does, and say so to the saver.
fn restructure(book: &mut Workbook, changes: &mut Changes, lines: aster_sheets::structure::Lines) {
    for cell in book.restructure(0, &lines) {
        changes.cells.insert(cell);
        changes.edited.insert(cell);
    }
    changes.formulas_changed = true;
    changes.merges.insert(0);
    changes.structure.push((0, lines));
}

/// Every cell's value and formula, by sheet.
fn contents(book: &Workbook) -> Vec<Vec<(CellRef, Value, Option<String>)>> {
    book.sheets
        .iter()
        .map(|sheet| {
            sheet
                .rows()
                .iter()
                .flat_map(|row| {
                    row.cells.iter().map(move |c| {
                        (
                            CellRef::new(row.index, c.col),
                            c.value.clone(),
                            c.formula.as_deref().map(str::to_owned),
                        )
                    })
                })
                .collect()
        })
        .collect()
}

#[test]
fn deleting_a_row_moves_cells_ranges_names_and_other_sheets_formulas() {
    use aster_sheets::structure::Lines;
    let source = linked();
    let mut book = Xlsx.read(source.clone()).unwrap();
    assert_eq!(book.sheets[0].structure_blocker, None);
    // The name for formulas is read; Excel's print area is not one.
    assert_eq!(book.names.len(), 1);
    assert_eq!(
        (book.names[0].name.as_str(), book.names[0].formula.as_str()),
        ("区域", "数据!$A$2:$A$4")
    );
    let mut changes = Changes::default();
    let lines = Lines {
        columns: false,
        at: 1,
        count: 1,
        insert: false,
    };
    restructure(&mut book, &mut changes, lines);
    let saved = save(source.clone(), &book, &changes).unwrap();
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    // Row 2 went; the rows after it keep their bytes under new numbers.
    assert!(
        sheet.contains(r#"<row r="1" spans="1:3"><c r="A1"><v>1</v></c>"#),
        "{sheet}"
    );
    assert!(!sheet.contains("<v>20</v>"), "{sheet}");
    assert!(
        sheet.contains(r#"<row r="2"><c r="A2"><v>3</v></c></row>"#),
        "{sheet}"
    );
    assert!(sheet.contains("<f>SUM(A1:A2)</f>"), "{sheet}");
    assert!(sheet.contains("<f>A2&amp;&quot;x&quot;</f>"), "{sheet}");
    assert!(
        sheet.contains(r#"<row r="4"><c r="A4"><v>5</v></c></row>"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<dimension ref="A1:C4"/>"#), "{sheet}");
    assert!(
        sheet.contains(r#"<selection activeCell="B3" sqref="B3"/>"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<mergeCell ref="B2:C2"/>"#), "{sheet}");
    assert!(
        sheet.contains(r#"<conditionalFormatting sqref="A1:A4">"#),
        "{sheet}"
    );
    assert!(sheet.contains("<formula>A1&gt;$A$3</formula>"), "{sheet}");
    // The validation of A2 went with it.
    assert!(sheet.contains(r#"<dataValidations count="1"><dataValidation type="whole" sqref="C1:C4"><formula1>0</formula1><formula2>$A$4</formula2></dataValidation></dataValidations>"#), "{sheet}");
    assert!(
        sheet.contains(r#"<brk id="2" max="16383" man="1"/>"#),
        "{sheet}"
    );
    let other = part(&saved, "xl/worksheets/sheet2.xml");
    assert!(other.contains("<f>数据!A3*2</f>"), "{other}");
    assert!(other.contains("<f>SUM(数据!A1:A4)</f>"), "{other}");
    // So does the other sheet's conditional format.
    assert!(
        other.contains("<formula>数据!$A$3&gt;0</formula>"),
        "{other}"
    );
    let names = part(&saved, "xl/workbook.xml");
    assert!(
        names.contains(r#"<definedName name="区域">数据!$A$2:$A$3</definedName>"#),
        "{names}"
    );
    assert!(
        names.contains(r#"localSheetId="0">数据!$A$1:$C$4</definedName>"#),
        "{names}"
    );
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(contents(&again), contents(&book));
    assert_eq!(again.sheets[0].merges, book.sheets[0].merges);
    // The model's names moved as the file's did.
    assert_eq!(again.names, book.names);
}

#[test]
fn inserting_columns_and_taking_them_out_again() {
    use aster_sheets::structure::Lines;
    let source = linked();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    let lines = Lines {
        columns: true,
        at: 1,
        count: 2,
        insert: true,
    };
    restructure(&mut book, &mut changes, lines);
    let saved = save(source.clone(), &book, &changes).unwrap();
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    // The widths move with their columns; spans would be wrong and go.
    assert!(
        sheet.contains(r#"<cols><col min="4" max="5" width="20" customWidth="1"/></cols>"#),
        "{sheet}"
    );
    assert!(
        sheet.contains(r#"<row r="1"><c r="A1"><v>1</v></c><c r="D1"><v>10</v></c></row>"#),
        "{sheet}"
    );
    assert!(sheet.contains(r#"<mergeCell ref="D3:E3"/>"#), "{sheet}");
    assert!(sheet.contains(r#"sqref="E1:E5""#), "{sheet}");
    let again = Xlsx.read(saved.clone()).unwrap();
    assert_eq!(contents(&again), contents(&book));
    assert_eq!(
        again.sheets[0].columns.size(3),
        book.sheets[0].columns.size(3)
    );
    assert_eq!(
        again.sheets[0].columns.size(1),
        book.sheets[0].columns.size(1)
    );

    // Deleting them again, as undo does, gives the cells back where they were.
    let first = Xlsx.read(source.clone()).unwrap();
    let back = Lines {
        insert: false,
        ..lines
    };
    restructure(&mut book, &mut changes, back);
    let saved = save(source, &book, &changes).unwrap();
    let again = Xlsx.read(saved.clone()).unwrap();
    assert_eq!(contents(&again), contents(&first));
    let sheet = part(&saved, "xl/worksheets/sheet1.xml");
    assert!(
        sheet.contains(r#"<cols><col min="2" max="3" width="20" customWidth="1"/></cols>"#),
        "{sheet}"
    );
}

#[test]
fn what_would_not_follow_the_lines_is_named() {
    // G4's sheet has a table and comments.
    let book = Xlsx.read(g4()).unwrap();
    assert!(book.sheets[0].structure_blocker.is_some());
}

#[test]
fn renaming_a_sheet_renames_it_everywhere() {
    let source = linked();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    for cell in book.rename_sheet(0, "Q1 数据") {
        changes.cells.insert(cell);
        changes.edited.insert(cell);
    }
    changes.formulas_changed = true;
    let saved = save(source.clone(), &book, &changes).unwrap();
    assert_eq!(
        differences(&source, &saved),
        BTreeSet::from([
            "docProps/app.xml".to_owned(),
            "xl/workbook.xml".to_owned(),
            "xl/worksheets/sheet2.xml".to_owned()
        ])
    );
    let app = part(&saved, "docProps/app.xml");
    assert!(
        app.contains(
            "<vt:lpstr>Q1 数据</vt:lpstr><vt:lpstr>汇总</vt:lpstr><vt:lpstr>区域</vt:lpstr>"
        ),
        "{app}"
    );
    let names = part(&saved, "xl/workbook.xml");
    assert!(
        names.contains(r#"<sheet name="Q1 数据" sheetId="1" r:id="rId1"/>"#),
        "{names}"
    );
    assert!(
        names.contains(r#"<definedName name="区域">'Q1 数据'!$A$2:$A$4</definedName>"#),
        "{names}"
    );
    let other = part(&saved, "xl/worksheets/sheet2.xml");
    assert!(other.contains("<f>'Q1 数据'!A4*2</f>"), "{other}");
    assert!(
        other.contains("<formula>'Q1 数据'!$A$4&gt;0</formula>"),
        "{other}"
    );
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(again.sheets[0].name, "Q1 数据");
    assert_eq!(contents(&again), contents(&book));
}

#[test]
fn added_and_deleted_sheets_get_and_lose_their_parts() {
    let source = linked();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let mut changes = Changes::default();
    // 数据 goes; a new sheet comes after 汇总 with a number and a formula.
    for cell in book.delete_sheet(0) {
        changes.cells.insert(cell);
        changes.edited.insert(cell);
    }
    let added = book.add_sheet("Sheet3");
    book.set_cells(
        added,
        vec![
            (at("A1"), Some(number(0, 4.0))),
            (
                at("B2"),
                Some(Cell {
                    formula: Some("汇总!A1+A1".into()),
                    ..Cell::new(1, Value::Text("新".into()))
                }),
            ),
        ],
    );
    changes.formulas_changed = true;
    changes.origins = vec![Some(1), None];
    let saved = save(source.clone(), &book, &changes).unwrap();
    let names = part(&saved, "xl/workbook.xml");
    assert!(
        names.contains(r#"<sheets><sheet name="汇总" sheetId="2" r:id="rId2"/><sheet name="Sheet3" sheetId="3" r:id="rId3"/></sheets>"#),
        "{names}"
    );
    // The name on 数据 went; the other lost its sheet.
    assert!(
        names.contains(
            r#"<definedNames><definedName name="区域">#REF!</definedName></definedNames>"#
        ),
        "{names}"
    );
    let rels = part(&saved, "xl/_rels/workbook.xml.rels");
    assert!(!rels.contains("sheet1.xml"), "{rels}");
    assert!(
        rels.contains(r#"Id="rId3""#) && rels.contains(r#"Target="worksheets/sheet3.xml""#),
        "{rels}"
    );
    let types = part(&saved, "[Content_Types].xml");
    assert!(
        types.contains(r#"PartName="/xl/worksheets/sheet3.xml""#),
        "{types}"
    );
    let mut package = Package::open(saved.clone()).unwrap();
    assert!(!package.has("xl/worksheets/sheet1.xml"));
    assert!(package.has("xl/worksheets/sheet3.xml"));
    let new_sheet = String::from_utf8(package.read("xl/worksheets/sheet3.xml").unwrap()).unwrap();
    assert!(new_sheet.starts_with("<?xml"), "{new_sheet}");
    let other = part(&saved, "xl/worksheets/sheet2.xml");
    assert!(other.contains("<f>#REF!*2</f>"), "{other}");
    assert!(other.contains("<formula>#REF!&gt;0</formula>"), "{other}");
    // The document properties list the worksheets there are now.
    let app = part(&saved, "docProps/app.xml");
    assert!(app.contains("<vt:i4>2</vt:i4><"), "{app}");
    assert!(app.contains(r#"<vt:vector size="3" baseType="lpstr"><vt:lpstr>汇总</vt:lpstr><vt:lpstr>Sheet3</vt:lpstr><vt:lpstr>区域</vt:lpstr>"#), "{app}");
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(
        again
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["汇总", "Sheet3"]
    );
    assert_eq!(contents(&again), contents(&book));
}

#[test]
fn a_new_sheet_with_a_resized_column_saves() {
    let source = linked();
    let mut book = Xlsx.read(source.clone()).unwrap();
    let added = book.add_sheet("新");
    book.set_cells(added, vec![(at("A1"), Some(number(0, 1.0)))]);
    book.sheets[added].columns.set(0, 1, 150.0);
    let mut changes = Changes {
        origins: vec![Some(0), Some(1), None],
        ..Changes::default()
    };
    changes.columns.insert((added, 0));
    let saved = save(source, &book, &changes).unwrap();
    let sheet = part(&saved, "xl/worksheets/sheet3.xml");
    assert!(sheet.contains(r#"<cols><col min="1" max="1""#), "{sheet}");
    assert!(
        sheet.contains(r#"</cols><sheetData><row r="1">"#),
        "{sheet}"
    );
    let again = Xlsx.read(saved).unwrap();
    assert_eq!(
        again.sheets[2].cell(at("A1")).map(|c| c.value.clone()),
        Some(Value::Number(1.0))
    );
}
