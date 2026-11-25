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
pub mod clipboard;
pub mod comment;
pub mod edit;
pub mod fill;
pub mod filter;
pub mod format;
pub mod formula;
pub mod hints;
pub mod input;
pub mod model;
pub mod restyle;
pub mod sheets;
pub mod sort;
pub mod structure;
pub mod style;
pub mod validation;

pub use address::{CellRange, CellRef, MAX_COLUMNS, MAX_ROWS, column_name};
pub use axis::Axis;
pub use calc::{CalcEngine, CalcError, Cancel, Computed, Edit, Mismatch};
pub use clipboard::{Clip, parse_tsv};
pub use comment::Comment;
pub use edit::Change;
pub use filter::{AutoFilter, Shown};
pub use format::{DateOrder, Format, date_parts, date_serial};
pub use model::{
    Anchor, Cell, Display, DisplayKind, Picture, Placement, Row, Sheet, SheetBuilder, SheetKind,
    Value, Workbook,
};
pub use restyle::StyleEdit;
pub use sheets::NameProblem;
pub use style::{Borders, CellStyle, Font, HAlign, Line, LineStyle, Rgba, VAlign};
pub use validation::{ErrorStyle, ListSource, Validation};
