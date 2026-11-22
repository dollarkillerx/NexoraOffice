//! BetterOffice 0.5.1, through its three facade crates.

use betteroffice_docx::Document;
use betteroffice_pptx::{EditCtx, Presentation};
use betteroffice_xlsx::{CalculationOptions, CellRef, SheetId, Workbook};

use super::{Candidate, Edited, MARKER, NUMBER, own};
use crate::package::Kind;

pub struct BetterOffice;

fn text<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// The facade can replace the text of a paragraph only by its `w14:paraId`, and
/// only if the paragraph is empty or a single run of plain text. Try the
/// paragraphs in order and say how many it turned down first.
fn edit_docx(bytes: &[u8]) -> Result<Edited, String> {
    let mut document = Document::open(bytes).map_err(text)?;
    let ids: Vec<String> = document
        .paragraphs()
        .iter()
        .filter(|paragraph| {
            !betteroffice_docx::get_paragraph_text(paragraph)
                .trim()
                .is_empty()
        })
        .filter_map(|paragraph| paragraph.para_id.clone())
        .collect();
    if ids.is_empty() {
        return Err("no paragraph carries a w14:paraId, which the edit API needs".into());
    }
    let mut refused = Vec::new();
    for id in &ids {
        match document.replace_paragraph_text(id, MARKER) {
            Ok(_) => {
                let note = refused.first().map(|first: &String| {
                    format!(
                        "refused {} earlier paragraph(s), first with: {first}",
                        refused.len()
                    )
                });
                return Ok(Edited {
                    bytes: document.save().map_err(text)?,
                    note,
                });
            }
            Err(why) => refused.push(why.to_string()),
        }
    }
    Err(format!(
        "every paragraph was refused, the first with: {}",
        refused[0]
    ))
}

fn edit_xlsx(bytes: &[u8]) -> Result<Edited, String> {
    let cell = own::first_number_cell(bytes)?;
    let mut workbook = Workbook::open(bytes).map_err(text)?;
    let result = workbook
        .edit_cell(
            SheetId(0),
            CellRef::parse_a1(&cell).map_err(|err| format!("{cell}: {err:?}"))?,
            NUMBER,
            CalculationOptions::default(),
        )
        .map_err(text)?;
    if !result.applied {
        return Err(format!("the edit of {cell} was not applied"));
    }
    Ok(Edited {
        bytes: workbook.save().map_err(text)?,
        note: Some(format!(
            "recalculated: {} cell(s) changed",
            result.changed.len()
        )),
    })
}

/// Text is edited by offsets into a "story". Replace the first paragraph of the
/// first story that has text, keeping the style of its first run.
fn edit_pptx(bytes: &[u8]) -> Result<Edited, String> {
    let presentation = Presentation::open(bytes).map_err(text)?;
    let snapshot = presentation.snapshot().map_err(text)?;
    let story = snapshot
        .slides
        .iter()
        .flat_map(|slide| slide.shapes.iter())
        .flat_map(|shape| shape.text_stories.iter())
        .find(|story| {
            story.paragraphs.first().is_some_and(|paragraph| {
                paragraph.runs.iter().any(|run| !run.text.trim().is_empty())
            })
        })
        .ok_or("no shape with text")?;
    let first = &story.paragraphs[0];
    let length: u32 = first
        .runs
        .iter()
        .map(|run| run.text.encode_utf16().count() as u32)
        .sum();
    let style = first.runs[0].style.clone();
    let context = EditCtx::local("g4");
    presentation
        .delete_text(&context, &story.id, 0, length)
        .map_err(text)?;
    presentation
        .insert_text(&context, &story.id, 0, MARKER, &style)
        .map_err(text)?;
    Ok(Edited {
        bytes: presentation.save().map_err(text)?,
        note: None,
    })
}

impl Candidate for BetterOffice {
    fn name(&self) -> &'static str {
        "betteroffice"
    }

    fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
        match kind {
            Kind::Docx => Document::open(bytes).map_err(text)?.save().map_err(text),
            Kind::Xlsx => Workbook::open(bytes).map_err(text)?.save().map_err(text),
            Kind::Pptx => Presentation::open(bytes)
                .map_err(text)?
                .save()
                .map_err(text),
        }
    }

    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
        match kind {
            Kind::Docx => edit_docx(bytes),
            Kind::Xlsx => edit_xlsx(bytes),
            Kind::Pptx => edit_pptx(bytes),
        }
    }
}
