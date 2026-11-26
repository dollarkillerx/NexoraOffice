//! Check tracked changes and comments through editing and saving, for a
//! folder of real files.
//!
//! `cargo run -p aster-ooxml --example track_check -- file.docx`
//!
//! Prints one tab-separated line: `ok` or `failed` and why, the changes the
//! file has, its comments, and what was checked. A file without changes and
//! comments prints `ok` with zeros and nothing else is done. For one with
//! some, a character is typed into a paragraph that has a change (or a
//! comment mark) and the file saved: it must read back with the same
//! changes and comment ranges, the comments part must be the file's own,
//! and the text must be the old text with the character in it. Then every
//! change is accepted, and in another copy rejected, and each is saved and
//! read back: nothing must be left, and the text must be what accepting or
//! rejecting gives.
//!
//! With a directory as the second argument the saved files are written
//! there (`typed.docx`, `accepted.docx`, `rejected.docx`).

use aster_ooxml::docx::read;
use aster_ooxml::docx_save::save;
use aster_writer::edit::{accept_all, insert_text, reject_all};
use aster_writer::track::{ChangeInfo, ChangeKind};
use aster_writer::{Annotation, Document, Pos};

/// What identifies a change: its kind, who and when.
fn summary(changes: &[ChangeInfo]) -> Vec<String> {
    let mut out: Vec<String> = changes
        .iter()
        .map(|c| format!("{:?} {:?} {:?}", c.kind, c.author, c.date))
        .collect();
    out.sort();
    out
}

fn ranges(doc: &Document) -> Vec<(i64, bool)> {
    let mut out: Vec<(i64, bool)> = doc
        .comment_anchors()
        .iter()
        .map(|a| (a.id, a.start == a.end))
        .collect();
    out.sort();
    out
}

fn comments_part(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut package = aster_ooxml::Package::open(bytes.to_vec()).ok()?;
    package.read("word/comments.xml").ok()
}

/// A place in the paragraph of the first change (or comment mark) to type at:
/// at the start of its text, which is next to whatever else it has.
fn place(doc: &Document) -> Option<Pos> {
    for (path, p) in doc.paragraphs() {
        let marked = !p.changes.is_none()
            || p.runs.iter().any(|r| {
                matches!(
                    r.annotation,
                    Some(
                        Annotation::Inserted(_)
                            | Annotation::Deleted(_)
                            | Annotation::CommentStart(_)
                            | Annotation::CommentEnd(_)
                    )
                )
            });
        if marked && !p.is_empty() {
            let text = p.text();
            // After the first character, if there is one: inside whatever
            // run it is in.
            let at = text
                .chars()
                .next()
                .filter(|c| *c != '\u{FFFC}')
                .map_or(0, char::len_utf8);
            return Some(Pos::new(path, at));
        }
    }
    None
}

fn check(bytes: Vec<u8>, keep: Option<&str>) -> Result<String, String> {
    let write = |name: &str, bytes: &[u8]| {
        if let Some(dir) = keep {
            let _ = std::fs::write(format!("{dir}/{name}.docx"), bytes);
        }
    };
    let doc = read(bytes.clone()).map_err(|e| format!("unreadable: {e}"))?;
    let changes = doc.changes();
    let comments = doc.comments.len();
    if changes.is_empty() && doc.comment_anchors().is_empty() {
        return Ok(format!("0\t{comments}\t-"));
    }
    let mut did = Vec::new();
    // Type next to a change.
    if let Some(at) = place(&doc) {
        let mut edited = doc.clone();
        insert_text(&mut edited, &at, "#", None);
        let saved = save(bytes.clone(), &edited).map_err(|e| format!("save: {e}"))?;
        write("typed", &saved);
        let back = read(saved.clone()).map_err(|e| format!("reread: {e}"))?;
        if summary(&back.changes()) != summary(&changes) {
            return Err(format!(
                "changes differ after typing: {} then {}",
                changes.len(),
                back.changes().len()
            ));
        }
        if ranges(&back) != ranges(&doc) {
            return Err("comment ranges differ after typing".into());
        }
        if back.comments != doc.comments {
            return Err("comments differ after typing".into());
        }
        if comments_part(&saved) != comments_part(&bytes) {
            return Err("the comments part changed".into());
        }
        let mut expected = doc.plain_text();
        let mut offset = 0;
        for (path, p) in doc.paragraphs() {
            if path == at.path {
                break;
            }
            offset += p.text().len() + 1;
        }
        expected.insert(offset + at.offset, '#');
        if back.plain_text() != expected {
            return Err("the text differs after typing".into());
        }
        did.push("typed");
    }
    // Accept, and reject, everything.
    for (accept, name) in [(true, "accepted"), (false, "rejected")] {
        let mut edited = doc.clone();
        if accept {
            accept_all(&mut edited);
        } else {
            reject_all(&mut edited);
        }
        let saved = save(bytes.clone(), &edited).map_err(|e| format!("save {name}: {e}"))?;
        write(name, &saved);
        let back = read(saved).map_err(|e| format!("reread {name}: {e}"))?;
        // Formatting changes cannot be rejected and stay.
        let left: Vec<_> = back
            .changes()
            .into_iter()
            .filter(|c| accept || c.kind != ChangeKind::Formatting)
            .collect();
        if !left.is_empty() {
            return Err(format!(
                "{} changes left after {name}: {:?} at {:?}",
                left.len(),
                left[0].kind,
                left[0].start
            ));
        }
        if back.plain_text() != edited.plain_text() {
            return Err(format!("the text differs after {name}"));
        }
        if ranges(&back) != ranges(&doc) {
            return Err(format!("comment ranges differ after {name}"));
        }
        did.push(name);
    }
    Ok(format!("{}\t{comments}\t{}", changes.len(), did.join(",")))
}

fn main() {
    let file = std::env::args().nth(1).expect("a .docx file");
    let bytes = std::fs::read(&file).expect("read the file");
    match check(bytes, std::env::args().nth(2).as_deref()) {
        Ok(line) => println!("ok\t{line}"),
        Err(why) => println!("failed\t{why}"),
    }
}
