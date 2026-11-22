//! office_oxide 0.1.17, through its editing API. Its other save path rebuilds
//! the file from an intermediate representation and is lossy by design; it is
//! not what an editor would use and is not tested here.

use std::io::Cursor;

use office_oxide::DocumentFormat;
use office_oxide::edit::EditableDocument;
use office_oxide::xlsx::edit::CellValue;

use super::{Candidate, Edited, MARKER, NUMBER, own};
use crate::package::Kind;

pub struct OfficeOxide;

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

fn open(kind: Kind, bytes: &[u8]) -> Result<EditableDocument, String> {
    let format = match kind {
        Kind::Docx => DocumentFormat::Docx,
        Kind::Xlsx => DocumentFormat::Xlsx,
        Kind::Pptx => DocumentFormat::Pptx,
    };
    EditableDocument::from_reader(Cursor::new(bytes.to_vec()), format).map_err(text)
}

fn save(document: &EditableDocument) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    document.write_to(&mut out).map_err(text)?;
    Ok(out.into_inner())
}

impl Candidate for OfficeOxide {
    fn name(&self) -> &'static str {
        "office_oxide"
    }

    fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
        save(&open(kind, bytes)?)
    }

    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
        let mut document = open(kind, bytes)?;
        let note = match kind {
            Kind::Xlsx => {
                let cell = own::first_number_cell(bytes)?;
                let number: f64 = NUMBER.parse().map_err(text)?;
                document
                    .set_cell(0, &cell, CellValue::Number(number))
                    .map_err(text)?;
                None
            }
            // Text is edited by search and replace over every part that holds
            // text, so the same words elsewhere change too.
            Kind::Docx | Kind::Pptx => {
                let find = own::first_text(kind, bytes)?;
                let count = document.replace_text(&find, MARKER).map_err(text)?;
                (count != 1).then(|| format!("search and replace changed {count} places"))
            }
        };
        Ok(Edited {
            bytes: save(&document)?,
            note,
        })
    }
}
