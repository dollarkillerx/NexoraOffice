//! Write a rich sample Word document for trying the DOCX reader and, later,
//! the Writer tab:
//!
//! `cargo run -p aster-ooxml --example make_docx_sample -- sample.docx`
//!
//! Three sections (A4 upright with headers, footers and a line grid; two
//! columns; landscape), headings, East Asian and Latin text, direct
//! formatting, numbered and bullet lists, tabs with leaders, ruby, links,
//! a footnote and an endnote, a bookmark, a comment, tracked changes, a
//! content control, a table with merged cells, borders, shading and a
//! nested table, an inline and a floating picture, and a text box.

#[path = "../tests/common/docx_sample.rs"]
mod docx_sample;

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!("usage: make_docx_sample <out.docx>");
        std::process::exit(2);
    };
    let bytes = docx_sample::sample();
    if let Err(err) = std::fs::write(&out, &bytes) {
        eprintln!("cannot write {out}: {err}");
        std::process::exit(1);
    }
    println!("wrote {out} ({} bytes)", bytes.len());
}
