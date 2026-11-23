//! Office Open XML: our own package layer (ADR 0001) and the readers that
//! turn XLSX and PPTX files into the Sheets and Slides models.
//!
//! [`package`] reads the zip container part by part under safety limits,
//! with content types and relationships. [`xlsx`] and [`pptx`] read the
//! parts this version models; everything else is left in the package
//! untouched. Nothing is written yet: the tabs are read-only. DOCX is not
//! read yet.
//!
//! Each format is a [`FormatCodec`]: bytes in, model out. Saving will be its
//! other half, writing back untouched parts as they were read.

pub mod error;
pub mod package;
pub mod pptx;
pub mod xlsx;
mod xml;

pub use error::{Error, ErrorKind, Result};
pub use package::{Kind, Limits, Package};

/// One file format and the model it reads into.
pub trait FormatCodec {
    type Model;
    /// File name extensions, lower case, without the dot.
    const EXTENSIONS: &'static [&'static str];
    fn read(&self, bytes: Vec<u8>) -> Result<Self::Model>;
}

/// `.xlsx` and `.xlsm` into [`aster_sheets::Workbook`]. Macros are not run
/// or read.
pub struct Xlsx;

impl FormatCodec for Xlsx {
    type Model = aster_sheets::Workbook;
    const EXTENSIONS: &'static [&'static str] = &["xlsx", "xlsm"];
    fn read(&self, bytes: Vec<u8>) -> Result<Self::Model> {
        xlsx::read(bytes)
    }
}

/// `.pptx` and `.pptm` into [`aster_slides::Presentation`].
pub struct Pptx;

impl FormatCodec for Pptx {
    type Model = aster_slides::Presentation;
    const EXTENSIONS: &'static [&'static str] = &["pptx", "pptm"];
    fn read(&self, bytes: Vec<u8>) -> Result<Self::Model> {
        pptx::read(bytes)
    }
}
