//! Open a document, make a few visible edits as the Writer tab would, and
//! save a copy that can be looked at in other programs.
//!
//! `cargo run -p aster-ooxml --example edit_docx -- in.docx out.docx`
//!
//! The edits: a sentence typed at the end of the first paragraph, the first
//! words of the second made bold, the third paragraph split in two, and a
//! bullet list of three items added at the end. `blank` as the input starts
//! from a new document.

use aster_ooxml::docx_save::{blank, save};
use aster_writer::edit::{ListKind, apply_run_patch, insert_text, set_list, split_paragraph};
use aster_writer::{Pos, RunPatch};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: edit_docx in.docx out.docx");
        std::process::exit(2);
    };
    let source = if input == "blank" {
        blank()
    } else {
        std::fs::read(&input).expect("the input file reads")
    };
    let mut document = aster_ooxml::docx::read(source.clone()).expect("a Word document");

    let paths: Vec<_> = document
        .paragraphs()
        .into_iter()
        .map(|(path, p)| (path, p.len()))
        .collect();
    let (first, first_len) = paths.first().cloned().expect("a paragraph");
    insert_text(
        &mut document,
        &Pos::new(first, first_len),
        " Edited with Aster Office: 在这里输入了一句话。",
        None,
    );
    if let Some((second, len)) = paths.get(1).cloned() {
        let to = len.min(8);
        // On a char boundary: the operations move the end to one.
        apply_run_patch(
            &mut document,
            &Pos::new(second.clone(), 0),
            &Pos::new(second, to),
            &RunPatch {
                bold: Some(true),
                ..RunPatch::default()
            },
        );
    }
    if let Some((third, len)) = paths.get(2).cloned() {
        split_paragraph(&mut document, &Pos::new(third, len / 2));
    }
    let end = document.end().expect("an end");
    let at = split_paragraph(&mut document, &end);
    let at = insert_text(
        &mut document,
        &at,
        "First point\nSecond point\nThird point",
        None,
    );
    let start = Pos::new(
        {
            let mut path = at.path.clone();
            if let Some(last) = path.0.last_mut() {
                *last = last.saturating_sub(2);
            }
            path
        },
        0,
    );
    set_list(&mut document, &start, &at, Some(ListKind::Bullet));

    let saved = save(source, &document).expect("the document saves");
    std::fs::write(&output, saved).expect("the output file is written");
    println!("wrote {output}");
}
