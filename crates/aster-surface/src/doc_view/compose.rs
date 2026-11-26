//! The input method: text being composed is shown at the caret, underlined,
//! without being in the document until it is committed.
//!
//! While composing, the view lays out a copy of the document with the
//! selection taken out and the preedit text put in; the document itself is
//! not touched, so a composition that is cancelled leaves the text, and the
//! selection that was going to be replaced, as they were. A commit replaces
//! the selection with the committed text, as one step of undo.

use aster_writer::edit;
use aster_writer::{Block, Document, Paragraph, Pos, Underline, UnderlineStyle};

use super::DocView;
use crate::text_surface::ImeEvent;

/// A composition in progress.
pub(super) struct Compose {
    /// The document with the preedit text in it: what is laid out.
    pub shown: Document,
    /// Where the cursor of the preedit text is, in `shown`.
    pub caret: Pos,
}

/// A paragraph to put the caret in, for a body that has none: the look the
/// document's default style gives.
pub(super) fn blank_paragraph(document: &Document) -> Paragraph {
    let styles = &document.styles;
    let (props, run) = styles.paragraph_look(None);
    let mut props = props.clone();
    props.style = styles.default_style.clone();
    Paragraph {
        props,
        base: run.clone(),
        mark: run.clone(),
        ..Paragraph::default()
    }
}

/// `document` with a paragraph in its body if it had none.
pub(super) fn with_paragraph(document: &mut Document) -> bool {
    if document.paragraphs().is_empty() && document.body.blocks.iter().all(|b| b.as_table().is_none())
    {
        let paragraph = blank_paragraph(document);
        document.body.blocks.push(Block::paragraph(paragraph));
        return true;
    }
    false
}

impl DocView {
    /// Where the caret is: in the preedit text while composing.
    pub(super) fn caret_pos(&self) -> Option<Pos> {
        match &self.compose {
            Some(compose) => Some(compose.caret.clone()),
            None => self.sel.as_ref().map(|sel| sel.focus.clone()),
        }
    }

    /// An input method event. Returns whether the picture changed.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        match event {
            ImeEvent::Enabled => false,
            ImeEvent::Disabled => self.cancel_composition(),
            ImeEvent::Preedit { text, cursor } => {
                if text.is_empty() {
                    return self.cancel_composition();
                }
                self.preedit(&text, cursor)
            }
            ImeEvent::Commit(text) => {
                let had = self.compose.take().is_some();
                if had {
                    self.relayout();
                    self.touch();
                }
                let typed = self.type_text(&text);
                had || typed
            }
        }
    }

    /// Drop what is being composed; the document and the selection are as
    /// they were. Returns whether there was a composition.
    pub fn cancel_composition(&mut self) -> bool {
        if self.compose.take().is_none() {
            return false;
        }
        self.relayout();
        self.touch();
        true
    }

    fn preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) -> bool {
        if self.press.is_some() {
            return false;
        }
        let Some(sel) = self.sel.clone() else {
            // No paragraph yet: the preedit goes into a new one.
            if !self.document.paragraphs().is_empty() {
                return false;
            }
            self.ensure_paragraph();
            return self.preedit(text, cursor);
        };
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let mut shown = self.document.clone();
        let (from, to) = if sel.anchor <= sel.focus {
            (sel.anchor.clone(), sel.focus.clone())
        } else {
            (sel.focus.clone(), sel.anchor.clone())
        };
        let at = if from == to {
            from
        } else {
            edit::delete(&mut shown, &from, &to)
        };
        let mut look = shown
            .paragraph(&at.path)
            .map(|p| p.look_at(at.offset).clone())
            .unwrap_or_default();
        if let Some(pending) = &self.pending {
            pending.apply(&mut look);
        }
        look.underline = Some(Underline {
            style: UnderlineStyle::Single,
            color: None,
        });
        let end = edit::insert_text(&mut shown, &at, &text, Some(&look));
        // The cursor inside the preedit text, as bytes of it.
        let inside = cursor.map_or(text.len(), |(_, to)| to.min(text.len()));
        let mut inside = inside;
        while !text.is_char_boundary(inside) {
            inside -= 1;
        }
        let caret = Pos::new(end.path.clone(), end.offset - (text.len() - inside));
        self.compose = Some(Compose { shown, caret });
        self.relayout();
        self.touch();
        self.reveal_caret();
        true
    }
}
