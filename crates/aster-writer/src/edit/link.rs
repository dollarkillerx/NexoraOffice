//! Links: putting a hyperlink on text, taking it off, finding the stretch a
//! link covers, and typing new linked text.
//!
//! **The look of a link.** Adding a link gives the runs the character style
//! `Hyperlink` when the document's style sheet has it (and the run has no
//! character style of its own); otherwise the direct look Word's own
//! default style has: blue `0563C1` and a single underline. Taking a link
//! off undoes exactly that: the style goes and what it set goes with it
//! (where the run still has the style's value), or the direct blue and
//! underline go (where the run still has them). Anything else the run was
//! given stays.

use std::sync::Arc;

use super::{ends, insert_text, merge_runs, paths_between, snap, split_run};
use crate::{
    Document, Inline, Link, Paragraph, Pos, Rgba, Run, RunLook, StyleKind, StyleSheet, Underline,
    UnderlineStyle,
};

/// The character style a link is given when the document has it.
const HYPERLINK_STYLE: &str = "Hyperlink";
const LINK_BLUE: Rgba = [0x05, 0x63, 0xC1, 0xFF];

fn direct_look() -> (Option<Rgba>, Option<Underline>) {
    (
        Some(LINK_BLUE),
        Some(Underline {
            style: UnderlineStyle::Single,
            color: None,
        }),
    )
}

fn hyperlink_style(styles: &StyleSheet) -> Option<&crate::Style> {
    styles
        .get(HYPERLINK_STYLE)
        .filter(|style| style.kind == StyleKind::Character)
}

/// Runs a link can be on: text, tabs and drawings.
fn linkable(run: &Run) -> bool {
    matches!(run.kind, Inline::Text | Inline::Tab | Inline::Drawing(_))
}

/// Give `run` the look of a link.
fn add_look(run: &mut Run, styles: &StyleSheet) {
    if !matches!(run.kind, Inline::Text) {
        return;
    }
    if let Some(style) = hyperlink_style(styles)
        && run.style.is_none()
    {
        let (set, default) = (&style.run, &styles.default_run);
        macro_rules! apply {
            ($($f:ident),+) => {
                $( if set.$f != default.$f { run.look.$f = set.$f.clone(); } )+
            };
        }
        apply!(
            font,
            east_asian_font,
            size,
            bold,
            italic,
            underline,
            strike,
            double_strike,
            color,
            highlight,
            shading,
            position,
            caps,
            small_caps,
            hidden,
            spacing,
            scale,
            raise
        );
        run.style = Some(style.id.clone());
        return;
    }
    let (color, underline) = direct_look();
    run.look.color = color;
    run.look.underline = underline;
}

/// Take the look of a link off `run`, which has `base` as its paragraph's
/// base look.
fn remove_look(run: &mut Run, base: &RunLook, styles: &StyleSheet) {
    if !matches!(run.kind, Inline::Text) {
        return;
    }
    if let Some(style) = hyperlink_style(styles)
        && run.style.as_deref() == Some(HYPERLINK_STYLE)
    {
        let (set, default) = (&style.run, &styles.default_run);
        macro_rules! revert {
            ($($f:ident),+) => {
                $( if set.$f != default.$f && run.look.$f == set.$f {
                    run.look.$f = base.$f.clone();
                } )+
            };
        }
        revert!(
            font,
            east_asian_font,
            size,
            bold,
            italic,
            underline,
            strike,
            double_strike,
            color,
            highlight,
            shading,
            position,
            caps,
            small_caps,
            hidden,
            spacing,
            scale,
            raise
        );
        run.style = None;
        return;
    }
    let (color, underline) = direct_look();
    if run.look.color == color {
        run.look.color = base.color;
    }
    if run.look.underline == underline {
        run.look.underline = base.underline;
    }
}

/// Whether setting `link` on `run` changes it.
fn changes(run: &Run, link: &Option<Link>) -> bool {
    linkable(run)
        && match (&run.link, link) {
            (None, None) => false,
            (Some(a), Some(b)) => **a != *b,
            _ => true,
        }
}

/// Put `link` on the text from `from` to `to` (either order, possibly over
/// several paragraphs), or with `None` take links off it. Runs are split at
/// the ends of the range. Text, tabs and pictures take the link; line breaks,
/// notes, page numbers and markers do not. A stretch that already has the
/// link is left as it is, and a stretch with another link takes the new one
/// (and keeps the look it has). See the module documentation for the look
/// links get.
pub fn set_link(doc: &mut Document, from: &Pos, to: &Pos, link: Option<Link>) {
    let Some((f, t)) = ends(doc, from, to) else {
        return;
    };
    let styles = doc.styles.clone();
    let wanted = link;
    let link = wanted.clone().map(Arc::new);
    for path in paths_between(doc, &f.path, &t.path) {
        let Some(p) = doc.paragraph(&path) else {
            continue;
        };
        let a = if path == f.path { f.offset } else { 0 };
        let b = if path == t.path { t.offset } else { p.len() };
        if a >= b || !touches(p, a, b, &wanted) {
            continue;
        }
        let Some(p) = doc.paragraph_mut(&path) else {
            continue;
        };
        split_run(p, b);
        split_run(p, a);
        let base = p.base.clone();
        let mut start = 0;
        for run in &mut p.runs {
            let end = start + run.text.len();
            if start >= a && end <= b && !run.text.is_empty() && changes(run, &wanted) {
                match &link {
                    Some(link) => {
                        if run.link.is_none() {
                            add_look(run, &styles);
                        }
                        run.link = Some(link.clone());
                    }
                    None => {
                        remove_look(run, &base, &styles);
                        run.link = None;
                    }
                }
            }
            start = end;
        }
        merge_runs(p);
    }
}

/// Some run in `a..b` of `p` would change.
fn touches(p: &Paragraph, a: usize, b: usize, link: &Option<Link>) -> bool {
    let mut start = 0;
    for run in &p.runs {
        let end = start + run.text.len();
        if end > a && start < b && changes(run, link) {
            return true;
        }
        start = end;
    }
    false
}

/// The whole stretch of text around `at` that has one link: where it starts
/// and ends in the paragraph, and the link. The caret at either end of the
/// stretch counts as in it (a position between a linked and an unlinked
/// stretch belongs to the linked one; between two links, to the one after).
/// A link does not go across paragraphs. `None` where the text has no link.
/// Markers inside a stretch do not end it; neither does text with the same
/// link in another run (a different look, say).
pub fn link_range(doc: &Document, at: &Pos) -> Option<(Pos, Pos, Arc<Link>)> {
    let p = doc.paragraph(&at.path)?;
    let offset = snap(p, at.offset, false);
    // The runs with text, with where they start and end.
    let mut spans: Vec<(usize, usize, Option<&Arc<Link>>)> = Vec::new();
    let mut start = 0;
    for run in p.runs.iter().filter(|r| !r.text.is_empty()) {
        spans.push((start, start + run.text.len(), run.link.as_ref()));
        start += run.text.len();
    }
    let after = spans
        .iter()
        .position(|(s, e, l)| offset >= *s && offset < *e && l.is_some());
    let before = spans
        .iter()
        .position(|(s, e, l)| offset > *s && offset <= *e && l.is_some());
    let index = after.or(before)?;
    let link = spans[index].2?.clone();
    let same = |i: usize| spans[i].2.is_some_and(|l| **l == *link);
    let (mut first, mut last) = (index, index);
    while first > 0 && same(first - 1) {
        first -= 1;
    }
    while last + 1 < spans.len() && same(last + 1) {
        last += 1;
    }
    Some((
        Pos::new(at.path.clone(), spans[first].0),
        Pos::new(at.path.clone(), spans[last].1),
        link,
    ))
}

/// Type `text` as a link to `link` at `at`; where the caret goes: after it.
/// The text has the look of a link (see the module documentation) over the
/// look it would be typed with. A line feed in `text` starts a new
/// paragraph as typing does; each line is linked. Empty text does nothing.
pub fn insert_link(doc: &mut Document, at: &Pos, text: &str, link: Link) -> Pos {
    let end = insert_text(doc, at, text, None);
    if end == *at {
        return end;
    }
    set_link(doc, at, &end, Some(link));
    end
}
