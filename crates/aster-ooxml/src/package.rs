//! The OPC package: a zip file of parts, the content types that say what
//! each part is, and the relationships between them.
//!
//! Parts are read when asked for, not all at once, so the pictures and media
//! of a large presentation are never inflated just to show its text. Every
//! read is checked against [`Limits`]: a damaged or hostile file can claim
//! small sizes and inflate to gigabytes, so sizes are counted while reading,
//! not taken from the zip directory.
//!
//! Following ADR 0001, saving will write untouched parts back as they were
//! read; this version only reads.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use crate::error::{Error, Result};
use crate::xml::{Ev, XmlReader};

/// What a package may hold before it is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_parts: usize,
    /// Uncompressed bytes of one part.
    pub max_part: u64,
    /// Uncompressed bytes of all parts read, together.
    pub max_total: u64,
    /// Uncompressed to compressed size, for parts over 1 MiB. Ordinary XML
    /// compresses 10 to 100 times; zip bombs a million.
    pub max_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parts: 20_000,
            max_part: 1 << 30,
            max_total: 2 << 30,
            max_ratio: 1_000,
        }
    }
}

/// The document kinds, told apart by the content type of the main part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Docx,
    Xlsx,
    Pptx,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Docx => "a Word document",
            Kind::Xlsx => "an Excel workbook",
            Kind::Pptx => "a PowerPoint presentation",
        }
    }

    fn of_content_type(content_type: &str) -> Option<Kind> {
        let ct = content_type.to_ascii_lowercase();
        if ct.contains("spreadsheetml") || ct.contains("ms-excel") {
            Some(Kind::Xlsx)
        } else if ct.contains("presentationml") || ct.contains("ms-powerpoint") {
            Some(Kind::Pptx)
        } else if ct.contains("wordprocessingml") || ct.contains("ms-word") {
            Some(Kind::Docx)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    /// The last segment of the type URI: `worksheet`, `slideLayout`. The
    /// same in Transitional and Strict files.
    pub kind: String,
    /// The part it points at, resolved to a package name without a leading
    /// slash. `None` for external targets.
    pub target: Option<String>,
}

pub struct Package {
    archive: ZipArchive<Cursor<Vec<u8>>>,
    /// Lower-cased part name to zip index: part names compare without case.
    index: HashMap<String, usize>,
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
    limits: Limits,
    read_total: u64,
}

/// The first bytes of an OLE compound file.
const COMPOUND_FILE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

impl Package {
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        Self::open_with(bytes, Limits::default())
    }

    pub fn open_with(bytes: Vec<u8>, limits: Limits) -> Result<Self> {
        if bytes.starts_with(COMPOUND_FILE) {
            return Err(Error::LegacyOrEncrypted);
        }
        let archive =
            ZipArchive::new(Cursor::new(bytes)).map_err(|err| Error::NotZip(err.to_string()))?;
        if archive.len() > limits.max_parts {
            return Err(Error::TooLarge(format!("{} parts", archive.len())));
        }
        let mut index = HashMap::with_capacity(archive.len());
        for i in 0..archive.len() {
            if let Some(name) = archive.name_for_index(i) {
                let name = name.trim_start_matches('/').to_ascii_lowercase();
                if !name.is_empty() && !name.ends_with('/') {
                    index.insert(name, i);
                }
            }
        }
        let mut package = Self {
            archive,
            index,
            defaults: HashMap::new(),
            overrides: HashMap::new(),
            limits,
            read_total: 0,
        };
        package.read_content_types()?;
        Ok(package)
    }

    pub fn has(&self, name: &str) -> bool {
        self.index.contains_key(&name.to_ascii_lowercase())
    }

    /// The uncompressed bytes of a part.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let key = name.trim_start_matches('/').to_ascii_lowercase();
        let &at = self
            .index
            .get(&key)
            .ok_or_else(|| Error::Broken(format!("{name}: missing")))?;
        let mut entry = self
            .archive
            .by_index(at)
            .map_err(|err| Error::broken(name, err))?;
        let declared = entry.size();
        let compressed = entry.compressed_size().max(1);
        let limits = self.limits;
        if declared > limits.max_part {
            return Err(Error::TooLarge(format!("{name}: {declared} bytes")));
        }
        if declared > 1 << 20 && declared / compressed > limits.max_ratio {
            return Err(Error::TooLarge(format!(
                "{name}: compressed {}:1",
                declared / compressed
            )));
        }
        let budget = limits
            .max_part
            .min(limits.max_total.saturating_sub(self.read_total));
        let mut data = Vec::with_capacity(declared.min(budget) as usize);
        (&mut entry)
            .take(budget + 1)
            .read_to_end(&mut data)
            .map_err(|err| Error::broken(name, err))?;
        if data.len() as u64 > budget {
            return Err(Error::TooLarge(format!(
                "{name}: more than {budget} bytes uncompressed"
            )));
        }
        // A part over 1 MiB that inflates far beyond its compressed size,
        // whatever the directory claimed.
        if data.len() as u64 > 1 << 20 && data.len() as u64 / compressed > limits.max_ratio {
            return Err(Error::TooLarge(format!("{name}: compressed too well")));
        }
        self.read_total += data.len() as u64;
        Ok(data)
    }

    fn read_content_types(&mut self) -> Result<()> {
        const NAME: &str = "[Content_Types].xml";
        if !self.has(NAME) {
            return Err(Error::WrongKind {
                expected: "an Office document",
                found: "a zip file without [Content_Types].xml".to_owned(),
            });
        }
        let bytes = self.read(NAME)?;
        let mut reader = XmlReader::new(&bytes, NAME);
        loop {
            match reader.next()? {
                Ev::Open(tag) if tag.is("Default") => {
                    if let (Some(ext), Some(ct)) = (tag.attr("Extension"), tag.attr("ContentType"))
                    {
                        self.defaults.insert(ext.to_ascii_lowercase(), ct);
                    }
                }
                Ev::Open(tag) if tag.is("Override") => {
                    if let (Some(part), Some(ct)) = (tag.attr("PartName"), tag.attr("ContentType"))
                    {
                        let part = percent_decode(part.trim_start_matches('/'));
                        self.overrides.insert(part.to_ascii_lowercase(), ct);
                    }
                }
                Ev::Eof => return Ok(()),
                _ => {}
            }
        }
    }

    pub fn content_type(&self, name: &str) -> Option<&str> {
        let key = name.trim_start_matches('/').to_ascii_lowercase();
        if let Some(ct) = self.overrides.get(&key) {
            return Some(ct);
        }
        let ext = key.rsplit_once('.').map(|(_, ext)| ext)?;
        self.defaults.get(ext).map(String::as_str)
    }

    /// The relationships of `source` (`""` for the package itself). A part
    /// without a relationships part has none.
    pub fn relationships(&mut self, source: &str) -> Result<Vec<Relationship>> {
        let rels = rels_name(source);
        if !self.has(&rels) {
            return Ok(Vec::new());
        }
        let bytes = self.read(&rels)?;
        let mut reader = XmlReader::new(&bytes, &rels);
        let mut out = Vec::new();
        loop {
            match reader.next()? {
                Ev::Open(tag) if tag.is("Relationship") => {
                    let (Some(id), Some(kind), Some(target)) =
                        (tag.attr("Id"), tag.attr("Type"), tag.attr("Target"))
                    else {
                        continue;
                    };
                    let external = tag
                        .attr("TargetMode")
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                    out.push(Relationship {
                        id,
                        kind: kind.rsplit('/').next().unwrap_or("").to_owned(),
                        target: (!external).then(|| resolve(source, &target)),
                    });
                }
                Ev::Eof => return Ok(out),
                _ => {}
            }
        }
    }

    /// The main part and what kind of document it makes the package.
    pub fn main_part(&mut self) -> Result<(String, Kind)> {
        let main = self
            .relationships("")?
            .into_iter()
            .find(|rel| rel.kind == "officeDocument")
            .and_then(|rel| rel.target)
            .ok_or_else(|| Error::WrongKind {
                expected: "an Office document",
                found: "a package without a main document".to_owned(),
            })?;
        let kind = self
            .content_type(&main)
            .and_then(Kind::of_content_type)
            .ok_or_else(|| Error::WrongKind {
                expected: "an Office document",
                found: format!(
                    "a main part of type {}",
                    self.content_type(&main).unwrap_or("unknown")
                ),
            })?;
        Ok((main, kind))
    }

    /// The main part, if the package is of `kind`.
    pub fn expect(&mut self, kind: Kind) -> Result<String> {
        let (main, found) = self.main_part()?;
        if found != kind {
            return Err(Error::WrongKind {
                expected: kind.name(),
                found: found.name().to_owned(),
            });
        }
        Ok(main)
    }
}

/// `xl/workbook.xml` → `xl/_rels/workbook.xml.rels`; `""` → `_rels/.rels`.
pub fn rels_name(source: &str) -> String {
    match source.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{source}.rels"),
    }
}

/// A relationship target, relative to the folder of its source part, as a
/// package name.
pub fn resolve(source: &str, target: &str) -> String {
    let target = percent_decode(target);
    let target = target.split('#').next().unwrap_or("");
    let mut segments: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
        dir.split('/').filter(|s| !s.is_empty()).collect()
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

fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("ASCII");
            out.push(u8::from_str_radix(hex, 16).expect("hex digits"));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_against_the_source_folder() {
        assert_eq!(
            resolve("xl/workbook.xml", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve("xl/workbook.xml", "/xl/styles.xml"),
            "xl/styles.xml"
        );
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "../slideLayouts/slideLayout2.xml"),
            "ppt/slideLayouts/slideLayout2.xml"
        );
        assert_eq!(resolve("", "xl/workbook.xml"), "xl/workbook.xml");
        assert_eq!(resolve("a/b.xml", "./c%20d.xml"), "a/c d.xml");
        assert_eq!(resolve("a/b.xml", "../../../x.xml"), "x.xml");
        assert_eq!(resolve("a/b.xml", "%E8%A1%A8.xml"), "a/表.xml");
    }

    #[test]
    fn rels_names() {
        assert_eq!(rels_name(""), "_rels/.rels");
        assert_eq!(rels_name("xl/workbook.xml"), "xl/_rels/workbook.xml.rels");
    }

    #[test]
    fn not_a_zip_and_legacy_files_are_told_apart() {
        assert!(matches!(
            Package::open(b"hello".to_vec()),
            Err(Error::NotZip(_))
        ));
        let mut legacy = COMPOUND_FILE.to_vec();
        legacy.extend_from_slice(&[0; 512]);
        assert!(matches!(
            Package::open(legacy),
            Err(Error::LegacyOrEncrypted)
        ));
        assert!(matches!(Package::open(Vec::new()), Err(Error::NotZip(_))));
    }
}
