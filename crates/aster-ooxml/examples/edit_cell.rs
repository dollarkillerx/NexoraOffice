//! Open a workbook, change one cell of its first sheet as if it were typed,
//! and save a copy: to check what saving changes.
//!
//! `cargo run -p aster-ooxml --example edit_cell -- in.xlsx out.xlsx B2 "東京"`

use aster_ooxml::xlsx_save::{Changes, save};
use aster_sheets::input::{Reading, read};
use aster_sheets::{CellRef, DateOrder};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, cell, text] = args.as_slice() else {
        eprintln!("usage: edit_cell <in.xlsx> <out.xlsx> <cell> <text>");
        std::process::exit(2);
    };
    let source = std::fs::read(input).expect("read");
    let mut workbook = aster_ooxml::xlsx::read(source.clone()).expect("decode");
    let at = CellRef::parse(cell).expect("a cell address such as B2");
    let reading = Reading {
        order: DateOrder::Ymd,
        date1904: workbook.date1904,
        this_year: 2026,
    };
    let (entry, look) = read(text, &reading);
    let new = workbook.entry_cell(0, at, entry, look);
    let mut changes = Changes::default();
    for change in workbook.set_cells(0, vec![(at, new)]) {
        changes.cells.insert((0, change.at));
        changes.edited.insert((0, change.at));
        changes.formulas_changed |= change.touches_formula();
    }
    let bytes = save(source, &workbook, &changes).expect("save");
    std::fs::write(output, &bytes).expect("write");
    let again = aster_ooxml::xlsx::read(bytes).expect("the saved file reads");
    let shown = again.sheets[0]
        .cell(at)
        .and_then(|c| again.display(c))
        .map(|d| d.text)
        .unwrap_or_default();
    println!("{output}: {cell} = {shown}");
}
