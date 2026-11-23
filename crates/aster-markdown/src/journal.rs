//! How a Markdown document's edits are written into a recovery journal.
//!
//! Every edit, undo and redo reaches the text as one [`Change`]: a byte range
//! replaced by new text. That is all a journal needs, so a journal entry is a
//! change, encoded with a version byte first. Replaying the entries in order on
//! top of a checkpoint gives back the text, and nothing else: the selection and
//! the undo history are not recovered.

use ropey::Rope;

use crate::document::{Change, is_char_boundary, replace};

/// Version byte at the start of every entry. An entry with another version is
/// refused rather than guessed at.
const FORMAT: u8 = 1;
const HEADER_LEN: usize = 1 + 8 + 8;

/// A journal entry, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Encode a change as a journal entry.
pub fn encode(change: &Change) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + change.inserted.len());
    out.push(FORMAT);
    out.extend_from_slice(&(change.range.start as u64).to_le_bytes());
    out.extend_from_slice(&(change.range.end as u64).to_le_bytes());
    out.extend_from_slice(change.inserted.as_bytes());
    out
}

pub fn decode(bytes: &[u8]) -> Result<Edit, String> {
    let header = bytes
        .get(..HEADER_LEN)
        .ok_or_else(|| "entry is shorter than its header".to_owned())?;
    if header[0] != FORMAT {
        return Err(format!("unknown entry format {}", header[0]));
    }
    let number = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("8 bytes"));
    let (start, end) = (number(1), number(9));
    let text = String::from_utf8(bytes[HEADER_LEN..].to_vec())
        .map_err(|_| "inserted text is not UTF-8".to_owned())?;
    let to_usize = |n: u64| usize::try_from(n).map_err(|_| format!("offset {n} is too large"));
    Ok(Edit {
        start: to_usize(start)?,
        end: to_usize(end)?,
        text,
    })
}

impl Edit {
    /// Apply to `text`. Fails, leaving `text` alone, if the edit does not fit:
    /// the journal and the checkpoint then disagree.
    pub fn apply(&self, text: &mut Rope) -> Result<(), String> {
        let fits = self.start <= self.end
            && self.end <= text.len_bytes()
            && is_char_boundary(text, self.start)
            && is_char_boundary(text, self.end);
        if !fits {
            return Err(format!(
                "{}..{} does not fit a {} byte text",
                self.start,
                self.end,
                text.len_bytes()
            ));
        }
        replace(text, self.start..self.end, &self.text);
        Ok(())
    }
}

/// The text a checkpoint and the entries after it amount to.
pub fn rebuild(checkpoint: &[u8], entries: &[Vec<u8>]) -> Result<Rope, String> {
    let checkpoint =
        std::str::from_utf8(checkpoint).map_err(|_| "checkpoint is not UTF-8".to_owned())?;
    let mut text = Rope::from_str(checkpoint);
    for (index, bytes) in entries.iter().enumerate() {
        decode(bytes)
            .and_then(|edit| edit.apply(&mut text))
            .map_err(|why| format!("entry {}: {why}", index + 1))?;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, EditCommand, Selection};

    #[test]
    fn entries_round_trip() {
        let change = Change {
            range: 3..9,
            inserted: "日本語\n".to_owned(),
            removed: String::new(),
            selection: Selection::caret(0),
        };
        let edit = decode(&encode(&change)).unwrap();
        assert_eq!(
            edit,
            Edit {
                start: 3,
                end: 9,
                text: "日本語\n".to_owned()
            }
        );
    }

    #[test]
    fn damaged_entries_are_refused() {
        let good = encode(&Change {
            range: 0..1,
            inserted: "x".to_owned(),
            removed: String::new(),
            selection: Selection::caret(1),
        });
        assert!(decode(&good[..5]).is_err());
        let mut version = good.clone();
        version[0] = 2;
        assert!(decode(&version).unwrap_err().contains("format 2"));
        let mut bad_text = good;
        bad_text.push(0xFF);
        assert!(decode(&bad_text).is_err());
    }

    #[test]
    fn edits_that_do_not_fit_leave_the_text_alone() {
        let mut text = Rope::from_str("中文");
        let past_end = Edit {
            start: 0,
            end: 99,
            text: String::new(),
        };
        let mid_character = Edit {
            start: 1,
            end: 1,
            text: "x".to_owned(),
        };
        let backwards = Edit {
            start: 3,
            end: 0,
            text: String::new(),
        };
        for edit in [past_end, mid_character, backwards] {
            assert!(edit.apply(&mut text).is_err(), "{edit:?}");
        }
        assert_eq!(text, "中文");
    }

    /// The property recovery rests on: replaying every change a document
    /// reports, undo and redo included, gives back its text.
    #[test]
    fn replaying_a_documents_changes_rebuilds_its_text() {
        let mut document = Document::new("# 标题\n\nbody");
        let checkpoint = document.text().to_string().into_bytes();
        let mut entries = Vec::new();
        let mut record = |change: Option<Change>| {
            if let Some(change) = change {
                entries.push(encode(&change));
            }
        };
        document.set_selection(Selection::caret(document.len()));
        for c in "，追加".chars() {
            record(document.apply(EditCommand::Type(c.to_string())));
        }
        record(document.apply(EditCommand::LineBreak));
        record(document.apply(EditCommand::Commit("日本語".to_owned())));
        document.set_selection(Selection {
            anchor: 2,
            focus: 8,
        });
        record(document.begin_composition());
        record(document.apply(EditCommand::Commit("見出し".to_owned())));
        record(document.apply(EditCommand::Backspace));
        record(document.undo());
        record(document.undo());
        record(document.redo());
        record(document.apply(EditCommand::Paste("a\r\nb".to_owned())));

        assert_eq!(&rebuild(&checkpoint, &entries).unwrap(), document.text());
    }

    #[test]
    fn a_bad_entry_names_its_position() {
        let entries = vec![
            encode(&Change {
                range: 0..0,
                inserted: "ok".to_owned(),
                removed: String::new(),
                selection: Selection::caret(2),
            }),
            encode(&Change {
                range: 10..20,
                inserted: String::new(),
                removed: String::new(),
                selection: Selection::caret(0),
            }),
        ];
        let err = rebuild(b"", &entries).unwrap_err();
        assert!(err.starts_with("entry 2:"), "{err}");
        assert!(rebuild(b"\xFF", &[]).is_err());
    }
}
