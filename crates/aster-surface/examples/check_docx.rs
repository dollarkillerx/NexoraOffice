//! Read a DOCX file, lay it out and print one line of facts about it, for
//! checking the reader and the layout against a folder of files.
//!
//! `cargo run -p aster-surface --example check_docx -- file.docx`
//!
//! The line is tab-separated: `ok`, paragraphs, paragraphs that are not as
//! read, tables, pages, the page count the file's own properties record (or
//! `-`), milliseconds to read, milliseconds to lay out, places whose caret
//! could not be found, tracked changes in the file, comments in the file.
//! A file that cannot be read prints `error` and why.
//!
//! The layout shows tracked changes and comments; with `ASTER_MARKUP=off` in
//! the environment it is made as if every change was accepted.

use std::time::Instant;

use aster_layout::Layouter;
use aster_ooxml::Package;
use aster_writer::{Block, Document, Pos};

fn tables(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(_) => 0,
            Block::Table(table) => {
                1 + table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .map(|cell| tables(&cell.blocks))
                    .sum::<usize>()
            }
        })
        .sum()
}

/// `<Pages>` of `docProps/app.xml`: what the program that saved the file
/// counted.
fn recorded_pages(bytes: &[u8]) -> Option<u32> {
    let mut package = Package::open(bytes.to_vec()).ok()?;
    let app = package.read("docProps/app.xml").ok()?;
    let text = String::from_utf8_lossy(&app);
    let start = text.find("<Pages>")? + "<Pages>".len();
    let end = start + text[start..].find("</Pages>")?;
    text[start..end].trim().parse().ok()
}

fn missing_carets(document: &Document, layout: &aster_layout::Layout) -> usize {
    document
        .paragraphs()
        .into_iter()
        .filter(|(path, paragraph)| {
            [0, paragraph.len()]
                .into_iter()
                .any(|offset| layout.caret(&Pos::new(path.clone(), offset)).is_none())
        })
        .count()
}

fn main() {
    let file = std::env::args().nth(1).expect("a .docx file");
    let bytes = std::fs::read(&file).expect("read the file");
    let recorded = recorded_pages(&bytes).map_or_else(|| "-".to_owned(), |n| n.to_string());
    let started = Instant::now();
    let mut document = match aster_ooxml::docx::read(bytes) {
        Ok(document) => document,
        Err(error) => {
            println!("error\t{error}");
            return;
        }
    };
    let paragraphs = document.paragraphs();
    let count = paragraphs.len();
    let changed = paragraphs.iter().filter(|(_, p)| !p.is_as_read()).count();
    drop(paragraphs);
    document.renumber();
    let read = started.elapsed();
    let started = Instant::now();
    let mut layouter = Layouter::new();
    layouter.set_markup(std::env::var("ASTER_MARKUP").as_deref() != Ok("off"));
    let layout = layouter.layout(&document);
    let laid = started.elapsed();
    println!(
        "ok\t{count}\t{changed}\t{}\t{}\t{recorded}\t{:.1}\t{:.1}\t{}\t{}\t{}",
        tables(&document.body.blocks),
        layout.pages.len(),
        read.as_secs_f64() * 1000.0,
        laid.as_secs_f64() * 1000.0,
        missing_carets(&document, &layout),
        document.changes().len(),
        document.comments.len(),
    );
}
