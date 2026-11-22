//! An OPC package as the list of its parts: names and uncompressed bytes, in
//! the order the zip file stores them. Everything the gate measures is about
//! part bytes, never about the zip container around them.

use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// The three document kinds, told apart by the main part they carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Docx,
    Xlsx,
    Pptx,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Docx, Kind::Xlsx, Kind::Pptx];

    pub fn extension(self) -> &'static str {
        match self {
            Kind::Docx => "docx",
            Kind::Xlsx => "xlsx",
            Kind::Pptx => "pptx",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.extension().eq_ignore_ascii_case(extension))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Without a leading slash, as zip stores it: `word/document.xml`.
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    pub parts: Vec<Part>,
}

impl Package {
    pub fn read(bytes: &[u8]) -> Result<Package, String> {
        let mut archive =
            ZipArchive::new(Cursor::new(bytes)).map_err(|err| format!("not a zip file: {err}"))?;
        let mut parts = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|err| format!("zip entry {index}: {err}"))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().trim_start_matches('/').to_owned();
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut data)
                .map_err(|err| format!("{name}: {err}"))?;
            if parts.iter().any(|part: &Part| part.name == name) {
                return Err(format!("{name} is stored twice"));
            }
            parts.push(Part { name, data });
        }
        Ok(Package { parts })
    }

    /// Deterministic: fixed timestamps, parts in the order they are held.
    pub fn write(&self) -> Result<Vec<u8>, String> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for part in &self.parts {
            writer
                .start_file(part.name.as_str(), options)
                .map_err(|err| format!("{}: {err}", part.name))?;
            writer
                .write_all(&part.data)
                .map_err(|err| format!("{}: {err}", part.name))?;
        }
        let cursor = writer.finish().map_err(|err| err.to_string())?;
        Ok(cursor.into_inner())
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|part| part.name == name)
            .map(|part| part.data.as_slice())
    }

    pub fn set(&mut self, name: &str, data: Vec<u8>) {
        match self.parts.iter_mut().find(|part| part.name == name) {
            Some(part) => part.data = data,
            None => self.parts.push(Part {
                name: name.to_owned(),
                data,
            }),
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().map(|part| part.name.as_str())
    }
}

/// Whether a part holds XML, judged by its name.
pub fn is_xml(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".xml") || lower.ends_with(".rels") || lower.ends_with(".vml")
}

/// The relationships part that belongs to `part`; `""` stands for the package.
pub fn rels_name(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None if part.is_empty() => "_rels/.rels".to_owned(),
        None => format!("_rels/{part}.rels"),
    }
}

/// The part a relationships part describes; `""` for the package itself.
pub fn rels_source(rels: &str) -> Option<String> {
    let (dir, file) = match rels.rsplit_once('/') {
        Some((dir, file)) => (dir, file),
        None => ("", rels),
    };
    let file = file.strip_suffix(".rels")?;
    let dir = dir.strip_suffix("_rels")?.trim_end_matches('/');
    Some(match (dir.is_empty(), file.is_empty()) {
        (_, true) => dir.to_owned(),
        (true, false) => file.to_owned(),
        (false, false) => format!("{dir}/{file}"),
    })
}

/// Resolve a relationship target against the part that owns the relationship.
pub fn resolve_target(source: &str, target: &str) -> String {
    let mut segments: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut dir: Vec<&str> = source.split('/').collect();
        dir.pop();
        dir
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_survives_writing_and_reading() {
        let package = Package {
            parts: vec![
                Part {
                    name: "[Content_Types].xml".into(),
                    data: b"<Types/>".to_vec(),
                },
                Part {
                    name: "word/media/image1.png".into(),
                    data: vec![0, 159, 146, 150, 255],
                },
            ],
        };
        let bytes = package.write().unwrap();
        assert_eq!(Package::read(&bytes).unwrap(), package);
        assert_eq!(package.write().unwrap(), bytes, "writing is deterministic");
    }

    #[test]
    fn relationship_part_names() {
        assert_eq!(rels_name(""), "_rels/.rels");
        assert_eq!(
            rels_name("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
        assert_eq!(rels_source("_rels/.rels").as_deref(), Some(""));
        assert_eq!(
            rels_source("word/_rels/document.xml.rels").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(rels_source("word/document.xml"), None);
    }

    #[test]
    fn targets_resolve_relative_to_the_source_part() {
        assert_eq!(resolve_target("", "word/document.xml"), "word/document.xml");
        assert_eq!(
            resolve_target("word/document.xml", "styles.xml"),
            "word/styles.xml"
        );
        assert_eq!(
            resolve_target("word/document.xml", "../customXml/item1.xml"),
            "customXml/item1.xml"
        );
        assert_eq!(
            resolve_target("xl/worksheets/sheet1.xml", "/xl/media/image1.png"),
            "xl/media/image1.png"
        );
    }
}
