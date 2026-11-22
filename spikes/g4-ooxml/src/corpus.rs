//! The files the candidates are run on.
//!
//! The built-in samples live in `corpus/` as plain directories of indented XML,
//! so that they can be read and reviewed, and are packed into packages here.
//! They were written by hand for this gate; they are not output of Microsoft
//! Office. Any directory of real `.docx`, `.xlsx` and `.pptx` files can be used
//! instead or in addition.

use std::path::{Path, PathBuf};

use crate::package::{Kind, Package, Part, is_xml};
use crate::xml::minify;

#[derive(Debug, Clone)]
pub struct Sample {
    pub name: String,
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|err| err.to_string())?.path();
        if path.is_dir() {
            files_under(&path, found)?;
        } else {
            found.push(path);
        }
    }
    Ok(())
}

/// Content types first, then the package relationships, then by name: the
/// order office applications write.
fn rank(name: &str) -> (u8, &str) {
    match name {
        "[Content_Types].xml" => (0, name),
        "_rels/.rels" => (1, name),
        _ => (2, name),
    }
}

/// Pack a directory tree into a package, compacting its XML on the way.
pub fn pack(dir: &Path) -> Result<Vec<u8>, String> {
    let mut files = Vec::new();
    files_under(dir, &mut files)?;
    let mut parts = Vec::with_capacity(files.len());
    for path in files {
        let name = path
            .strip_prefix(dir)
            .map_err(|err| err.to_string())?
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if name.ends_with(".DS_Store") {
            continue;
        }
        let raw = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let data = if is_xml(&name) {
            minify(&raw).map_err(|why| format!("{name}: {why}"))?
        } else {
            raw
        };
        parts.push(Part { name, data });
    }
    parts.sort_by(|left, right| rank(&left.name).cmp(&rank(&right.name)));
    Package { parts }.write()
}

/// The three hand-written samples.
pub fn built_in() -> Result<Vec<Sample>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    Kind::ALL
        .into_iter()
        .map(|kind| {
            Ok(Sample {
                name: format!("aster.{}", kind.extension()),
                kind,
                bytes: pack(&root.join(kind.extension()))?,
            })
        })
        .collect()
}

/// Every office file below `dir`, by path.
pub fn from_dir(dir: &Path) -> Result<Vec<Sample>, String> {
    let mut files = Vec::new();
    files_under(dir, &mut files)?;
    files.sort();
    let mut samples = Vec::new();
    for path in files {
        let kind = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(Kind::from_extension);
        let Some(kind) = kind else { continue };
        // Lock files that Word leaves next to an open document.
        let name = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if name
            .rsplit('/')
            .next()
            .is_some_and(|file| file.starts_with("~$"))
        {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        samples.push(Sample { name, kind, bytes });
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate;

    #[test]
    fn the_built_in_samples_are_consistent_packages() {
        let samples = built_in().unwrap();
        assert_eq!(samples.len(), 3);
        for sample in samples {
            let package = Package::read(&sample.bytes).unwrap();
            assert_eq!(validate(&package), Vec::<String>::new(), "{}", sample.name);
            assert_eq!(package.parts[0].name, "[Content_Types].xml");
            assert_eq!(package.parts[1].name, "_rels/.rels");
        }
    }

    /// An element written as a start tag, a line break and an end tag would
    /// be packed with that line break as its content, which no office
    /// application writes. Such elements must be written `<x/>` in `corpus/`.
    #[test]
    fn no_sample_part_has_layout_white_space_as_content() {
        for sample in built_in().unwrap() {
            for part in Package::read(&sample.bytes).unwrap().parts {
                if !is_xml(&part.name) {
                    continue;
                }
                let text = String::from_utf8(part.data).unwrap();
                for (index, _) in text.match_indices("</") {
                    let before = &text[..index];
                    let content = &before[before.rfind('>').map_or(0, |at| at + 1)..];
                    assert!(
                        !(content.contains('\n') && content.trim().is_empty()),
                        "{} in {}: an element holds only a line break",
                        part.name,
                        sample.name
                    );
                }
            }
        }
    }

    #[test]
    fn packing_is_reproducible() {
        let first = built_in().unwrap();
        let second = built_in().unwrap();
        for (one, two) in first.iter().zip(&second) {
            assert_eq!(one.bytes, two.bytes, "{}", one.name);
        }
    }

    #[test]
    fn the_samples_hold_what_libraries_tend_to_drop() {
        let samples = built_in().unwrap();
        let part = |kind: Kind, name: &str| {
            let sample = samples.iter().find(|sample| sample.kind == kind).unwrap();
            let package = Package::read(&sample.bytes).unwrap();
            String::from_utf8(package.get(name).unwrap().to_vec()).unwrap()
        };
        let document = part(Kind::Docx, "word/document.xml");
        for needle in [
            "mc:AlternateContent",
            "aster:marker",
            "w:ins ",
            "w:sdt",
            "m:oMath",
            "w:ruby",
        ] {
            assert!(document.contains(needle), "docx lacks {needle}");
        }
        let sheet = part(Kind::Xlsx, "xl/worksheets/sheet1.xml");
        for needle in [
            "x14:sparklineGroups",
            "dataValidation ",
            "conditionalFormatting",
            "t=\"shared\"",
        ] {
            assert!(sheet.contains(needle), "xlsx lacks {needle}");
        }
        let slide = part(Kind::Pptx, "ppt/slides/slide1.xml");
        for needle in ["mc:AlternateContent", "p14:prism", "p14:creationId"] {
            assert!(slide.contains(needle), "pptx lacks {needle}");
        }
    }
}
