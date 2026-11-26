//! List labels: counting through the paragraphs of a document and writing
//! the numbers in each level's format.

use crate::{Document, NumberFormat};

/// See [`Document::renumber`].
///
/// Not written yet: labels are left as they are.
pub fn renumber(document: &mut Document) {
    let _ = document;
}

/// `n` written in `format`: `3` as `3`, `c`, `iii`, `③`, `三`. Formats
/// without numbers (bullets, none) give an empty string.
///
/// Not written yet: every format gives decimal digits.
pub fn format_number(n: u32, format: &NumberFormat) -> String {
    match format {
        NumberFormat::Bullet | NumberFormat::None => String::new(),
        _ => n.to_string(),
    }
}
