//! Sheets added, renamed and deleted, with the formulas that name them.

use crate::address::CellRef;
use crate::model::{Sheet, SheetKind, Workbook};
use crate::structure::is_name_byte;

/// Why a name cannot be a sheet's, as Excel checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// More than 31 characters.
    TooLong,
    /// One of `: \ / ? * [ ]`, or `'` at either end.
    Character(char),
    /// Another sheet has it (case does not count).
    Taken,
    /// `History`, which Excel keeps for itself.
    Reserved,
}

/// The longest sheet name Excel takes.
pub const MAX_NAME: usize = 31;

/// Whether `name` must be quoted in a formula: anything but letters,
/// digits, `_` and `.` (any non-ASCII character counts as a letter, as in
/// Excel), a leading digit, or a name that reads as a cell or a range end.
pub fn needs_quotes(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.is_empty()
        || !bytes.iter().all(|&b| is_name_byte(b))
        || bytes[0].is_ascii_digit()
        || looks_like_reference(name)
        || name.eq_ignore_ascii_case("TRUE")
        || name.eq_ignore_ascii_case("FALSE")
}

fn looks_like_reference(name: &str) -> bool {
    // A1 style: up to three letters then digits.
    let letters = name.bytes().take_while(u8::is_ascii_alphabetic).count();
    let digits = &name[letters..];
    let a1 = (1..=3).contains(&letters)
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit());
    // R1C1 style.
    let upper = name.to_ascii_uppercase();
    let r1c1 = upper.starts_with('R')
        && upper[1..].split('C').count() <= 2
        && upper[1..].bytes().all(|b| b.is_ascii_digit() || b == b'C');
    a1 || r1c1
}

/// `name` as a formula writes it before `!`.
pub fn qualifier(name: &str) -> String {
    if needs_quotes(name) {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_owned()
    }
}

/// `formula` with references to sheet `from` going to `to` instead, or,
/// with `to` `None`, to a sheet that is gone: then each such reference is
/// `#REF!`. Unqualified references and other workbooks' sheets stay.
pub fn retarget(formula: &str, from: &str, to: Option<&str>) -> String {
    let bytes = formula.as_bytes();
    let mut out = String::with_capacity(formula.len() + 8);
    let mut i = 0;
    let same = |name: &str| name.eq_ignore_ascii_case(from);
    // After a qualifier of `from` when deleting: skip the reference itself.
    let skip_reference = |i: &mut usize| {
        let take = |i: &mut usize| {
            while *i < bytes.len() && (is_name_byte(bytes[*i]) || bytes[*i] == b'$') {
                *i += 1;
            }
        };
        take(i);
        if bytes.get(*i) == Some(&b':') {
            *i += 1;
            take(i);
        }
    };
    while i < bytes.len() {
        let b = bytes[i];
        let external = i > 0 && bytes[i - 1] == b']';
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&formula[start..i]);
            continue;
        }
        if b == b'[' {
            let end = formula[i..].find(']').map_or(bytes.len(), |at| i + at + 1);
            out.push_str(&formula[i..end]);
            i = end;
            continue;
        }
        if b == b'\'' {
            let start = i;
            i += 1;
            let mut name = String::new();
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        name.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let len = formula[i..].chars().next().map_or(1, char::len_utf8);
                name.push_str(&formula[i..i + len]);
                i += len;
            }
            if bytes.get(i) == Some(&b'!') && !external && same(&name) {
                i += 1;
                match to {
                    Some(to) => {
                        out.push_str(&qualifier(to));
                        out.push('!');
                    }
                    None => {
                        skip_reference(&mut i);
                        out.push_str("#REF!");
                    }
                }
            } else {
                out.push_str(&formula[start..i]);
            }
            continue;
        }
        if is_name_byte(b) {
            let start = i;
            let preceded = start > 0 && is_name_byte(bytes[start - 1]);
            let mut end = i;
            while end < bytes.len() && is_name_byte(bytes[end]) {
                end += 1;
            }
            let word = &formula[start..end];
            i = end;
            if bytes.get(end) == Some(&b'!') && !preceded && !external && same(word) {
                i += 1;
                match to {
                    Some(to) => {
                        out.push_str(&qualifier(to));
                        out.push('!');
                    }
                    None => {
                        skip_reference(&mut i);
                        out.push_str("#REF!");
                    }
                }
            } else {
                out.push_str(word);
            }
            continue;
        }
        let len = formula[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&formula[i..i + len]);
        i += len;
    }
    out
}

impl Workbook {
    /// Whether `name` can be the name of sheet `except` (or of a new sheet
    /// with `None`).
    pub fn check_sheet_name(&self, name: &str, except: Option<usize>) -> Result<(), NameProblem> {
        if name.is_empty() {
            return Err(NameProblem::Empty);
        }
        if name.chars().count() > MAX_NAME {
            return Err(NameProblem::TooLong);
        }
        if let Some(c) = name.chars().find(|c| ":\\/?*[]".contains(*c)) {
            return Err(NameProblem::Character(c));
        }
        if name.starts_with('\'') || name.ends_with('\'') {
            return Err(NameProblem::Character('\''));
        }
        if name.eq_ignore_ascii_case("History") {
            return Err(NameProblem::Reserved);
        }
        let taken = self
            .sheets
            .iter()
            .enumerate()
            .any(|(i, s)| Some(i) != except && s.name.to_lowercase() == name.to_lowercase());
        if taken {
            return Err(NameProblem::Taken);
        }
        Ok(())
    }

    /// `base` and the first number from the sheet count up that no sheet
    /// has: `Sheet4` in a workbook of three.
    pub fn new_sheet_name(&self, base: &str) -> String {
        (self.sheets.len() + 1..)
            .map(|n| format!("{base}{n}"))
            .find(|name| self.check_sheet_name(name, None).is_ok())
            .unwrap_or_else(|| base.to_owned())
    }

    /// Add an empty worksheet at the end, sized as the first worksheet is.
    /// Returns its index.
    pub fn add_sheet(&mut self, name: &str) -> usize {
        let mut sheet = Sheet::empty(name, SheetKind::Worksheet);
        if let Some(first) = self.sheets.iter().find(|s| s.kind == SheetKind::Worksheet) {
            sheet.columns =
                crate::axis::Axis::new(first.columns.default_size(), first.columns.count());
            sheet.row_sizes =
                crate::axis::Axis::new(first.row_sizes.default_size(), first.row_sizes.count());
        }
        self.sheets.push(sheet);
        self.sheets.len() - 1
    }

    /// The name Excel gives a copy of sheet `index`: `Sheet1 (2)`, the
    /// first number from 2 up that no sheet has, cut to fit.
    pub fn copy_sheet_name(&self, index: usize) -> Option<String> {
        let base = self.sheets.get(index)?.name.clone();
        (2..).find_map(|n| {
            let suffix = format!(" ({n})");
            let room = MAX_NAME.saturating_sub(suffix.chars().count());
            let name: String = base.chars().take(room).chain(suffix.chars()).collect();
            self.check_sheet_name(&name, None).ok().map(|()| name)
        })
    }

    /// Put a copy of worksheet `from` into place `to` (the copy's index)
    /// under `name`. Its cells, formats, merged areas, sizes, frozen panes,
    /// filter, notes and list rules are copied; formulas in it that name the
    /// original sheet name the copy. Pictures and charts are not copied.
    /// Returns the copy's index, or `None` for a sheet that is not a
    /// worksheet or a name that is not allowed.
    pub fn copy_sheet(&mut self, from: usize, to: usize, name: &str) -> Option<usize> {
        if self.check_sheet_name(name, None).is_err() {
            return None;
        }
        let source = self.sheets.get(from)?;
        if source.kind != SheetKind::Worksheet {
            return None;
        }
        let old = source.name.clone();
        let mut copy = source.clone();
        copy.name = name.to_owned();
        copy.hidden = false;
        copy.pictures.clear();
        copy.structure_blocker = None;
        copy.delete_blocker = None;
        let mut touched = false;
        for row in copy.rows_mut().iter_mut() {
            for cell in &mut row.cells {
                let Some(formula) = &cell.formula else {
                    continue;
                };
                let moved = retarget(formula, &old, Some(name));
                if moved != **formula {
                    cell.formula = Some(moved.into());
                    touched = true;
                }
            }
        }
        for rule in &mut copy.validations {
            if let crate::validation::ListSource::Formula(formula) = &mut rule.source {
                *formula = retarget(formula, &old, Some(name));
            }
        }
        if touched {
            copy.refresh();
        }
        let to = to.min(self.sheets.len());
        self.sheets.insert(to, copy);
        for name in &mut self.names {
            if let Some(local) = &mut name.local
                && *local >= to
            {
                *local += 1;
            }
        }
        if self.active_sheet >= to {
            self.active_sheet += 1;
        }
        Some(to)
    }

    /// Give sheet `index` a new name; formulas that name it follow. Returns
    /// the cells whose formulas changed.
    pub fn rename_sheet(&mut self, index: usize, name: &str) -> Vec<(usize, CellRef)> {
        let Some(old) = self.sheets.get(index).map(|s| s.name.clone()) else {
            return Vec::new();
        };
        let changed = self.retarget_all(&old, Some(name));
        self.sheets[index].name = name.to_owned();
        changed
    }

    /// Take sheet `index` away; references to it become `#REF!`. Returns
    /// the cells whose formulas changed, by their sheets' new indices. The
    /// active sheet stays the one it was, or the one after a deleted one.
    pub fn delete_sheet(&mut self, index: usize) -> Vec<(usize, CellRef)> {
        if index >= self.sheets.len() {
            return Vec::new();
        }
        let sheet = self.sheets.remove(index);
        if self.active_sheet > index || self.active_sheet >= self.sheets.len() {
            self.active_sheet = self.active_sheet.saturating_sub(1);
        }
        // Its own names go with it; the others' sheet numbers move up.
        self.names.retain(|name| name.local != Some(index));
        for name in &mut self.names {
            if let Some(local) = &mut name.local
                && *local > index
            {
                *local -= 1;
            }
        }
        self.retarget_all(&sheet.name, None)
    }

    /// Move sheet `from` to place `to` among the sheets. Formulas name
    /// sheets, so they stay as they are; names that belong to a sheet keep
    /// to it.
    pub fn move_sheet(&mut self, from: usize, to: usize) {
        if from >= self.sheets.len() || to >= self.sheets.len() || from == to {
            return;
        }
        let sheet = self.sheets.remove(from);
        self.sheets.insert(to, sheet);
        let place = |i: usize| moved_index(i, from, to);
        for name in &mut self.names {
            if let Some(local) = &mut name.local {
                *local = place(*local);
            }
        }
        self.active_sheet = place(self.active_sheet);
    }

    fn retarget_all(&mut self, from: &str, to: Option<&str>) -> Vec<(usize, CellRef)> {
        for name in &mut self.names {
            name.formula = retarget(&name.formula, from, to);
        }
        let mut changed = Vec::new();
        for (index, sheet) in self.sheets.iter_mut().enumerate() {
            let mut touched = false;
            for row in sheet.rows_mut().iter_mut() {
                for cell in &mut row.cells {
                    let Some(formula) = &cell.formula else {
                        continue;
                    };
                    let moved = retarget(formula, from, to);
                    if moved != **formula {
                        cell.formula = Some(moved.into());
                        changed.push((index, CellRef::new(row.index, cell.col)));
                        touched = true;
                    }
                }
            }
            if touched {
                sheet.refresh();
            }
        }
        changed
    }
}

/// Where sheet `i` is after the sheet at `from` moved to `to`.
pub fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if to < from && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, SheetBuilder, Value};

    fn book() -> Workbook {
        let mut first = SheetBuilder::new("数据", SheetKind::Worksheet);
        first.push(0, Cell::new(0, Value::Number(1.0)));
        let mut second = SheetBuilder::new("Sheet2", SheetKind::Worksheet);
        let mut cell = Cell::new(0, Value::Number(2.0));
        cell.formula = Some("数据!A1*2+SUM('数据'!A1:B2)+\"数据!A1\"+[1]数据!A1".into());
        second.push(0, cell);
        Workbook {
            sheets: vec![first.finish(), second.finish()],
            ..Workbook::default()
        }
    }

    #[test]
    fn a_copy_names_itself_and_its_formulas_point_to_its_own_cells() {
        let mut book = book();
        // 数据 gets a formula on itself, one without a sheet name, a note.
        let mut own = Cell::new(1, Value::Number(2.0));
        own.formula = Some("数据!A1*2+A1".into());
        let mut sheet =
            std::mem::replace(&mut book.sheets[0], Sheet::empty("", SheetKind::Worksheet));
        let mut builder = SheetBuilder::new("数据", SheetKind::Worksheet);
        for row in sheet.rows() {
            for cell in &row.cells {
                builder.push(row.index, cell.clone());
            }
        }
        builder.push(0, own);
        builder.comments.insert(
            CellRef::new(0, 0),
            crate::comment::Comment::note("我", "备注"),
        );
        builder.columns.set(2, 3, 200.0);
        builder.merge(crate::address::CellRange::parse("D1:E2").unwrap());
        sheet = builder.finish();
        book.sheets[0] = sheet;
        book.names.push(crate::model::DefinedName {
            name: "Mine".into(),
            local: Some(1),
            formula: "Sheet2!$A$1".into(),
        });
        book.active_sheet = 1;
        assert_eq!(book.copy_sheet_name(0).as_deref(), Some("数据 (2)"));
        // Copied in front of Sheet2.
        let index = book.copy_sheet(0, 1, "数据 (2)").unwrap();
        assert_eq!(index, 1);
        let names: Vec<&str> = book.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["数据", "数据 (2)", "Sheet2"]);
        let copy = &book.sheets[1];
        let formula = |s: &Sheet, a: &str| {
            s.cell(CellRef::parse(a).unwrap())
                .and_then(|c| c.formula.clone())
                .map(|f| f.to_string())
        };
        assert_eq!(formula(copy, "B1").as_deref(), Some("'数据 (2)'!A1*2+A1"));
        assert_eq!(
            formula(&book.sheets[0], "B1").as_deref(),
            Some("数据!A1*2+A1")
        );
        assert_eq!(copy.comments.len(), 1);
        assert_eq!(copy.columns.size(2), 200.0);
        assert_eq!(copy.merges.len(), 1);
        // The other sheet still points to the original; names that belong to
        // a sheet keep to theirs, and so does the active sheet.
        assert!(
            formula(&book.sheets[2], "A1")
                .unwrap()
                .starts_with("数据!A1*2")
        );
        assert_eq!(book.names[0].local, Some(2));
        assert_eq!(book.active_sheet, 2);
        // A second copy is numbered on, and long names are cut to fit.
        assert_eq!(book.copy_sheet_name(0).as_deref(), Some("数据 (3)"));
        book.sheets[0].name = "x".repeat(31);
        let long = book.copy_sheet_name(0).unwrap();
        assert_eq!(long.chars().count(), 31);
        assert!(long.ends_with(" (2)"));
        // Not a name that is taken, and not a chart.
        assert_eq!(book.copy_sheet(0, 0, "sheet2"), None);
    }

    #[test]
    fn defined_names_follow_renames_and_deletions() {
        use crate::model::DefinedName;
        let mut book = book();
        book.names = vec![
            DefinedName {
                name: "Rate".into(),
                local: None,
                formula: "数据!$A$1".into(),
            },
            DefinedName {
                name: "Mine".into(),
                local: Some(1),
                formula: "Sheet2!$A$1".into(),
            },
        ];
        book.rename_sheet(0, "Data 1");
        assert_eq!(book.names[0].formula, "'Data 1'!$A$1");
        book.delete_sheet(0);
        assert_eq!(book.names[0].formula, "#REF!");
        assert_eq!(book.names[1].local, Some(0));
        book.delete_sheet(0);
        assert_eq!(book.names.len(), 1);
    }

    #[test]
    fn a_moved_sheet_keeps_its_names() {
        use crate::model::DefinedName;
        let mut book = book();
        book.add_sheet("Sheet3");
        book.names = vec![DefinedName {
            name: "Mine".into(),
            local: Some(0),
            formula: "数据!$A$1".into(),
        }];
        book.move_sheet(0, 2);
        let names: Vec<&str> = book.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Sheet2", "Sheet3", "数据"]);
        assert_eq!(book.names[0].local, Some(2));
        assert_eq!(
            (0..3).map(|i| moved_index(i, 0, 2)).collect::<Vec<_>>(),
            [2, 0, 1]
        );
        assert_eq!(
            (0..3).map(|i| moved_index(i, 2, 0)).collect::<Vec<_>>(),
            [1, 2, 0]
        );
    }

    #[test]
    fn names_are_checked_as_excel_does() {
        let book = book();
        assert_eq!(book.check_sheet_name("", None), Err(NameProblem::Empty));
        assert_eq!(
            book.check_sheet_name("a/b", None),
            Err(NameProblem::Character('/'))
        );
        assert_eq!(
            book.check_sheet_name("'x", None),
            Err(NameProblem::Character('\''))
        );
        assert_eq!(
            book.check_sheet_name(&"長".repeat(32), None),
            Err(NameProblem::TooLong)
        );
        assert_eq!(
            book.check_sheet_name("sheet2", None),
            Err(NameProblem::Taken)
        );
        assert_eq!(book.check_sheet_name("sheet2", Some(1)), Ok(()));
        assert_eq!(
            book.check_sheet_name("history", None),
            Err(NameProblem::Reserved)
        );
        assert_eq!(book.new_sheet_name("Sheet"), "Sheet3");
    }

    #[test]
    fn formulas_follow_a_rename_and_lose_a_deleted_sheet() {
        let mut book = book();
        let changed = book.rename_sheet(0, "Q1 数据");
        assert_eq!(changed, [(1, CellRef::new(0, 0))]);
        let formula = |book: &Workbook, sheet: usize| {
            book.sheets[sheet]
                .cell(CellRef::new(0, 0))
                .unwrap()
                .formula
                .clone()
                .unwrap()
        };
        assert_eq!(
            &*formula(&book, 1),
            "'Q1 数据'!A1*2+SUM('Q1 数据'!A1:B2)+\"数据!A1\"+[1]数据!A1"
        );
        book.rename_sheet(0, "A1");
        assert_eq!(
            &*formula(&book, 1),
            "'A1'!A1*2+SUM('A1'!A1:B2)+\"数据!A1\"+[1]数据!A1"
        );
        let changed = book.delete_sheet(0);
        assert_eq!(changed, [(0, CellRef::new(0, 0))]);
        assert_eq!(
            &*formula(&book, 0),
            "#REF!*2+SUM(#REF!)+\"数据!A1\"+[1]数据!A1"
        );
        assert_eq!(book.sheets.len(), 1);
        assert!(book.names.is_empty());
        let added = book.add_sheet("Sheet3");
        assert_eq!(added, 1);
        assert_eq!(
            book.check_sheet_name("sheet3", None),
            Err(NameProblem::Taken)
        );
    }
}
