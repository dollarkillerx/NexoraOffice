//! ooxmlsdk 0.15.0: types generated from the Open XML SDK schemas.
//!
//! A part is parsed only when its root is asked for, and only parsed parts are
//! written from the model; the rest are copied. So the edits here reach the
//! target through the part lists, which follow relationship order, and never
//! read the workbook or presentation root.
//!
//! That is the best case. An editor that uses these types as its model reads
//! every part it understands, so there is a second candidate that opens the
//! package with every part parsed. It shows what the typed model itself keeps.

use std::io::Cursor;

use ooxmlsdk::parts::presentation_document::PresentationDocument;
use ooxmlsdk::parts::spreadsheet_document::SpreadsheetDocument;
use ooxmlsdk::parts::wordprocessing_document::WordprocessingDocument;
use ooxmlsdk::schemas::{a, p, w, x};
use ooxmlsdk::sdk::{OpenSettings, PackageOpenMode};

use super::{Candidate, Edited, MARKER, NUMBER};
use crate::package::Kind;

/// Parses only what the edit touches.
pub struct OoxmlSdk;
/// Parses every part on opening.
pub struct OoxmlSdkEager;

fn settings(eager: bool) -> OpenSettings {
    OpenSettings {
        open_mode: if eager {
            PackageOpenMode::Eager
        } else {
            PackageOpenMode::Lazy
        },
        ..OpenSettings::default()
    }
}

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

fn docx(bytes: &[u8], edit: bool, eager: bool) -> Result<Vec<u8>, String> {
    let mut package =
        WordprocessingDocument::new_with_settings(Cursor::new(bytes.to_vec()), settings(eager))
            .map_err(text)?;
    if edit {
        let main = package.main_document_part().map_err(text)?;
        let document = main.root_element_mut(&mut package).map_err(text)?;
        let body = document.body.as_mut().ok_or("the document has no body")?;
        let target = body
            .body_choice
            .iter_mut()
            .filter_map(|choice| match choice {
                w::BodyChoice::Paragraph(paragraph) => Some(paragraph),
                _ => None,
            })
            .flat_map(|paragraph| paragraph.paragraph_choice.iter_mut())
            .filter_map(|choice| match choice {
                w::ParagraphChoice::WRun(run) => Some(run),
                _ => None,
            })
            .flat_map(|run| run.run_choice.iter_mut())
            .find_map(|choice| match choice {
                w::RunChoice::Text(found)
                    if found
                        .0
                        .xml_content
                        .as_deref()
                        .is_some_and(|t| !t.trim().is_empty()) =>
                {
                    Some(found)
                }
                _ => None,
            })
            .ok_or("no run with text directly in a body paragraph")?;
        target.0.xml_content = Some(MARKER.to_owned());
    }
    package.to_package_bytes().map_err(text)
}

fn xlsx(bytes: &[u8], edit: bool, eager: bool) -> Result<Vec<u8>, String> {
    let mut package =
        SpreadsheetDocument::new_with_settings(Cursor::new(bytes.to_vec()), settings(eager))
            .map_err(text)?;
    if edit {
        let workbook = package.workbook_part().map_err(text)?;
        let sheet = workbook
            .worksheet_parts(&package)
            .next()
            .ok_or("the workbook has no worksheet")?;
        let worksheet = sheet.root_element_mut(&mut package).map_err(text)?;
        let cell = worksheet
            .sheet_data
            .row
            .iter_mut()
            .flat_map(|row| row.cell.iter_mut())
            .find(|cell| {
                matches!(cell.data_type, None | Some(x::CellValues::Number))
                    && cell.cell_formula.is_none()
                    && cell.cell_value.is_some()
            })
            .ok_or("the first worksheet holds no plain number")?;
        if let Some(value) = cell.cell_value.as_mut() {
            value.0.xml_content = Some(NUMBER.to_owned());
        }
    }
    package.to_package_bytes().map_err(text)
}

fn pptx(bytes: &[u8], edit: bool, eager: bool) -> Result<Vec<u8>, String> {
    let mut package =
        PresentationDocument::new_with_settings(Cursor::new(bytes.to_vec()), settings(eager))
            .map_err(text)?;
    if edit {
        let presentation = package.presentation_part().map_err(text)?;
        let slide = presentation
            .slide_parts(&package)
            .next()
            .ok_or("the presentation has no slide")?;
        let root = slide.root_element_mut(&mut package).map_err(text)?;
        let run = root
            .common_slide_data
            .shape_tree
            .shape_tree_choice
            .iter_mut()
            .filter_map(|choice| match choice {
                p::ShapeTreeChoice::Shape(shape) => shape.text_body.as_mut(),
                _ => None,
            })
            .flat_map(|body| body.paragraph.iter_mut())
            .flat_map(|paragraph| paragraph.paragraph_choice.iter_mut())
            .find_map(|choice| match choice {
                a::ParagraphChoice::Run(run) if !run.text.trim().is_empty() => Some(run),
                _ => None,
            })
            .ok_or("the first slide has no text run")?;
        run.text = MARKER.into();
    }
    package.to_package_bytes().map_err(text)
}

fn convert(kind: Kind, bytes: &[u8], edit: bool, eager: bool) -> Result<Vec<u8>, String> {
    match kind {
        Kind::Docx => docx(bytes, edit, eager),
        Kind::Xlsx => xlsx(bytes, edit, eager),
        Kind::Pptx => pptx(bytes, edit, eager),
    }
}

impl Candidate for OoxmlSdk {
    fn name(&self) -> &'static str {
        "ooxmlsdk"
    }

    fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
        convert(kind, bytes, false, false)
    }

    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
        let bytes = convert(kind, bytes, true, false)?;
        Ok(Edited { bytes, note: None })
    }
}

impl Candidate for OoxmlSdkEager {
    fn name(&self) -> &'static str {
        "ooxmlsdk/all"
    }

    fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
        convert(kind, bytes, false, true)
    }

    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
        let bytes = convert(kind, bytes, true, true)?;
        Ok(Edited { bytes, note: None })
    }
}
