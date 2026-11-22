//! The workbooks the engines are timed on, the edits made to them, and the
//! values every engine has to arrive at.
//!
//! Each workbook holds exactly 100,000 formulas, or a multiple of that when a
//! scale is given. They differ in how much of the
//! workbook one edit touches, which is what separates an engine that follows
//! dependencies from one that recalculates everything.
//!
//! The expected values are not taken from any engine. They are worked out here
//! in plain arithmetic. Every number in the workbooks is a multiple of 1/32
//! that stays far below 2^53, so every sum and product is exact in an `f64` and
//! does not depend on the order an engine adds things up in. A correct engine
//! therefore has to match bit for bit; there is no tolerance to hide behind.

pub const FORMULAS: usize = 100_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Number(f64),
    Text(String),
    /// Without the leading `=`. Its result is a number.
    Formula(String),
    /// A formula whose result is text.
    TextFormula(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// Counted from 1, as in `A1`.
    pub row: u32,
    /// Counted from 0: column A is 0.
    pub col: u32,
    pub content: Content,
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: &'static str,
    /// In row order, and in column order within a row.
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone)]
pub struct Book {
    pub sheets: Vec<Sheet>,
}

impl Book {
    pub fn formula_count(&self) -> usize {
        self.sheets
            .iter()
            .flat_map(|sheet| &sheet.cells)
            .filter(|cell| matches!(cell.content, Content::Formula(_) | Content::TextFormula(_)))
            .count()
    }
}

/// A cell address: sheet index, row from 1, column from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
}

/// `A1`-style reference of a row and column.
pub fn a1(row: u32, col: u32) -> String {
    let mut letters = Vec::new();
    let mut rest = col + 1;
    while rest > 0 {
        let digit = (rest - 1) % 26;
        letters.push(b'A' + digit as u8);
        rest = (rest - 1) / 26;
    }
    letters.reverse();
    format!(
        "{}{row}",
        String::from_utf8(letters).expect("ASCII letters")
    )
}

impl At {
    pub fn a1(&self) -> String {
        a1(self.row, self.col)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expected {
    Number(f64),
    Text(&'static str),
}

pub type Probe = (At, Expected);

/// One edit and what a few cells must hold once it has been recalculated.
#[derive(Debug, Clone)]
pub struct Step {
    pub at: At,
    pub value: f64,
    pub probes: Vec<Probe>,
}

pub struct Workload {
    pub name: &'static str,
    /// What one edit makes out of date.
    pub reach: &'static str,
    pub book: Book,
    /// What the probed cells hold before any edit.
    pub initial: Vec<Probe>,
    pub steps: Vec<Step>,
}

/// A small deterministic generator, so that the workbooks are the same on every
/// machine and in every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn number(row: u32, col: u32, value: f64) -> Cell {
    Cell {
        row,
        col,
        content: Content::Number(value),
    }
}

fn text(row: u32, col: u32, value: impl Into<String>) -> Cell {
    Cell {
        row,
        col,
        content: Content::Text(value.into()),
    }
}

fn formula(row: u32, col: u32, value: impl Into<String>) -> Cell {
    Cell {
        row,
        col,
        content: Content::Formula(value.into()),
    }
}

const BIG: &str = "大";
const SMALL: &str = "小";

/// An order table: four formulas in each of 24,999 rows and four totals over
/// the columns. Changing one quantity makes eight cells out of date, but the
/// four totals each read a whole column again.
pub fn rows(steps: usize, scale: u32) -> Workload {
    const FIRST: u32 = 2;
    const RATE: f64 = 0.125;
    #[allow(non_snake_case)]
    let LAST: u32 = 25_000 * scale;
    let count = (LAST - FIRST + 1) as usize;
    let mut rng = Rng(0x0a57_e205);
    let mut quantity: Vec<f64> = (0..count).map(|_| (1 + rng.below(50)) as f64).collect();
    let price: Vec<f64> = (0..count)
        .map(|_| (4 + rng.below(397)) as f64 * 0.25)
        .collect();

    let mut cells = vec![
        text(1, 0, "数量"),
        text(1, 1, "单价"),
        text(1, 2, "金额"),
        text(1, 3, "税"),
        text(1, 4, "合计"),
        text(1, 5, "档"),
        number(1, 7, RATE),
    ];
    for index in 0..count {
        let row = FIRST + index as u32;
        cells.push(number(row, 0, quantity[index]));
        cells.push(number(row, 1, price[index]));
        cells.push(formula(row, 2, format!("A{row}*B{row}")));
        cells.push(formula(row, 3, format!("C{row}*$H$1")));
        cells.push(formula(row, 4, format!("C{row}+D{row}")));
        cells.push(Cell {
            row,
            col: 5,
            content: Content::TextFormula(format!("IF(E{row}>1000,\"{BIG}\",\"{SMALL}\")")),
        });
        // The totals sit beside rows 3 to 6.
        let total = match row {
            3 => Some(format!("SUM(C{FIRST}:C{LAST})")),
            4 => Some(format!("SUM(D{FIRST}:D{LAST})")),
            5 => Some(format!("SUM(E{FIRST}:E{LAST})")),
            6 => Some(format!("COUNTIF(F{FIRST}:F{LAST},\"{BIG}\")")),
            _ => None,
        };
        if let Some(total) = total {
            cells.push(formula(row, 7, total));
        }
    }

    let total_of = |quantity: &[f64], index: usize| {
        let amount = quantity[index] * price[index];
        amount + amount * RATE
    };
    let probes = |quantity: &[f64], index: usize| -> Vec<Probe> {
        let row = FIRST + index as u32;
        let at = |row, col| At { sheet: 0, row, col };
        let totals: Vec<f64> = (0..count).map(|i| total_of(quantity, i)).collect();
        let size = if totals[index] > 1000.0 { BIG } else { SMALL };
        vec![
            (at(row, 4), Expected::Number(totals[index])),
            (at(row, 5), Expected::Text(size)),
            (at(5, 7), Expected::Number(totals.iter().sum())),
            (
                at(6, 7),
                Expected::Number(totals.iter().filter(|total| **total > 1000.0).count() as f64),
            ),
        ]
    };

    let initial = probes(&quantity, 0);
    let mut made = Vec::with_capacity(steps);
    for _ in 0..steps {
        let index = rng.below(count as u64) as usize;
        let mut value = (1 + rng.below(50)) as f64;
        if value == quantity[index] {
            value = value % 50.0 + 1.0;
        }
        quantity[index] = value;
        made.push(Step {
            at: At {
                sheet: 0,
                row: FIRST + index as u32,
                col: 0,
            },
            value,
            probes: probes(&quantity, index),
        });
    }
    Workload {
        name: "rows",
        reach: "8 cells: 4 in the row, and 4 totals that each read a whole column",
        book: Book {
            sheets: vec![Sheet {
                name: "Orders",
                cells,
            }],
        },
        initial,
        steps: made,
    }
}

/// A running balance: every one of the 100,000 formulas adds to the one above
/// it. Changing the first amount makes the whole workbook out of date, so this
/// is what a full recalculation costs.
pub fn chain(steps: usize, scale: u32) -> Workload {
    const OPENING: f64 = 1000.0;
    let formulas = FORMULAS * scale as usize;
    #[allow(non_snake_case)]
    let LAST: u32 = formulas as u32 + 1;
    let mut rng = Rng(0x0c4a_1205);
    let mut amount: Vec<f64> = (0..formulas)
        .map(|_| (rng.below(4001) as f64 - 2000.0) * 0.25)
        .collect();

    let mut cells = vec![number(1, 0, OPENING)];
    for (index, value) in amount.iter().enumerate() {
        let row = index as u32 + 2;
        cells.push(formula(row, 0, format!("A{}+B{row}", row - 1)));
        cells.push(number(row, 1, *value));
    }

    let probes = |amount: &[f64]| -> Vec<Probe> {
        let mut balance = OPENING;
        let mut found = Vec::new();
        for (index, value) in amount.iter().enumerate() {
            balance += value;
            let row = index as u32 + 2;
            if row == 2 || row == LAST / 2 || row == LAST {
                found.push((
                    At {
                        sheet: 0,
                        row,
                        col: 0,
                    },
                    Expected::Number(balance),
                ));
            }
        }
        found
    };

    let initial = probes(&amount);
    let mut made = Vec::with_capacity(steps);
    for _ in 0..steps {
        let mut value = (rng.below(4001) as f64 - 2000.0) * 0.25;
        if value == amount[0] {
            value += 0.25;
        }
        amount[0] = value;
        made.push(Step {
            at: At {
                sheet: 0,
                row: 2,
                col: 1,
            },
            value,
            probes: probes(&amount),
        });
    }
    Workload {
        name: "chain",
        reach: "every formula, each depending on the one before",
        book: Book {
            sheets: vec![Sheet {
                name: "Ledger",
                cells,
            }],
        },
        initial,
        steps: made,
    }
}

/// 50,000 order lines that look their price up in a table of 1,000 products on
/// another sheet, and multiply. Changing one price matters to about fifty
/// lines, but every lookup names the whole table as its range.
pub fn lookup(steps: usize, scale: u32) -> Workload {
    const PRODUCTS: usize = 1000;
    #[allow(non_snake_case)]
    let ORDERS: usize = FORMULAS / 2 * scale as usize;
    let mut rng = Rng(0x100c_0b05);
    let mut price: Vec<f64> = (0..PRODUCTS)
        .map(|_| (4 + rng.below(397)) as f64 * 0.25)
        .collect();
    let product: Vec<usize> = (0..ORDERS)
        .map(|_| rng.below(PRODUCTS as u64) as usize)
        .collect();
    let quantity: Vec<f64> = (0..ORDERS).map(|_| (1 + rng.below(50)) as f64).collect();
    let code = |index: usize| format!("P{:04}", index + 1);

    let mut prices = vec![text(1, 0, "code"), text(1, 1, "price")];
    for (index, value) in price.iter().enumerate() {
        let row = index as u32 + 2;
        prices.push(text(row, 0, code(index)));
        prices.push(number(row, 1, *value));
    }
    let mut orders = vec![
        text(1, 0, "code"),
        text(1, 1, "qty"),
        text(1, 2, "price"),
        text(1, 3, "amount"),
    ];
    for index in 0..ORDERS {
        let row = index as u32 + 2;
        orders.push(text(row, 0, code(product[index])));
        orders.push(number(row, 1, quantity[index]));
        orders.push(formula(
            row,
            2,
            format!("VLOOKUP(A{row},Prices!$A$2:$B${},2,FALSE)", PRODUCTS + 1),
        ));
        orders.push(formula(row, 3, format!("B{row}*C{row}")));
    }

    // For a product: the first order line that uses it, and the first that
    // uses another one and so must not change.
    let lines = |changed: usize| -> (usize, usize) {
        let uses = product
            .iter()
            .position(|p| *p == changed)
            .expect("50,000 lines over 1,000 products use every product");
        let other = product
            .iter()
            .position(|p| *p != changed)
            .expect("more than one product is ordered");
        (uses, other)
    };
    let probes = |price: &[f64], changed: usize| -> Vec<Probe> {
        let (uses, other) = lines(changed);
        [uses, other]
            .into_iter()
            .flat_map(|line| {
                let row = line as u32 + 2;
                let unit = price[product[line]];
                [
                    (
                        At {
                            sheet: 1,
                            row,
                            col: 2,
                        },
                        Expected::Number(unit),
                    ),
                    (
                        At {
                            sheet: 1,
                            row,
                            col: 3,
                        },
                        Expected::Number(quantity[line] * unit),
                    ),
                ]
            })
            .collect()
    };

    let initial = probes(&price, 0);
    let mut made = Vec::with_capacity(steps);
    for _ in 0..steps {
        let changed = rng.below(PRODUCTS as u64) as usize;
        let mut value = (4 + rng.below(397)) as f64 * 0.25;
        if value == price[changed] {
            value += 0.25;
        }
        price[changed] = value;
        made.push(Step {
            at: At {
                sheet: 0,
                row: changed as u32 + 2,
                col: 1,
            },
            value,
            probes: probes(&price, changed),
        });
    }
    Workload {
        name: "lookup",
        reach: "about 1 line in 1,000 by value; every lookup by the range it names",
        book: Book {
            sheets: vec![
                Sheet {
                    name: "Prices",
                    cells: prices,
                },
                Sheet {
                    name: "Orders",
                    cells: orders,
                },
            ],
        },
        initial,
        steps: made,
    }
}

pub const NAMES: [&str; 3] = ["rows", "chain", "lookup"];

/// `scale` multiplies the number of formulas: 1 is the gate's 100,000.
pub fn by_name(name: &str, steps: usize, scale: u32) -> Option<Workload> {
    match name {
        "rows" => Some(rows(steps, scale)),
        "chain" => Some(chain(steps, scale)),
        "lookup" => Some(lookup(steps, scale)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_workbook_holds_exactly_one_hundred_thousand_formulas() {
        for name in NAMES {
            let workload = by_name(name, 3, 1).unwrap();
            assert_eq!(workload.book.formula_count(), FORMULAS, "{name}");
            assert_eq!(workload.steps.len(), 3, "{name}");
            let doubled = by_name(name, 0, 2).unwrap();
            assert_eq!(doubled.book.formula_count(), 2 * FORMULAS, "{name}");
        }
    }

    #[test]
    fn cells_are_in_reading_order_and_unique() {
        for name in NAMES {
            for sheet in by_name(name, 0, 1).unwrap().book.sheets {
                for pair in sheet.cells.windows(2) {
                    assert!(
                        (pair[0].row, pair[0].col) < (pair[1].row, pair[1].col),
                        "{name}/{}: {:?} then {:?}",
                        sheet.name,
                        pair[0],
                        pair[1]
                    );
                }
            }
        }
    }

    #[test]
    fn column_letters() {
        assert_eq!(a1(1, 0), "A1");
        assert_eq!(a1(25_000, 7), "H25000");
        assert_eq!(a1(3, 25), "Z3");
        assert_eq!(a1(3, 26), "AA3");
        assert_eq!(a1(3, 701), "ZZ3");
        assert_eq!(a1(3, 702), "AAA3");
    }

    #[test]
    fn generation_is_deterministic() {
        for name in NAMES {
            let one = by_name(name, 5, 1).unwrap();
            let two = by_name(name, 5, 1).unwrap();
            assert_eq!(one.book.sheets[0].cells, two.book.sheets[0].cells, "{name}");
            for (a, b) in one.steps.iter().zip(&two.steps) {
                assert_eq!((a.at, a.value, &a.probes), (b.at, b.value, &b.probes));
            }
        }
    }

    #[test]
    fn every_edit_changes_the_value_it_replaces_and_what_is_probed() {
        for name in NAMES {
            let workload = by_name(name, 40, 1).unwrap();
            let mut last = workload.initial.clone();
            let mut changed = 0;
            for step in &workload.steps {
                assert!(!step.probes.is_empty());
                changed += usize::from(step.probes != last);
                last = step.probes.clone();
            }
            assert!(
                changed >= 39,
                "{name}: only {changed} of 40 edits show in the probes"
            );
        }
    }

    /// The claim the comparison rests on: no rounding anywhere.
    #[test]
    fn every_number_is_a_multiple_of_one_thirty_second() {
        for name in NAMES {
            let workload = by_name(name, 20, 1).unwrap();
            let exact = |value: f64| (value * 32.0).fract() == 0.0 && value.abs() < 1e12;
            for sheet in &workload.book.sheets {
                for cell in &sheet.cells {
                    if let Content::Number(value) = cell.content {
                        assert!(exact(value), "{name}: {value}");
                    }
                }
            }
            for step in &workload.steps {
                assert!(exact(step.value));
                for (_, expected) in &step.probes {
                    if let Expected::Number(value) = expected {
                        assert!(exact(*value), "{name}: {value}");
                    }
                }
            }
        }
    }
}
