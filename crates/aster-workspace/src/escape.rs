//! The line format of the files this crate keeps: one record per line, fields
//! separated by tabs. A path may contain tabs, line breaks and backslashes;
//! they are written as `\t`, `\n`, `\r` and `\\`.

use std::path::{Path, PathBuf};

/// A path as one field, or `None` if it is not valid Unicode. Such paths are
/// rare and are not remembered.
pub(crate) fn path(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    Some(out)
}

/// The path a field holds. `None` for an escape this format never writes.
pub(crate) fn unpath(field: &str) -> Option<PathBuf> {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            '\\' => '\\',
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            _ => return None,
        });
    }
    Some(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awkward_paths_survive_a_round_trip() {
        for text in [
            "/Users/某人/文稿/会议纪要.md",
            "/tmp/a\tb\nc\rd\\e.md",
            "C:\\Users\\山田\\書類\\報告.md",
            "/tmp/👨‍👩‍👧/x.txt",
            "",
        ] {
            let field = path(Path::new(text)).unwrap();
            assert!(!field.contains(['\t', '\n', '\r']), "{field:?}");
            assert_eq!(unpath(&field).unwrap(), PathBuf::from(text));
        }
    }

    #[test]
    fn unknown_escapes_are_refused() {
        assert_eq!(unpath("a\\x"), None);
        assert_eq!(unpath("a\\"), None);
    }
}
