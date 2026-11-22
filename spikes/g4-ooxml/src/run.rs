//! Run every candidate on every sample in both scenarios and judge the result
//! against the gate's criterion: parts that were not modified come back byte
//! for byte, and nothing is lost from the one that was.

use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::candidate::{Candidate, MARKER, NUMBER};
use crate::compare::{Change, Comparison, Delta, compare};
use crate::corpus::Sample;
use crate::package::{Kind, Package};
use crate::validate::validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    Resave,
    Edit,
}

impl Scenario {
    pub const ALL: [Scenario; 2] = [Scenario::Resave, Scenario::Edit];

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Resave => "resave",
            Scenario::Edit => "edit",
        }
    }
}

/// From best to worst. A result gets the worst label that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Meets the criterion: untouched parts are byte-identical, nothing lost.
    Pass,
    /// Nothing is lost, but parts nobody touched were written out differently.
    Rewritten,
    /// Elements, attributes, text or whole parts of the original are gone.
    Lossy,
    /// The result has structural errors the original did not have.
    Invalid,
    /// The library refused, crashed, or the edit is not in what it wrote.
    Failed,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Rewritten => "rewritten",
            Verdict::Lossy => "LOSSY",
            Verdict::Invalid => "INVALID",
            Verdict::Failed => "FAILED",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tally {
    pub parts: usize,
    pub identical: usize,
    /// Other bytes, same content.
    pub equivalent: usize,
    /// Content reordered or added to, nothing removed.
    pub reshaped: usize,
    /// Something removed, or a binary part altered.
    pub damaged: usize,
    pub missing: usize,
    pub added: usize,
    pub lost_elements: u32,
    pub lost_attributes: u32,
    pub lost_text: u32,
    /// Attribute values altered to something that means something else.
    pub changed_attributes: u32,
    /// Every part that is not byte-identical is a relationships part or the
    /// content-type list: packaging, not document content.
    pub only_packaging_differs: bool,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub sample: String,
    pub kind: Kind,
    pub candidate: &'static str,
    pub scenario: Scenario,
    pub verdict: Verdict,
    pub tally: Tally,
    /// Why it failed, when it did.
    pub error: Option<String>,
    pub note: Option<String>,
    /// Structural errors the result has and the original has not.
    pub problems: Vec<String>,
    /// Parts the edit landed in.
    pub edited: Vec<String>,
    pub comparison: Option<Comparison>,
}

fn carries_the_edit(kind: Kind, delta: &Delta) -> bool {
    let wanted = match kind {
        Kind::Xlsx => format!("v : {NUMBER}"),
        Kind::Docx | Kind::Pptx => format!("t : {MARKER}"),
    };
    delta
        .gained_text
        .iter()
        .any(|(key, _)| key.ends_with(&wanted))
}

/// Excel's calculation chain is a cache of evaluation order. Dropping it when a
/// cell changes is what Excel itself expects, and takes one content-type entry
/// and one relationship with it. That is not counted as loss.
fn is_calc_chain_cleanup(delta: &Delta) -> bool {
    delta.lost_element_count() == 1
        && delta.lost_text.is_empty()
        && delta.gained_elements.is_empty()
        && delta
            .lost_attributes
            .iter()
            .any(|(key, _)| key.contains("calcChain"))
}

/// Text of the original that is gone and counts as lost. After a cell edit the
/// cached results of formulas change, in the sheet, in other sheets and in
/// chart caches; a number in a `v` element replaced by another is not a loss.
fn lost_text(delta: &Delta, recalculated: bool, edited: bool) -> u32 {
    if edited && !recalculated {
        return 0;
    }
    delta
        .lost_text
        .iter()
        .filter(|(key, _)| {
            let is_cached_number = key
                .rsplit_once("}v : ")
                .is_some_and(|(_, value)| value.parse::<f64>().is_ok());
            !(recalculated && is_cached_number)
        })
        .map(|(_, count)| count)
        .sum()
}

/// Elements that only group properties. One that is empty says nothing, and a
/// library that leaves it out has lost nothing. If such an element had held
/// anything, that would be missing too and show up beside it.
const PROPERTY_CONTAINERS: [&str; 6] = ["}pPr", "}rPr", "}tcPr", "}trPr", "}tblPr", "}sectPr"];

fn lost_only_empty_containers(delta: &Delta) -> bool {
    delta.lost_attributes.is_empty()
        && delta.lost_text.is_empty()
        && delta.changed_attributes.is_empty()
        && delta.lost_elements.iter().all(|(name, _)| {
            PROPERTY_CONTAINERS
                .iter()
                .any(|container| name.ends_with(container))
        })
}

fn is_packaging(part: &str) -> bool {
    part.ends_with(".rels") || part == "[Content_Types].xml"
}

fn failed(sample: &Sample, candidate: &'static str, scenario: Scenario, error: String) -> Outcome {
    Outcome {
        sample: sample.name.clone(),
        kind: sample.kind,
        candidate,
        scenario,
        verdict: Verdict::Failed,
        tally: Tally::default(),
        error: Some(error),
        note: None,
        problems: Vec::new(),
        edited: Vec::new(),
        comparison: None,
    }
}

pub fn evaluate(sample: &Sample, candidate: &dyn Candidate, scenario: Scenario) -> Outcome {
    let name = candidate.name();
    let original = match Package::read(&sample.bytes) {
        Ok(package) => package,
        Err(why) => return failed(sample, name, scenario, format!("sample unreadable: {why}")),
    };
    let produced = catch_unwind(AssertUnwindSafe(|| match scenario {
        Scenario::Resave => candidate
            .resave(sample.kind, &sample.bytes)
            .map(|bytes| (bytes, None)),
        Scenario::Edit => candidate
            .edit(sample.kind, &sample.bytes)
            .map(|edited| (edited.bytes, edited.note)),
    }));
    let (bytes, note) = match produced {
        Ok(Ok(result)) => result,
        Ok(Err(why)) => return failed(sample, name, scenario, why),
        Err(_) => return failed(sample, name, scenario, "the library panicked".into()),
    };
    let result = match Package::read(&bytes) {
        Ok(package) => package,
        Err(why) => return failed(sample, name, scenario, format!("output unreadable: {why}")),
    };

    let comparison = compare(&original, &result);
    let before = validate(&original);
    let problems: Vec<String> = validate(&result)
        .into_iter()
        .filter(|problem| !before.contains(problem))
        .collect();

    let edited: Vec<String> = comparison
        .parts
        .iter()
        .filter_map(|(part, change)| match change {
            Change::Different(delta)
                if scenario == Scenario::Edit && carries_the_edit(sample.kind, delta) =>
            {
                Some(part.clone())
            }
            _ => None,
        })
        .collect();
    let chain_dropped = scenario == Scenario::Edit
        && sample.kind == Kind::Xlsx
        && comparison
            .parts
            .iter()
            .any(|(part, change)| part.ends_with("calcChain.xml") && *change == Change::Missing);

    let mut tally = Tally {
        parts: comparison.parts.len(),
        added: comparison.added.len(),
        ..Tally::default()
    };
    let recalculated = scenario == Scenario::Edit && sample.kind == Kind::Xlsx;
    let mut untouched_differ = !comparison.added.is_empty();
    tally.only_packaging_differs = comparison.added.is_empty();
    for (part, change) in &comparison.parts {
        let is_edited = edited.contains(part);
        if *change != Change::Identical && !is_edited && !is_packaging(part) {
            tally.only_packaging_differs = false;
        }
        match change {
            Change::Identical => tally.identical += 1,
            Change::Equivalent => {
                tally.equivalent += 1;
                untouched_differ = true;
            }
            Change::Missing if chain_dropped && part.ends_with("calcChain.xml") => {}
            Change::Missing => tally.missing += 1,
            Change::BinaryDiffers | Change::Unreadable(_) => tally.damaged += 1,
            Change::Different(delta) if chain_dropped && is_calc_chain_cleanup(delta) => {
                tally.reshaped += 1;
            }
            Change::Different(delta) => {
                // In the part that was edited, the old text is expected to go.
                let lost_text = lost_text(delta, recalculated, is_edited);
                let lost_elements = if lost_only_empty_containers(delta) {
                    0
                } else {
                    delta.lost_element_count()
                };
                tally.lost_elements += lost_elements;
                tally.lost_attributes += delta.lost_attribute_count();
                tally.changed_attributes += delta.changed_attribute_count();
                tally.lost_text += lost_text;
                let loss = lost_elements
                    + delta.lost_attribute_count()
                    + delta.changed_attribute_count()
                    + lost_text;
                if loss > 0 {
                    tally.damaged += 1;
                } else if !is_edited {
                    tally.reshaped += 1;
                    // Parts that hold cached results had to change with them.
                    untouched_differ |= !(recalculated && delta.lost_text_count() > 0);
                }
            }
        }
    }

    let (verdict, error) = if scenario == Scenario::Edit && edited.is_empty() {
        (
            Verdict::Failed,
            Some("the edit is not in the output".to_owned()),
        )
    } else if !problems.is_empty() {
        (Verdict::Invalid, None)
    } else if tally.damaged + tally.missing > 0 {
        (Verdict::Lossy, None)
    } else if untouched_differ {
        (Verdict::Rewritten, None)
    } else {
        (Verdict::Pass, None)
    };
    Outcome {
        sample: sample.name.clone(),
        kind: sample.kind,
        candidate: name,
        scenario,
        verdict,
        tally,
        error,
        note,
        problems,
        edited,
        comparison: Some(comparison),
    }
}

pub fn run(samples: &[Sample], candidates: &[Box<dyn Candidate>]) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for sample in samples {
        for candidate in candidates {
            for scenario in Scenario::ALL {
                outcomes.push(evaluate(sample, candidate.as_ref(), scenario));
            }
        }
    }
    outcomes
}

fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(limit.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// One line per outcome. `same/eq/reshaped/damaged/missing/added` count parts;
/// `lost` counts elements, attributes and text runs that are gone.
pub fn table(outcomes: &[Outcome]) -> String {
    let wide = outcomes
        .iter()
        .map(|outcome| outcome.sample.chars().count())
        .max()
        .unwrap_or(6)
        .clamp(6, 44);
    let mut text = String::new();
    writeln!(
        text,
        "{:wide$}  {:12}  {:8}  {:9}  {:>5}  {:>25}  {:>13}  {:>3}  {:>4}  remark",
        "sample",
        "candidate",
        "scenario",
        "verdict",
        "parts",
        "same/eq/resh/dmg/miss/add",
        "lost el/at/tx",
        "chg",
        "errs"
    )
    .unwrap();
    for outcome in outcomes {
        let tally = &outcome.tally;
        let packaging = (outcome.verdict == Verdict::Rewritten && tally.only_packaging_differs)
            .then_some("only relationships and content types were rewritten");
        let remark = outcome
            .error
            .as_deref()
            .or(outcome.problems.first().map(String::as_str))
            .or(outcome.note.as_deref())
            .or(packaging)
            .unwrap_or("");
        writeln!(
            text,
            "{:wide$}  {:12}  {:8}  {:9}  {:>5}  {:>25}  {:>13}  {:>3}  {:>4}  {}",
            shorten(&outcome.sample, wide),
            outcome.candidate,
            outcome.scenario.name(),
            outcome.verdict.name(),
            tally.parts,
            format!(
                "{}/{}/{}/{}/{}/{}",
                tally.identical,
                tally.equivalent,
                tally.reshaped,
                tally.damaged,
                tally.missing,
                tally.added
            ),
            format!(
                "{}/{}/{}",
                tally.lost_elements, tally.lost_attributes, tally.lost_text
            ),
            tally.changed_attributes,
            outcome.problems.len(),
            shorten(remark, 90),
        )
        .unwrap();
    }
    text
}

fn candidate_names(outcomes: &[Outcome]) -> Vec<&'static str> {
    let mut names = Vec::new();
    for outcome in outcomes {
        if !names.contains(&outcome.candidate) {
            names.push(outcome.candidate);
        }
    }
    names
}

/// How many outcomes of each verdict a candidate has, per kind and scenario.
pub fn totals(outcomes: &[Outcome]) -> String {
    let mut text = String::new();
    writeln!(
        text,
        "{:12}  {:4}  {:8}  {:>5}  {:>5}  {:>9}  {:>5}  {:>7}  {:>6}",
        "candidate", "kind", "scenario", "files", "pass", "rewritten", "lossy", "invalid", "failed"
    )
    .unwrap();
    for name in candidate_names(outcomes) {
        for kind in Kind::ALL {
            for scenario in Scenario::ALL {
                let mine: Vec<&Outcome> = outcomes
                    .iter()
                    .filter(|o| o.candidate == name && o.kind == kind && o.scenario == scenario)
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                let of = |verdict: Verdict| mine.iter().filter(|o| o.verdict == verdict).count();
                writeln!(
                    text,
                    "{:12}  {:4}  {:8}  {:>5}  {:>5}  {:>9}  {:>5}  {:>7}  {:>6}",
                    name,
                    kind.extension(),
                    scenario.name(),
                    mine.len(),
                    of(Verdict::Pass),
                    of(Verdict::Rewritten),
                    of(Verdict::Lossy),
                    of(Verdict::Invalid),
                    of(Verdict::Failed),
                )
                .unwrap();
            }
        }
    }
    text
}

/// `{namespace}local` with the namespace cut down to its last segment.
fn short_name(expanded: &str) -> String {
    let Some((namespace, local)) = expanded
        .strip_prefix('{')
        .and_then(|rest| rest.split_once('}'))
    else {
        return expanded.to_owned();
    };
    let tail = namespace
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(namespace);
    format!("{tail}:{local}")
}

/// What each candidate loses most often across the samples, and why it fails
/// when it does: elements by name with the number of files they went missing
/// from.
pub fn losses(outcomes: &[Outcome]) -> String {
    const SHOWN: usize = 14;
    let mut text = String::new();
    for name in candidate_names(outcomes) {
        for scenario in Scenario::ALL {
            let mut elements: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
            let mut attributes: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
            let mut reasons: std::collections::BTreeMap<String, u32> = Default::default();
            for outcome in outcomes
                .iter()
                .filter(|o| o.candidate == name && o.scenario == scenario)
            {
                if let Some(error) = &outcome.error {
                    // Quoted identifiers differ per file; the reason is the rest.
                    let mut reason = String::new();
                    for (index, piece) in error.split('"').enumerate() {
                        reason.push_str(if index % 2 == 0 { piece } else { "…" });
                    }
                    *reasons.entry(shorten(&reason, 110)).or_default() += 1;
                }
                let mut in_this_file: std::collections::BTreeMap<String, u32> = Default::default();
                let mut attributes_here: std::collections::BTreeMap<String, u32> =
                    Default::default();
                for (_, change) in outcome.comparison.iter().flat_map(|c| c.parts.iter()) {
                    if let Change::Different(delta) = change {
                        if !lost_only_empty_containers(delta) {
                            for (element, count) in &delta.lost_elements {
                                *in_this_file.entry(short_name(element)).or_default() += count;
                            }
                        }
                        // `element @attribute`, without the value.
                        let named = |key: &str| {
                            let (name, _) = key.split_once(" = ").unwrap_or((key, ""));
                            let (element, attribute) = name.split_once(" @").unwrap_or((name, ""));
                            format!("{} @{}", short_name(element), short_name(attribute))
                        };
                        for (key, count) in &delta.lost_attributes {
                            *attributes_here.entry(named(key)).or_default() += count;
                        }
                        for (name, old, new) in &delta.changed_attributes {
                            let entry = format!("{} changed, e.g. {old:?} to {new:?}", named(name));
                            *attributes_here.entry(entry).or_default() += 1;
                        }
                    }
                }
                for (element, count) in in_this_file {
                    let entry = elements.entry(element).or_default();
                    entry.0 += 1;
                    entry.1 += count;
                }
                for (attribute, count) in attributes_here {
                    let entry = attributes.entry(attribute).or_default();
                    entry.0 += 1;
                    entry.1 += count;
                }
            }
            if elements.is_empty() && attributes.is_empty() && reasons.is_empty() {
                continue;
            }
            writeln!(text, "{name} · {}", scenario.name()).unwrap();
            let mut reasons: Vec<_> = reasons.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (reason, count) in reasons.iter().take(SHOWN) {
                writeln!(text, "  failed {count:>4}×  {reason}").unwrap();
            }
            let mut elements: Vec<_> = elements.into_iter().collect();
            elements.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (element, (files, count)) in elements.iter().take(SHOWN) {
                writeln!(text, "  lost in {files:>3} file(s), {count:>4}×  {element}").unwrap();
            }
            if elements.len() > SHOWN {
                writeln!(
                    text,
                    "  … and {} more element names",
                    elements.len() - SHOWN
                )
                .unwrap();
            }
            let mut attributes: Vec<_> = attributes.into_iter().collect();
            attributes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (attribute, (files, count)) in attributes.iter().take(SHOWN) {
                writeln!(
                    text,
                    "  attribute lost in {files:>3} file(s), {count:>4}×  {attribute}"
                )
                .unwrap();
            }
            if attributes.len() > SHOWN {
                writeln!(
                    text,
                    "  … and {} more attribute names",
                    attributes.len() - SHOWN
                )
                .unwrap();
            }
        }
    }
    text
}

fn list(text: &mut String, label: &str, items: &[(String, u32)]) {
    const SHOWN: usize = 12;
    for (key, count) in items.iter().take(SHOWN) {
        writeln!(text, "      {label} {count}× {}", shorten(key, 150)).unwrap();
    }
    if items.len() > SHOWN {
        writeln!(
            text,
            "      {label} … and {} more kinds",
            items.len() - SHOWN
        )
        .unwrap();
    }
}

/// Everything about one outcome: each part that changed and what it lost.
pub fn detail(outcome: &Outcome) -> String {
    let mut text = String::new();
    writeln!(
        text,
        "{} · {} · {} → {}",
        outcome.sample,
        outcome.candidate,
        outcome.scenario.name(),
        outcome.verdict.name()
    )
    .unwrap();
    if let Some(error) = &outcome.error {
        writeln!(text, "  error: {error}").unwrap();
    }
    if let Some(note) = &outcome.note {
        writeln!(text, "  note: {note}").unwrap();
    }
    for problem in &outcome.problems {
        writeln!(text, "  new structural error: {problem}").unwrap();
    }
    let Some(comparison) = &outcome.comparison else {
        return text;
    };
    for (part, change) in &comparison.parts {
        let edited = if outcome.edited.contains(part) {
            " (edited)"
        } else {
            ""
        };
        match change {
            Change::Identical => {}
            Change::Equivalent => writeln!(text, "  {part}: other bytes, same content").unwrap(),
            Change::Missing => writeln!(text, "  {part}: MISSING").unwrap(),
            Change::BinaryDiffers => writeln!(text, "  {part}: binary content differs").unwrap(),
            Change::Unreadable(why) => writeln!(text, "  {part}: no longer parses: {why}").unwrap(),
            Change::Different(delta) => {
                writeln!(
                    text,
                    "  {part}{edited}: lost {} elements, {} attributes, {} texts; gained {}, {}, {}",
                    delta.lost_element_count(),
                    delta.lost_attribute_count(),
                    delta.lost_text_count(),
                    delta.gained_elements.iter().map(|(_, n)| n).sum::<u32>(),
                    delta.gained_attributes.iter().map(|(_, n)| n).sum::<u32>(),
                    delta.gained_text.iter().map(|(_, n)| n).sum::<u32>(),
                )
                .unwrap();
                list(&mut text, "lost element", &delta.lost_elements);
                list(&mut text, "lost attribute", &delta.lost_attributes);
                for (name, old, new) in &delta.changed_attributes {
                    writeln!(
                        text,
                        "      changed attribute {}: {old:?} became {new:?}",
                        shorten(name, 120)
                    )
                    .unwrap();
                }
                for (name, old, new) in delta.respelled_attributes.iter().take(6) {
                    writeln!(
                        text,
                        "      respelled attribute {}: {old:?} written as {new:?}",
                        shorten(name, 120)
                    )
                    .unwrap();
                }
                list(&mut text, "lost text", &delta.lost_text);
                list(&mut text, "gained element", &delta.gained_elements);
                list(&mut text, "gained attribute", &delta.gained_attributes);
                list(&mut text, "gained text", &delta.gained_text);
            }
        }
    }
    for part in &comparison.added {
        writeln!(text, "  {part}: ADDED").unwrap();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Edited, own::Own};
    use crate::corpus::built_in;

    #[test]
    fn our_own_layer_meets_the_criterion_on_the_built_in_samples() {
        for sample in built_in().unwrap() {
            for scenario in Scenario::ALL {
                let outcome = evaluate(&sample, &Own, scenario);
                assert_eq!(outcome.verdict, Verdict::Pass, "{}", detail(&outcome));
                if scenario == Scenario::Edit {
                    assert_eq!(outcome.edited.len(), 1, "{}", detail(&outcome));
                }
            }
        }
    }

    /// The recorded results of every candidate on the built-in samples. When a
    /// candidate is upgraded and this fails, the comparison in the README and in
    /// the plan has to be redone, not just the file regenerated:
    /// `cargo run -- run > golden/built-in.txt`.
    #[cfg(all(
        feature = "betteroffice",
        feature = "ooxmlsdk",
        feature = "office-oxide"
    ))]
    #[test]
    fn the_candidates_behave_as_recorded() {
        let outcomes = run(&built_in().unwrap(), &crate::candidate::all());
        let now = format!("{}\n{}", table(&outcomes), totals(&outcomes));
        let recorded = include_str!("../golden/built-in.txt");
        assert!(
            now == recorded,
            "results differ from golden/built-in.txt:\n{now}"
        );
    }

    /// A stand-in library with a chosen defect, to check that the judge sees it.
    struct Faulty(fn(Kind, Package) -> Package);

    impl Candidate for Faulty {
        fn name(&self) -> &'static str {
            "faulty"
        }

        fn resave(&self, kind: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
            (self.0)(kind, Package::read(bytes)?).write()
        }

        fn edit(&self, kind: Kind, bytes: &[u8]) -> Result<Edited, String> {
            let edited = Own.edit(kind, bytes)?;
            Ok(Edited {
                bytes: (self.0)(kind, Package::read(&edited.bytes)?).write()?,
                note: None,
            })
        }
    }

    fn verdicts(fault: fn(Kind, Package) -> Package, kind: Kind) -> [Verdict; 2] {
        let sample = built_in()
            .unwrap()
            .into_iter()
            .find(|s| s.kind == kind)
            .unwrap();
        Scenario::ALL.map(|scenario| evaluate(&sample, &Faulty(fault), scenario).verdict)
    }

    fn replace(mut package: Package, part: &str, from: &str, to: &str) -> Package {
        let text = String::from_utf8(package.get(part).unwrap().to_vec()).unwrap();
        assert!(text.contains(from), "{part} does not contain {from}");
        package.set(part, text.replace(from, to).into_bytes());
        package
    }

    #[test]
    fn a_dropped_part_is_lossy_and_leaves_a_dangling_relationship() {
        let fault = |_, mut package: Package| {
            package
                .parts
                .retain(|part| part.name != "word/comments.xml");
            package
        };
        assert_eq!(verdicts(fault, Kind::Docx), [Verdict::Invalid; 2]);
    }

    #[test]
    fn a_dropped_unknown_element_is_lossy() {
        let fault = |_, package| {
            replace(
                package,
                "word/document.xml",
                "<aster:marker aster:id=\"m-001\" aster:kind=\"review\"/>",
                "",
            )
        };
        assert_eq!(verdicts(fault, Kind::Docx), [Verdict::Lossy; 2]);
    }

    #[test]
    fn rewriting_an_untouched_part_without_loss_is_not_a_pass() {
        let fault = |_, package| {
            replace(
                package,
                "ppt/presProps.xml",
                "<p:present/>",
                "<p:present></p:present>",
            )
        };
        assert_eq!(verdicts(fault, Kind::Pptx), [Verdict::Rewritten; 2]);
    }

    #[test]
    fn dropping_the_calculation_chain_after_an_edit_is_forgiven() {
        let fault = |_, mut package: Package| {
            package.parts.retain(|part| part.name != "xl/calcChain.xml");
            let package = replace(
                package,
                "[Content_Types].xml",
                "<Override PartName=\"/xl/calcChain.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml\"/>",
                "",
            );
            replace(
                package,
                "xl/_rels/workbook.xml.rels",
                "<Relationship Id=\"rId6\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain\" Target=\"calcChain.xml\"/>",
                "",
            )
        };
        // Without an edit there is no reason to drop it.
        assert_eq!(verdicts(fault, Kind::Xlsx), [Verdict::Lossy, Verdict::Pass]);
    }

    #[test]
    fn leaving_out_an_empty_property_container_is_not_a_loss() {
        let empty = |_, package| {
            replace(
                package,
                "word/document.xml",
                "<w:sectPr>",
                "<w:pPr></w:pPr><w:sectPr>",
            )
        };
        let sample = built_in()
            .unwrap()
            .into_iter()
            .find(|s| s.kind == Kind::Docx)
            .unwrap();
        let padded = Sample {
            bytes: empty(Kind::Docx, Package::read(&sample.bytes).unwrap())
                .write()
                .unwrap(),
            ..sample.clone()
        };
        // A library that turns the padded file back into the original.
        struct Tidy(Vec<u8>);
        impl Candidate for Tidy {
            fn name(&self) -> &'static str {
                "tidy"
            }
            fn resave(&self, _: Kind, _: &[u8]) -> Result<Vec<u8>, String> {
                Ok(self.0.clone())
            }
            fn edit(&self, kind: Kind, _: &[u8]) -> Result<Edited, String> {
                Own.edit(kind, &self.0)
            }
        }
        let outcome = evaluate(&padded, &Tidy(sample.bytes.clone()), Scenario::Resave);
        assert_eq!(outcome.verdict, Verdict::Rewritten, "{}", detail(&outcome));
        // The same element with something in it is a loss.
        let fault = |_, package| {
            replace(
                package,
                "word/document.xml",
                "<w:trPr><w:tblHeader/></w:trPr>",
                "",
            )
        };
        assert_eq!(verdicts(fault, Kind::Docx), [Verdict::Lossy; 2]);
    }

    #[test]
    fn a_library_that_does_not_apply_the_edit_fails() {
        struct Lazy;
        impl Candidate for Lazy {
            fn name(&self) -> &'static str {
                "lazy"
            }
            fn resave(&self, _: Kind, bytes: &[u8]) -> Result<Vec<u8>, String> {
                Ok(bytes.to_vec())
            }
            fn edit(&self, _: Kind, bytes: &[u8]) -> Result<Edited, String> {
                Ok(Edited {
                    bytes: bytes.to_vec(),
                    note: None,
                })
            }
        }
        let sample = &built_in().unwrap()[0];
        assert_eq!(
            evaluate(sample, &Lazy, Scenario::Resave).verdict,
            Verdict::Pass
        );
        assert_eq!(
            evaluate(sample, &Lazy, Scenario::Edit).verdict,
            Verdict::Failed
        );
    }
}
