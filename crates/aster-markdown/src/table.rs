//! Tables in Markdown source (GitHub's pipe tables): Tab and Shift+Tab move
//! from cell to cell, Tab in the last cell adds a row, Enter adds a row
//! below, and Enter on an empty last row ends the table.
//!
//! A table is a header row, a delimiter row (`| --- | :-: |`) with as many
//! cells, and the rows after it up to a blank line or a line without a pipe.
//! Only the lines around the caret are read, at most 64 KB each way, so a
//! large document costs no more than a small one. Like the commands in
//! [`crate::format`], each returns one [`Edit`].

use std::ops::Range;

use ropey::Rope;

use crate::document::{Selection, slice};
use crate::format::Edit;
use crate::highlight::{Highlighter, Marks};

/// How far above and below the caret the rows of its table are read.
const TABLE_REACH: usize = 64 * 1024;
/// Longer lines are not taken for table rows.
const ROW_REACH: usize = 16 * 1024;

/// A line of the text: where it starts and what it holds, without the line
/// break.
#[derive(Clone, Debug)]
struct Line {
    start: usize,
    text: String,
}

impl Line {
    fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

/// The line `line`, unless it is too long to be a table row.
fn line_at(text: &Rope, line: usize) -> Option<Line> {
    let start = text.line_to_byte(line);
    let end = if line + 1 < text.len_lines() {
        text.line_to_byte(line + 1) - 1
    } else {
        text.len_bytes()
    };
    (end - start <= ROW_REACH).then(|| Line {
        start,
        text: slice(text, start..end),
    })
}

/// The offsets of the pipes in a line that are not escaped with `\`.
fn pipes(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte == b'|' {
            let backslashes = bytes[..at]
                .iter()
                .rev()
                .take_while(|b| **b == b'\\')
                .count();
            if backslashes % 2 == 0 {
                out.push(at);
            }
        }
    }
    out
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Whether a line could be a row: it has a pipe, and is not indented as
/// code or quoted.
fn row_like(line: &str) -> bool {
    let body = line.trim();
    indent_of(line) <= 3
        && !line.starts_with('\t')
        && !body.is_empty()
        && !body.starts_with('>')
        && !pipes(line).is_empty()
}

/// The cells of a row, as ranges of the line between its pipes, spaces
/// included. A pipe at the start and at the end of the row is optional.
fn cells(line: &str) -> Vec<Range<usize>> {
    let start = indent_of(line);
    let end = line.trim_end().len().max(start);
    let mut at = start;
    let mut out = Vec::new();
    let pipes = pipes(line);
    let mut pipes = pipes.iter().copied().filter(|p| *p < end).peekable();
    if pipes.peek() == Some(&start) {
        pipes.next();
        at = start + 1;
    }
    for pipe in pipes {
        out.push(at..pipe);
        at = pipe + 1;
    }
    if at < end {
        out.push(at..end);
    }
    out
}

/// Whether a line is a delimiter row: every cell is dashes, with an
/// optional colon at either end for the alignment.
fn is_delimiter(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = line[cell.clone()].trim();
            let dashes = cell.strip_prefix(':').unwrap_or(cell);
            let dashes = dashes.strip_suffix(':').unwrap_or(dashes);
            !dashes.is_empty() && dashes.bytes().all(|b| b == b'-')
        })
}

/// A table: its rows from the header on, the second being the delimiter row.
struct Table {
    rows: Vec<Line>,
    /// The header's cells: what a new row has.
    columns: usize,
    /// The header's indentation, for new rows.
    indent: String,
}

impl Table {
    /// The table with the caret on one of its rows, and that row's index.
    fn at(text: &Rope, highlighter: Option<&Highlighter>, caret: usize) -> Option<(Self, usize)> {
        let caret_line = text.byte_to_line(caret);
        let here = line_at(text, caret_line)?;
        if !row_like(&here.text) {
            return None;
        }
        // By the highlighting, if it is up to date: not in a code block, and
        // read as a table.
        if let Some(highlighter) = highlighter
            && !highlighter
                .spans(here.start..here.end().max(here.start + 1))
                .iter()
                .any(|span| span.marks.contains(Marks::TABLE))
        {
            return None;
        }

        let mut above = Vec::new();
        let mut line = caret_line;
        while line > 0 {
            line -= 1;
            match line_at(text, line) {
                Some(row) if row_like(&row.text) && here.start - row.start <= TABLE_REACH => {
                    above.push(row)
                }
                _ => break,
            }
        }
        let mut lines: Vec<Line> = above.into_iter().rev().collect();
        let caret_index = lines.len();
        lines.push(here);
        let mut line = caret_line + 1;
        while line < text.len_lines() {
            match line_at(text, line) {
                Some(row) if row_like(&row.text) && row.start - caret <= TABLE_REACH => {
                    lines.push(row)
                }
                _ => break,
            }
            line += 1;
        }

        // The first header and delimiter in the run of lines.
        let header = (0..lines.len().saturating_sub(1)).find(|&i| {
            is_delimiter(&lines[i + 1].text)
                && cells(&lines[i].text).len() == cells(&lines[i + 1].text).len()
        })?;
        if header > caret_index {
            return None;
        }
        let rows = lines.split_off(header);
        let columns = cells(&rows[0].text).len();
        let indent = " ".repeat(indent_of(&rows[0].text));
        Some((
            Self {
                rows,
                columns,
                indent,
            },
            caret_index - header,
        ))
    }

    /// The cell of row `row` at `offset`: the one the offset is in, or next
    /// to.
    fn cell_at(&self, row: usize, offset: usize) -> usize {
        let line = &self.rows[row];
        let within = offset.saturating_sub(line.start);
        let cells = cells(&line.text);
        cells
            .iter()
            .position(|cell| within <= cell.end)
            .unwrap_or(cells.len().saturating_sub(1))
    }

    /// Every cell the caret can go to, row by row: all but the delimiter row.
    fn stops(&self) -> Vec<(usize, Range<usize>)> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(row, _)| *row != 1)
            .flat_map(|(row, line)| cells(&line.text).into_iter().map(move |cell| (row, cell)))
            .collect()
    }

    /// The selection that puts the caret in a cell: its text selected, or
    /// in an empty cell the caret after the first space.
    fn select(&self, row: usize, cell: &Range<usize>) -> Selection {
        select_in(&self.rows[row], cell)
    }

    /// A row of empty cells, as the header has, with the line break before
    /// it, and where the caret goes in it.
    fn new_row(&self) -> (String, usize) {
        let row = format!("\n{}", empty_row(&self.indent, self.columns));
        (row, 1 + self.indent.len() + 2)
    }
}

/// The selection that puts the caret in a cell of `line`: its text
/// selected, or in an empty cell the caret after the first space.
fn select_in(line: &Line, cell: &Range<usize>) -> Selection {
    let content = &line.text[cell.clone()];
    let trimmed = content.trim();
    let at = line.start + cell.start;
    if trimmed.is_empty() {
        return Selection::caret(at + content.len().min(1));
    }
    let start = at + (content.len() - content.trim_start().len());
    Selection {
        anchor: start,
        focus: start + trimmed.len(),
    }
}

/// A row of `columns` empty cells.
fn empty_row(indent: &str, columns: usize) -> String {
    format!("{indent}|{}", "  |".repeat(columns.max(1)))
}

/// A row with its cells changed by `change`, written with a pipe at both
/// ends and the cells' own spacing kept.
fn rebuild(line: &str, change: impl FnOnce(&mut Vec<String>)) -> String {
    let indent = &line[..indent_of(line)];
    let mut cells: Vec<String> = cells(line)
        .into_iter()
        .map(|cell| line[cell].to_owned())
        .collect();
    change(&mut cells);
    format!("{indent}|{}|", cells.join("|"))
}

/// What the table commands of the Format menu do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableEdit {
    RowAbove,
    RowBelow,
    ColumnLeft,
    ColumnRight,
    DeleteRow,
    DeleteColumn,
}

/// Whether the caret is in a table, for offering the table commands.
pub fn in_table(text: &Rope, highlighter: Option<&Highlighter>, caret: usize) -> bool {
    Table::at(text, highlighter, caret).is_some()
}

/// Add or delete a row or column of the table with the caret, beside the
/// caret's cell. A new row has as many cells as the header; a new column
/// gets `---` in the delimiter row, and every row is then written with a
/// pipe at both ends. Rows with fewer cells than the header get the new
/// cell at their end. The caret goes into the new row or column, or after
/// a deletion to the cell that took the deleted one's place. The header
/// and the delimiter row cannot be deleted or have a row put above them,
/// and the last column cannot be deleted: `None` then, and outside a table.
pub fn edit_table(
    text: &Rope,
    highlighter: Option<&Highlighter>,
    selection: Selection,
    what: TableEdit,
) -> Option<Edit> {
    let from = selection.range().start;
    let (table, row) = Table::at(text, highlighter, from)?;
    let column = table.cell_at(row, from);
    let mut lines: Vec<String> = table.rows.iter().map(|line| line.text.clone()).collect();
    let (target_row, target_cell) = match what {
        TableEdit::RowAbove | TableEdit::RowBelow => {
            let at = match what {
                TableEdit::RowAbove if row < 2 => return None,
                TableEdit::RowAbove => row,
                _ => row.max(1) + 1,
            };
            lines.insert(at, empty_row(&table.indent, table.columns));
            (at, column)
        }
        TableEdit::DeleteRow => {
            if row < 2 {
                return None;
            }
            lines.remove(row);
            let next = if row < lines.len() { row } else { row - 1 };
            // The only row went: the caret goes up to the header.
            (if next == 1 { 0 } else { next }, column)
        }
        TableEdit::ColumnLeft | TableEdit::ColumnRight => {
            let at = if what == TableEdit::ColumnLeft {
                column
            } else {
                column + 1
            };
            for (index, line) in lines.iter_mut().enumerate() {
                let fill = if index == 1 { " --- " } else { "  " };
                *line = rebuild(line, |cells| {
                    cells.insert(at.min(cells.len()), fill.to_owned())
                });
            }
            (if row == 1 { 0 } else { row }, at)
        }
        TableEdit::DeleteColumn => {
            if table.columns <= 1 {
                return None;
            }
            for line in &mut lines {
                *line = rebuild(line, |cells| {
                    if column < cells.len() {
                        cells.remove(column);
                    }
                });
            }
            (
                if row == 1 { 0 } else { row },
                column.min(table.columns - 2),
            )
        }
    };

    let start = table.rows[0].start;
    let end = table.rows.last().expect("a header").end();
    let mut at = start;
    let mut selection = Selection::caret(start);
    for (index, text) in lines.iter().enumerate() {
        if index == target_row {
            let line = Line {
                start: at,
                text: text.clone(),
            };
            let cells = cells(text);
            if let Some(cell) = cells.get(target_cell.min(cells.len().saturating_sub(1))) {
                selection = select_in(&line, cell);
            }
        }
        at += text.len() + 1;
    }
    Some(Edit {
        range: start..end,
        text: lines.join("\n"),
        selection,
    })
}

/// Tab and Shift+Tab in a table: the caret goes to the next or previous
/// cell, row by row, with the cell's text selected. Tab in the last cell
/// adds an empty row and goes to its first cell; Shift+Tab in the first
/// cell stays. `None` outside a table: Tab is then an ordinary key. Pass the
/// highlighting while it is up to date, so that lines in code blocks are not
/// taken for rows.
pub fn tab(
    text: &Rope,
    highlighter: Option<&Highlighter>,
    selection: Selection,
    backwards: bool,
) -> Option<Edit> {
    let from = selection.range().start;
    let (table, row) = Table::at(text, highlighter, from)?;
    let stops = table.stops();
    let moved = |selection: Selection| Edit {
        range: from..from,
        text: String::new(),
        selection,
    };
    // From the delimiter row, the cells just above and below it.
    let target = if row == 1 {
        if backwards {
            stops.iter().rposition(|(row, _)| *row == 0)
        } else {
            stops.iter().position(|(row, _)| *row > 1)
        }
    } else {
        let cell = table.cell_at(row, from);
        let here = stops
            .iter()
            .position(|(r, _)| *r == row)
            .map(|first| first + cell)?;
        if backwards {
            Some(here.saturating_sub(1))
        } else {
            Some(here + 1).filter(|next| *next < stops.len())
        }
    };
    match target {
        Some(index) => {
            let (row, cell) = &stops[index];
            Some(moved(table.select(*row, cell)))
        }
        None => {
            // Past the last cell: a new row.
            let end = table.rows.last().expect("a header").end();
            let (row, caret) = table.new_row();
            Some(Edit {
                range: end..end,
                text: row,
                selection: Selection::caret(end + caret),
            })
        }
    }
}

/// Enter in a table: an empty row goes in below the caret's row (below the
/// delimiter row from the header), and the caret goes to its first cell; the
/// text of the cell is not split, as a cell cannot hold a line break. Enter
/// on an empty last row ends the table: the row goes, and the caret is on a
/// new line after a blank one. `None` outside a table or with a selection.
pub fn line_break(
    text: &Rope,
    highlighter: Option<&Highlighter>,
    selection: Selection,
) -> Option<Edit> {
    if !selection.is_caret() {
        return None;
    }
    let (table, row) = Table::at(text, highlighter, selection.focus)?;
    let line = &table.rows[row];
    let last = row + 1 == table.rows.len();
    let empty = cells(&line.text)
        .iter()
        .all(|cell| line.text[cell.clone()].trim().is_empty());
    if row >= 2 && last && empty {
        return Some(Edit {
            range: line.start..line.end(),
            text: "\n".to_owned(),
            selection: Selection::caret(line.start + 1),
        });
    }
    let after = table.rows[row.max(1)].end();
    let (new, caret) = table.new_row();
    Some(Edit {
        range: after..after,
        text: new,
        selection: Selection::caret(after + caret),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text with the caret marked `‸` or a selection marked `«…»`, and the
    /// text without the marks.
    fn marked(input: &str) -> (String, Range<usize>) {
        if let Some(at) = input.find('‸') {
            return (input.replacen('‸', "", 1), at..at);
        }
        let start = input.find('«').expect("a mark");
        let text = input.replacen('«', "", 1);
        let end = text.find('»').expect("an end mark");
        (text.replacen('»', "", 1), start..end)
    }

    /// Apply an edit and mark the selection after it the same way.
    fn run(
        input: &str,
        edit: impl Fn(&Rope, Option<&Highlighter>, Selection) -> Option<Edit>,
    ) -> Option<String> {
        let (text, range) = marked(input);
        let rope = Rope::from_str(&text);
        let highlighter = Highlighter::new(&rope);
        let selection = Selection {
            anchor: range.start,
            focus: range.end,
        };
        let edit = edit(&rope, Some(&highlighter), selection)?;
        let mut out = text.clone();
        out.replace_range(edit.range.clone(), &edit.text);
        let range = edit.selection.range();
        if range.is_empty() {
            out.insert(range.start, '‸');
        } else {
            out.insert(range.end, '»');
            out.insert(range.start, '«');
        }
        Some(out)
    }

    fn tab_in(input: &str) -> Option<String> {
        run(input, |rope, highlighter, selection| {
            tab(rope, highlighter, selection, false)
        })
    }

    fn shift_tab_in(input: &str) -> Option<String> {
        run(input, |rope, highlighter, selection| {
            tab(rope, highlighter, selection, true)
        })
    }

    fn enter_in(input: &str) -> Option<String> {
        run(input, line_break)
    }

    const TABLE: &str = "| 名称 | 数量 |\n| --- | ---: |\n| 苹果 | 3 |\n| 梨 |  |";

    fn with_caret(at: &str) -> String {
        let offset = TABLE.find(at).expect("in the table");
        let mut out = TABLE.to_owned();
        out.insert(offset, '‸');
        out
    }

    #[test]
    fn rows_cells_and_delimiters() {
        assert_eq!(cells("| a | b |"), vec![1..4, 5..8]);
        assert_eq!(cells("a | b"), vec![0..2, 3..5]);
        assert_eq!(cells("  | a \\| b |  "), vec![3..11]);
        assert_eq!(cells("| a ||"), vec![1..4, 5..5]);
        assert!(is_delimiter("| --- | :-: | ---: |"));
        assert!(is_delimiter("---|---"));
        assert!(!is_delimiter("| --- | a |"));
        assert!(!is_delimiter("| ::-- |"));
        assert!(row_like("a | b"));
        assert!(!row_like("    | code |"));
        assert!(!row_like("> | quoted |"));
        assert!(!row_like("no pipe"));
    }

    #[test]
    fn tab_goes_from_cell_to_cell() {
        // To the next cell, its text selected, across the delimiter row.
        assert_eq!(
            tab_in(&with_caret("名称")).unwrap(),
            "| 名称 | «数量» |\n| --- | ---: |\n| 苹果 | 3 |\n| 梨 |  |"
        );
        assert_eq!(
            tab_in(&with_caret("数量")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| «苹果» | 3 |\n| 梨 |  |"
        );
        // From the delimiter row to the first cell below it.
        assert_eq!(
            tab_in(&with_caret(":")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| «苹果» | 3 |\n| 梨 |  |"
        );
        // An empty cell: the caret after its first space.
        assert_eq!(
            tab_in(&with_caret("梨")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| 苹果 | 3 |\n| 梨 | ‸ |"
        );
        // With the text of a cell selected, as Tab leaves it.
        assert_eq!(
            tab_in("| «a» | b |\n|---|---|").unwrap(),
            "| a | «b» |\n|---|---|"
        );
    }

    #[test]
    fn tab_in_the_last_cell_adds_a_row() {
        let input = format!("{TABLE}‸");
        assert_eq!(tab_in(&input).unwrap(), format!("{TABLE}\n| ‸ |  |"));
        // Indented tables get indented rows; text after the table stays.
        assert_eq!(
            tab_in("  a | b\n  --|--\n  1 | 2‸\n\n后文").unwrap(),
            "  a | b\n  --|--\n  1 | 2\n  | ‸ |  |\n\n后文"
        );
    }

    #[test]
    fn shift_tab_goes_back() {
        assert_eq!(
            shift_tab_in(&with_caret("苹果")).unwrap(),
            "| 名称 | «数量» |\n| --- | ---: |\n| 苹果 | 3 |\n| 梨 |  |"
        );
        assert_eq!(
            shift_tab_in(&with_caret("3")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| «苹果» | 3 |\n| 梨 |  |"
        );
        // In the first cell it stays there.
        assert_eq!(
            shift_tab_in(&with_caret("名称")).unwrap(),
            "| «名称» | 数量 |\n| --- | ---: |\n| 苹果 | 3 |\n| 梨 |  |"
        );
    }

    #[test]
    fn enter_adds_a_row_below() {
        assert_eq!(
            enter_in(&with_caret("3")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| 苹果 | 3 |\n| ‸ |  |\n| 梨 |  |"
        );
        // From the header, the new row is the first under the delimiter.
        assert_eq!(
            enter_in(&with_caret("名称")).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| ‸ |  |\n| 苹果 | 3 |\n| 梨 |  |"
        );
        // An empty last row ends the table.
        let ended = enter_in("| a | b |\n|---|---|\n| 1 | 2 |\n|‸  |  |").unwrap();
        assert_eq!(ended, "| a | b |\n|---|---|\n| 1 | 2 |\n\n‸");
        // With a selection, Enter is not the table's.
        assert_eq!(enter_in("| «a» | b |\n|---|---|"), None);
    }

    #[test]
    fn outside_a_table_tab_and_enter_are_ordinary_keys() {
        assert_eq!(tab_in("正文 | 不是表格‸"), None);
        // Rows above the header are not in the table.
        assert_eq!(tab_in("x | y‸\n\na | b\n--|--"), None);
        assert_eq!(enter_in("前文 | 一\n\n| a | b |\n|---|---|\n\n后文‸"), None);
        // Inside a code block, by the highlighting.
        assert_eq!(tab_in("```\n| a | b‸ |\n|---|---|\n```"), None);
        // Quoted tables are left alone.
        assert_eq!(tab_in("> | a | b‸ |\n> |---|---|"), None);
    }

    fn table_edit(input: &str, what: TableEdit) -> Option<String> {
        run(input, |rope, highlighter, selection| {
            edit_table(rope, highlighter, selection, what)
        })
    }

    #[test]
    fn rows_go_in_above_and_below_and_come_out() {
        let input = with_caret("3");
        assert_eq!(
            table_edit(&input, TableEdit::RowAbove).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n|  | ‸ |\n| 苹果 | 3 |\n| 梨 |  |"
        );
        assert_eq!(
            table_edit(&input, TableEdit::RowBelow).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| 苹果 | 3 |\n|  | ‸ |\n| 梨 |  |"
        );
        // From the header, below means under the delimiter row; above is
        // not possible.
        assert_eq!(
            table_edit(&with_caret("名称"), TableEdit::RowBelow).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| ‸ |  |\n| 苹果 | 3 |\n| 梨 |  |"
        );
        assert_eq!(table_edit(&with_caret("名称"), TableEdit::RowAbove), None);
        // Deleting a row: the caret goes to the row that took its place, or
        // the one above at the end, or the header when none is left.
        assert_eq!(
            table_edit(&input, TableEdit::DeleteRow).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| 梨 | ‸ |"
        );
        assert_eq!(
            table_edit(&with_caret("梨"), TableEdit::DeleteRow).unwrap(),
            "| 名称 | 数量 |\n| --- | ---: |\n| «苹果» | 3 |"
        );
        assert_eq!(
            table_edit("| a |\n|---|\n| ‸x |", TableEdit::DeleteRow).unwrap(),
            "| «a» |\n|---|"
        );
        assert_eq!(table_edit(&with_caret("名称"), TableEdit::DeleteRow), None);
        assert_eq!(table_edit(&with_caret(":"), TableEdit::DeleteRow), None);
    }

    #[test]
    fn columns_go_in_left_and_right_and_come_out() {
        let input = with_caret("苹果");
        assert_eq!(
            table_edit(&input, TableEdit::ColumnLeft).unwrap(),
            "|  | 名称 | 数量 |\n| --- | --- | ---: |\n| ‸ | 苹果 | 3 |\n|  | 梨 |  |"
        );
        assert_eq!(
            table_edit(&input, TableEdit::ColumnRight).unwrap(),
            "| 名称 |  | 数量 |\n| --- | --- | ---: |\n| 苹果 | ‸ | 3 |\n| 梨 |  |  |"
        );
        assert_eq!(
            table_edit(&input, TableEdit::DeleteColumn).unwrap(),
            "| 数量 |\n| ---: |\n| «3» |\n|  |"
        );
        // The last column of a row: the one before takes the caret.
        assert_eq!(
            table_edit(&with_caret("3"), TableEdit::DeleteColumn).unwrap(),
            "| 名称 |\n| --- |\n| «苹果» |\n| 梨 |"
        );
        // A row without pipes at its ends gets them; a short row gets the
        // new cell at its end.
        assert_eq!(
            table_edit("a | b‸\n--|--\n1 |", TableEdit::ColumnRight).unwrap(),
            "|a | b| ‸ |\n|--|--| --- |\n|1 |  |"
        );
        // The only column stays.
        assert_eq!(table_edit("| a‸ |\n|---|", TableEdit::DeleteColumn), None);
        assert_eq!(table_edit("正文‸", TableEdit::RowBelow), None);
    }

    #[test]
    fn the_caret_is_in_a_table_or_not() {
        let rope = Rope::from_str(TABLE);
        let highlighter = Highlighter::new(&rope);
        assert!(in_table(&rope, Some(&highlighter), 0));
        assert!(in_table(&rope, Some(&highlighter), TABLE.len()));
        let rope = Rope::from_str("正文 | 一\n\n```\n| a |\n|---|\n```");
        let highlighter = Highlighter::new(&rope);
        assert!(!in_table(&rope, Some(&highlighter), 0));
        assert!(!in_table(&rope, Some(&highlighter), rope.len_bytes() - 6));
    }
}
