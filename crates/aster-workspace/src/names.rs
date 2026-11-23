//! Matching and ordering file names.

use std::cmp::Ordering;

use icu_normalizer::ComposingNormalizerBorrowed;

/// A name in the form names are compared in: composed (NFC) and lower case.
/// macOS often hands out names decomposed, so "が" typed by the user would
/// otherwise not match "が" read from the disk.
fn fold(text: &str) -> String {
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .to_lowercase()
}

/// What the sidebar's search field holds: file names containing it match,
/// ignoring case and Unicode normalisation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    folded: String,
}

impl Filter {
    /// `None` for an empty or blank field: then nothing is filtered.
    pub fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty()).then(|| Self { folded: fold(text) })
    }

    pub fn matches(&self, name: &str) -> bool {
        fold(name).contains(&self.folded)
    }
}

/// The order of names in a folder listing: ignoring case, with runs of digits
/// compared as numbers, so "第2章" comes before "第10章".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (fa, fb) = (fold(a), fold(b));
    let mut x = fa.chars().peekable();
    let mut y = fb.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let n = digits(&mut x);
                let m = digits(&mut y);
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let order = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

fn digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut out = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtering_ignores_case_and_normalisation() {
        let filter = Filter::new("  read ").unwrap();
        assert!(filter.matches("README.md"));
        assert!(!filter.matches("notes.md"));
        // "が" decomposed, as macOS may report it, against the composed form.
        let decomposed = "か\u{3099}いき.md";
        assert!(Filter::new("がい").unwrap().matches(decomposed));
        assert!(Filter::new("会议").unwrap().matches("周会议纪要.md"));
        assert!(Filter::new("").is_none());
        assert!(Filter::new("   ").is_none());
    }

    #[test]
    fn numbers_sort_by_value() {
        let mut names = vec![
            "第10章.md",
            "第2章.md",
            "第1章.md",
            "a.md",
            "B.md",
            "a02.md",
            "a1.md",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "a.md",
                "a1.md",
                "a02.md",
                "B.md",
                "第1章.md",
                "第2章.md",
                "第10章.md"
            ]
        );
    }

    #[test]
    fn the_order_is_total() {
        // Names equal when folded still come out in a fixed order.
        assert_eq!(natural_cmp("a.md", "A.md"), "a.md".cmp("A.md"));
        assert_eq!(natural_cmp("a1", "a01"), "a1".cmp("a01"));
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
    }
}
