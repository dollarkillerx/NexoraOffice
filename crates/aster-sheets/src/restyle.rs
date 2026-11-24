//! Changing how cells look: bold, sizes, alignment, number formats, fills,
//! borders, and merging cells. A changed look becomes a style; one that
//! looks the same is reused, so formatting a thousand cells adds one style.

use std::collections::HashMap;
use std::sync::Arc;

use crate::address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS};
use crate::format::Format;
use crate::model::{Cell, Value, Workbook};
use crate::style::{Borders, CellStyle, HAlign, Line, LineStyle, Rgba};

/// One change to cell formatting.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleEdit {
    Bold(bool),
    Italic(bool),
    Underline(bool),
    Strike(bool),
    /// Points.
    Size(f32),
    /// `None` is "by value": text left, numbers right.
    Align(Option<HAlign>),
    Wrap(bool),
    /// A number format: its id in the file (a built-in one, or
    /// [`crate::style::UNNUMBERED_FORMAT`] for one saving numbers) and the
    /// format.
    Format(u32, Arc<Format>),
    Fill(Option<Rgba>),
    /// Thin lines on every side of every cell, or none.
    Borders(bool),
}

impl StyleEdit {
    fn apply(&self, style: &CellStyle) -> CellStyle {
        let mut out = style.clone();
        let font = |f: &mut CellStyle, change: &dyn Fn(&mut crate::style::Font)| {
            let mut font = (*f.font).clone();
            change(&mut font);
            if font != *f.font {
                f.font = Arc::new(font);
                f.font_id = None;
            }
        };
        match self {
            StyleEdit::Bold(on) => font(&mut out, &|f| f.bold = *on),
            StyleEdit::Italic(on) => font(&mut out, &|f| f.italic = *on),
            StyleEdit::Underline(on) => font(&mut out, &|f| f.underline = *on),
            StyleEdit::Strike(on) => font(&mut out, &|f| f.strike = *on),
            StyleEdit::Size(points) => font(&mut out, &|f| f.size = points.clamp(1.0, 409.0)),
            StyleEdit::Align(align) => out.align = *align,
            StyleEdit::Wrap(on) => out.wrap = *on,
            StyleEdit::Format(id, format) => {
                if format.code() != out.format.code() {
                    out.format = format.clone();
                    out.format_id = *id;
                }
            }
            StyleEdit::Fill(fill) => {
                if *fill != out.fill {
                    out.fill = *fill;
                    out.fill_id = None;
                }
            }
            StyleEdit::Borders(on) => {
                let line = on.then_some(Line {
                    style: LineStyle::Thin,
                    color: None,
                });
                let borders = Borders {
                    left: line,
                    right: line,
                    top: line,
                    bottom: line,
                };
                if borders != *out.borders {
                    out.borders = Arc::new(borders);
                    out.border_id = None;
                }
            }
        }
        out
    }
}

/// Ranges larger than this are formatted only where there are cells, as
/// formatting every cell of a whole column would make a million.
const MAX_FORMATTED: u64 = 200_000;

impl Workbook {
    /// The index of a style that looks like `style`, added if there is none.
    pub fn style_index(&mut self, style: CellStyle) -> u32 {
        if let Some(found) = self.styles.iter().position(|s| s.looks_like(&style)) {
            return found as u32;
        }
        // The default style that is not in the list yet.
        if self.styles.is_empty() && style.looks_like(&CellStyle::default()) {
            return 0;
        }
        self.styles.push(style);
        (self.styles.len() - 1) as u32
    }

    /// The cells to put to apply `edit` to `range` on `sheet`. Empty places
    /// get a cell holding only the new style; cells that would not change
    /// are left out.
    pub fn restyle(
        &mut self,
        sheet: usize,
        range: CellRange,
        edit: &StyleEdit,
    ) -> Vec<(CellRef, Option<Cell>)> {
        let Some(source) = self.sheets.get(sheet) else {
            return Vec::new();
        };
        let count = u64::from(range.rows()) * u64::from(range.cols());
        let places: Vec<(CellRef, Option<Cell>)> = if count <= MAX_FORMATTED {
            let mut out = Vec::with_capacity(count as usize);
            for row in range.first.row..=range.last.row {
                for col in range.first.col..=range.last.col {
                    let at = CellRef::new(row, col);
                    out.push((at, source.cell(at).cloned()));
                }
            }
            out
        } else {
            source
                .cells_in(range)
                .map(|(at, cell)| (at, Some(cell.clone())))
                .collect()
        };
        let styles_of: Vec<u32> = places
            .iter()
            .map(|(at, cell)| {
                cell.as_ref()
                    .map_or_else(|| source.style_at(*at), |c| c.style)
            })
            .collect();
        let mut made: HashMap<u32, u32> = HashMap::new();
        let mut out = Vec::new();
        for ((at, cell), old) in places.into_iter().zip(styles_of) {
            let new = match made.get(&old) {
                Some(&new) => new,
                None => {
                    let edited = edit.apply(&self.style(old));
                    let new = if edited.looks_like(&self.style(old)) {
                        old
                    } else {
                        self.style_index(edited)
                    };
                    made.insert(old, new);
                    new
                }
            };
            let current = cell.as_ref().map_or(0, |c| c.style);
            if (cell.is_some() && new == current) || (cell.is_none() && new == old) {
                continue;
            }
            let mut cell = cell.unwrap_or_else(|| Cell::new(at.col, Value::Empty));
            cell.style = new;
            out.push((at, (!(cell.is_blank() && new == 0)).then_some(cell)));
        }
        out
    }
}

/// Excel's fill button colour.
pub const YELLOW: Rgba = [0xFF, 0xFF, 0x00, 0xFF];

/// Whether `range` is a whole sheet, row or column.
pub fn is_whole(range: &CellRange) -> bool {
    range.rows() == MAX_ROWS || range.cols() == MAX_COLUMNS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SheetBuilder, SheetKind};

    fn at(text: &str) -> CellRef {
        CellRef::parse(text).unwrap()
    }

    fn book() -> Workbook {
        let mut sheet = SheetBuilder::new("表", SheetKind::Worksheet);
        sheet.push(0, Cell::new(0, Value::Text("見出し".into())));
        sheet.push(1, Cell::new(0, Value::Number(1.0)));
        let styles = vec![CellStyle {
            xf: Some(0),
            font_id: Some(0),
            fill_id: Some(0),
            border_id: Some(0),
            ..CellStyle::default()
        }];
        Workbook {
            sheets: vec![sheet.finish()],
            styles,
            ..Workbook::default()
        }
    }

    #[test]
    fn bold_on_a_range_adds_one_style_and_fills_empty_places() {
        let mut book = book();
        let cells = book.restyle(
            0,
            CellRange::parse("A1:B2").unwrap(),
            &StyleEdit::Bold(true),
        );
        assert_eq!(cells.len(), 4);
        assert_eq!(book.styles.len(), 2);
        let bold = &book.styles[1];
        assert!(bold.font.bold);
        assert_eq!(
            (bold.xf, bold.font_id, bold.fill_id),
            (Some(0), None, Some(0))
        );
        assert!(cells.iter().all(|(_, c)| c.as_ref().unwrap().style == 1));
        // The text keeps its value; the empty places are blank cells.
        assert_eq!(
            cells[0].1.as_ref().unwrap().value,
            Value::Text("見出し".into())
        );
        assert!(cells[1].1.as_ref().unwrap().is_blank());
        book.set_cells(0, cells);
        // Again: nothing to change.
        assert!(
            book.restyle(
                0,
                CellRange::parse("A1:B2").unwrap(),
                &StyleEdit::Bold(true)
            )
            .is_empty()
        );
        // Back to plain: the original style is found, and blank cells go.
        let plain = book.restyle(
            0,
            CellRange::parse("A1:B2").unwrap(),
            &StyleEdit::Bold(false),
        );
        assert_eq!(book.styles.len(), 2);
        assert!(plain.iter().filter(|(_, c)| c.is_none()).count() == 2);
    }

    #[test]
    fn each_edit_changes_its_part() {
        let base = CellStyle::default();
        let sized = StyleEdit::Size(14.0).apply(&base);
        assert_eq!(sized.font.size, 14.0);
        let filled = StyleEdit::Fill(Some(YELLOW)).apply(&base);
        assert_eq!((filled.fill, filled.fill_id), (Some(YELLOW), None));
        let boxed = StyleEdit::Borders(true).apply(&base);
        assert!(boxed.borders.left.is_some() && boxed.borders.bottom.is_some());
        assert!(StyleEdit::Borders(false).apply(&boxed).borders.is_empty());
        let percent = StyleEdit::Format(10, Arc::new(Format::builtin(10))).apply(&base);
        assert_eq!(
            (percent.format_id, percent.format.number(0.125, false)),
            (10, "12.50%".into())
        );
        let centred = StyleEdit::Align(Some(HAlign::Center)).apply(&base);
        assert_eq!(centred.align, Some(HAlign::Center));
        assert!(StyleEdit::Wrap(true).apply(&base).wrap);
    }

    #[test]
    fn whole_columns_format_only_their_cells() {
        let mut book = book();
        let column = CellRange::new(CellRef::new(0, 0), CellRef::new(MAX_ROWS - 1, 0));
        assert!(is_whole(&column));
        let cells = book.restyle(0, column, &StyleEdit::Italic(true));
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].0, at("A2"));
    }
}
