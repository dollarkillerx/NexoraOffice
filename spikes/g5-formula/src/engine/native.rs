//! Not an engine: the three workbooks' arithmetic written out by hand.
//!
//! On every edit it recomputes all 100,000 values from the inputs, with no
//! parsing, no dependency tracking and no cell storage in the way. That is the
//! floor for a full recalculation on this machine, and what the engines'
//! numbers should be read against.

use std::collections::HashMap;

use super::{Candidate, Engine, Source, Value};
use crate::workload::{self, At, Book, Content};

pub const CANDIDATE: Candidate = Candidate {
    name: "by hand",
    about: "the same arithmetic in plain Rust, everything recomputed on each edit",
    open,
};

fn numbers(book: &Book, sheet: usize, col: u32, first_row: u32) -> Vec<f64> {
    book.sheets[sheet]
        .cells
        .iter()
        .filter(|cell| cell.col == col && cell.row >= first_row)
        .filter_map(|cell| match cell.content {
            Content::Number(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn texts(book: &Book, sheet: usize, col: u32, first_row: u32) -> Vec<String> {
    book.sheets[sheet]
        .cells
        .iter()
        .filter(|cell| cell.col == col && cell.row >= first_row)
        .filter_map(|cell| match &cell.content {
            Content::Text(value) => Some(value.clone()),
            _ => None,
        })
        .collect()
}

enum Native {
    Rows {
        quantity: Vec<f64>,
        price: Vec<f64>,
        rate: f64,
        amount: Vec<f64>,
        tax: Vec<f64>,
        total: Vec<f64>,
        big: Vec<bool>,
        sums: [f64; 4],
    },
    Chain {
        opening: f64,
        amount: Vec<f64>,
        balance: Vec<f64>,
    },
    Lookup {
        price: Vec<f64>,
        row_of: HashMap<String, usize>,
        code: Vec<String>,
        quantity: Vec<f64>,
        unit: Vec<f64>,
        amount: Vec<f64>,
    },
}

fn open(source: &Source) -> Result<Box<dyn Engine>, String> {
    let book = workload::by_name(source.workload, 0, source.scale)
        .ok_or_else(|| format!("no workload called {}", source.workload))?
        .book;
    let native = match source.workload {
        "rows" => {
            let quantity = numbers(&book, 0, 0, 2);
            let count = quantity.len();
            Native::Rows {
                quantity,
                price: numbers(&book, 0, 1, 2),
                rate: numbers(&book, 0, 7, 1)[0],
                amount: vec![0.0; count],
                tax: vec![0.0; count],
                total: vec![0.0; count],
                big: vec![false; count],
                sums: [0.0; 4],
            }
        }
        "chain" => {
            let amount = numbers(&book, 0, 1, 2);
            Native::Chain {
                opening: numbers(&book, 0, 0, 1)[0],
                balance: vec![0.0; amount.len()],
                amount,
            }
        }
        _ => {
            let quantity = numbers(&book, 1, 1, 2);
            let count = quantity.len();
            Native::Lookup {
                price: numbers(&book, 0, 1, 2),
                row_of: texts(&book, 0, 0, 2)
                    .into_iter()
                    .enumerate()
                    .map(|(index, code)| (code, index))
                    .collect(),
                code: texts(&book, 1, 0, 2),
                quantity,
                unit: vec![0.0; count],
                amount: vec![0.0; count],
            }
        }
    };
    Ok(Box::new(native))
}

impl Engine for Native {
    fn calculate_all(&mut self) -> Result<(), String> {
        match self {
            Native::Rows {
                quantity,
                price,
                rate,
                amount,
                tax,
                total,
                big,
                sums,
            } => {
                *sums = [0.0; 4];
                for index in 0..quantity.len() {
                    amount[index] = quantity[index] * price[index];
                    tax[index] = amount[index] * *rate;
                    total[index] = amount[index] + tax[index];
                    big[index] = total[index] > 1000.0;
                    sums[0] += amount[index];
                    sums[1] += tax[index];
                    sums[2] += total[index];
                    sums[3] += f64::from(u8::from(big[index]));
                }
            }
            Native::Chain {
                opening,
                amount,
                balance,
            } => {
                let mut running = *opening;
                for (index, value) in amount.iter().enumerate() {
                    running += value;
                    balance[index] = running;
                }
            }
            Native::Lookup {
                price,
                row_of,
                code,
                quantity,
                unit,
                amount,
            } => {
                for index in 0..code.len() {
                    // The lookup is by text, as in the workbook.
                    let row = *row_of.get(&code[index]).ok_or("unknown product code")?;
                    unit[index] = price[row];
                    amount[index] = quantity[index] * unit[index];
                }
            }
        }
        Ok(())
    }

    fn edit(&mut self, at: At, value: f64) -> Result<(), String> {
        let index = at.row as usize - 2;
        match self {
            Native::Rows { quantity, .. } if at.col == 0 => quantity[index] = value,
            Native::Chain { amount, .. } if at.col == 1 => amount[index] = value,
            Native::Lookup { price, .. } if at.sheet == 0 && at.col == 1 => price[index] = value,
            _ => return Err(format!("{} is not an input of this workload", at.a1())),
        }
        self.calculate_all()
    }

    fn value(&mut self, at: At) -> Result<Value, String> {
        let index = (at.row as usize).wrapping_sub(2);
        let found = match self {
            Native::Rows {
                amount,
                tax,
                total,
                big,
                sums,
                ..
            } => match (at.col, at.row) {
                (2, _) => amount.get(index).copied().map(Value::Number),
                (3, _) => tax.get(index).copied().map(Value::Number),
                (4, _) => total.get(index).copied().map(Value::Number),
                (5, _) => big
                    .get(index)
                    .map(|big| Value::Text(if *big { "大" } else { "小" }.to_owned())),
                (7, 3..=6) => Some(Value::Number(sums[at.row as usize - 3])),
                _ => None,
            },
            Native::Chain { balance, .. } => match at.col {
                0 => balance.get(index).copied().map(Value::Number),
                _ => None,
            },
            Native::Lookup { unit, amount, .. } => match (at.sheet, at.col) {
                (1, 2) => unit.get(index).copied().map(Value::Number),
                (1, 3) => amount.get(index).copied().map(Value::Number),
                _ => None,
            },
        };
        found.ok_or_else(|| format!("{} is not a result of this workload", at.a1()))
    }
}
