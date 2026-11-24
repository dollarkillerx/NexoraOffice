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
