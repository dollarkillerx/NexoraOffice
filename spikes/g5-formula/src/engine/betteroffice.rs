//! BetterOffice 0.5.1's calculation engine, two ways.
//!
//! Through the workbook facade, which is how the library is meant to be used:
//! an edit there also writes an undo record and a collaboration transaction.
//! And by driving the engine crate directly on the bare model, which is how it
//! would sit behind our own `CalcEngine` adapter.
//!
//! The engine keeps a dependency graph and recalculates what depends on the
//! edit, on one thread. Every recalculation has a fixed budget of ten million
//! cell reads; a formula that runs out keeps its old value. The budget cannot
//! be set from outside.

use betteroffice_xlsx::{CalculationOptions, CellRef, CellValue, SheetId, Workbook};
use xlsx_calc::graph::DepGraph;

use super::{Candidate, Engine, Source, Value};
use crate::workload::At;

pub const CANDIDATES: [Candidate; 2] = [
    Candidate {
        name: "betteroffice",
        about: "what depends on the edit, one thread, with the facade's bookkeeping",
        open: open_facade,
    },
    Candidate {
        name: "betteroffice/calc",
        about: "the same engine driven directly, without undo or collaboration records",
        open: open_direct,
    },
];

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

fn cell_ref(at: At) -> Result<CellRef, String> {
    CellRef::parse_a1(&at.a1()).map_err(|err| format!("{}: {err:?}", at.a1()))
}

fn sheet_id(at: At) -> SheetId {
    SheetId(at.sheet as u32)
}

fn convert(value: Option<&CellValue>) -> Value {
    match value {
        Some(CellValue::Number { value }) => Value::Number(*value),
        Some(CellValue::Text { value }) => Value::Text(value.clone()),
        Some(CellValue::Bool { value }) => Value::Bool(*value),
        Some(CellValue::Error { value }) => Value::Error(value.as_str().to_owned()),
        Some(CellValue::Empty) | None => Value::Empty,
    }
}

/// Cells the engine gave up on, summed over a run.
#[derive(Default)]
struct GaveUp {
    first: usize,
    edits: usize,
}

impl GaveUp {
    fn note(&self) -> Option<String> {
        (self.first + self.edits > 0).then(|| {
            format!(
                "ran out of its cell-read budget: {} formulas left uncalculated at first, {} over the edits",
                self.first, self.edits
            )
        })
    }
}

struct Facade {
    workbook: Workbook,
    gave_up: GaveUp,
}

fn open_facade(source: &Source) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Facade {
        workbook: Workbook::open(source.xlsx).map_err(text)?,
        gave_up: GaveUp::default(),
    }))
}

impl Engine for Facade {
    fn calculate_all(&mut self) -> Result<(), String> {
        let result = self.workbook.recalculate_all(CalculationOptions::default());
        self.gave_up.first = result.limited_cells.len();
        Ok(())
    }

    fn edit(&mut self, at: At, value: f64) -> Result<(), String> {
        let result = self
            .workbook
            .edit_cell(
                sheet_id(at),
                cell_ref(at)?,
                &value.to_string(),
                CalculationOptions::default(),
            )
            .map_err(text)?;
        if !result.applied {
            return Err(format!("the edit of {} was not applied", at.a1()));
        }
        self.gave_up.edits += result.limited_cells.len();
        Ok(())
    }

    fn value(&mut self, at: At) -> Result<Value, String> {
        let sheet = self.workbook.sheet(sheet_id(at)).map_err(text)?;
        Ok(convert(sheet.cell(cell_ref(at)?).map(|cell| &cell.value)))
    }

    fn note(&self) -> Option<String> {
        self.gave_up.note()
    }
}

struct Direct {
    model: xlsx_model::Workbook,
    graph: Option<DepGraph>,
    gave_up: GaveUp,
}

fn open_direct(source: &Source) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Direct {
        model: Workbook::open(source.xlsx).map_err(text)?.into_model(),
        graph: None,
        gave_up: GaveUp::default(),
    }))
}

impl Engine for Direct {
    fn calculate_all(&mut self) -> Result<(), String> {
        let (graph, result) = xlsx_calc::rebuild_and_recalc_all(&mut self.model, None);
        self.gave_up.first = result.limited_cells.len();
        self.graph = Some(graph);
        Ok(())
    }

    fn edit(&mut self, at: At, value: f64) -> Result<(), String> {
        let (sheet, cell) = (sheet_id(at), cell_ref(at)?);
        let target = self.model.sheet_mut(sheet).ok_or("no such sheet")?;
        let mut content = target
            .cell(cell)
            .cloned()
            .ok_or("the cell to edit is empty")?;
        content.value = CellValue::Number { value };
        target.set_cell(cell, content);
        let graph = self.graph.as_mut().ok_or("calculate_all was not called")?;
        let result = xlsx_calc::recalc_after(&mut self.model, graph, &[(sheet, cell)], None);
        self.gave_up.edits += result.limited_cells.len();
        Ok(())
    }

    fn value(&mut self, at: At) -> Result<Value, String> {
        let sheet = self.model.sheet(sheet_id(at)).ok_or("no such sheet")?;
        Ok(convert(sheet.cell(cell_ref(at)?).map(|cell| &cell.value)))
    }

    fn note(&self) -> Option<String> {
        self.gave_up.note()
    }
}
