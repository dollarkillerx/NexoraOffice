//! The engines under evaluation, each behind the same few operations.

use crate::workload::At;

#[cfg(feature = "betteroffice")]
mod betteroffice;
#[cfg(feature = "formualizer")]
mod formualizer;
#[cfg(feature = "ironcalc")]
mod ironcalc;
pub mod native;

/// What a cell holds after calculation.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// An error value, spelled as Excel shows it: `#DIV/0!`.
    Error(String),
    Empty,
    /// Anything else the engine reports.
    Other(String),
}

pub trait Engine {
    /// Calculate every formula. Called once, after loading.
    fn calculate_all(&mut self) -> Result<(), String>;

    /// Put a number into a cell and bring the workbook up to date. This is the
    /// operation the gate times.
    fn edit(&mut self, at: At, value: f64) -> Result<(), String>;

    fn value(&mut self, at: At) -> Result<Value, String>;

    /// Anything the engine said along the way that the numbers do not show,
    /// such as having given up on some cells.
    fn note(&self) -> Option<String> {
        None
    }
}

/// How an engine is opened: from the XLSX bytes, or, for the hand-written
/// baseline, from the name of the workload it was written for.
pub struct Source<'a> {
    pub workload: &'a str,
    pub scale: u32,
    /// Sheet names, in workbook order.
    pub sheets: Vec<&'static str>,
    pub xlsx: &'a [u8],
}

pub type Open = fn(&Source) -> Result<Box<dyn Engine>, String>;

pub struct Candidate {
    pub name: &'static str,
    /// One line on how it recalculates, as read from its code.
    pub about: &'static str,
    pub open: Open,
}

/// The candidates in this build. The first is not an engine: it is the same
/// arithmetic written by hand, to show how far the engines are from the floor.
pub fn all() -> Vec<Candidate> {
    #[allow(unused_mut)]
    let mut candidates = vec![native::CANDIDATE];
    #[cfg(feature = "ironcalc")]
    candidates.push(ironcalc::CANDIDATE);
    #[cfg(feature = "formualizer")]
    candidates.extend(formualizer::CANDIDATES);
    #[cfg(feature = "betteroffice")]
    candidates.extend(betteroffice::CANDIDATES);
    candidates
}
