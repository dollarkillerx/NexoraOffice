//! Headers, footers and notes: making them, and what deleting a reference
//! does to its note.
//!
//! Every other edit works in any story because it goes through the
//! accessors of [`Document`]; what is here is what only these stories need.
//!
//! **Footnotes.** [`insert_footnote`] puts a reference at the caret and a new
//! note, a paragraph with the note's own mark and a space, at the end of the
//! document's footnotes. The reference takes the character style
//! `FootnoteReference` and the note's paragraph the style `FootnoteText`
//! when the style sheet has them; without them the reference is raised and
//! the note's text is no bigger than 10 points. Deleting a reference takes
//! its note with it (a note nothing refers to is never shown, and Word
//! drops it too); undo brings both back because it keeps whole documents.
//! Notes cannot be made inside notes, headers or footers.
//!
//! **Headers and footers.** A section that has none for a kind and a slot,
//! either of its own or from an earlier section it follows, gets an empty
//! one when [`ensure_marginal`] is called, which is when the first edit is
//! made: looking at the place where one would be changes nothing. A section
//! that follows another without one of its own shows the other's, and
//! editing it edits that story (Word's "link to previous"). Where a header
//! is made, the sections after it that had none show it too.

use std::collections::HashSet;
use std::sync::Arc;

use super::{Selection, insert_index, snap};
use crate::{
    Block, Document, Inline, MarginalKind, MarginalSlot, Note, PageField, Paragraph, Pos, Run,
    RunLook, Section, Story, StoryId, StyleKind, VertAlign,
};

const FOOTNOTE_TEXT: &str = "FootnoteText";
const FOOTNOTE_REFERENCE: &str = "FootnoteReference";
const HEADER_STYLE: &str = "Header";
const FOOTER_STYLE: &str = "Footer";
/// The size of a note's text where the document has no style for it.
const NOTE_POINTS: f32 = 10.0;

// ---------------------------------------------------------------------------
// Footnotes
// ---------------------------------------------------------------------------

/// The look a run of character style `id` has, in a paragraph whose runs
/// have `look` where the style sets nothing: what the style sets over the
/// document defaults. `None` when the style sheet has no such style.
fn with_character_style(doc: &Document, look: &RunLook, id: &str) -> Option<RunLook> {
    let style = doc
        .styles
        .get(id)
        .filter(|style| style.kind == StyleKind::Character)?;
    let mut out = look.clone();
    let defaults = &doc.styles.default_run;
    macro_rules! apply {
        ($($f:ident),+) => {
            $( if style.run.$f != defaults.$f { out.$f = style.run.$f.clone(); } )+
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
    Some(out)
}

/// The run that marks a note, in the text or at the head of the note.
fn reference_run(doc: &Document, kind: Inline, look: &RunLook) -> Run {
    match with_character_style(doc, look, FOOTNOTE_REFERENCE) {
        Some(styled) => {
            let mut run = Run::object(kind, styled);
            run.style = Some(Arc::from(FOOTNOTE_REFERENCE));
            run
        }
        None => {
            let mut raised = look.clone();
            raised.position = VertAlign::Superscript;
            Run::object(kind, raised)
        }
    }
}

/// The empty paragraph of a note: style `FootnoteText` where there is one.
fn note_paragraph(doc: &Document, id: i32) -> (Paragraph, usize) {
    let styles = &doc.styles;
    let has_style = styles
        .get(FOOTNOTE_TEXT)
        .is_some_and(|s| s.kind == StyleKind::Paragraph);
    let (props, base) = styles.paragraph_look(has_style.then_some(FOOTNOTE_TEXT));
    let (mut props, base) = (props.clone(), base.clone());
    // What the style sheet does not give is direct formatting of the runs
    // and the mark, not of the paragraph's base look, as a file has it.
    let mut look = base.clone();
    if has_style {
        props.style = Some(Arc::from(FOOTNOTE_TEXT));
    } else {
        look.size = look.size.min(NOTE_POINTS);
        props.space_after = 0;
    }
    let mark = reference_run(doc, Inline::Footnote(id), &look);
    let space = Run::text(" ", look.clone());
    let paragraph = Paragraph {
        runs: vec![mark, space],
        props,
        mark: look,
        base,
        ..Paragraph::default()
    };
    let len = paragraph.len();
    (paragraph, len)
}

/// A footnote at `at`, which must be in the body (a table cell of it
/// included): the reference goes there and a new note, with the note's mark
/// and a space in one paragraph, joins the footnotes. The selection is a
/// caret in that paragraph, after the space. `None` where the place is not
/// in the body or leads nowhere; nothing changes then.
pub fn insert_footnote(doc: &mut Document, at: &Pos) -> Option<Selection> {
    if !at.path.story.is_body() {
        return None;
    }
    let paragraph = doc.paragraph(&at.path)?;
    let offset = snap(paragraph, at.offset, false);
    let id = doc
        .footnotes
        .notes
        .iter()
        .map(|n| n.id)
        .max()
        .map_or(1, |max| max.max(0) + 1);
    let look = paragraph.look_at(offset).clone();
    let mut run = reference_run(doc, Inline::Footnote(id), &look);
    // Text that continues a link or a character style does not carry it into
    // the reference.
    run.link = None;
    let (note, caret) = note_paragraph(doc, id);
    let p = doc.paragraph_mut(&at.path)?;
    let index = insert_index(p, offset);
    p.runs.insert(index, run);
    doc.footnotes.notes.push(Note {
        id,
        blocks: vec![Block::paragraph(note)],
    });
    doc.renumber();
    Some(Selection::caret(Pos::new(
        crate::Path::in_story(StoryId::Footnote(id), 0),
        caret,
    )))
}

/// A note reference: which kind of note, and its id.
pub(super) type NoteRef = (bool, i32);

fn refs_of(p: &Paragraph, out: &mut Vec<NoteRef>) {
    for run in &p.runs {
        match run.kind {
            Inline::Footnote(id) => out.push((false, id)),
            Inline::Endnote(id) => out.push((true, id)),
            _ => {}
        }
    }
}

/// The note references in the paragraphs of the body from `from` to `to`
/// (both inclusive; paths of the body only), or in the one paragraph
/// when they are the same.
pub(super) fn refs_between(doc: &Document, from: &crate::Path, to: &crate::Path) -> Vec<NoteRef> {
    let mut out = Vec::new();
    if !from.story.is_body() || from.story != to.story {
        return out;
    }
    if from == to {
        if let Some(p) = doc.paragraph(from) {
            refs_of(p, &mut out);
        }
        return out;
    }
    for (path, p) in doc.paragraphs_in(from.story) {
        if path >= *from && path <= *to {
            refs_of(p, &mut out);
        }
    }
    out
}

/// Take away the notes of `candidates` that nothing in the body refers to
/// any more.
pub(super) fn drop_lost_notes(doc: &mut Document, candidates: &[NoteRef]) {
    if candidates.is_empty() {
        return;
    }
    let mut still: Vec<NoteRef> = Vec::new();
    for (_, p) in doc.paragraphs_in(StoryId::Body) {
        refs_of(p, &mut still);
    }
    let still: HashSet<NoteRef> = still.into_iter().collect();
    for &(end, id) in candidates {
        if still.contains(&(end, id)) {
            continue;
        }
        let notes = if end {
            &mut doc.endnotes.notes
        } else {
            &mut doc.footnotes.notes
        };
        notes.retain(|n| n.id != id);
    }
    doc.renumber();
}

/// After an edit that changed `after` from `before`: the notes whose last
/// reference in the body the edit took away go too. [`delete`](super::delete)
/// does this itself; this is for the edits that take blocks out (a table with
/// references in it) and for whatever else may have.
pub fn drop_notes_without_references(before: &Document, after: &mut Document) {
    if before.footnotes.notes.is_empty() && before.endnotes.notes.is_empty() {
        return;
    }
    let mut was: Vec<NoteRef> = Vec::new();
    for (_, p) in before.paragraphs_in(StoryId::Body) {
        refs_of(p, &mut was);
    }
    was.sort_unstable();
    was.dedup();
    drop_lost_notes(after, &was);
}

// ---------------------------------------------------------------------------
// Headers and footers
// ---------------------------------------------------------------------------

/// The section `index` of [`Document::sections`], to change.
fn section_mut(doc: &mut Document, index: usize) -> Option<&mut Section> {
    let carriers: Vec<usize> = doc
        .body
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.as_paragraph().is_some_and(|p| p.section.is_some()))
        .map(|(i, _)| i)
        .collect();
    if index < carriers.len() {
        let Block::Paragraph(p) = &mut doc.body.blocks[carriers[index]] else {
            return None;
        };
        let section = Arc::make_mut(p).section.as_mut()?;
        Some(Arc::make_mut(section))
    } else if index == carriers.len() {
        Some(Arc::make_mut(&mut doc.last_section))
    } else {
        None
    }
}

fn slot_of(section: &Section, kind: MarginalKind, slot: MarginalSlot) -> Option<usize> {
    match kind {
        MarginalKind::Header => section.headers.get(slot),
        MarginalKind::Footer => section.footers.get(slot),
    }
}

/// The story of the header or footer a section shows in `slot`, if it has
/// one, of its own or from an earlier section. Changes nothing.
pub fn existing_marginal(
    doc: &Document,
    section: usize,
    kind: MarginalKind,
    slot: MarginalSlot,
) -> Option<StoryId> {
    let sections = doc.sections();
    let (_, found) = sections.get(section)?;
    let index = slot_of(found, kind, slot)?;
    (index < doc.marginals.len()).then_some(StoryId::Marginal(index))
}

/// The header or footer a section shows in `slot` for editing: the one it
/// has (editing one it inherits edits the earlier section's), or a new
/// empty one, which the sections after that showed none now show too.
/// `None` for a section that does not exist.
pub fn ensure_marginal(
    doc: &mut Document,
    section: usize,
    kind: MarginalKind,
    slot: MarginalSlot,
) -> Option<StoryId> {
    if let Some(found) = existing_marginal(doc, section, kind, slot) {
        return Some(found);
    }
    let count = doc.sections().len();
    if section >= count {
        return None;
    }
    let style = match kind {
        MarginalKind::Header => HEADER_STYLE,
        MarginalKind::Footer => FOOTER_STYLE,
    };
    let styles = &doc.styles;
    let has_style = styles
        .get(style)
        .is_some_and(|s| s.kind == StyleKind::Paragraph);
    let (props, base) = styles.paragraph_look(has_style.then_some(style));
    let (mut props, base) = (props.clone(), base.clone());
    if has_style {
        props.style = Some(Arc::from(style));
    }
    let paragraph = Paragraph {
        props,
        mark: base.clone(),
        base,
        ..Paragraph::default()
    };
    let index = doc.marginals.len();
    doc.marginals.push(Arc::new(Story {
        blocks: vec![Block::paragraph(paragraph)],
        part: None,
    }));
    for later in section..count {
        let Some(target) = section_mut(doc, later) else {
            break;
        };
        let entry = match kind {
            MarginalKind::Header => target.headers.slot_mut(slot),
            MarginalKind::Footer => target.footers.slot_mut(slot),
        };
        if later > section && entry.is_some() {
            break;
        }
        *entry = Some(index);
    }
    Some(StoryId::Marginal(index))
}

// ---------------------------------------------------------------------------
// Page numbers
// ---------------------------------------------------------------------------

/// A page-number field at `at` showing `field`; where the caret goes: after
/// it. The text it holds is what a file keeps as the field's last result;
/// layout shows the real number. Works in any story.
pub fn insert_page_number(doc: &mut Document, at: &Pos, field: PageField) -> Pos {
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return at.clone();
    };
    let offset = snap(p, at.offset, false);
    let look = p.look_at(offset).clone();
    let neighbour = p.run_at(offset).map(|(i, _)| i);
    let mut run = Run::object(Inline::PageNumber(field), look);
    run.text = "1".to_owned();
    if let Some(i) = neighbour {
        run.style = p.runs[i].style.clone();
    }
    let size = run.text.len();
    let index = insert_index(p, offset);
    p.runs.insert(index, run);
    Pos::new(at.path.clone(), offset + size)
}
