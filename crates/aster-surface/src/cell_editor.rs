//! The text of a cell being typed: a caret, the committed text and an input
//! method's composition. No layout or drawing here; the sheet view draws it.

use aster_sheets::CellRef;

use crate::text_surface::ImeEvent;

/// How the editor was opened, which decides what the arrow keys do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Typing started on a selected cell and replaced its content: the
    /// arrow keys finish the entry and move to the next cell, as in Excel.
    Enter,
    /// F2, a double click or a click in the formula bar: editing the
    /// content in place, the arrow keys move the caret.
    Edit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CellEditor {
    pub at: CellRef,
    pub mode: EditMode,
    text: String,
    /// Byte offset in `text`.
    caret: usize,
    /// A composition not committed yet and its caret, a byte range in it.
    preedit: Option<(String, Option<(usize, usize)>)>,
}

impl CellEditor {
    pub fn new(at: CellRef, mode: EditMode, text: String) -> Self {
        let caret = text.len();
        Self {
            at,
            mode,
            text,
            caret,
            preedit: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_composing(&self) -> bool {
        self.preedit.is_some()
    }

    /// The text as shown: the composition sits at the caret. Returns the
    /// text, the composition's byte range in it, and the caret's byte
    /// offset in it.
    pub fn shown(&self) -> (String, Option<std::ops::Range<usize>>, usize) {
        match &self.preedit {
            Some((preedit, cursor)) => {
                let mut text = String::with_capacity(self.text.len() + preedit.len());
                text.push_str(&self.text[..self.caret]);
                text.push_str(preedit);
                text.push_str(&self.text[self.caret..]);
                let range = self.caret..self.caret + preedit.len();
                let caret = self.caret + cursor.map_or(preedit.len(), |(start, _)| start);
                (text, Some(range), caret)
            }
            None => (self.text.clone(), None, self.caret),
        }
    }

    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
    }

    /// Replace the `len` bytes before the caret by `text`, the caret after
    /// it: a completion taking the place of the word typed so far. Does
    /// nothing, and says so, if those bytes are not whole characters.
    pub fn replace_before_caret(&mut self, len: usize, text: &str) -> bool {
        let Some(start) = self.caret.checked_sub(len) else {
            return false;
        };
        if !self.text.is_char_boundary(start) {
            return false;
        }
        self.text.replace_range(start..self.caret, text);
        self.caret = start + text.len();
        true
    }

    pub fn backspace(&mut self) -> bool {
        let Some((at, _)) = self.text[..self.caret].char_indices().next_back() else {
            return false;
        };
        self.text.replace_range(at..self.caret, "");
        self.caret = at;
        true
    }

    pub fn delete(&mut self) -> bool {
        let Some(c) = self.text[self.caret..].chars().next() else {
            return false;
        };
        self.text
            .replace_range(self.caret..self.caret + c.len_utf8(), "");
        true
    }

    pub fn left(&mut self) -> bool {
        match self.text[..self.caret].char_indices().next_back() {
            Some((at, _)) => {
                self.caret = at;
                true
            }
            None => false,
        }
    }

    pub fn right(&mut self) -> bool {
        match self.text[self.caret..].chars().next() {
            Some(c) => {
                self.caret += c.len_utf8();
                true
            }
            None => false,
        }
    }

    pub fn home(&mut self) -> bool {
        let moved = self.caret != 0;
        self.caret = 0;
        moved
    }

    pub fn end(&mut self) -> bool {
        let moved = self.caret != self.text.len();
        self.caret = self.text.len();
        moved
    }

    /// An input method event. Returns whether anything shown changed.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        match event {
            ImeEvent::Preedit { text, cursor } => {
                self.preedit = (!text.is_empty()).then_some((text, cursor));
                true
            }
            ImeEvent::Commit(text) => {
                self.preedit = None;
                self.insert(&text);
                true
            }
            ImeEvent::Enabled => false,
            ImeEvent::Disabled => self.preedit.take().is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> CellEditor {
        CellEditor::new(CellRef::default(), EditMode::Edit, text.to_owned())
    }

    #[test]
    fn caret_moves_by_characters() {
        let mut e = editor("東京a");
        assert_eq!(e.caret(), "東京a".len());
        assert!(e.left());
        assert!(e.left());
        assert_eq!(e.caret(), "東".len());
        e.insert("の");
        assert_eq!(e.text(), "東の京a");
        assert!(e.backspace());
        assert!(e.delete());
        assert_eq!(e.text(), "東a");
        assert!(e.home());
        assert!(!e.left());
        assert!(!e.backspace());
        assert!(e.end());
        assert!(!e.right());
        assert!(!e.delete());
    }

    #[test]
    fn a_completion_replaces_the_word_before_the_caret() {
        let mut e = editor("=su+1");
        e.left();
        e.left();
        assert_eq!(e.caret(), 3);
        assert!(e.replace_before_caret(2, "SUM("));
        assert_eq!(e.text(), "=SUM(+1");
        assert_eq!(e.caret(), "=SUM(".len());
        // Not into the middle of a character, nor before the start.
        let mut e = editor("=あ");
        assert!(!e.replace_before_caret(1, "x"));
        assert!(!e.replace_before_caret(9, "x"));
        assert_eq!(e.text(), "=あ");
    }

    #[test]
    fn a_composition_sits_at_the_caret_until_committed() {
        let mut e = editor("ab");
        e.left();
        assert!(e.ime(ImeEvent::Preedit {
            text: "とうきょう".into(),
            cursor: Some((3, 3)),
        }));
        assert!(e.is_composing());
        let (shown, range, caret) = e.shown();
        assert_eq!(shown, "aとうきょうb");
        assert_eq!(range, Some(1..1 + "とうきょう".len()));
        assert_eq!(caret, 1 + 3);
        // The committed text has not changed yet.
        assert_eq!(e.text(), "ab");
        e.ime(ImeEvent::Commit("東京".into()));
        assert!(!e.is_composing());
        assert_eq!(e.text(), "a東京b");
        assert_eq!(e.caret(), 1 + "東京".len());
        // Abandoned compositions leave nothing behind.
        e.ime(ImeEvent::Preedit {
            text: "x".into(),
            cursor: None,
        });
        assert!(e.ime(ImeEvent::Disabled));
        assert_eq!(e.shown().0, "a東京b");
        e.ime(ImeEvent::Preedit {
            text: String::new(),
            cursor: None,
        });
        assert!(!e.is_composing());
    }
}
