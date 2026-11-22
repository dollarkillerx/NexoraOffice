//! Structural checks on a package: the mistakes that make an office application
//! offer to "repair" a file, as far as they can be found without one.
//!
//! This is weaker than opening the file in Microsoft Office, which also checks
//! every part against its schema. A package that passes here can still be
//! refused there; a package that fails here will not open cleanly.

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::package::{Package, is_xml, rels_name, rels_source, resolve_target};
use crate::xml::{NS_RELATIONSHIPS, attribute_value, relationships, summarize};

const CONTENT_TYPES: &str = "[Content_Types].xml";

struct ContentTypes {
    defaults: BTreeSet<String>,
    overrides: BTreeSet<String>,
}

fn content_types(source: &[u8]) -> Result<ContentTypes, String> {
    let mut reader = Reader::from_reader(source);
    let mut types = ContentTypes {
        defaults: BTreeSet::new(),
        overrides: BTreeSet::new(),
    };
    loop {
        match reader.read_event().map_err(|err| err.to_string())? {
            Event::Start(element) | Event::Empty(element) => {
                let wanted = match element.local_name().as_ref() {
                    "Default" => "Extension",
                    "Override" => "PartName",
                    _ => continue,
                };
                for attribute in element.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|err| err.to_string())?;
                    if attribute.key.as_ref() != wanted {
                        continue;
                    }
                    let value = attribute_value(&attribute);
                    if wanted == "Extension" {
                        types.defaults.insert(value.to_ascii_lowercase());
                    } else {
                        types
                            .overrides
                            .insert(value.trim_start_matches('/').to_owned());
                    }
                }
            }
            Event::Eof => return Ok(types),
            _ => {}
        }
    }
}

/// Everything found wrong with the package, one line each, sorted.
pub fn validate(package: &Package) -> Vec<String> {
    let mut problems = BTreeSet::new();
    let names: BTreeSet<&str> = package.names().collect();

    match package.get(CONTENT_TYPES).map(content_types) {
        None => {
            problems.insert(format!("{CONTENT_TYPES} is missing"));
        }
        Some(Err(why)) => {
            problems.insert(format!("{CONTENT_TYPES}: {why}"));
        }
        Some(Ok(types)) => {
            for name in names.iter().filter(|name| **name != CONTENT_TYPES) {
                let extension = name
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_ascii_lowercase())
                    .unwrap_or_default();
                if !types.overrides.contains(*name) && !types.defaults.contains(&extension) {
                    problems.insert(format!("{name} has no content type"));
                }
            }
        }
    }

    // Relationship ids each part declares, to check the ids its XML uses.
    let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut has_main_part = false;
    for name in names.iter().filter(|name| name.ends_with(".rels")) {
        let Some(source) = rels_source(name) else {
            problems.insert(format!("{name} is not where a relationships part belongs"));
            continue;
        };
        if !source.is_empty() && !names.contains(source.as_str()) {
            problems.insert(format!("{name} describes {source}, which does not exist"));
        }
        let entries = match relationships(package.get(name).unwrap_or_default()) {
            Ok(entries) => entries,
            Err(why) => {
                problems.insert(format!("{name}: {why}"));
                continue;
            }
        };
        let ids = declared.entry(source.clone()).or_default();
        for entry in entries {
            if !ids.insert(entry.id.clone()) {
                problems.insert(format!(
                    "{name}: relationship id {} is used twice",
                    entry.id
                ));
            }
            if entry.external {
                continue;
            }
            let target = resolve_target(&source, &entry.target);
            if !names.contains(target.as_str()) {
                problems.insert(format!(
                    "{name}: {} points at {target}, which does not exist",
                    entry.id
                ));
            } else if source.is_empty() && entry.kind.ends_with("/officeDocument") {
                has_main_part = true;
            }
        }
    }
    if !names.contains(rels_name("").as_str()) {
        problems.insert("_rels/.rels is missing".to_owned());
    } else if !has_main_part {
        problems.insert("_rels/.rels names no main document part".to_owned());
    }

    let marker = format!(" @{{{NS_RELATIONSHIPS}}}");
    for name in names.iter().filter(|name| is_xml(name)) {
        let summary = match summarize(package.get(name).unwrap_or_default()) {
            Ok(summary) => summary,
            Err(why) => {
                problems.insert(format!("{name} is not well-formed: {why}"));
                continue;
            }
        };
        for problem in &summary.problems {
            problems.insert(format!("{name}: {problem}"));
        }
        // `r:id`, `r:embed` and the like must name a relationship of this part.
        let ids = declared.get(*name);
        for key in summary
            .attributes
            .keys()
            .filter(|key| key.contains(&marker))
        {
            let Some((_, id)) = key.rsplit_once(" = ") else {
                continue;
            };
            if !id.is_empty() && !ids.is_some_and(|ids| ids.contains(id)) {
                problems.insert(format!(
                    "{name} uses relationship {id}, which it does not have"
                ));
            }
        }
    }
    problems.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Part;

    fn package(parts: &[(&str, &str)]) -> Package {
        Package {
            parts: parts
                .iter()
                .map(|(name, data)| Part {
                    name: (*name).to_owned(),
                    data: data.as_bytes().to_vec(),
                })
                .collect(),
        }
    }

    const TYPES: &str = r#"<Types><Default Extension="rels" ContentType="a"/><Default Extension="xml" ContentType="b"/></Types>"#;
    const ROOT: &str = r#"<Relationships><Relationship Id="rId1" Type="x/officeDocument" Target="word/document.xml"/></Relationships>"#;
    const DOC: &str = r#"<w:document xmlns:w="urn:w" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:link r:id="rId5"/></w:document>"#;
    const DOC_RELS: &str = r#"<Relationships><Relationship Id="rId5" Type="x/image" Target="media/a.png"/></Relationships>"#;

    #[test]
    fn a_consistent_package_has_no_problems() {
        let ok = package(&[
            (
                "[Content_Types].xml",
                r#"<Types><Default Extension="rels" ContentType="a"/><Default Extension="xml" ContentType="b"/><Default Extension="png" ContentType="c"/></Types>"#,
            ),
            ("_rels/.rels", ROOT),
            ("word/document.xml", DOC),
            ("word/_rels/document.xml.rels", DOC_RELS),
            ("word/media/a.png", "png"),
        ]);
        assert_eq!(validate(&ok), Vec::<String>::new());
    }

    #[test]
    fn each_kind_of_damage_is_named() {
        let broken = package(&[
            ("[Content_Types].xml", TYPES),
            ("_rels/.rels", ROOT),
            ("word/document.xml", DOC),
            ("word/_rels/document.xml.rels", DOC_RELS),
            ("word/_rels/gone.xml.rels", "<Relationships/>"),
            ("word/extra.bin", "?"),
            ("word/bad.xml", "<a><b></a>"),
        ]);
        let problems = validate(&broken).join("\n");
        assert!(
            problems.contains("word/extra.bin has no content type"),
            "{problems}"
        );
        assert!(
            problems.contains("points at word/media/a.png, which does not exist"),
            "{problems}"
        );
        assert!(
            problems.contains("describes word/gone.xml, which does not exist"),
            "{problems}"
        );
        assert!(
            problems.contains("word/bad.xml is not well-formed"),
            "{problems}"
        );
    }

    #[test]
    fn a_dangling_relationship_id_is_found() {
        let dangling = package(&[
            ("[Content_Types].xml", TYPES),
            ("_rels/.rels", ROOT),
            ("word/document.xml", DOC),
        ]);
        let problems = validate(&dangling).join("\n");
        assert!(
            problems.contains("uses relationship rId5, which it does not have"),
            "{problems}"
        );
    }

    #[test]
    fn a_package_without_a_main_part_is_refused() {
        let empty = package(&[
            ("[Content_Types].xml", TYPES),
            ("_rels/.rels", "<Relationships/>"),
        ]);
        assert!(
            validate(&empty)
                .join("\n")
                .contains("names no main document part")
        );
    }
}
