//! Accepting and rejecting tracked changes.
//!
//! - **Insertion**: accepted, the text stays and is no longer marked;
//!   rejected, the text goes.
//! - **Deletion**: accepted, the deleted text is gone for good; rejected, it
//!   comes back as ordinary text where it was, with the look it had.
//! - **Paragraph mark inserted**: accepted, the record goes; rejected, the
//!   paragraph is joined to the next one. **Paragraph mark deleted**: the
//!   other way round.
//! - **Formatting changes** (`w:pPrChange`, `w:rPrChange`): accepted, the
//!   record goes. Rejecting does *not* restore the old formatting (the model
//!   does not keep it): the record stays and the change is not counted.
//!
//! A move is a deletion and an insertion with the same author and date;
//! resolving one end alone leaves the other as it was.
//!
//! The functions take the whole document or the changes a range touches, and
//! return how many changes they resolved. A caret touches the change it is in
//! or at the edge of; a range touches every change it overlaps, a deletion
//! or paragraph mark it goes over, and a paragraph mark at its ends, but a
//! deletion at an end only when that is the start or the end of the
//! paragraph.

use std::collections::HashSet;

use crate::track::{Annotation, story_ids};
use crate::{Document, Paragraph, Path, Pos, Run};

use super::{delete, merge_runs};

/// Accept every change in the document.
pub fn accept_all(doc: &mut Document) -> usize {
    resolve(doc, None, true)
}

/// Reject every change in the document.
pub fn reject_all(doc: &mut Document) -> usize {
    resolve(doc, None, false)
}

/// Accept the changes the range from `from` to `to` (in either order, in one
/// story) touches.
pub fn accept_in(doc: &mut Document, from: &Pos, to: &Pos) -> usize {
    match ordered(from, to) {
        Some(range) => resolve(doc, Some(range), true),
        None => 0,
    }
}

/// Reject the changes the range touches.
pub fn reject_in(doc: &mut Document, from: &Pos, to: &Pos) -> usize {
    match ordered(from, to) {
        Some(range) => resolve(doc, Some(range), false),
        None => 0,
    }
}

/// The two ends in order; nothing for ends in different stories.
fn ordered(from: &Pos, to: &Pos) -> Option<(Pos, Pos)> {
    if from.path.story != to.path.story {
        return None;
    }
    Some(if from <= to {
        (from.clone(), to.clone())
    } else {
        (to.clone(), from.clone())
    })
}

/// Does the stretch `a..b` of the paragraph at `path` (`len` long) lie in
/// the range? A caret touches what it is in or at the edge of. A stretch
/// with no width (a deletion, a paragraph mark) is touched by a range that
/// goes over it; at the edge of the range only a paragraph mark counts, and a
/// deletion at the very start or end of its paragraph when the range goes up
/// to there (selecting "two " does not accept the deletion after it).
fn touched(
    range: &Option<(Pos, Pos)>,
    path: &Path,
    a: usize,
    b: usize,
    len: usize,
    mark: bool,
) -> bool {
    let Some((f, t)) = range else {
        return true;
    };
    let start = Pos::new(path.clone(), a);
    let end = Pos::new(path.clone(), b);
    if f == t {
        start <= *f && *f <= end
    } else if a == b {
        (*f < start && start < *t)
            || (start == *f && (mark || a == 0))
            || (start == *t && (mark || a == len))
    } else {
        start < *t && end > *f
    }
}

/// There is something to resolve in the paragraph, found before changing it
/// so that a paragraph with nothing to do stays shared.
fn has_work(range: &Option<(Pos, Pos)>, path: &Path, p: &Paragraph, accept: bool) -> bool {
    let len = p.len();
    let marks = p.changes.inserted_mark.is_some() || p.changes.deleted_mark.is_some();
    if marks && touched(range, path, len, len, len, true) {
        return true;
    }
    if accept && p.changes.formatting && touched(range, path, 0, len, len, false) {
        return true;
    }
    let mut at = 0;
    for run in &p.runs {
        let stop = at + run.text.len();
        match &run.annotation {
            Some(Annotation::Inserted(_)) if touched(range, path, at, stop, len, false) => {
                return true;
            }
            Some(Annotation::Deleted(_)) if touched(range, path, at, at, len, false) => {
                return true;
            }
            _ => {}
        }
        at = stop;
    }
    false
}

fn resolve(doc: &mut Document, range: Option<(Pos, Pos)>, accept: bool) -> usize {
    let stories = match &range {
        Some((from, _)) => vec![from.path.story],
        None => story_ids(doc),
    };
    let mut count = 0;
    let mut joins: Vec<Path> = Vec::new();
    for story in stories {
        let work: Vec<Path> = doc
            .paragraphs_in(story)
            .into_iter()
            .filter(|(path, p)| has_work(&range, path, p, accept))
            .map(|(path, _)| path)
            .collect();
        for path in work {
            let Some(p) = doc.paragraph_mut(&path) else {
                continue;
            };
            let (n, join) = resolve_paragraph(&range, &path, p, accept);
            count += n;
            if join {
                joins.push(path);
            }
        }
    }
    // Joins last and from the end, so that the paths stay good.
    let mut joined = false;
    for path in joins.into_iter().rev() {
        joined |= join_with_next(doc, &path);
    }
    if joined {
        doc.renumber();
    }
    count
}

/// Resolve what is in one paragraph. How many changes it was, and whether
/// the paragraph's mark goes (it is to be joined to the next paragraph).
fn resolve_paragraph(
    range: &Option<(Pos, Pos)>,
    path: &Path,
    p: &mut Paragraph,
    accept: bool,
) -> (usize, bool) {
    let len = p.len();
    let mut seen: HashSet<(bool, i64)> = HashSet::new();
    let mut runs: Vec<Run> = Vec::with_capacity(p.runs.len());
    let mut at = 0;
    for run in std::mem::take(&mut p.runs) {
        let end = at + run.text.len();
        match run.annotation.clone() {
            Some(Annotation::Inserted(change)) if touched(range, path, at, end, len, false) => {
                seen.insert((true, change.id));
                if accept {
                    let mut run = run;
                    run.annotation = None;
                    runs.push(run);
                }
            }
            Some(Annotation::Deleted(deleted)) if touched(range, path, at, at, len, false) => {
                seen.insert((false, deleted.change.id));
                if !accept {
                    for mut shown in deleted.shown(&p.base) {
                        shown.annotation = None;
                        shown.source = None;
                        runs.push(shown);
                    }
                }
            }
            _ => runs.push(run),
        }
        at = end;
    }
    p.runs = runs;
    merge_runs(p);
    let mut count = seen.len();
    let mut join = false;
    if touched(range, path, at, at, len, true) {
        if p.changes.inserted_mark.take().is_some() {
            count += 1;
            join = !accept;
        }
        if p.changes.deleted_mark.take().is_some() {
            count += 1;
            join = accept;
        }
    }
    if accept && p.changes.formatting && touched(range, path, 0, at, len, false) {
        p.changes.formatting = false;
        count += 1;
    }
    (count, join)
}

/// The paragraph's mark goes: it and the paragraph after it in the same
/// container become one. Not possible for the last paragraph of a container;
/// its mark is only forgotten.
fn join_with_next(doc: &mut Document, path: &Path) -> bool {
    let Some(&last) = path.steps.last() else {
        return false;
    };
    let mut next = path.steps.clone();
    *next.last_mut().expect("a path has steps") = last + 1;
    let next = path.with_steps(next);
    let Some(p) = doc.paragraph(path) else {
        return false;
    };
    if doc.paragraph(&next).is_none() {
        return false;
    }
    let at = Pos::new(path.clone(), p.len());
    delete(doc, &at, &Pos::new(next, 0));
    true
}
