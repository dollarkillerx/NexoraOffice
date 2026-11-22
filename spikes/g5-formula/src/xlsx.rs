//! Write a workbook as a minimal XLSX file, so that every engine loads the same
//! bytes through its own importer.
//!
//! Files saved by Excel carry the last result of every formula. Ours carry a
//! result too, because an importer may complain about each cell that has none,
//! but a deliberately wrong one: 0 for numbers and `?` for text. No expected
//! value is either, so an engine that reports a right value has calculated it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::workload::{Book, Content, a1};

const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PACKAGE: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn write(book: &Book) -> Vec<u8> {
    let mut strings: Vec<&str> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut uses = 0_usize;
    let mut sheets = Vec::new();
    for sheet in &book.sheets {
        let mut xml = String::with_capacity(sheet.cells.len() * 40);
        let last_row = sheet.cells.last().map_or(1, |cell| cell.row);
        let last_col = sheet.cells.iter().map(|cell| cell.col).max().unwrap_or(0);
        write!(
            xml,
            "{DECLARATION}<worksheet xmlns=\"{MAIN}\"><dimension ref=\"A1:{}\"/><sheetData>",
            a1(last_row, last_col)
        )
        .unwrap();
        let mut open_row = 0;
        for cell in &sheet.cells {
            if cell.row != open_row {
                if open_row != 0 {
                    xml.push_str("</row>");
                }
                write!(xml, "<row r=\"{}\">", cell.row).unwrap();
                open_row = cell.row;
            }
            let at = a1(cell.row, cell.col);
            match &cell.content {
                Content::Number(value) => write!(xml, "<c r=\"{at}\"><v>{value}</v></c>"),
                Content::Formula(formula) => {
                    write!(xml, "<c r=\"{at}\"><f>{}</f><v>0</v></c>", escape(formula))
                }
                Content::TextFormula(formula) => write!(
                    xml,
                    "<c r=\"{at}\" t=\"str\"><f>{}</f><v>?</v></c>",
                    escape(formula)
                ),
                Content::Text(text) => {
                    let next = strings.len();
                    let id = *index.entry(text.as_str()).or_insert_with(|| {
                        strings.push(text.as_str());
                        next
                    });
                    uses += 1;
                    write!(xml, "<c r=\"{at}\" t=\"s\"><v>{id}</v></c>")
                }
            }
            .unwrap();
        }
        if open_row != 0 {
            xml.push_str("</row>");
        }
        xml.push_str("</sheetData></worksheet>");
        sheets.push(xml);
    }

    let mut shared = format!(
        "{DECLARATION}<sst xmlns=\"{MAIN}\" count=\"{uses}\" uniqueCount=\"{}\">",
        strings.len()
    );
    for text in &strings {
        write!(shared, "<si><t>{}</t></si>", escape(text)).unwrap();
    }
    shared.push_str("</sst>");

    let mut types = format!(
        "{DECLARATION}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/xl/workbook.xml\" ContentType=\"{TYPE}.sheet.main+xml\"/>\
         <Override PartName=\"/xl/styles.xml\" ContentType=\"{TYPE}.styles+xml\"/>\
         <Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"{TYPE}.sharedStrings+xml\"/>"
    );
    let mut workbook =
        format!("{DECLARATION}<workbook xmlns=\"{MAIN}\" xmlns:r=\"{RELATIONSHIPS}\"><sheets>");
    let mut links = format!("{DECLARATION}<Relationships xmlns=\"{PACKAGE}\">");
    for (number, sheet) in book.sheets.iter().enumerate() {
        let number = number + 1;
        write!(
            types,
            "<Override PartName=\"/xl/worksheets/sheet{number}.xml\" ContentType=\"{TYPE}.worksheet+xml\"/>"
        )
        .unwrap();
        write!(
            workbook,
            "<sheet name=\"{}\" sheetId=\"{number}\" r:id=\"rId{number}\"/>",
            escape(sheet.name)
        )
        .unwrap();
        write!(
            links,
            "<Relationship Id=\"rId{number}\" Type=\"{RELATIONSHIPS}/worksheet\" Target=\"worksheets/sheet{number}.xml\"/>"
        )
        .unwrap();
    }
    types.push_str("</Types>");
    workbook.push_str("</sheets><calcPr calcId=\"191029\" fullCalcOnLoad=\"1\"/></workbook>");
    let after = book.sheets.len() + 1;
    write!(
        links,
        "<Relationship Id=\"rId{after}\" Type=\"{RELATIONSHIPS}/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rId{}\" Type=\"{RELATIONSHIPS}/sharedStrings\" Target=\"sharedStrings.xml\"/>\
         </Relationships>",
        after + 1
    )
    .unwrap();
    let root = format!(
        "{DECLARATION}<Relationships xmlns=\"{PACKAGE}\">\
         <Relationship Id=\"rId1\" Type=\"{RELATIONSHIPS}/officeDocument\" Target=\"xl/workbook.xml\"/>\
         </Relationships>"
    );
    let styles = format!(
        "{DECLARATION}<styleSheet xmlns=\"{MAIN}\">\
         <fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
         <fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill>\
         <fill><patternFill patternType=\"gray125\"/></fill></fills>\
         <borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
         <cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
         <cellXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/></cellXfs>\
         <cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>\
         </styleSheet>"
    );

    let mut parts: Vec<(String, String)> = vec![
        ("[Content_Types].xml".into(), types),
        ("_rels/.rels".into(), root),
        ("xl/workbook.xml".into(), workbook),
        ("xl/_rels/workbook.xml.rels".into(), links),
        ("xl/styles.xml".into(), styles),
        ("xl/sharedStrings.xml".into(), shared),
    ];
    for (number, xml) in sheets.into_iter().enumerate() {
        parts.push((format!("xl/worksheets/sheet{}.xml", number + 1), xml));
    }
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in parts {
        zip.start_file(name, options).expect("writing to memory");
        zip.write_all(data.as_bytes()).expect("writing to memory");
    }
    zip.finish().expect("writing to memory").into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{Cell, Sheet};
    use std::io::Read;

    #[test]
    fn a_small_workbook_is_written_as_expected() {
        let book = Book {
            sheets: vec![Sheet {
                name: "S&P",
                cells: vec![
                    Cell {
                        row: 1,
                        col: 0,
                        content: Content::Text("大".into()),
                    },
                    Cell {
                        row: 1,
                        col: 1,
                        content: Content::Number(0.125),
                    },
                    Cell {
                        row: 2,
                        col: 0,
                        content: Content::TextFormula("IF(B1<1,\"大\",\"x\")".into()),
                    },
                    Cell {
                        row: 2,
                        col: 1,
                        content: Content::Formula("B1*2".into()),
                    },
                    Cell {
                        row: 2,
                        col: 2,
                        content: Content::Text("大".into()),
                    },
                ],
            }],
        };
        let bytes = write(&book);
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut read = |name: &str| {
            let mut text = String::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            text
        };
        let sheet = read("xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<dimension ref=\"A1:C2\"/>"), "{sheet}");
        assert!(
            sheet.contains(
                "<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>0.125</v></c></row>\
                 <row r=\"2\"><c r=\"A2\" t=\"str\"><f>IF(B1&lt;1,\"大\",\"x\")</f><v>?</v></c>\
                 <c r=\"B2\"><f>B1*2</f><v>0</v></c><c r=\"C2\" t=\"s\"><v>0</v></c></row>"
            ),
            "{sheet}"
        );
        assert!(
            read("xl/sharedStrings.xml")
                .contains("count=\"2\" uniqueCount=\"1\"><si><t>大</t></si>")
        );
        assert!(
            read("xl/workbook.xml")
                .contains("<sheet name=\"S&amp;P\" sheetId=\"1\" r:id=\"rId1\"/>")
        );
    }
}
