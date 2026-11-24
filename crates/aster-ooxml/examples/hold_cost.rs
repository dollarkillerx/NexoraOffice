//! What a workbook's recovery record costs the editor: how long saving a
//! workbook into bytes takes (the record writer holds the workbook that
//! long), and what one edit costs with and without that hold. With the
//! hold, the edit copies the whole workbook first.
//!
//! `cargo run --release -p aster-ooxml --example hold_cost -- in.xlsx [edits]`
//! (or `--cells=1000000` for a made-up workbook of that many cells: rows of
//! a number, a text and a formula ten wide).

use std::sync::Arc;
use std::time::{Duration, Instant};

use aster_ooxml::xlsx_save::{Changes, save};
use aster_sheets::{Cell, CellRef, Value, Workbook};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(input) = args.first() else {
        eprintln!("usage: hold_cost <in.xlsx> [edits]");
        std::process::exit(2);
    };
    let edits: u32 = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(20);
    let source = match input.strip_prefix("--cells=") {
        Some(count) => synthetic(count.parse().expect("a number of cells")),
        None => std::fs::read(input).expect("read"),
    };
    let started = Instant::now();
    let book = aster_ooxml::xlsx::read(source.clone()).expect("decode");
    let cells: usize = book.sheets.iter().map(|s| s.cell_count()).sum();
    println!(
        "{input}: {} sheets, {cells} cells, read in {:.0?}",
        book.sheets.len(),
        started.elapsed()
    );
    let mut book = Arc::new(book);
    let mut changes = Changes::default();

    // An edit with nobody else holding the workbook.
    let alone = time_edits(&mut book, &mut changes, edits, false);
    // An edit right after a snapshot was taken, the writer still holding it.
    let held = time_edits(&mut book, &mut changes, edits, true);
    let started = Instant::now();
    let bytes = save(source.clone(), &book, &changes).expect("save");
    let saving = started.elapsed();
    println!(
        "saved {} bytes in {saving:.0?} (the writer holds the workbook this long)",
        bytes.len()
    );
    println!(
        "one edit, nobody holding:  median {:.2?}, worst {:.2?}",
        median(&alone),
        worst(&alone)
    );
    println!(
        "one edit, snapshot held:   median {:.2?}, worst {:.2?}",
        median(&held),
        worst(&held)
    );
}

/// Time `count` one-cell edits as the editor makes them: `Arc::make_mut`,
/// then the cell. With `hold`, a snapshot is taken before each.
fn time_edits(
    book: &mut Arc<Workbook>,
    changes: &mut Changes,
    count: u32,
    hold: bool,
) -> Vec<Duration> {
    let mut times = Vec::new();
    for n in 0..count {
        let snapshot = hold.then(|| book.clone());
        let at = CellRef::new(n, 0);
        let started = Instant::now();
        let workbook = Arc::make_mut(book);
        for change in workbook.set_cells(
            0,
            vec![(at, Some(Cell::new(0, Value::Number(f64::from(n)))))],
        ) {
            changes.cells.insert((0, change.at));
            changes.edited.insert((0, change.at));
        }
        times.push(started.elapsed());
        drop(snapshot);
    }
    times
}

/// A workbook of about `count` cells, saved: each row has numbers in
/// A..H, a text in I and a formula in J.
fn synthetic(count: usize) -> Vec<u8> {
    let blank = aster_ooxml::xlsx_save::blank("Sheet1", "Calibri");
    let mut book = aster_ooxml::xlsx::read(blank.clone()).expect("blank reads");
    let rows = (count / 10) as u32;
    let mut cells = Vec::with_capacity(count);
    for row in 0..rows {
        for col in 0..8 {
            cells.push((
                CellRef::new(row, col),
                Some(Cell::new(col, Value::Number(f64::from(row * 8 + col)))),
            ));
        }
        cells.push((
            CellRef::new(row, 8),
            Some(Cell::new(8, Value::Text(Arc::from(format!("行 {row}"))))),
        ));
        let mut formula = Cell::new(9, Value::Number(0.0));
        formula.formula = Some(Arc::from(format!("SUM(A{0}:H{0})", row + 1)));
        cells.push((CellRef::new(row, 9), Some(formula)));
    }
    let mut changes = Changes::default();
    for change in book.set_cells(0, cells) {
        changes.cells.insert((0, change.at));
        changes.edited.insert((0, change.at));
    }
    changes.formulas_changed = true;
    save(blank, &book, &changes).expect("the made-up workbook saves")
}

fn median(times: &[Duration]) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

fn worst(times: &[Duration]) -> Duration {
    times.iter().max().copied().unwrap_or_default()
}
