//! Tracked changes and comments.
//!
//! **What is kept.** A document read from a file knows which of its text was
//! inserted or deleted with change tracking on, by whom and when, and which
//! text its comments are about. Nothing here records changes: typing and
//! deleting are never tracked (see [`crate::edit`]), they only keep what is
//! there coherent.
//!
//! **How it is represented.** Everything hangs off [`Run::annotation`], so
//! that code that does not care about changes sees the same runs as before:
//!
//! - Text inside an insertion (`w:ins`, and the destination of a move) is
//!   ordinary text, in the paragraph's text, with
//!   [`Annotation::Inserted`].
//! - A deletion (`w:del`, and the source of a move) is one zero-width
//!   [`Inline::Marker`] run with [`Annotation::Deleted`], which holds the
//!   removed text as runs of its own. That text is not part of the
//!   paragraph's text: offsets, find, counting and plain text see the
//!   document as if every change was accepted. Layout shows it by
//!   translating between the two, as it does for a page number.
//! - The two ends of a comment's range and the run that refers to the comment
//!   are markers with [`Annotation::CommentStart`], [`Annotation::CommentEnd`]
//!   and [`Annotation::CommentReference`]. Markers are never removed by
//!   edits, so a comment keeps its place with its text; when all the text of
//!   a range is deleted the two ends are next to each other and the comment
//!   stays where the text was.
//! - A paragraph's own changes (its mark inserted or deleted, formatting
//!   changed) are [`Paragraph::changes`].
//!
//! Comments themselves (author, initials, date, text) are
//! [`Document::comments`]; the part they are read from is never written
//! again.

use std::sync::Arc;

use crate::{Document, Inline, Paragraph, Path, Pos, Run, RunLook, StoryId};

/// Who made a tracked change, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The file's `w:id`.
    pub id: i64,
    pub author: Arc<str>,
    /// As the file has it (`2024-05-01T09:30:00Z`), if it has a date.
    pub date: Option<Arc<str>>,
    /// Part of a move (`w:moveFrom`, `w:moveTo`) rather than a plain
    /// insertion or deletion.
    pub moved: bool,
}

/// A deletion: the text that was taken out, to show and to bring back.
#[derive(Clone, Debug, PartialEq)]
pub struct Deleted {
    pub change: Change,
    /// What was deleted: text, tabs and line breaks, in order. Pictures and
    /// other things that cannot be shown are not here (the file's own bytes
    /// keep them).
    pub runs: Vec<Run>,
    /// The base look of the paragraph the runs' looks are relative to (what
    /// they had of it follows the paragraph when its style changes, and a
    /// deletion can move to another paragraph).
    pub base: RunLook,
}

impl Deleted {
    /// The deleted runs as they look in a paragraph whose base look is `base`.
    pub fn shown(&self, base: &RunLook) -> Vec<Run> {
        let mut runs = self.runs.clone();
        if self.base != *base {
            for run in &mut runs {
                crate::edit::rebase_look(&mut run.look, &self.base, base);
            }
        }
        runs
    }

    /// The same change and the same text, whatever the looks.
    pub fn same_text(&self, other: &Deleted) -> bool {
        self.change == other.change
            && self.runs.len() == other.runs.len()
            && self
                .runs
                .iter()
                .zip(&other.runs)
                .all(|(a, b)| a.text == b.text && a.kind == b.kind)
    }
}

/// What a run is besides plain text.
#[derive(Clone, Debug, PartialEq)]
pub enum Annotation {
    /// The run is inside a tracked insertion.
    Inserted(Arc<Change>),
    /// A marker standing for a tracked deletion.
    Deleted(Arc<Deleted>),
    /// A marker: where the text of the comment with this id starts.
    CommentStart(i64),
    /// Where it ends.
    CommentEnd(i64),
    /// The mark in the text that refers to the comment.
    CommentReference(i64),
}

impl Annotation {
    /// The tracked change this is, if it is one.
    pub fn change(&self) -> Option<&Change> {
        match self {
            Annotation::Inserted(change) => Some(change),
            Annotation::Deleted(deleted) => Some(&deleted.change),
            _ => None,
        }
    }
}

/// The tracked changes a paragraph carries itself.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ParaChanges {
    /// Its mark was inserted: rejecting the change joins it to the next
    /// paragraph.
    pub inserted_mark: Option<Arc<Change>>,
    /// Its mark was deleted: accepting the change joins it to the next one.
    pub deleted_mark: Option<Arc<Change>>,
    /// The formatting of the paragraph, its mark or one of its runs was
    /// changed (`w:pPrChange`, `w:rPrChange`). Only known as a fact.
    pub formatting: bool,
}

impl ParaChanges {
    pub fn is_none(&self) -> bool {
        *self == ParaChanges::default()
    }
}

/// A comment of `word/comments.xml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// What the markers in the text refer to.
    pub id: i64,
    pub author: String,
    pub initials: String,
    /// As the file has it, if it has one.
    pub date: Option<String>,
    /// The text of the comment, a string for each paragraph.
    pub paragraphs: Vec<String>,
}

impl Comment {
    /// All of the comment's text, paragraphs on lines of their own.
    pub fn text(&self) -> String {
        self.paragraphs.join("\n")
    }
}

/// What kind of change a [`ChangeInfo`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Inserted,
    /// Zero-width in the text: the deleted text is not in it.
    Deleted,
    /// The paragraph's mark was inserted.
    MarkInserted,
    /// The paragraph's mark was deleted.
    MarkDeleted,
    /// Formatting of the paragraph or something in it changed.
    Formatting,
}

/// A change in the text, for stepping through them and listing them.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeInfo {
    pub kind: ChangeKind,
    pub start: Pos,
    /// The same as `start` for a deletion; for a change of the paragraph's
    /// mark both are the end of the paragraph, for formatting the paragraph's
    /// start and end.
    pub end: Pos,
    pub author: Option<Arc<str>>,
    pub date: Option<Arc<str>>,
}

/// Where a comment's text is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentAnchor {
    pub id: i64,
    /// Both in the same story; equal for a comment on no text. When the two
    /// ends are in different paragraphs the range runs through every
    /// paragraph between.
    pub start: Pos,
    pub end: Pos,
}

/// The paragraph's offset at each of its runs: where run `i` starts.
fn starts(p: &Paragraph) -> impl Iterator<Item = (usize, &Run)> {
    let mut at = 0;
    p.runs.iter().map(move |run| {
        let start = at;
        at += run.text.len();
        (start, run)
    })
}

/// Every story of the document: the body, headers and footers, footnotes,
/// endnotes.
pub fn story_ids(doc: &Document) -> Vec<StoryId> {
    let mut out = vec![StoryId::Body];
    out.extend((0..doc.marginals.len()).map(StoryId::Marginal));
    out.extend(doc.footnotes.notes.iter().map(|n| StoryId::Footnote(n.id)));
    out.extend(doc.endnotes.notes.iter().map(|n| StoryId::Endnote(n.id)));
    out
}

impl Document {
    /// Some text of the document is a tracked insertion or deletion, or some
    /// paragraph has a tracked change of its own.
    pub fn has_changes(&self) -> bool {
        story_ids(self).into_iter().any(|story| {
            self.paragraphs_in(story).iter().any(|(_, p)| {
                !p.changes.is_none()
                    || p.runs.iter().any(|run| {
                        matches!(
                            run.annotation,
                            Some(Annotation::Inserted(_) | Annotation::Deleted(_))
                        )
                    })
            })
        })
    }

    /// The comment with this id.
    pub fn comment(&self, id: i64) -> Option<&Comment> {
        self.comments.iter().find(|c| c.id == id).map(|c| &**c)
    }

    /// The authors of the changes and comments, in the order they first
    /// appear (the body first, then headers, footers and notes, then
    /// authors of comments that are in no text). Layout gives each a colour
    /// by this order.
    pub fn authors(&self) -> Vec<Arc<str>> {
        let mut out: Vec<Arc<str>> = Vec::new();
        let mut add = |name: &Arc<str>| {
            if !out.contains(name) {
                out.push(name.clone());
            }
        };
        for story in story_ids(self) {
            for (_, p) in self.paragraphs_in(story) {
                for change in [&p.changes.inserted_mark, &p.changes.deleted_mark]
                    .into_iter()
                    .flatten()
                {
                    add(&change.author);
                }
                for run in &p.runs {
                    match &run.annotation {
                        Some(Annotation::CommentStart(id)) => {
                            if let Some(comment) = self.comment(*id) {
                                add(&Arc::from(comment.author.as_str()));
                            }
                        }
                        Some(annotation) => {
                            if let Some(change) = annotation.change() {
                                add(&change.author);
                            }
                        }
                        None => {}
                    }
                }
            }
        }
        for comment in &self.comments {
            add(&Arc::from(comment.author.as_str()));
        }
        out
    }

    /// Every tracked change in reading order, a change that continues over
    /// several runs as one entry. Stories after the body follow in the
    /// order of [`story_ids`].
    pub fn changes(&self) -> Vec<ChangeInfo> {
        let mut out = Vec::new();
        for story in story_ids(self) {
            for (path, p) in self.paragraphs_in(story) {
                paragraph_changes(&path, p, &mut out);
            }
        }
        out
    }

    /// Where each comment's text is. A comment with no range markers is
    /// where its reference is; one with neither is left out.
    pub fn comment_anchors(&self) -> Vec<CommentAnchor> {
        #[derive(Default)]
        struct Found {
            start: Option<Pos>,
            end: Option<Pos>,
            reference: Option<Pos>,
        }
        let mut found: Vec<(i64, Found)> = Vec::new();
        for story in story_ids(self) {
            for (path, p) in self.paragraphs_in(story) {
                for (at, run) in starts(p) {
                    let (id, which) = match run.annotation {
                        Some(Annotation::CommentStart(id)) => (id, 0),
                        Some(Annotation::CommentEnd(id)) => (id, 1),
                        Some(Annotation::CommentReference(id)) => (id, 2),
                        _ => continue,
                    };
                    let index = match found.iter().position(|(i, _)| *i == id) {
                        Some(index) => index,
                        None => {
                            found.push((id, Found::default()));
                            found.len() - 1
                        }
                    };
                    let slot = match which {
                        0 => &mut found[index].1.start,
                        1 => &mut found[index].1.end,
                        _ => &mut found[index].1.reference,
                    };
                    slot.get_or_insert_with(|| Pos::new(path.clone(), at));
                }
            }
        }
        let mut out = Vec::new();
        for (id, f) in found {
            let (start, end) = match (f.start, f.end, f.reference) {
                (Some(s), Some(e), _) if s.path.story == e.path.story && s <= e => (s, e),
                (Some(s), Some(e), _) if s.path.story == e.path.story => (e, s),
                (Some(s), _, _) => (s.clone(), s),
                (None, Some(e), _) => (e.clone(), e),
                (None, None, Some(r)) => (r.clone(), r),
                _ => continue,
            };
            out.push(CommentAnchor { id, start, end });
        }
        out.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
        out
    }
}

fn paragraph_changes(path: &Path, p: &Paragraph, out: &mut Vec<ChangeInfo>) {
    let first = out.len();
    let end_of_paragraph = Pos::new(path.clone(), p.len());
    // The insertion being followed: its id, where it starts and ends, who.
    let mut open: Option<(i64, usize, usize, Arc<Change>)> = None;
    fn close(
        open: &mut Option<(i64, usize, usize, Arc<Change>)>,
        path: &Path,
        out: &mut Vec<ChangeInfo>,
    ) {
        if let Some((_, from, to, change)) = open.take() {
            out.push(ChangeInfo {
                kind: ChangeKind::Inserted,
                start: Pos::new(path.clone(), from),
                end: Pos::new(path.clone(), to),
                author: Some(change.author.clone()),
                date: change.date.clone(),
            });
        }
    }
    for (at, run) in starts(p) {
        match &run.annotation {
            Some(Annotation::Inserted(change)) if !run.text.is_empty() => {
                let end = at + run.text.len();
                match &mut open {
                    Some((id, _, to, _)) if *id == change.id => *to = end,
                    _ => {
                        close(&mut open, path, out);
                        open = Some((change.id, at, end, change.clone()));
                    }
                }
            }
            Some(Annotation::Deleted(deleted)) => {
                // A deletion inside an insertion does not end it.
                out.push(ChangeInfo {
                    kind: ChangeKind::Deleted,
                    start: Pos::new(path.clone(), at),
                    end: Pos::new(path.clone(), at),
                    author: Some(deleted.change.author.clone()),
                    date: deleted.change.date.clone(),
                });
            }
            // Zero-width things do not interrupt an insertion.
            _ if run.text.is_empty() => {}
            _ => close(&mut open, path, out),
        }
    }
    close(&mut open, path, out);
    for (kind, change) in [
        (ChangeKind::MarkInserted, &p.changes.inserted_mark),
        (ChangeKind::MarkDeleted, &p.changes.deleted_mark),
    ] {
        if let Some(change) = change {
            out.push(ChangeInfo {
                kind,
                start: end_of_paragraph.clone(),
                end: end_of_paragraph.clone(),
                author: Some(change.author.clone()),
                date: change.date.clone(),
            });
        }
    }
    if p.changes.formatting {
        out.push(ChangeInfo {
            kind: ChangeKind::Formatting,
            start: Pos::new(path.clone(), 0),
            end: end_of_paragraph,
            author: None,
            date: None,
        });
    }
    // Reading order within the paragraph.
    out[first..].sort_by(|a, b| a.start.cmp(&b.start));
}

/// The run is the place of a tracked deletion.
pub fn is_deletion(run: &Run) -> bool {
    matches!(run.annotation, Some(Annotation::Deleted(_))) && matches!(run.kind, Inline::Marker)
}
