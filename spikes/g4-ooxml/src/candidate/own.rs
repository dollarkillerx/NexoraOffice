//! Our own minimal layer: the package is a list of parts kept as bytes, and an
//! edit rewrites one part as a stream of XML events in which everything except
//! the target passes through untouched.
//!
//! This is the approach the plan names as the fallback ("write the parsing
//! layer ourselves on quick-xml"), reduced to what the gate needs. It models
//! nothing, so it proves only that the fidelity target is reachable and shows
//! what the other candidates are measured against.

use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer};

use super::{Candidate, Edited, MARKER, NUMBER};
use crate::package::{Kind, Package, rels_name, resolve_target};
use crate::xml::{first_relationship_id, general_ref, relationships};

pub struct Own;

/// The part an edit goes into: the main document, the first worksheet in tab
/// order, or the first slide in show order.
pub fn target_part(kind: Kind, package: &Package) -> Result<String, String> {
    let root = relationships(
        package
            .get(&rels_name(""))
            .ok_or("no package relationships")?,
    )?;
    let main = root
        .iter()
        .find(|entry| entry.kind.ends_with("/officeDocument"))
        .map(|entry| resolve_target("", &entry.target))
        .ok_or("no main document part")?;
    let element = match kind {
        Kind::Docx => return Ok(main),
        Kind::Xlsx => "sheet",
        Kind::Pptx => "sldId",
    };
    let id = first_relationship_id(package.get(&main).ok_or("main part missing")?, element)?
        .ok_or_else(|| format!("{main} lists no {element}"))?;
    let entries = relationships(
        package
            .get(&rels_name(&main))
            .ok_or("the main part has no relationships")?,
    )?;
    entries
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| resolve_target(&main, &entry.target))
        .ok_or_else(|| format!("{main} has no relationship {id}"))
}

fn write(writer: &mut Writer<Vec<u8>>, event: Event) -> Result<(), String> {
    writer.write_event(event).map_err(|err| err.to_string())
}

/// Read events up to and including the end tag that closes `name`.
fn element_body<'a>(reader: &mut Reader<&'a [u8]>, name: &str) -> Result<Vec<Event<'a>>, String> {
    let mut events = Vec::new();
    let mut depth = 0_usize;
    loop {
        let event = reader.read_event().map_err(|err| err.to_string())?;
        match &event {
            Event::Start(start) if start.name().as_ref() == name => depth += 1,
            Event::End(end) if end.name().as_ref() == name => {
                if depth == 0 {
                    events.push(event);
                    return Ok(events);
                }
                depth -= 1;
            }
            Event::Eof => return Err("the part ends inside an element".into()),
            _ => {}
        }
        events.push(event);
    }
}

/// The content of the first `t` element that holds visible text: what the edit
/// scenario replaces. Libraries that edit by searching for text need it; in a
/// build without those libraries nothing calls it.
#[allow(dead_code)]
pub fn first_text(kind: Kind, bytes: &[u8]) -> Result<String, String> {
    let package = Package::read(bytes)?;
    let target = target_part(kind, &package)?;
    let mut reader = Reader::from_reader(package.get(&target).ok_or("target part missing")?);
    loop {
        match reader.read_event().map_err(|err| err.to_string())? {
            Event::Eof => return Err(format!("{target} holds no text")),
            Event::Start(start) if start.local_name().as_ref() == "t" => {
                let name = start.name().as_ref().to_owned();
                let mut text = String::new();
                for inner in element_body(&mut reader, &name)? {
                    match inner {
                        Event::Text(part) => text.push_str(&part.xml10_content()),
                        Event::CData(part) => text.push_str(&part.into_inner()),
                        Event::GeneralRef(reference) => text.push_str(&general_ref(&reference)),
                        _ => {}
                    }
                }
                if !text.trim().is_empty() {
                    return Ok(text);
                }
            }
            _ => {}
        }
    }
}

/// The reference (`B2`) of the cell the edit scenario changes: the first plain
/// number of the first worksheet. Used by the libraries that address cells.
#[allow(dead_code)]
pub fn first_number_cell(bytes: &[u8]) -> Result<String, String> {
    let package = Package::read(bytes)?;
    let target = target_part(Kind::Xlsx, &package)?;
    let mut reader = Reader::from_reader(package.get(&target).ok_or("target part missing")?);
    loop {
        match reader.read_event().map_err(|err| err.to_string())? {
            Event::Eof => return Err(format!("{target} holds no plain number")),
            Event::Start(start) if start.local_name().as_ref() == "c" => {
                let mut reference = None;
                let mut numeric = true;
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| err.to_string())?;
                    match attribute.key.as_ref() {
                        "r" => reference = Some(attribute.value.to_string()),
                        "t" if attribute.value.as_ref() != "n" => numeric = false,
                        _ => {}
                    }
                }
                let name = start.name().as_ref().to_owned();
                let body = element_body(&mut reader, &name)?;
                if numeric && is_plain_value(&body) {
                    return reference.ok_or_else(|| "the cell has no reference".to_owned());
                }
            }
            _ => {}
        }
    }
}

fn is_local(event: &Event, wanted: &str) -> bool {
    match event {
        Event::Start(inner) | Event::Empty(inner) => inner.local_name().as_ref() == wanted,
        _ => false,
    }
}

/// A cell body with a value and no formula.
fn is_plain_value(body: &[Event]) -> bool {
    let has_formula = body.iter().any(|inner| is_local(inner, "f"));
    let has_value = body
        .iter()
        .any(|inner| matches!(inner, Event::Start(_)) && is_local(inner, "v"));
    has_value && !has_formula
}

/// Replace the content of the first `t` element that holds visible text.
fn replace_first_text(source: &[u8], text: &str) -> Result<Option<Vec<u8>>, String> {
    let mut reader = Reader::from_reader(source);
    let mut writer = Writer::new(Vec::with_capacity(source.len() + text.len()));
    let mut done = false;
    loop {
        let event = reader.read_event().map_err(|err| err.to_string())?;
        match event {
            Event::Eof => break,
            Event::Start(start) if !done && start.local_name().as_ref() == "t" => {
                let name = start.name().as_ref().to_owned();
                let body = element_body(&mut reader, &name)?;
                let visible = body.iter().any(|inner| match inner {
                    Event::Text(text) => !text.clone().into_inner().trim().is_empty(),
                    Event::GeneralRef(_) | Event::CData(_) => true,
                    _ => false,
                });
                write(&mut writer, Event::Start(start))?;
                if visible {
                    done = true;
                    write(&mut writer, Event::Text(BytesText::new(text)))?;
                    let end = body
                        .into_iter()
                        .last()
                        .expect("the end tag is always there");
                    write(&mut writer, end)?;
                } else {
                    for inner in body {
                        write(&mut writer, inner)?;
                    }
                }
            }
            other => write(&mut writer, other)?,
        }
    }
    Ok(done.then(|| writer.into_inner()))
}

/// Replace the value of the first cell that holds a plain number: no type
/// attribute other than `n`, a `v` child and no formula.
fn replace_first_number(source: &[u8], number: &str) -> Result<Option<Vec<u8>>, String> {
    let mut reader = Reader::from_reader(source);
    let mut writer = Writer::new(Vec::with_capacity(source.len() + number.len()));
    let mut done = false;
    loop {
        let event = reader.read_event().map_err(|err| err.to_string())?;
        match event {
            Event::Eof => break,
            Event::Start(start) if !done && start.local_name().as_ref() == "c" => {
                let mut numeric = true;
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| err.to_string())?;
                    if attribute.key.as_ref() == "t" && attribute.value.as_ref() != "n" {
                        numeric = false;
                    }
                }
                let name = start.name().as_ref().to_owned();
                let body = element_body(&mut reader, &name)?;
                write(&mut writer, Event::Start(start))?;
                if !(numeric && is_plain_value(&body)) {
                    for inner in body {
                        write(&mut writer, inner)?;
                    }
                    continue;
                }
                done = true;
                let mut in_value = false;
                for inner in body {
                    let opens = matches!(inner, Event::Start(_)) && is_local(&inner, "v");
                    let closes =
                        matches!(&inner, Event::End(end) if end.local_name().as_ref() == "v");
                    if closes {
                        in_value = false;
                    }
                    if !in_value {
                        write(&mut writer, inner)?;
                    }
                    if opens {
                        in_value = true;
                        write(&mut writer, Event::Text(BytesText::new(number)))?;
                    }
                }
            }
            other => write(&mut writer, other)?,
        }
    }
    Ok(done.then(|| writer.into_inner()))
}

impl Candidate for Own {
    fn name(&self) -> &'static str {
        "own layer"
    }

    fn resave(&self, _kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
        Package::read(bytes)?.write()
    }

    fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
        let mut package = Package::read(bytes)?;
        let target = target_part(kind, &package)?;
        let source = package
            .get(&target)
            .ok_or_else(|| format!("{target} is missing"))?;
        let rewritten = match kind {
            Kind::Xlsx => replace_first_number(source, NUMBER)?,
            Kind::Docx | Kind::Pptx => replace_first_text(source, MARKER)?,
        };
        let data = rewritten.ok_or_else(|| format!("{target} holds nothing to edit"))?;
        package.set(&target, data);
        // Cached results of formulas that depend on the cell are now stale. A
        // real layer marks the workbook for recalculation; that belongs to G5.
        let note = (kind == Kind::Xlsx).then(|| "cached formula results are left stale".to_owned());
        Ok(Edited {
            bytes: package.write()?,
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_but_the_target_text_passes_through() {
        let source = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:p xmlns:w=\"urn:w\" ><w:r><w:t xml:space=\"preserve\"> </w:t></w:r><w:r><w:t>old &amp; gone</w:t></w:r><w:r><w:t>kept</w:t><x:y  xmlns:x='urn:x' a = \"1\" /></w:r></w:p>";
        let edited = replace_first_text(source.as_bytes(), "a < b")
            .unwrap()
            .unwrap();
        assert_eq!(
            String::from_utf8(edited).unwrap(),
            source.replace("old &amp; gone", "a &lt; b")
        );
    }

    #[test]
    fn the_targets_of_the_built_in_samples_are_the_expected_ones() {
        let samples = crate::corpus::built_in().unwrap();
        let of = |kind: Kind| samples.iter().find(|sample| sample.kind == kind).unwrap();
        assert_eq!(
            first_text(Kind::Docx, &of(Kind::Docx).bytes).unwrap(),
            "Aster Office 往返测试"
        );
        assert_eq!(
            first_text(Kind::Pptx, &of(Kind::Pptx).bytes).unwrap(),
            "Aster Office 往返测试"
        );
        assert_eq!(first_number_cell(&of(Kind::Xlsx).bytes).unwrap(), "B2");
    }

    #[test]
    fn a_part_without_text_is_left_alone() {
        assert_eq!(replace_first_text(b"<a><t> </t></a>", "x").unwrap(), None);
    }

    #[test]
    fn the_first_plain_number_is_replaced() {
        let source = r#"<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><f>A1</f><v>3</v></c><c r="C1" s="2"><v>1200</v></c><c r="D1"><v>7</v></c></row></sheetData>"#;
        let edited = replace_first_number(source.as_bytes(), "4242")
            .unwrap()
            .unwrap();
        assert_eq!(
            String::from_utf8(edited).unwrap(),
            source.replace("<v>1200</v>", "<v>4242</v>")
        );
    }
}
