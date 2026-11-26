//! Saving edited tables: what the file had and the model has no field for
//! (`tcPr`, `trPr` and `tblPr` children) survives merging, splitting,
//! resizing and adding or removing columns, and the grid, the cell widths
//! and the table width agree in what is written.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use aster_ooxml::docx_save::save;
use aster_ooxml::{Docx, FormatCodec};
use aster_writer::edit::*;
use aster_writer::{Block, Document, Path, Pos, RowHeight, VMerge};

fn parts(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        out.insert(entry.name().to_owned(), data);
    }
    out
}

fn main_xml(bytes: &[u8]) -> String {
    String::from_utf8(parts(bytes)["word/document.xml"].clone()).unwrap()
}

fn read(bytes: &[u8]) -> Document {
    Docx.read(bytes.to_vec()).unwrap()
}

const STYLES: &str = r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblCellMar><w:left w:w="108" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>"#;

fn tc(width: u32, props: &str, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{props}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

/// A table of three rows over three columns of 2000 twips. Cells and rows
/// carry properties the model has no field for.
fn crafted_body() -> String {
    let mut t = String::from(
        r#"<w:tbl><w:tblPr><w:tblW w:w="6000" w:type="dxa"/><w:tblInd w:w="100" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/><w:tblCaption w:val="Kept"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>"#,
    );
    t += r#"<w:tr w:rsidR="00A10001"><w:trPr><w:cnfStyle w:val="100000000000"/><w:trHeight w:val="400" w:hRule="atLeast"/><w:jc w:val="center"/></w:trPr>"#;
    t += &tc(
        2000,
        r#"<w:cnfStyle w:val="001000000000"/><w:noWrap/>"#,
        "r0c0",
    );
    t += &tc(2000, r#"<w:tcFitText/>"#, "r0c1");
    t += &tc(2000, r#"<w:hideMark/>"#, "r0c2");
    t += "</w:tr>";
    t += r#"<w:tr w:rsidR="00A10002">"#;
    t += &tc(2000, r#"<w:noWrap/>"#, "r1c0");
    t += &tc(2000, r#"<w:textDirection w:val="btLr"/>"#, "r1c1");
    t += &tc(
        2000,
        r#"<w:shd w:val="clear" w:color="auto" w:fill="FFFF00"/><w:noWrap/>"#,
        "r1c2",
    );
    t += "</w:tr>";
    t += r#"<w:tr w:rsidR="00A10003">"#;
    t += &tc(2000, "", "r2c0");
    t += &tc(2000, r#"<w:noWrap/>"#, "r2c1");
    t += &tc(2000, "", "r2c2");
    t += "</w:tr></w:tbl>";
    format!("<w:p><w:r><w:t>before</w:t></w:r></w:p>{t}<w:p><w:r><w:t>after</w:t></w:r></w:p>")
}

fn crafted() -> Vec<u8> {
    docx_doc::doc(&crafted_body()).styles(STYLES).bytes()
}

fn cell_pos(row: usize, cell: usize) -> Pos {
    Pos::new(Path::body(1).cell_block(row, cell, 0), 0)
}

fn table(doc: &Document) -> &aster_writer::Table {
    doc.body.blocks[1].as_table().unwrap()
}

/// What the saved file must satisfy whatever was done: it reads back with
/// the content of the edited model, and in every table the grid, the cell
/// widths and the table width agree.
fn saved(source: &[u8], doc: &Document) -> (Vec<u8>, String) {
    let bytes = save(source.to_vec(), doc).expect("the document saves");
    let back = read(&bytes);
    assert!(same_content(doc, &back), "{}", main_xml(&bytes));
    if let Block::Table(t) = &back.body.blocks[1] {
        check_table(t).unwrap();
    }
    let xml = main_xml(&bytes);
    assert_widths_agree(&xml);
    (bytes, xml)
}

/// In the first table of `xml`: every `tcW` is the sum of the grid columns
/// the cell covers, and a `tblW` in twips is the sum of the grid.
fn assert_widths_agree(xml: &str) {
    let start = xml.find("<w:tbl>").unwrap();
    let end = xml[start..].find("</w:tbl>").unwrap() + start;
    let tbl = &xml[start..end];
    let grid: Vec<i32> = tbl
        .split("<w:gridCol w:w=\"")
        .skip(1)
        .map(|s| s[..s.find('"').unwrap()].parse().unwrap())
        .collect();
    if let Some(i) = tbl.find("<w:tblW w:w=\"") {
        let rest = &tbl[i + 13..];
        let w: i32 = rest[..rest.find('"').unwrap()].parse().unwrap();
        assert_eq!(w, grid.iter().sum::<i32>(), "tblW against the grid");
    }
    for row in tbl.split("<w:tr").skip(1) {
        let mut at = 0usize;
        let before = row
            .find("<w:gridBefore w:val=\"")
            .map(|i| {
                let r = &row[i + 21..];
                r[..r.find('"').unwrap()].parse::<usize>().unwrap()
            })
            .unwrap_or(0);
        at += before;
        for cell in row.split("<w:tc>").skip(1) {
            let w: i32 = {
                let i = cell.find("<w:tcW w:w=\"").unwrap() + 12;
                cell[i..i + cell[i..].find('"').unwrap()].parse().unwrap()
            };
            let span: usize = cell
                .find("<w:gridSpan w:val=\"")
                .map(|i| {
                    let r = &cell[i + 19..];
                    r[..r.find('"').unwrap()].parse().unwrap()
                })
                .unwrap_or(1);
            let expect: i32 = grid.iter().skip(at).take(span).sum();
            assert_eq!(w, expect, "a cell at grid column {at} over {span}");
            at += span;
        }
    }
}

fn count(xml: &str, needle: &str) -> usize {
    xml.matches(needle).count()
}

// -- adding a column -------------------------------------------------------------

#[test]
fn a_column_inserted_keeps_what_the_cells_and_rows_had() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    insert_columns(&mut doc, &cell_pos(1, 1), &cell_pos(1, 1), false);
    let (_, xml) = saved(&bytes, &doc);
    for kept in [
        "<w:cnfStyle w:val=\"001000000000\"/>",
        "<w:cnfStyle w:val=\"100000000000\"/>",
        "<w:tcFitText/>",
        "<w:hideMark/>",
        "<w:textDirection w:val=\"btLr\"/>",
        "<w:jc w:val=\"center\"/>",
        "<w:tblLook w:val=\"04A0\"",
        "<w:tblCaption w:val=\"Kept\"/>",
        "<w:tblLayout w:type=\"fixed\"/>",
        "<w:tblInd w:w=\"100\"",
        "<w:tr w:rsidR=\"00A10002\">",
    ] {
        assert!(xml.contains(kept), "{kept} is gone\n{xml}");
    }
    assert_eq!(count(&xml, "<w:noWrap/>"), 4);
    // Four grid columns of the new table, 8000 wide.
    assert_eq!(count(&xml, "<w:gridCol "), 4);
    assert!(xml.contains("<w:tblW w:w=\"8000\" w:type=\"dxa\"/>"));
    // The row height stays and the shading kept its place before noWrap.
    assert!(xml.contains("<w:trHeight w:val=\"400\" w:hRule=\"atLeast\"/>"));
    assert!(xml.contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFFF00\"/><w:noWrap/>"));
}

#[test]
fn the_content_of_cells_not_changed_is_written_from_its_bytes() {
    let bytes = crafted();
    let doc0 = read(&bytes);
    let t = table(&doc0);
    let original = main_xml(&bytes);
    let mut doc = doc0.clone();
    insert_columns(&mut doc, &cell_pos(0, 0), &cell_pos(0, 0), true);
    let (_, xml) = saved(&bytes, &doc);
    // Every original paragraph of the table is there as it was.
    for row in &t.rows {
        for cell in &row.cells {
            for block in &cell.blocks {
                let range = &block.as_paragraph().unwrap().source.as_ref().unwrap().range;
                assert!(xml.contains(&original[range.clone()]));
            }
        }
    }
    // Text outside the table is the file's.
    assert!(xml.starts_with(&original[..original.find("<w:tbl>").unwrap()]));
}

#[test]
fn a_column_removed_keeps_the_properties_of_the_cells_left() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    delete_columns(&mut doc, &cell_pos(0, 1), &cell_pos(0, 1));
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<w:hideMark/>"));
    assert!(xml.contains("<w:cnfStyle w:val=\"001000000000\"/>"));
    assert!(!xml.contains("<w:tcFitText/>"));
    assert_eq!(count(&xml, "<w:gridCol "), 2);
    assert!(xml.contains("<w:tblW w:w=\"4000\" w:type=\"dxa\"/>"));
}

#[test]
fn a_row_added_keeps_the_rows_and_cells_it_did_not_touch() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    insert_rows(&mut doc, &cell_pos(2, 0), &cell_pos(2, 0), false);
    let (_, xml) = saved(&bytes, &doc);
    assert_eq!(count(&xml, "<w:tr "), 3);
    assert_eq!(count(&xml, "<w:tr>"), 1);
    assert!(xml.contains("<w:tcFitText/>") && xml.contains("<w:hideMark/>"));
}

// -- merging and splitting ---------------------------------------------------------

#[test]
fn merged_cells_keep_the_properties_of_the_first_and_the_grid_stays_right() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    merge_cells(&mut doc, &cell_pos(0, 0), &cell_pos(1, 1));
    let (_, xml) = saved(&bytes, &doc);
    // The merged cell: two grid columns, restarting a vertical merge, with
    // the first cell's properties.
    assert!(xml.contains("<w:gridSpan w:val=\"2\"/><w:vMerge w:val=\"restart\"/>"));
    assert!(xml.contains("<w:cnfStyle w:val=\"001000000000\"/>"));
    assert!(xml.contains("<w:vMerge/>"));
    // Properties of cells that were not merged away stay.
    assert!(xml.contains("<w:hideMark/>"));
    // The text of every merged cell is in the first.
    let back = read(&save(bytes.clone(), &doc).unwrap());
    let first = &table(&back).rows[0].cells[0];
    let text: Vec<String> = aster_writer::paragraphs_of(&first.blocks)
        .iter()
        .map(|(_, p)| p.text())
        .collect();
    assert_eq!(text, ["r0c0", "r0c1", "r1c0", "r1c1"]);
    assert_eq!(first.merge, VMerge::Start);
    // The paragraphs moved are written from their own bytes.
    let original = main_xml(&bytes);
    let doc0 = read(&bytes);
    for (_, p) in aster_writer::paragraphs_of(&table(&doc0).rows[0].cells[1].blocks) {
        let range = p.source.as_ref().unwrap().range.clone();
        assert!(xml.contains(&original[range]));
    }
}

#[test]
fn a_split_cell_grows_the_cells_over_it_and_keeps_their_properties() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    split_cell(&mut doc, &cell_pos(1, 1), 2, 1);
    let (_, xml) = saved(&bytes, &doc);
    assert_eq!(count(&xml, "<w:gridCol "), 4);
    // The cells over the cut column span two grid columns now.
    assert_eq!(count(&xml, "<w:gridSpan w:val=\"2\"/>"), 2);
    assert!(xml.contains("<w:tcFitText/>") && xml.contains("<w:hideMark/>"));
    assert!(xml.contains("<w:textDirection w:val=\"btLr\"/>"));
    assert!(xml.contains("<w:tblW w:w=\"6000\" w:type=\"dxa\"/>"));
}

#[test]
fn a_cell_unmerged_gets_its_cells_back() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    merge_cells(&mut doc, &cell_pos(1, 0), &cell_pos(1, 1));
    let merged = save(bytes.clone(), &doc).unwrap();
    let mut doc2 = read(&merged);
    unmerge_cell(&mut doc2, &cell_pos(1, 0));
    let (_, xml) = saved(&merged, &doc2);
    assert_eq!(count(&xml, "<w:gridSpan"), 0);
    assert!(xml.contains("<w:noWrap/>"));
}

// -- sizes ----------------------------------------------------------------------

#[test]
fn a_column_width_changes_the_grid_every_cell_and_the_table() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    set_column_width(&mut doc, &Path::body(1), 1, 3000);
    let (_, xml) = saved(&bytes, &doc);
    assert_eq!(count(&xml, "<w:tcW w:w=\"3000\" w:type=\"dxa\"/>"), 3);
    assert!(xml.contains("<w:tblW w:w=\"7000\" w:type=\"dxa\"/>"));
    assert!(xml.contains("<w:tcFitText/>"));
}

#[test]
fn a_moved_border_keeps_the_table_as_wide() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    move_column_border(&mut doc, &Path::body(1), 1, 500);
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<w:gridCol w:w=\"2500\"/><w:gridCol w:w=\"1500\"/>"));
    assert!(xml.contains("<w:tblW w:w=\"6000\" w:type=\"dxa\"/>"));
}

#[test]
fn the_left_border_moves_the_indent() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    move_column_border(&mut doc, &Path::body(1), 0, 300);
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<w:tblInd w:w=\"400\" w:type=\"dxa\"/>"));
    assert!(xml.contains("<w:tblW w:w=\"5700\" w:type=\"dxa\"/>"));
    assert!(xml.contains("<w:tblCaption w:val=\"Kept\"/>"));
    assert_eq!(
        table(&read(&save(bytes.clone(), &doc).unwrap()))
            .props
            .indent,
        400
    );
}

#[test]
fn a_row_height_changes_only_the_height() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    set_row_height(
        &mut doc,
        &cell_pos(0, 0),
        &cell_pos(1, 0),
        RowHeight::Exactly(900),
    );
    let (_, xml) = saved(&bytes, &doc);
    assert!(xml.contains("<w:trHeight w:val=\"900\" w:hRule=\"exact\"/>"));
    assert!(!xml.contains("w:val=\"400\" w:hRule"));
    assert_eq!(count(&xml, "<w:trHeight "), 2);
    // The row's other properties stay.
    assert!(xml.contains("<w:cnfStyle w:val=\"100000000000\"/>"));
    assert!(xml.contains("<w:jc w:val=\"center\"/>"));
    // The third row is the bytes it was; the cells of the others are too.
    let original = main_xml(&bytes);
    let row2 = original.find("<w:tr w:rsidR=\"00A10003\">").unwrap();
    let end = original[row2..].find("</w:tr>").unwrap() + row2 + 7;
    assert!(xml.contains(&original[row2..end]));
    assert!(xml.contains("<w:tcFitText/>"));
}

#[test]
fn a_height_taken_off_leaves_the_other_row_properties() {
    let bytes = crafted();
    let mut doc = read(&bytes);
    set_row_height(&mut doc, &cell_pos(0, 0), &cell_pos(0, 0), RowHeight::Auto);
    let (_, xml) = saved(&bytes, &doc);
    assert!(!xml.contains("<w:trHeight"));
    assert!(xml.contains("<w:jc w:val=\"center\"/>"));
}

#[test]
fn a_table_that_was_not_changed_is_the_file_it_was() {
    let bytes = crafted();
    let doc = read(&bytes);
    assert_eq!(save(bytes.clone(), &doc).unwrap(), bytes);
}

// -- random sequences ----------------------------------------------------------------

/// Every part is well-formed XML with one root.
fn assert_well_formed(bytes: &[u8]) {
    for (name, data) in parts(bytes) {
        if !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        let mut reader = quick_xml::Reader::from_reader(data.as_slice());
        let (mut depth, mut roots) = (0i32, 0);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(_)) => {
                    roots += i32::from(depth == 0);
                    depth += 1;
                }
                Ok(quick_xml::events::Event::Empty(_)) => roots += i32::from(depth == 0),
                Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(err) => panic!("{name} is not well-formed: {err}"),
            }
        }
        assert_eq!((depth, roots), (0, 1), "{name}");
    }
}

#[test]
fn random_sequences_of_table_edits_save_and_read_back() {
    let bytes = crafted();
    let mut seed = 99u64;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n.max(1)
    };
    let mut saved_count = 0;
    for round in 0..150 {
        let mut doc = read(&bytes);
        let mut log = Vec::new();
        for _ in 0..1 + next(6) {
            let t = table(&doc);
            let r1 = next(t.rows.len());
            let r2 = next(t.rows.len());
            let a = cell_pos(r1, next(t.rows[r1].cells.len()));
            let b = cell_pos(r2, next(t.rows[r2].cells.len()));
            let op = next(12);
            log.push(op);
            match op {
                0 | 1 => {
                    merge_cells(&mut doc, &a, &b);
                }
                2 => {
                    unmerge_cell(&mut doc, &a);
                }
                3 => {
                    split_cell(&mut doc, &a, 1 + next(3), 1 + next(3));
                }
                4 => {
                    insert_columns(&mut doc, &a, &b, next(2) == 0);
                }
                5 => {
                    insert_rows(&mut doc, &a, &b, next(2) == 0);
                }
                6 => {
                    delete_columns(&mut doc, &a, &b);
                }
                7 => {
                    delete_rows(&mut doc, &a, &b);
                }
                8 | 9 => {
                    move_column_border(&mut doc, &Path::body(1), next(6), next(800) as i32 - 400);
                }
                10 => {
                    set_row_height(&mut doc, &a, &b, RowHeight::AtLeast(next(900) as i32));
                }
                _ => {
                    set_column_width(&mut doc, &Path::body(1), next(4), 300 + next(3000) as i32);
                }
            }
            if !matches!(doc.body.blocks.get(1), Some(Block::Table(_))) {
                break;
            }
        }
        if !matches!(doc.body.blocks.get(1), Some(Block::Table(_))) {
            continue;
        }
        let out =
            save(bytes.clone(), &doc).unwrap_or_else(|e| panic!("round {round} {log:?}: {e}"));
        assert_well_formed(&out);
        let back = read(&out);
        assert!(
            same_content(&doc, &back),
            "round {round} {log:?}\n{}",
            main_xml(&out)
        );
        check_table(table(&back)).unwrap();
        assert_widths_agree(&main_xml(&out));
        // Saving what was read again changes nothing.
        assert_eq!(
            save(out.clone(), &back).unwrap(),
            out,
            "round {round} {log:?}"
        );
        saved_count += 1;
    }
    assert!(saved_count > 100);
}
