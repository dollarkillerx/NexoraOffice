//! formualizer 0.15.0.
//!
//! It keeps a dependency graph between calls. Setting a value marks what
//! depends on it, and `evaluate_all` then calculates only what is marked. It
//! can spread a layer of independent cells over a thread pool, so it is run
//! twice: with its default pool and on one thread.

use formualizer::workbook::{NativeXlsxAdapter, SpreadsheetReader};
use formualizer::{LiteralValue, LoadStrategy, Workbook, WorkbookConfig};

use super::{Candidate, Engine, Source, Value};
use crate::workload::At;

pub const CANDIDATES: [Candidate; 2] = [
    Candidate {
        name: "formualizer",
        about: "what depends on the edit, layers spread over a thread pool",
        open: |source| open(source, true),
    },
    Candidate {
        name: "formualizer/1",
        about: "the same on one thread",
        open: |source| open(source, false),
    },
];

struct Formualizer {
    workbook: Workbook,
    sheets: Vec<&'static str>,
    /// Vertices the last edit made the engine compute.
    computed: Vec<usize>,
}

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

fn open(source: &Source, parallel: bool) -> Result<Box<dyn Engine>, String> {
    // No change log: nothing here undoes anything.
    let mut config = WorkbookConfig::ephemeral();
    config.eval.enable_parallel = parallel;
    let reader = NativeXlsxAdapter::open_bytes(source.xlsx.to_vec()).map_err(text)?;
    let workbook = Workbook::from_reader(reader, LoadStrategy::EagerAll, config).map_err(text)?;
    Ok(Box::new(Formualizer {
        workbook,
        sheets: source.sheets.clone(),
        computed: Vec::new(),
    }))
}

impl Engine for Formualizer {
    fn calculate_all(&mut self) -> Result<(), String> {
        self.workbook.evaluate_all().map_err(text)?;
        Ok(())
    }

    fn edit(&mut self, at: At, value: f64) -> Result<(), String> {
        self.workbook
            .set_value(
                self.sheets[at.sheet],
                at.row,
                at.col + 1,
                LiteralValue::Number(value),
            )
            .map_err(text)?;
        let result = self.workbook.evaluate_all().map_err(text)?;
        self.computed.push(result.computed_vertices);
        Ok(())
    }

    fn value(&mut self, at: At) -> Result<Value, String> {
        Ok(
            match self
                .workbook
                .get_value(self.sheets[at.sheet], at.row, at.col + 1)
            {
                Some(LiteralValue::Number(number)) => Value::Number(number),
                Some(LiteralValue::Int(number)) => Value::Number(number as f64),
                Some(LiteralValue::Text(text)) => Value::Text(text),
                Some(LiteralValue::Boolean(flag)) => Value::Bool(flag),
                Some(LiteralValue::Error(error)) => Value::Error(error.kind.to_string()),
                Some(LiteralValue::Empty) | None => Value::Empty,
                // Dates are a type of their own here. Excel has none: a date is
                // its serial number, so that is what is compared.
                Some(other) => match other.as_serial_number() {
                    Some(serial) => Value::Number(serial),
                    None => Value::Other(format!("{other:?}")),
                },
            },
        )
    }

    fn note(&self) -> Option<String> {
        let most = self.computed.iter().max()?;
        let least = self.computed.iter().min()?;
        Some(format!("computed {least} to {most} cells per edit"))
    }
}
