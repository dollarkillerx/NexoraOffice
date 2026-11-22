//! The libraries under evaluation, each behind the same two operations.

use crate::package::Kind;

#[cfg(feature = "betteroffice")]
mod betteroffice;
#[cfg(feature = "office-oxide")]
mod office_oxide;
#[cfg(feature = "ooxmlsdk")]
mod ooxmlsdk;
pub mod own;

/// The text an edit writes into a DOCX paragraph or a PPTX shape.
pub const MARKER: &str = "EDITED 已修改";
/// The number an edit writes into an XLSX cell.
pub const NUMBER: &str = "4242";

/// The result of an edit, and what had to be said about it: for example that
/// the library refused the first paragraph and a later one was edited instead.
#[derive(Debug, Clone)]
pub struct Edited {
    pub bytes: Vec<u8>,
    pub note: Option<String>,
}

pub trait Candidate {
    fn name(&self) -> &'static str;

    /// Open the file and save it again without changing anything.
    fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String>;

    /// Make one small change and save: put [`MARKER`] into the first paragraph
    /// of a document or the first text of the first slide, or [`NUMBER`] into
    /// the first plain number of the first worksheet.
    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String>;
}

/// The candidates this build includes. The first is our own minimal layer, the
/// fallback the plan names and the baseline the others are read against.
pub fn all() -> Vec<Box<dyn Candidate>> {
    #[allow(unused_mut)]
    let mut candidates: Vec<Box<dyn Candidate>> = vec![Box::new(own::Own)];
    #[cfg(feature = "betteroffice")]
    candidates.push(Box::new(betteroffice::BetterOffice));
    #[cfg(feature = "ooxmlsdk")]
    candidates.push(Box::new(ooxmlsdk::OoxmlSdk));
    #[cfg(feature = "ooxmlsdk")]
    candidates.push(Box::new(ooxmlsdk::OoxmlSdkEager));
    #[cfg(feature = "office-oxide")]
    candidates.push(Box::new(office_oxide::OfficeOxide));
    candidates
}
