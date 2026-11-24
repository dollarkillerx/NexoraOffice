//! The undo history of a text document.
//!
//! An entry is one replacement: `removed` at `start` gave way to `inserted`.
//! Entries merge while the user keeps doing the same small thing in the same
//! place, so that a word typed is one undo step and not one per letter.

use crate::document::Selection;

/// What kind of edit an entry records, which decides what it may merge with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Characters typed one after another.
    Typing,
    /// Characters removed backwards, one after another.
    Backspace,
    /// Characters removed forwards, one after another.
    Delete,
    /// The selection removed because an input-method composition started over
    /// it. The commit that follows merges into it.
    CompositionStart,
    /// Anything that is always a step of its own: an input-method commit, a
    /// paste, a cut, a line break.
    Single,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Identifies the document state right after this entry. Never reused.
    pub id: u64,
    pub kind: Kind,
    /// Byte offset where the replacement happened.
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    pub before: Selection,
    pub after: Selection,
}

impl Entry {
    /// Fold `next`, which was applied right after `self`, into `self` if the
    /// two read as one action. Returns `next` back if they do not.
    fn absorb(&mut self, next: Entry) -> Option<Entry> {
        let merged = match (self.kind, next.kind) {
            (Kind::Typing, Kind::Typing)
                if next.removed.is_empty()
                    && next.start == self.start + self.inserted.len()
                    && !next.inserted.contains('\n') =>
            {
                self.inserted.push_str(&next.inserted);
                true
            }
            (Kind::Backspace, Kind::Backspace)
                if next.inserted.is_empty() && next.start + next.removed.len() == self.start =>
            {
                self.removed.insert_str(0, &next.removed);
                self.start = next.start;
                true
            }
            (Kind::Delete, Kind::Delete)
                if next.inserted.is_empty() && next.start == self.start =>
            {
                self.removed.push_str(&next.removed);
                true
            }
            // The composition replaced the selection: one step for the user.
            (Kind::CompositionStart, Kind::Single)
                if next.removed.is_empty() && next.start == self.start =>
            {
                self.inserted.push_str(&next.inserted);
                self.kind = Kind::Single;
                true
            }
            _ => false,
        };
        if merged {
            self.after = next.after;
            self.id = next.id;
            None
        } else {
            Some(next)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// The entry on top of `undo` takes no more merges.
    sealed: bool,
    next_id: u64,
}

/// The state of a document that has never been edited.
pub(crate) const INITIAL_STATE: u64 = 0;

impl History {
    /// Identifies the current state of the text: the same number means the same
    /// text, reached through the same edits.
    pub fn state(&self) -> u64 {
        self.undo.last().map_or(INITIAL_STATE, |entry| entry.id)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Stop the entry on top from growing. Called when the user does anything
    /// that is not a continuation of typing: moves the caret, saves, undoes.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Record an edit that has just been applied.
    pub fn record(
        &mut self,
        kind: Kind,
        start: usize,
        removed: String,
        inserted: String,
        before: Selection,
        after: Selection,
    ) {
        self.next_id += 1;
        let entry = Entry {
            id: self.next_id,
            kind,
            start,
            removed,
            inserted,
            before,
            after,
        };
        // A new edit makes the undone ones unreachable.
        self.redo.clear();
        let entry = match self.undo.last_mut() {
            Some(top) if !self.sealed => top.absorb(entry),
            _ => Some(entry),
        };
        if let Some(entry) = entry {
            self.undo.push(entry);
        }
        // A step that is always on its own must not take the next one in either.
        self.sealed = kind == Kind::Single;
    }

    /// Fold a further replacement into the newest entry, so that undo takes
    /// both back at once. `between` gives the text as it is now (after the
    /// newest entry, before this replacement) over a range. The entry takes
    /// no more merges afterwards. `false` if there is no entry to fold into.
    pub fn amend(
        &mut self,
        between: impl FnOnce(std::ops::Range<usize>) -> String,
        start: usize,
        removed: &str,
        inserted: &str,
        after: Selection,
    ) -> bool {
        let Some(top) = self.undo.last_mut() else {
            return false;
        };
        let top_end = top.start + top.inserted.len();
        let end = start + removed.len();
        let low = top.start.min(start);
        let high = top_end.max(end);
        let span = between(low..high);
        top.removed = format!(
            "{}{}{}",
            &span[..top.start - low],
            top.removed,
            &span[top_end - low..]
        );
        top.inserted = format!("{}{inserted}{}", &span[..start - low], &span[end - low..]);
        top.start = low;
        top.after = after;
        self.next_id += 1;
        top.id = self.next_id;
        self.redo.clear();
        self.sealed = true;
        true
    }

    /// Take the newest entry off for undoing.
    pub fn pop_undo(&mut self) -> Option<Entry> {
        let entry = self.undo.pop()?;
        self.redo.push(entry.clone());
        self.sealed = true;
        Some(entry)
    }

    /// Take the most recently undone entry back for redoing.
    pub fn pop_redo(&mut self) -> Option<Entry> {
        let entry = self.redo.pop()?;
        self.undo.push(entry.clone());
        self.sealed = true;
        Some(entry)
    }

    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.undo.len()
    }
}
