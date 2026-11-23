//! A selection that runs across many laid-out texts, such as the paragraphs,
//! headings and table cells of a preview, and the plain text it copies as.

use std::ops::Range;

/// A position: which text, and the byte offset in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPoint {
    pub block: usize,
    pub offset: usize,
}

/// Where a drag started and where it is now; either may come first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: TextPoint,
    pub focus: TextPoint,
}

/// One text as the selection sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece<'a> {
    pub text: &'a str,
    /// What separates it from the text before it in copied text: a line
    /// break between paragraphs, a tab between table cells.
    pub before: &'static str,
}

impl Selection {
    pub fn caret(at: TextPoint) -> Self {
        Self {
            anchor: at,
            focus: at,
        }
    }

    /// Everything in `pieces`. `None` if there is nothing.
    pub fn all(pieces: &[Piece<'_>]) -> Option<Self> {
        let last = pieces.len().checked_sub(1)?;
        Some(Self {
            anchor: TextPoint {
                block: 0,
                offset: 0,
            },
            focus: TextPoint {
                block: last,
                offset: pieces[last].text.len(),
            },
        })
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// Start and end in reading order.
    pub fn ordered(&self) -> (TextPoint, TextPoint) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// The selected part of text `block`, which is `len` bytes long.
    pub fn range_in(&self, block: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if self.is_empty() || block < start.block || block > end.block {
            return None;
        }
        let from = if block == start.block {
            start.offset.min(len)
        } else {
            0
        };
        let to = if block == end.block {
            end.offset.min(len)
        } else {
            len
        };
        (from < to || (block != end.block && from == to)).then_some(from..to)
    }

    /// The selected text, with the pieces' separators between texts.
    pub fn text(&self, pieces: &[Piece<'_>]) -> String {
        let mut out = String::new();
        if self.is_empty() {
            return out;
        }
        let (start, end) = self.ordered();
        let last = end.block.min(pieces.len().saturating_sub(1));
        for (block, piece) in pieces.iter().enumerate().take(last + 1).skip(start.block) {
            let Some(range) = self.range_in(block, piece.text.len()) else {
                continue;
            };
            if block > start.block {
                out.push_str(piece.before);
            }
            let range = floor(piece.text, range.start)..floor(piece.text, range.end);
            out.push_str(&piece.text[range]);
        }
        out
    }
}

/// The nearest character boundary at or before `offset`.
fn floor(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(block: usize, offset: usize) -> TextPoint {
        TextPoint { block, offset }
    }

    const PIECES: [Piece<'static>; 4] = [
        Piece {
            text: "第一段。",
            before: "",
        },
        Piece {
            text: "第二段：日本語。",
            before: "\n\n",
        },
        Piece {
            text: "格",
            before: "\n\n",
        },
        Piece {
            text: "子",
            before: "\t",
        },
    ];

    #[test]
    fn within_one_text() {
        let selection = Selection {
            anchor: at(1, 3 * 3),
            focus: at(1, 3 * 6),
        };
        assert_eq!(selection.text(&PIECES), "：日本");
        // Dragged backwards: the same.
        let backwards = Selection {
            anchor: selection.focus,
            focus: selection.anchor,
        };
        assert_eq!(backwards.text(&PIECES), "：日本");
        assert_eq!(selection.range_in(0, 12), None);
    }

    #[test]
    fn across_texts_with_their_separators() {
        let selection = Selection {
            anchor: at(0, 6),
            focus: at(3, 3),
        };
        assert_eq!(
            selection.text(&PIECES),
            "段。\n\n第二段：日本語。\n\n格\t子"
        );
        assert_eq!(selection.range_in(0, 12), Some(6..12));
        assert_eq!(selection.range_in(1, 24), Some(0..24));
    }

    #[test]
    fn select_all_and_empty() {
        let all = Selection::all(&PIECES).unwrap();
        assert_eq!(all.text(&PIECES), "第一段。\n\n第二段：日本語。\n\n格\t子");
        assert!(Selection::all(&[]).is_none());
        let caret = Selection::caret(at(1, 3));
        assert!(caret.is_empty());
        assert_eq!(caret.text(&PIECES), "");
        assert_eq!(caret.range_in(1, 24), None);
    }

    #[test]
    fn offsets_inside_a_character_are_rounded_down() {
        let selection = Selection {
            anchor: at(0, 1),
            focus: at(0, 7),
        };
        assert_eq!(selection.text(&PIECES), "第一");
    }
}
