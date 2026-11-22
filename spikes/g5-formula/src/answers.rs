//! Small formulas whose results in Excel are known, put through every engine.
//!
//! The gate asks for results that agree with Excel. There is no Excel on the
//! machine this was written on, so the expected values here are Excel's
//! documented behaviour, written down from knowledge and not taken from a run.
//! The cases were chosen to be ones where that behaviour is not in doubt, and
//! where engines tend to differ: operator precedence, rounding of decimal
//! halves, text that looks like numbers, the 1900 date system, CJK text.
//!
//! Each formula is evaluated in a workbook of its own, so that an engine that
//! cannot load one formula still answers the others.

use crate::engine::{Candidate, Source, Value};
use crate::workload::{At, Book, Cell, Content, Sheet};
use crate::xlsx;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Answer {
    Number(f64),
    Text(&'static str),
    Bool(bool),
    Error(&'static str),
}

pub struct Case {
    pub group: &'static str,
    pub formula: &'static str,
    pub answer: Answer,
}

use Answer::{Bool, Error, Number, Text};

const fn case(group: &'static str, formula: &'static str, answer: Answer) -> Case {
    Case {
        group,
        formula,
        answer,
    }
}

/// The data the formulas refer to: column A is 1, 2, the text "3", 4.5 and
/// "大阪"; column B is a word for each.
pub const CASES: &[Case] = &[
    case("operators", "1+2*3", Number(7.0)),
    case("operators", "-2^2", Number(4.0)),
    case("operators", "2^3^2", Number(64.0)),
    case("operators", "10%*200", Number(20.0)),
    case("operators", "\"10\"+5", Number(15.0)),
    case("operators", "\"a\"&1+1", Text("a2")),
    case("operators", "\"a\"=\"A\"", Bool(true)),
    case("operators", "0.1+0.2=0.3", Bool(true)),
    case("operators", "\"a\"+1", Error("#VALUE!")),
    case("operators", "1/0", Error("#DIV/0!")),
    case("rounding", "ROUND(2.5,0)", Number(3.0)),
    case("rounding", "ROUND(-2.5,0)", Number(-3.0)),
    case("rounding", "ROUND(1.005,2)", Number(1.01)),
    case("rounding", "ROUNDUP(1.01,1)", Number(1.1)),
    case("rounding", "ROUNDDOWN(-1.19,1)", Number(-1.1)),
    case("rounding", "INT(-1.5)", Number(-2.0)),
    case("rounding", "MOD(-1,3)", Number(2.0)),
    case("rounding", "CEILING(2.1,1)", Number(3.0)),
    case("rounding", "FLOOR(-2.1,1)", Number(-3.0)),
    case("aggregates", "SUM(A1:A5)", Number(7.5)),
    case("aggregates", "COUNT(A1:A5)", Number(3.0)),
    case("aggregates", "COUNTA(A1:A5)", Number(5.0)),
    case("aggregates", "AVERAGE(A1:A5)", Number(2.5)),
    case("aggregates", "MAX(A1:A5)", Number(4.5)),
    case("aggregates", "SUM(1,\"2\",TRUE)", Number(4.0)),
    case("aggregates", "SUMIF(A1:A5,\">1\")", Number(6.5)),
    case("aggregates", "COUNTIF(A1:A5,\"大*\")", Number(1.0)),
    case("aggregates", "SUMPRODUCT(A1:A2,A1:A2)", Number(5.0)),
    case("aggregates", "SUM({1,2;3,4})", Number(10.0)),
    case("lookup", "INDEX(A1:A5,MATCH(4.5,A1:A5,0))", Number(4.5)),
    case("lookup", "VLOOKUP(2,A1:B5,2,FALSE)", Text("two")),
    case("lookup", "VLOOKUP(9,A1:B5,2,FALSE)", Error("#N/A")),
    case("lookup", "XLOOKUP(4.5,A1:A5,B1:B5)", Text("four")),
    case("lookup", "MATCH(\"b\",{\"a\",\"b\",\"c\"},0)", Number(2.0)),
    case("logic", "IF(1,\"y\",\"n\")", Text("y")),
    case("logic", "IFERROR(1/0,\"x\")", Text("x")),
    case("logic", "AND(TRUE,1)", Bool(true)),
    case("logic", "NOT(0)", Bool(true)),
    case("logic", "IFS(FALSE,1,TRUE,2)", Number(2.0)),
    case("logic", "SWITCH(2,1,\"a\",2,\"b\")", Text("b")),
    case("text", "LEN(\"大阪\")", Number(2.0)),
    case("text", "LEN(A5)", Number(2.0)),
    case("text", "MID(\"東京都港区\",3,2)", Text("都港")),
    case("text", "LEFT(\"東京都\",2)", Text("東京")),
    case("text", "FIND(\"都\",\"東京都\")", Number(3.0)),
    case("text", "REPT(\"あ\",3)", Text("あああ")),
    case("text", "UPPER(\"abc\")", Text("ABC")),
    case("text", "PROPER(\"hello world\")", Text("Hello World")),
    case("text", "TRIM(\"  a  b \")", Text("a b")),
    case("text", "SUBSTITUTE(\"a-b-c\",\"-\",\"+\")", Text("a+b+c")),
    case("text", "CONCAT(\"a\",\"b\")", Text("ab")),
    case("text", "TEXTJOIN(\"-\",TRUE,\"a\",\"\",\"b\")", Text("a-b")),
    case("text", "VALUE(\"1,234\")", Number(1234.0)),
    case("text", "TEXT(1234.5,\"#,##0.00\")", Text("1,234.50")),
    case("text", "TEXT(0.125,\"0.0%\")", Text("12.5%")),
    case("dates", "DATE(2026,11,22)", Number(46348.0)),
    case("dates", "DATE(1900,3,1)", Number(61.0)),
    case("dates", "YEAR(46348)", Number(2026.0)),
    case("dates", "MONTH(46348)", Number(11.0)),
    case("dates", "DAY(46348)", Number(22.0)),
    case("dates", "WEEKDAY(46348)", Number(1.0)),
    case("dates", "EDATE(46053,1)", Number(46081.0)),
    case("dates", "DATEDIF(46053,46348,\"d\")", Number(295.0)),
    case("dates", "TEXT(46348,\"yyyy-mm-dd\")", Text("2026-11-22")),
    case("newer functions", "SUM(SEQUENCE(5))", Number(15.0)),
    case("newer functions", "LET(x,2,x*3)", Number(6.0)),
    case("newer functions", "COUNTA(UNIQUE({1;1;2}))", Number(2.0)),
    case(
        "newer functions",
        "SUM(FILTER({1;2;3},{TRUE;FALSE;TRUE}))",
        Number(4.0),
    ),
    case("newer functions", "LAMBDA(x,x+1)(2)", Number(3.0)),
];

/// Where the formula sits in each workbook.
const TARGET: At = At {
    sheet: 0,
    row: 1,
    col: 3,
};

pub fn book(case: &Case) -> Book {
    let number = |row, value| Cell {
        row,
        col: 0,
        content: Content::Number(value),
    };
    let text = |row, col, value: &str| Cell {
        row,
        col,
        content: Content::Text(value.to_owned()),
    };
    let formula = case.formula.to_owned();
    let content = match case.answer {
        Answer::Text(_) => Content::TextFormula(formula),
        _ => Content::Formula(formula),
    };
    Book {
        sheets: vec![Sheet {
            name: "Sheet1",
            cells: vec![
                number(1, 1.0),
                text(1, 1, "one"),
                Cell {
                    row: TARGET.row,
                    col: TARGET.col,
                    content,
                },
                number(2, 2.0),
                text(2, 1, "two"),
                text(3, 0, "3"),
                text(3, 1, "three"),
                number(4, 4.5),
                text(4, 1, "four"),
                text(5, 0, "大阪"),
                text(5, 1, "五"),
            ],
        }],
    }
}

fn agrees(answer: Answer, value: &Value) -> bool {
    match (answer, value) {
        // Not bit for bit here: 1.01 has no exact form and engines may reach
        // its neighbour by another route.
        (Answer::Number(want), Value::Number(have)) => {
            (want - have).abs() <= 1e-9 * want.abs().max(1.0)
        }
        (Answer::Text(want), Value::Text(have)) => want == have,
        (Answer::Bool(want), Value::Bool(have)) => want == *have,
        (Answer::Error(want), Value::Error(have)) => want == have,
        _ => false,
    }
}

/// What the engine made of the case, when it is not the expected answer.
pub fn check(candidate: &Candidate, case: &Case) -> Result<(), String> {
    let bytes = xlsx::write(&book(case));
    let source = Source {
        workload: "answers",
        scale: 1,
        sheets: vec!["Sheet1"],
        xlsx: &bytes,
    };
    // An engine may reject, or panic on, a function it does not know.
    let outcome = std::panic::catch_unwind(|| -> Result<Value, String> {
        let mut engine = (candidate.open)(&source).map_err(|why| format!("cannot load: {why}"))?;
        engine.calculate_all()?;
        engine.value(TARGET)
    });
    let value = outcome.map_err(|_| "panicked".to_owned())??;
    if agrees(case.answer, &value) {
        Ok(())
    } else {
        Err(match value {
            Value::Number(number) => number.to_string(),
            Value::Text(text) => format!("\"{text}\""),
            Value::Bool(flag) => flag.to_string().to_uppercase(),
            Value::Error(code) => code,
            Value::Empty => "(empty)".to_owned(),
            Value::Other(other) => other,
        })
    }
}

/// One line per case with each engine's answer where it is wrong, then how
/// many each engine got right.
pub fn table(candidates: &[Candidate]) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    let mut right = vec![0_usize; candidates.len()];
    write!(text, "{:16} {:42} {:12}", "group", "formula", "Excel").unwrap();
    for candidate in candidates {
        write!(text, " {:18}", candidate.name).unwrap();
    }
    text.push('\n');
    for case in CASES {
        let expected = match case.answer {
            Answer::Number(number) => number.to_string(),
            Answer::Text(value) => format!("\"{value}\""),
            Answer::Bool(flag) => flag.to_string().to_uppercase(),
            Answer::Error(code) => code.to_owned(),
        };
        write!(
            text,
            "{:16} {:42} {:12}",
            case.group, case.formula, expected
        )
        .unwrap();
        for (index, candidate) in candidates.iter().enumerate() {
            let cell = match check(candidate, case) {
                Ok(()) => {
                    right[index] += 1;
                    "ok".to_owned()
                }
                Err(got) => {
                    let short: String = got.chars().take(16).collect();
                    format!("✗ {short}")
                }
            };
            write!(text, " {cell:18}").unwrap();
        }
        text.push('\n');
    }
    write!(
        text,
        "{:16} {:42} {:12}",
        "",
        "agree with Excel",
        CASES.len()
    )
    .unwrap();
    for count in right {
        write!(text, " {count:<18}").unwrap();
    }
    text.push('\n');
    text
}

/// The engines that can evaluate arbitrary formulas: all but the hand-written
/// baseline, and one of each pair that differs only in threading or wrapping.
pub fn engines() -> Vec<Candidate> {
    crate::engine::all()
        .into_iter()
        .filter(|candidate| {
            !matches!(
                candidate.name,
                "by hand" | "formualizer/1" | "betteroffice/calc"
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comparison_tells_kinds_apart() {
        assert!(agrees(Number(1.01), &Value::Number(1.01)));
        assert!(agrees(Number(1.01), &Value::Number(1.0100000000000002)));
        assert!(!agrees(Number(1.01), &Value::Number(1.0)));
        assert!(!agrees(Number(3.0), &Value::Text("3".into())));
        assert!(agrees(Bool(true), &Value::Bool(true)));
        assert!(!agrees(Bool(true), &Value::Number(1.0)));
        assert!(agrees(Error("#N/A"), &Value::Error("#N/A".into())));
        assert!(!agrees(Error("#N/A"), &Value::Text("#N/A".into())));
    }

    #[test]
    fn each_case_is_a_workbook_with_the_data_and_one_formula() {
        for case in CASES {
            let book = book(case);
            assert_eq!(book.formula_count(), 1, "{}", case.formula);
            let cells = &book.sheets[0].cells;
            for pair in cells.windows(2) {
                assert!((pair[0].row, pair[0].col) < (pair[1].row, pair[1].col));
            }
        }
    }

    /// The recorded answers of the engines. When an engine is upgraded and
    /// this fails, look at what changed before regenerating the file:
    /// `cargo run --release -- answers > golden/answers.txt`.
    #[cfg(all(
        feature = "ironcalc",
        feature = "formualizer",
        feature = "betteroffice"
    ))]
    #[test]
    fn the_engines_answer_as_recorded() {
        let now = table(&engines());
        let recorded = include_str!("../golden/answers.txt");
        assert!(
            now == recorded,
            "answers differ from golden/answers.txt:\n{now}"
        );
    }
}
