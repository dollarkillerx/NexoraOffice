//! Times reading a workbook and calculating it:
//! `cargo run --release -p aster-calc --example time_xlsx -- file.xlsx`

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
}
