//! IronCalc 0.9.1.
//!
//! `evaluate()` runs every formula in the workbook each time it is called:
//! there is no dependency graph between calls and no dirty tracking. Setting a
//! cell does not evaluate, so an edit here is a set followed by `evaluate()`.

use ironcalc::base::Model;
use ironcalc::base::cell::CellValue;
use ironcalc::import::load_from_xlsx_bytes;

use super::{Candidate, Engine, Source, Value};
use crate::workload::At;

pub const CANDIDATE: Candidate = Candidate {
    name: "ironcalc",
    about: "every formula on every evaluation, one thread",
    open,
};

struct IronCalc(Model<'static>);

fn open(source: &Source) -> Result<Box<dyn Engine>, String> {
    let workbook =
        load_from_xlsx_bytes(source.xlsx, "g5", "en", "UTC").map_err(|err| err.to_string())?;
    Ok(Box::new(IronCalc(Model::from_workbook(workbook, "en")?)))
}

impl Engine for IronCalc {
    fn calculate_all(&mut self) -> Result<(), String> {
        self.0.evaluate();
        Ok(())
    }

    fn edit(&mut self, at: At, value: f64) -> Result<(), String> {
        self.0
            .update_cell_with_number(at.sheet as u32, at.row as i32, at.col as i32 + 1, value)?;
        self.0.evaluate();
        Ok(())
    }

    fn value(&mut self, at: At) -> Result<Value, String> {
        let value =
            self.0
                .get_cell_value_by_index(at.sheet as u32, at.row as i32, at.col as i32 + 1)?;
        Ok(match value {
            CellValue::Number(number) => Value::Number(number),
            // IronCalc has no error type: an error is the text Excel shows.
            CellValue::String(text) if text.starts_with('#') => Value::Error(text),
            CellValue::String(text) => Value::Text(text),
            CellValue::None => Value::Empty,
            CellValue::Boolean(flag) => Value::Bool(flag),
        })
    }
}
