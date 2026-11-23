//! Times reading a workbook and calculating it, and lists the formulas
//! whose result differs from the one saved in the file:
//! `cargo run --release -p aster-calc --example time_xlsx -- file.xlsx`
//! Addresses after the file (`Orders!E2`) print those cells' results.

use std::time::Instant;

use aster_calc::Formualizer;
use aster_sheets::{CalcEngine, Cancel};

fn main() {
    let path = std::env::args().nth(1).expect("an .xlsx file");
    let bytes = std::fs::read(&path).expect("read");
    let started = Instant::now();
    let workbook = aster_ooxml::xlsx::read(bytes).expect("decode");
    let read = started.elapsed();
    let mut engine = Formualizer::load(&workbook).expect("load");
    let loaded = started.elapsed();
    let results = engine.apply(&[], &Cancel::new()).expect("calculate");
    let done = started.elapsed();
    println!(
        "{} formulas; read {read:?}, engine built {:?}, calculated {:?}",
        results.len(),
        loaded - read,
        done - loaded
    );
    for wanted in std::env::args().skip(2) {
        let Some((sheet, cell)) = wanted.split_once('!') else {
            continue;
        };
        let index = workbook.sheets.iter().position(|s| s.name == sheet);
        let at = aster_sheets::CellRef::parse(cell);
        let value = results
            .iter()
            .find(|r| Some(r.sheet) == index && Some(r.at) == at)
            .map(|r| format!("{:?}", r.value));
        println!(
            "{wanted} = {}",
            value.unwrap_or_else(|| "(not a formula)".into())
        );
    }
    let mismatches = workbook.mismatches(&results);
    println!("{} differ from the saved results", mismatches.len());
    for m in mismatches.iter().take(20) {
        let sheet = &workbook.sheets[m.sheet].name;
        println!(
            "  {sheet}!{}: saved {:?}, calculated {:?}",
            m.at, m.saved, m.computed
        );
    }
}
