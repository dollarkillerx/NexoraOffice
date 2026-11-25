//! The state of the formula hints while a cell is typed into: the list of
//! completions with its selection, the call whose signature is shown, and
//! what a chosen completion does to the text. The text analysis is
//! `aster_sheets::hints`; the window draws what this holds.
//!
//! Nothing here is kept while an input method is composing: the list, the
//! signature and the keys that act on them go away with the first
//! composition and come back with the committed text, so the input method
//! never has its keys taken.

use aster_sheets::hints::{self, Call, Completion, Kind};

/// Rows of the list on screen at once.
pub const ROWS: usize = 8;

/// What choosing a completion does to the text being typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insertion {
    /// How many bytes before the caret the completion replaces.
    pub replace: usize,
    /// What goes there: `SUM(` for a function, the name for a defined name.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct FormulaHints {
    items: Vec<Completion>,
    selected: usize,
    scroll: usize,
    call: Option<Call>,
    word_len: usize,
    paren_follows: bool,
    /// The text Esc closed the list at: the list stays closed until it
    /// changes.
    dismissed: Option<String>,
}

impl FormulaHints {
    /// Look at the text being typed again. `typing` is the text and the
    /// caret in it, `None` when nothing is typed or an input method is
    /// composing. Returns whether anything to show changed.
    pub fn update(&mut self, typing: Option<(&str, usize)>, names: &[(&str, &str)]) -> bool {
        let kept = self.selected_name().map(str::to_owned);
        let before = (
            self.items.clone(),
            self.selected,
            self.scroll,
            self.call.clone(),
        );
        match typing {
            Some((text, caret)) => {
                let found = hints::hints(text, caret, names);
                if self.dismissed.as_deref() != Some(text) {
                    self.dismissed = None;
                    self.items = found.items;
                } else {
                    self.items.clear();
                }
                self.call = found.call;
                self.word_len = caret.saturating_sub(found.word_start);
                self.paren_follows = found.paren_follows;
            }
            None => {
                self.items.clear();
                self.call = None;
                self.dismissed = None;
            }
        }
        self.selected = kept
            .and_then(|name| self.items.iter().position(|i| i.name == name))
            .unwrap_or(0);
        self.scroll = 0;
        self.keep_selection_in_view();
        before
            != (
                self.items.clone(),
                self.selected,
                self.scroll,
                self.call.clone(),
            )
    }

    /// The list is open: its keys (Up, Down, Tab, Esc; Enter is not one) are its own.
    pub fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn call(&self) -> Option<&Call> {
        self.call.as_ref()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.items.get(self.selected).map(|i| i.name.as_str())
    }

    /// The rows on screen and the selected one among them.
    pub fn window(&self) -> (&[Completion], usize) {
        let end = (self.scroll + ROWS).min(self.items.len());
        (&self.items[self.scroll..end], self.selected - self.scroll)
    }

    /// Select the next (or previous) completion, wrapping round.
    pub fn step(&mut self, forward: bool) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.keep_selection_in_view();
    }

    fn keep_selection_in_view(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + ROWS {
            self.scroll = self.selected + 1 - ROWS;
        }
    }

    /// Close the list for the text as it is now.
    pub fn dismiss(&mut self, text: &str) {
        self.dismissed = Some(text.to_owned());
        self.items.clear();
        self.selected = 0;
        self.scroll = 0;
    }

    /// What choosing a row of the window does (`None` for the selected
    /// one).
    pub fn insertion(&self, row: Option<usize>) -> Option<Insertion> {
        let index = row.map_or(Some(self.selected), |row| {
            (row < ROWS).then_some(self.scroll + row)
        })?;
        let item = self.items.get(index)?;
        let mut text = item.name.clone();
        if item.kind == Kind::Function && !self.paren_follows {
            text.push('(');
        }
        Some(Insertion {
            replace: self.word_len,
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing(hints: &mut FormulaHints, text: &str) -> bool {
        hints.update(Some((text, text.len())), &[])
    }

    #[test]
    fn the_list_follows_the_text_and_keeps_the_choice() {
        let mut hints = FormulaHints::default();
        assert!(!hints.is_open());
        assert!(typing(&mut hints, "=su"));
        assert!(hints.is_open() && hints.len() > 3);
        while hints.selected_name() != Some("SUMIF") {
            hints.step(true);
        }
        // The same choice stays while it still matches.
        typing(&mut hints, "=sumi");
        assert_eq!(hints.selected_name(), Some("SUMIF"));
        typing(&mut hints, "=sumif");
        assert_eq!(hints.selected_name(), Some("SUMIF"));
        // Once it no longer matches, the first.
        typing(&mut hints, "=sumx");
        assert_eq!(hints.selected_name(), Some("SUMX2MY2"));
        // Nothing changed: nothing to redraw.
        typing(&mut hints, "=sum");
        assert!(!typing(&mut hints, "=sum"));
        // No longer a formula: closed.
        assert!(typing(&mut hints, "sum"));
        assert!(!hints.is_open());
    }

    #[test]
    fn up_and_down_wrap_and_scroll_the_window() {
        let mut hints = FormulaHints::default();
        typing(&mut hints, "=s");
        let count = hints.len();
        assert!(count > ROWS);
        assert_eq!(hints.window().0.len(), ROWS);
        hints.step(false);
        assert_eq!(
            hints.selected_name(),
            hints.window().0.last().map(|i| i.name.as_str())
        );
        assert_eq!(hints.window().1, ROWS - 1, "wrapped to the last row");
        hints.step(true);
        assert_eq!(hints.window().1, 0);
        for _ in 0..ROWS {
            hints.step(true);
        }
        let (rows, at) = hints.window();
        assert_eq!(rows[at].name, hints.selected_name().unwrap());
        assert!(at < ROWS);
    }

    #[test]
    fn choosing_puts_the_name_and_an_opening_parenthesis_in() {
        let mut hints = FormulaHints::default();
        typing(&mut hints, "=1+sumi");
        assert_eq!(
            hints.insertion(None),
            Some(Insertion {
                replace: 4,
                text: "SUMIF(".into()
            })
        );
        // Out of the window: nothing.
        assert_eq!(hints.insertion(Some(ROWS)), None);
        // A name takes no parenthesis; a function already followed by one
        // takes none either.
        let names = [("TaxRate", "0.08")];
        hints.update(Some(("=tax", 4)), &names);
        assert_eq!(hints.insertion(None).unwrap().text, "TaxRate");
        hints.update(Some(("=su(1)", 3)), &[]);
        let insertion = hints.insertion(Some(0)).unwrap();
        assert!(!insertion.text.ends_with('('), "{insertion:?}");
    }

    #[test]
    fn escape_closes_the_list_until_the_text_changes() {
        let mut hints = FormulaHints::default();
        typing(&mut hints, "=su");
        hints.dismiss("=su");
        assert!(!hints.is_open());
        // The same text again (as after a caret move) stays closed.
        typing(&mut hints, "=su");
        assert!(!hints.is_open());
        typing(&mut hints, "=sum");
        assert!(hints.is_open());
    }

    #[test]
    fn nothing_is_shown_while_composing() {
        let mut hints = FormulaHints::default();
        typing(&mut hints, "=SUM(su");
        assert!(hints.is_open() && hints.call().is_some());
        // Composing is `None`: the list and signature go, and keys are free.
        assert!(hints.update(None, &[]));
        assert!(!hints.is_open() && hints.call().is_none());
    }
}
