//! Open a workbook, insert or delete rows and columns, add, rename or
//! delete sheets as the editor does, save a copy, and check that the copy
//! reads back as the edited workbook: to check what those saves change.
//!
//! ```text
//! cargo run -p aster-ooxml --example change_sheets -- in.xlsx out.xlsx \
//!     insert-rows 0 2 3   # sheet 0: 3 rows above row 3 (from 1)
//!     delete-columns 1 1 1  # sheet 1: column A
//!     rename 0 "新しい名前"
//!     add "Sheet9"
//!     delete 2
//! ```

use aster_ooxml::xlsx_save::{Changes, save};
use aster_sheets::structure::Lines;
use aster_sheets::{CellRef, Value, Workbook};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!(
            "usage: change_sheets <in.xlsx> <out.xlsx> \
             [insert-rows|delete-rows|insert-columns|delete-columns <sheet> <at> <count>] \
             [rename <sheet> <name>] [add <name>] [delete <sheet>] ..."
        );
        std::process::exit(2);
    }
    let source = std::fs::read(&args[0]).expect("read");
    let mut book = aster_ooxml::xlsx::read(source.clone()).expect("decode");
    let mut changes = Changes {
        origins: (0..book.sheets.len()).map(Some).collect(),
        ..Changes::default()
    };
    let mut rest = &args[2..];
    let number = |text: &str| -> usize { text.parse().expect("a number") };
    while let Some(op) = rest.first() {
        let mark = |changes: &mut Changes, cells: Vec<(usize, CellRef)>| {
            for cell in cells {
                changes.cells.insert(cell);
                changes.edited.insert(cell);
            }
            changes.formulas_changed = true;
        };
        match op.as_str() {
            "insert-rows" | "delete-rows" | "insert-columns" | "delete-columns" => {
                let [_, sheet, at, count, ..] = rest else {
                    panic!("{op} <sheet> <at> <count>");
                };
                let sheet = number(sheet);
                let lines = Lines {
                    columns: op.ends_with("columns"),
                    at: number(at) as u32 - 1,
                    count: number(count) as u32,
                    insert: op.starts_with("insert"),
                };
                if let Some(blocker) = &book.sheets[sheet].structure_blocker {
                    println!("refused on sheet {sheet}: it holds {blocker}");
                } else {
                    let cells = book.restructure(sheet, &lines);
                    mark(&mut changes, cells);
                    changes.merges.insert(sheet);
                    if let Some(file) = changes.origins[sheet] {
                        changes.structure.push((file, lines));
                    }
                    shift(&mut changes, sheet, &lines);
                }
                rest = &rest[4..];
            }
            "rename" => {
                let [_, sheet, name, ..] = rest else {
                    panic!("rename <sheet> <name>");
                };
                let sheet = number(sheet);
                book.check_sheet_name(name, Some(sheet))
                    .expect("a good name");
                let cells = book.rename_sheet(sheet, name);
                mark(&mut changes, cells);
                rest = &rest[3..];
            }
            "add" => {
                let [_, name, ..] = rest else {
                    panic!("add <name>");
                };
                book.check_sheet_name(name, None).expect("a good name");
                book.add_sheet(name);
                changes.origins.push(None);
                rest = &rest[2..];
            }
            "delete" => {
                let [_, sheet, ..] = rest else {
                    panic!("delete <sheet>");
                };
                let sheet = number(sheet);
                if let Some(blocker) = &book.sheets[sheet].delete_blocker {
                    println!("refused on sheet {sheet}: it holds {blocker}");
                } else {
                    let drop_sheet = |set: &mut std::collections::BTreeSet<(usize, CellRef)>| {
                        *set = std::mem::take(set)
                            .into_iter()
                            .filter(|(s, _)| *s != sheet)
                            .map(|(s, at)| (if s > sheet { s - 1 } else { s }, at))
                            .collect();
                    };
                    drop_sheet(&mut changes.cells);
                    drop_sheet(&mut changes.edited);
                    changes.merges = std::mem::take(&mut changes.merges)
                        .into_iter()
                        .filter(|s| *s != sheet)
                        .map(|s| if s > sheet { s - 1 } else { s })
                        .collect();
                    changes.origins.remove(sheet);
                    let cells = book.delete_sheet(sheet);
                    mark(&mut changes, cells);
                }
                rest = &rest[2..];
            }
            other => panic!("unknown change {other}"),
        }
    }
    let bytes = save(source, &book, &changes).expect("save");
    std::fs::write(&args[1], &bytes).expect("write");
    let again = aster_ooxml::xlsx::read(bytes).expect("the saved file reads");
    let names: Vec<&str> = again.sheets.iter().map(|s| s.name.as_str()).collect();
    println!("{}: sheets {names:?}", args[1]);
    let differing = differences(&book, &again);
    println!(
        "cells differing from the edited workbook: {}",
        differing.len()
    );
    for (sheet, at, what) in differing.iter().take(10) {
        println!("  sheet {sheet} {at}: {what}");
    }
}

/// Cells already marked for saving move with their lines.
fn shift(changes: &mut Changes, sheet: usize, lines: &Lines) {
    let moved = |at: CellRef| {
        if lines.columns {
            lines.moved(at.col).map(|col| CellRef::new(at.row, col))
        } else {
            lines.moved(at.row).map(|row| CellRef::new(row, at.col))
        }
    };
    let remap = |set: &mut std::collections::BTreeSet<(usize, CellRef)>| {
        *set = std::mem::take(set)
            .into_iter()
            .filter_map(|(s, at)| {
                if s == sheet {
                    Some((s, moved(at)?))
                } else {
                    Some((s, at))
                }
            })
            .collect();
    };
    remap(&mut changes.cells);
    remap(&mut changes.edited);
}

/// Cells whose value or formula is not the same in both.
fn differences(a: &Workbook, b: &Workbook) -> Vec<(usize, CellRef, String)> {
    let mut out = Vec::new();
    for (index, (x, y)) in a.sheets.iter().zip(&b.sheets).enumerate() {
        let cells = |sheet: &aster_sheets::Sheet| -> Vec<(CellRef, Value, Option<String>)> {
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
        };
        let (left, right) = (cells(x), cells(y));
        if left.len() != right.len() {
            out.push((
                index,
                CellRef::new(0, 0),
                format!("{} cells, read back {}", left.len(), right.len()),
            ));
        }
        for (l, r) in left.iter().zip(&right) {
            if l != r {
                out.push((index, l.0, format!("{l:?} read back as {r:?}")));
            }
        }
    }
    if a.sheets.len() != b.sheets.len() {
        out.push((0, CellRef::new(0, 0), "sheet count".to_owned()));
    }
    out
}
