//! Sections: page size, margins, columns, headers and footers.

use std::sync::Arc;

use aster_writer::{
    CellAlign, Columns, DocGrid, GridKind, Marginals, Margins, Section, SectionStart, Twips,
};

use super::dom::Node;
use super::numbering::number_format;
use super::{PartState, Reader};

/// Columns a section has at most.
const MAX_COLUMNS: usize = 45;
const LETTER: (Twips, Twips) = (12_240, 15_840);

/// The line pitch of a section's grid, if it has one.
pub fn grid_pitch(sect: &Node<'_>) -> Option<Twips> {
    let grid = sect.child("docGrid")?;
    matches!(
        grid.attr("type").as_deref(),
        Some("lines" | "linesAndChars" | "snapToChars")
    )
    .then(|| {
        grid.int("linePitch")
            .map_or(360, |p| p.clamp(1, 31_680) as Twips)
    })
}

fn length(node: &Node<'_>, names: &[&str]) -> Option<Twips> {
    names
        .iter()
        .find_map(|n| node.int(n))
        .map(|v| v.clamp(-1_000_000, 1_000_000) as Twips)
}

impl Reader<'_> {
    pub(super) fn section(&mut self, node: &Node<'_>, part: &PartState) -> Arc<Section> {
        let mut section = Section {
            page: LETTER,
            source: Some(node.range()),
            ..Section::default()
        };
        if let Some(size) = node.child("pgSz") {
            let w = size.int("w").unwrap_or(i64::from(LETTER.0));
            let h = size.int("h").unwrap_or(i64::from(LETTER.1));
            if w > 0 && h > 0 {
                section.page = (w.min(1_000_000) as Twips, h.min(1_000_000) as Twips);
            }
        }
        if let Some(m) = node.child("pgMar") {
            let mut margins = Margins::default();
            let set = |slot: &mut Twips, names: &[&str]| {
                if let Some(v) = length(m, names) {
                    *slot = v;
                }
            };
            set(&mut margins.top, &["top"]);
            set(&mut margins.right, &["right", "end"]);
            set(&mut margins.bottom, &["bottom"]);
            set(&mut margins.left, &["left", "start"]);
            set(&mut margins.header, &["header"]);
            set(&mut margins.footer, &["footer"]);
            set(&mut margins.gutter, &["gutter"]);
            margins.top = margins.top.abs();
            margins.bottom = margins.bottom.abs();
            section.margins = margins;
        }
        if let Some(c) = node.child("cols") {
            let mut columns = Columns::default();
            if let Some(space) = c.int("space") {
                columns.space = space.clamp(0, 1_000_000) as Twips;
            }
            columns.separator = c
                .attr("sep")
                .is_some_and(|v| matches!(v.trim(), "1" | "true" | "on"));
            let equal = c
                .attr("equalWidth")
                .is_none_or(|v| !matches!(v.trim(), "0" | "false" | "off"));
            let listed: Vec<(Twips, Twips)> = c
                .each("col")
                .take(MAX_COLUMNS)
                .map(|col| {
                    (
                        col.int("w").map_or(0, |w| w.clamp(0, 1_000_000) as Twips),
                        col.int("space")
                            .map_or(0, |w| w.clamp(0, 1_000_000) as Twips),
                    )
                })
                .collect();
            if !equal && !listed.is_empty() {
                columns.count = listed.len() as u32;
                columns.widths = listed;
            } else {
                columns.count = c
                    .int("num")
                    .map_or(1, |n| n.clamp(1, MAX_COLUMNS as i64) as u32);
            }
            section.columns = columns;
        }
        section.start = match node.child("type").and_then(Node::val).as_deref() {
            Some("continuous") => SectionStart::Continuous,
            Some("evenPage") => SectionStart::EvenPage,
            Some("oddPage") => SectionStart::OddPage,
            Some("nextColumn") => SectionStart::NextColumn,
            _ => SectionStart::NextPage,
        };
        section.title_page = node.toggle("titlePg").unwrap_or(false);
        if let Some(n) = node.child("pgNumType") {
            section.page_number_start = n.int("start").map(|v| v.clamp(0, 1_000_000) as u32);
            if let Some(fmt) = n.attr("fmt") {
                section.page_number_format = number_format(&fmt);
            }
        }
        if let Some(kind) = node.child("docGrid").and_then(|g| g.attr("type")) {
            let kind = match kind.as_str() {
                "lines" => Some(GridKind::Lines),
                "linesAndChars" => Some(GridKind::LinesAndChars),
                "snapToChars" => Some(GridKind::SnapToChars),
                _ => None,
            };
            if let (Some(kind), Some(pitch)) = (kind, grid_pitch(node)) {
                section.grid = Some(DocGrid {
                    kind,
                    line_pitch: pitch,
                });
            }
        }
        section.vertical_align = match node.child("vAlign").and_then(Node::val).as_deref() {
            Some("center") => CellAlign::Center,
            Some("bottom") => CellAlign::Bottom,
            _ => CellAlign::Top,
        };
        section.vertical_text = matches!(
            node.child("textDirection").and_then(Node::val).as_deref(),
            Some("tbRl" | "tbRlV" | "btLr" | "tbLrV" | "lrTbV")
        );

        // Headers and footers: what the section names, and for the rest
        // what the section before had.
        let mut headers = self.inherited.0;
        let mut footers = self.inherited.1;
        for reference in &node.children {
            let header = reference.is("headerReference");
            if !header && !reference.is("footerReference") {
                continue;
            }
            let Some(target) = reference
                .attrs("id")
                .into_iter()
                .find_map(|id| part.rels.get(&id))
                .and_then(|rel| rel.target.clone())
            else {
                continue;
            };
            let Some(index) = self.marginal(&target) else {
                continue;
            };
            let slots: &mut Marginals = if header { &mut headers } else { &mut footers };
            match reference.attr("type").as_deref() {
                Some("first") => slots.first = Some(index),
                Some("even") => slots.even = Some(index),
                _ => slots.default = Some(index),
            }
        }
        self.inherited = (headers, footers);
        section.headers = headers;
        section.footers = footers;
        Arc::new(section)
    }

    /// The index in the document's headers and footers of the part, reading
    /// it if it is new.
    fn marginal(&mut self, name: &str) -> Option<usize> {
        if let Some(&index) = self.marginal_index.get(name) {
            return Some(index);
        }
        let story = self.read_story(name)?;
        self.marginals.push(Arc::new(story));
        let index = self.marginals.len() - 1;
        self.marginal_index.insert(name.to_owned(), index);
        Some(index)
    }
}
