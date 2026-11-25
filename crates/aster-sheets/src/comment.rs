//! Comments on cells: Excel's notes (the yellow boxes, `comments1.xml`)
//! and, read only, its threaded comments.
//!
//! A note is its author and its text. The text is kept as plain text: the
//! file's runs (Excel puts the author's name first, in bold) are read as
//! their text, and a note written here is written as one run. A threaded
//! comment is shown with its replies; it is changed only in Excel, since
//! the file keeps it in two places that would have to agree.

use std::collections::BTreeMap;

use crate::address::CellRef;
use crate::model::Workbook;

/// A comment on a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// Who wrote it, as the file names them; may be empty.
    pub author: String,
    /// What it says. A note's text as the file has it, the author's name
    /// at its start included when Excel put it there.
    pub text: String,
    /// A threaded comment (with its replies, one per paragraph): shown,
    /// not changed here.
    pub threaded: bool,
}

impl Comment {
    /// A note by `author` saying `text`, as Excel starts one: the author's
    /// name and a colon on the first line.
    pub fn note(author: &str, text: &str) -> Self {
        Self {
            author: author.to_owned(),
            text: text.to_owned(),
            threaded: false,
        }
    }
}

/// Why a comment cannot be changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommentRefusal {
    /// A threaded comment: changed only in Excel.
    Threaded,
}

impl Workbook {
    /// Put (`Some`) or take away (`None`) the comment of a cell. Returns
    /// what it was, or why it cannot be changed.
    pub fn set_comment(
        &mut self,
        sheet: usize,
        at: CellRef,
        comment: Option<Comment>,
    ) -> Result<Option<Comment>, CommentRefusal> {
        let Some(target) = self.sheets.get_mut(sheet) else {
            return Ok(None);
        };
        if target.comments.get(&at).is_some_and(|c| c.threaded) {
            return Err(CommentRefusal::Threaded);
        }
        Ok(match comment {
            Some(comment) => target.comments.insert(at, comment),
            None => target.comments.remove(&at),
        })
    }
}

/// Comments moved with inserted or deleted lines; those on deleted cells
/// go.
pub fn moved(
    comments: BTreeMap<CellRef, Comment>,
    lines: &crate::structure::Lines,
) -> BTreeMap<CellRef, Comment> {
    comments
        .into_iter()
        .filter_map(|(at, comment)| {
            let at = if lines.columns {
                CellRef::new(at.row, lines.moved(at.col)?)
            } else {
                CellRef::new(lines.moved(at.row)?, at.col)
            };
            Some((at, comment))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SheetBuilder, SheetKind};
    use crate::structure::Lines;

    #[test]
    fn notes_are_put_taken_and_moved_but_threads_stay() {
        let mut book = Workbook {
            sheets: vec![SheetBuilder::new("表", SheetKind::Worksheet).finish()],
            ..Workbook::default()
        };
        let b2 = CellRef::parse("B2").unwrap();
        let c5 = CellRef::parse("C5").unwrap();
        assert_eq!(
            book.set_comment(0, b2, Some(Comment::note("我", "看这里"))),
            Ok(None)
        );
        let mut thread = Comment::note("她", "问题\n回答");
        thread.threaded = true;
        book.sheets[0].comments.insert(c5, thread);
        assert_eq!(book.set_comment(0, c5, None), Err(CommentRefusal::Threaded));
        let moved = moved(
            book.sheets[0].comments.clone(),
            &Lines {
                columns: false,
                at: 1,
                count: 2,
                insert: true,
            },
        );
        assert_eq!(
            moved.keys().map(|at| at.to_string()).collect::<Vec<_>>(),
            ["B4", "C7"]
        );
        let gone = super::moved(
            moved,
            &Lines {
                columns: true,
                at: 1,
                count: 1,
                insert: false,
            },
        );
        assert_eq!(
            gone.keys().map(|at| at.to_string()).collect::<Vec<_>>(),
            ["B7"]
        );
        let old = book.set_comment(0, b2, None).unwrap();
        assert_eq!(old.map(|c| c.text), Some("看这里".to_owned()));
    }
}
