//! The Sheets document model.
//!
//! A [`Workbook`] is a list of [`Sheet`]s. A sheet stores only the cells that
//! have something in them, row by row, so a sheet with a few values far apart
//! costs as little as a small one. Row heights and column widths are an
//! [`Axis`]: a default size and the runs that differ from it, so positions on
//! a sheet of a million rows are found by binary search.
//!
//! What a cell shows is its value put through its number format
//! ([`format`]). Formulas are calculated by an engine behind the
//! [`calc::CalcEngine`] interface (ADR 0002), on a worker thread; until it
//! is done they show the result the file was saved with. This first version
//! is read-only.
//!
//! Nothing here reads files; `aster-ooxml` turns XLSX packages into this
//! model. Nothing here knows about the GUI toolkit either.

pub mod address;
pub mod axis;
pub mod calc;
pub mod format;
pub mod formula;
pub mod model;

pub use address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS, column_name};
pub use axis::Axis;
pub use calc::{CalcEngine, CalcError, Cancel, Computed, Edit};
pub use format::{Format, date_parts};
pub use model::{
    Cell, CellStyle, Display, DisplayKind, HAlign, Row, Sheet, SheetBuilder, SheetKind, Value,
    Workbook,
};
