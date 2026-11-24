//! The assistant's and MCP clients' tools on workbooks: reading cells and
//! typing into them. Like the document tools in `assistant`, these run on the
//! interface thread on a tab the policy has already let the caller act on;
//! they only know cells.
//!
//! Cells are read as the grid shows them (formatted), with formulas listed
//! apart. Writing goes through the same path as typing into a cell, so
//! numbers, dates, percentages and `=formulas` are read the way the user's
//! own typing is, and the whole write is one undo step.

use aster_sheets::{CellRange, CellRef, Change, column_name};
use serde_json::{Map, Value, json};

use crate::office::SheetTab;

/// At most this many cells are read in one call.
pub const MAX_READ: usize = 10_000;
/// At most this many cells are written in one call.
pub const MAX_WRITE: usize = 10_000;
/// Without a range, at most this many rows and columns of the used range.
const DEFAULT_ROWS: u32 = 200;
const DEFAULT_COLUMNS: u32 = 26;

/// Run a workbook tool. Returns the result for the caller and the cells that
/// changed (empty for reads), which the shell recalculates.
pub fn run(
    tab: &mut SheetTab,
    tool: &str,
    args: &Value,
    language: &str,
) -> Result<(String, Vec<Change>), String> {
    match tool {
        "read_sheet" => read(tab, args).map(|text| (text, Vec::new())),
        "get_selection" => Ok((selection(tab), Vec::new())),
        "write_cells" => write(tab, args, language),
        other => Err(format!(
            "`{other}` works on Markdown documents; on a spreadsheet use read_sheet and write_cells"
        )),
    }
}

/// The sheet a call names, by name (any case), or the one in front.
fn sheet_index(tab: &SheetTab, args: &Value) -> Result<usize, String> {
    let workbook = tab.view.workbook();
    match args["sheet"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => Ok(tab.view.sheet_index()),
        Some(name) => workbook
            .sheets
            .iter()
            .position(|sheet| sheet.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let names: Vec<&str> = workbook.sheets.iter().map(|s| s.name.as_str()).collect();
                format!("no sheet named {name:?}; the sheets are {names:?}")
            }),
    }
}

fn sheet_names(tab: &SheetTab) -> Vec<String> {
    tab.view
        .workbook()
        .sheets
        .iter()
        .map(|sheet| sheet.name.clone())
        .collect()
}

/// `A1:C3`, `b2`, `$A$1:$B$2`: a range, with `$` and case ignored.
fn parse_range(text: &str) -> Option<CellRange> {
    let cleaned: String = text
        .trim()
        .chars()
        .filter(|c| *c != '$')
        .collect::<String>()
        .to_ascii_uppercase();
    CellRange::parse(&cleaned).or_else(|| CellRef::parse(&cleaned).map(CellRange::single))
}

fn read(tab: &SheetTab, args: &Value) -> Result<String, String> {
    let index = sheet_index(tab, args)?;
    let workbook = tab.view.workbook();
    let sheet = &workbook.sheets[index];
    let used = sheet.used_range();
    let asked =
        match args["range"].as_str().filter(|r| !r.trim().is_empty()) {
            Some(text) => Some(parse_range(text).ok_or_else(|| {
                format!("{text:?} is not a cell range; write it like A1:D20 or B3")
            })?),
            None => None,
        };
    let mut result = Map::new();
    result.insert("sheet".into(), json!(sheet.name));
    result.insert("sheets".into(), json!(sheet_names(tab)));
    result.insert(
        "used_range".into(),
        used.map_or(Value::Null, |range| json!(range.to_string())),
    );
    let Some(mut range) = asked.or_else(|| {
        used.map(|used| {
            CellRange::new(
                used.first,
                CellRef::new(
                    used.last.row.min(used.first.row + DEFAULT_ROWS - 1),
                    used.last.col.min(used.first.col + DEFAULT_COLUMNS - 1),
                ),
            )
        })
    }) else {
        result.insert("rows".into(), json!([]));
        result.insert("note".into(), json!("the sheet is empty"));
        return Ok(Value::Object(result).to_string());
    };
    // Keep a read to its budget by cutting rows off the bottom.
    let columns = range.cols() as usize;
    let max_rows = (MAX_READ / columns.max(1)).max(1);
    let truncated = range.rows() as usize > max_rows;
    if truncated {
        range = CellRange::new(
            range.first,
            CellRef::new(range.first.row + max_rows as u32 - 1, range.last.col),
        );
    }
    let mut rows = Vec::with_capacity(range.rows() as usize);
    let mut formulas = Map::new();
    for row in range.first.row..=range.last.row {
        let mut values = Vec::with_capacity(columns);
        for col in range.first.col..=range.last.col {
            let at = CellRef::new(row, col);
            let cell = sheet.cell(at);
            values.push(
                cell.and_then(|cell| workbook.display(cell))
                    .map_or_else(String::new, |display| display.text),
            );
            if let Some(formula) = cell.and_then(|cell| cell.formula.as_ref()) {
                formulas.insert(at.to_string(), json!(format!("={formula}")));
            }
        }
        // Trailing empty cells say nothing.
        while values.last().is_some_and(String::is_empty) {
            values.pop();
        }
        rows.push(values);
    }
    result.insert("range".into(), json!(range.to_string()));
    result.insert(
        "columns".into(),
        json!(
            (range.first.col..=range.last.col)
                .map(column_name)
                .collect::<Vec<_>>()
        ),
    );
    result.insert("first_row".into(), json!(range.first.row + 1));
    result.insert(
        "rows".into(),
        Value::Array(rows.into_iter().map(Value::from).collect()),
    );
    if !formulas.is_empty() {
        result.insert("formulas".into(), Value::Object(formulas));
    }
    if truncated {
        result.insert(
            "note".into(),
            json!(format!(
                "only the first {max_rows} rows of the range were read; read the rest with another range"
            )),
        );
    }
    Ok(Value::Object(result).to_string())
}

fn selection(tab: &SheetTab) -> String {
    let range = tab.view.selection();
    let name = tab
        .view
        .sheet()
        .map_or_else(String::new, |sheet| sheet.name.clone());
    json!({
        "sheet": name,
        "selection": range.to_string(),
        "active_cell": tab.view.selected().to_string(),
    })
    .to_string()
}

/// The entries a write call carries: `rows`, an array of arrays of strings
/// (numbers and booleans are taken as typed).
pub fn entries(args: &Value) -> Result<Vec<Vec<String>>, String> {
    let rows = args["rows"]
        .as_array()
        .ok_or("rows must be an array of rows, each an array of cell entries")?;
    let mut out = Vec::with_capacity(rows.len());
    let mut count = 0usize;
    for row in rows {
        let row = row
            .as_array()
            .ok_or("each row must be an array of cell entries")?;
        let mut cells = Vec::with_capacity(row.len());
        for value in row {
            cells.push(match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_owned(),
                Value::Null => String::new(),
                _ => return Err("a cell entry must be a string, number or boolean".to_owned()),
            });
        }
        count += cells.len();
        out.push(cells);
    }
    if count > MAX_WRITE {
        return Err(format!("at most {MAX_WRITE} cells can be written at once"));
    }
    Ok(out)
}

/// Where a write call puts its entries: from `start` over as many rows and
/// columns as the entries take.
pub fn target(args: &Value, rows: &[Vec<String>]) -> Result<CellRange, String> {
    let start_text = args["start"].as_str().unwrap_or_default();
    let start = parse_range(start_text)
        .map(|range| range.first)
        .ok_or_else(|| format!("{start_text:?} is not a cell address like B2"))?;
    // The range is walked cell by cell on the interface thread (to show and
    // fingerprint what it holds), so its area is bounded, not only the
    // entries: empty rows count as no entries but still stretch it.
    let height = rows.len().max(1);
    let width = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    if height.saturating_mul(width) > MAX_WRITE {
        return Err(format!(
            "at most {MAX_WRITE} cells can be written at once; these rows span {height} × {width}"
        ));
    }
    let (height, width) = (height as u32, width as u32);
    Ok(CellRange::new(
        start,
        CellRef::new(
            (start.row + height - 1).min(aster_sheets::MAX_ROWS - 1),
            (start.col + width - 1).min(aster_sheets::MAX_COLUMNS - 1),
        ),
    ))
}

/// What a write call would replace, for the user deciding on it: the
/// sheet's name, the cells it covers, and what they hold now as
/// tab-separated rows (as typed: formulas with their `=`), empty when they
/// are all empty. `None` when the call's arguments are not usable.
pub fn before_write(tab: &SheetTab, args: &Value) -> Option<(String, CellRange, String)> {
    let index = sheet_index(tab, args).ok()?;
    let rows = entries(args).ok()?;
    let range = target(args, &rows).ok()?;
    let workbook = tab.view.workbook();
    let sheet = workbook.sheets.get(index)?;
    let mut lines = Vec::new();
    for row in range.first.row..=range.last.row {
        let fields: Vec<String> = (range.first.col..=range.last.col)
            .map(|col| {
                sheet
                    .cell(CellRef::new(row, col))
                    .map_or_else(String::new, |cell| workbook.input_text(cell))
            })
            .collect();
        lines.push(fields.join("\t"));
    }
    while lines
        .last()
        .is_some_and(|line| line.trim_matches('\t').is_empty())
    {
        lines.pop();
    }
    Some((sheet.name.clone(), range, lines.join("\n")))
}

/// Entries as tab-separated rows, for showing.
pub fn as_rows(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What an approval of a write was given against: the cells it would
/// replace, as they are. The write runs only while this is unchanged.
pub fn fingerprint(tab: &SheetTab, args: &Value) -> String {
    match before_write(tab, args) {
        Some((sheet, range, now)) => format!("{sheet}!{range}:{now}"),
        None => String::new(),
    }
}

fn write(
    tab: &mut SheetTab,
    args: &Value,
    language: &str,
) -> Result<(String, Vec<Change>), String> {
    if tab.view.is_editing() {
        return Err("the user is typing into a cell; try again in a moment".to_owned());
    }
    let index = sheet_index(tab, args)?;
    let rows = entries(args)?;
    let range = target(args, &rows)?;
    if index != tab.view.sheet_index() {
        tab.view.set_sheet(index);
    }
    let changes = tab.type_cells(range.first, &rows, language);
    tab.view.select(range.first);
    tab.view.extend_to(range.last);
    let name = tab
        .view
        .sheet()
        .map_or_else(String::new, |sheet| sheet.name.clone());
    Ok((
        json!({
            "sheet": name,
            "range": range.to_string(),
            "changed_cells": changes.len(),
        })
        .to_string(),
        changes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ThemeMode, Tokens};
    use std::sync::Arc;

    fn tab() -> SheetTab {
        let source = crate::office::blank_workbook("en-US");
        let workbook = aster_ooxml::xlsx::read(source.clone()).unwrap();
        SheetTab::new(
            workbook,
            Arc::new(source),
            None,
            &Tokens::of(ThemeMode::Light),
        )
    }

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn empty_rows_cannot_stretch_a_write_past_the_limit() {
        // One full row and many empty ones: few entries, a huge range.
        let mut rows = vec![Value::Array(vec![Value::from("x"); 100])];
        rows.extend(std::iter::repeat_n(Value::Array(Vec::new()), 200));
        let args = serde_json::json!({"start": "A1", "rows": rows});
        let wide = entries(&args).unwrap();
        assert!(target(&args, &wide).is_err());
        assert!(before_write(&tab(), &args).is_none());

        let args = json(r#"{"start": "B2", "rows": [["1", "2"], [], ["3"]]}"#);
        let small = entries(&args).unwrap();
        assert!(target(&args, &small).is_ok());
    }

    #[test]
    fn written_cells_read_back_with_their_formulas() {
        let mut tab = tab();
        let (result, changes) = run(
            &mut tab,
            "write_cells",
            &json!({
                "start": "b2",
                "rows": [["Item", "Price"], ["Tea", 3.5], ["Cake", "4"], ["Total", "=SUM(C3:C4)"]]
            }),
            "en-US",
        )
        .unwrap();
        assert_eq!(json(&result)["range"], "B2:C5");
        assert_eq!(changes.len(), 8);
        assert!(tab.is_modified());
        // The formula has no result until the calculation thread runs.
        let read = json(
            &run(
                &mut tab,
                "read_sheet",
                &json!({ "range": "A1:C5" }),
                "en-US",
            )
            .unwrap()
            .0,
        );
        assert_eq!(read["rows"][1], json!(["", "Item", "Price"]));
        assert_eq!(read["rows"][2], json!(["", "Tea", "3.5"]));
        assert_eq!(read["formulas"]["C5"], "=SUM(C3:C4)");
        assert_eq!(read["first_row"], 1);
        // The written range is selected, and one undo takes all of it back.
        assert_eq!(tab.view.selection().to_string(), "B2:C5");
        assert!(!tab.undo().is_empty());
        let read = json(&run(&mut tab, "read_sheet", &json!({}), "en-US").unwrap().0);
        assert_eq!(read["rows"], json!([]));
    }

    #[test]
    fn reads_default_to_the_used_range_and_keep_to_a_budget() {
        let mut tab = tab();
        let rows: Vec<Vec<String>> = (0..20_001).map(|i| vec![i.to_string()]).collect();
        let too_many = run(
            &mut tab,
            "write_cells",
            &json!({ "start": "A1", "rows": rows }),
            "en-US",
        );
        assert!(too_many.is_err());
        let rows: Vec<Vec<String>> = (0..300).map(|i| vec![i.to_string(), "x".into()]).collect();
        run(
            &mut tab,
            "write_cells",
            &json!({ "start": "A1", "rows": rows }),
            "en-US",
        )
        .unwrap();
        let read = json(&run(&mut tab, "read_sheet", &json!({}), "en-US").unwrap().0);
        assert_eq!(read["used_range"], "A1:B300");
        // Without a range, the first 200 rows of the used range.
        assert_eq!(read["range"], "A1:B200");
        let big = json(
            &run(
                &mut tab,
                "read_sheet",
                &json!({ "range": "A1:Z1000" }),
                "en-US",
            )
            .unwrap()
            .0,
        );
        assert_eq!(big["range"], "A1:Z384");
        assert!(big["note"].as_str().unwrap().contains("384"));
    }

    #[test]
    fn bad_arguments_are_explained() {
        let err = |args: Value, tool: &str| run(&mut tab(), tool, &args, "en-US").unwrap_err();
        assert!(err(json!({ "range": "nonsense" }), "read_sheet").contains("A1:D20"));
        assert!(err(json!({ "sheet": "Nope" }), "read_sheet").contains("the sheets are"));
        assert!(err(json!({ "start": "1A", "rows": [["x"]] }), "write_cells").contains("B2"));
        assert!(err(json!({ "start": "A1", "rows": "x" }), "write_cells").contains("array"));
        assert!(err(json!({ "start": "A1", "rows": [[{}]]}), "write_cells").contains("string"));
        assert!(err(json!({}), "replace_lines").contains("read_sheet"));
        let selection = json(
            &run(&mut tab(), "get_selection", &json!({}), "en-US")
                .unwrap()
                .0,
        );
        assert_eq!(selection["selection"], "A1");
    }

    #[test]
    fn a_write_shows_what_it_replaces_and_notices_changes() {
        let mut tab = tab();
        let args = json!({ "start": "A1", "rows": [["new", "=1+1"]] });
        let (sheet, range, now) = before_write(&tab, &args).unwrap();
        assert_eq!(
            (sheet.as_str(), range.to_string().as_str(), now.as_str()),
            ("Sheet1", "A1:B1", "")
        );
        let before = fingerprint(&tab, &args);
        run(
            &mut tab,
            "write_cells",
            &json!({ "start": "B1", "rows": [["=2*3"]] }),
            "en-US",
        )
        .unwrap();
        assert_ne!(fingerprint(&tab, &args), before);
        assert_eq!(before_write(&tab, &args).unwrap().2, "\t=2*3");
        assert_eq!(as_rows(&entries(&args).unwrap()), "new\t=1+1");
    }

    #[test]
    fn ranges_are_read_loosely() {
        assert_eq!(parse_range("$a$1:$b$2").unwrap().to_string(), "A1:B2");
        assert_eq!(parse_range(" c7 ").unwrap().to_string(), "C7");
        assert!(parse_range("A0").is_none());
    }
}
