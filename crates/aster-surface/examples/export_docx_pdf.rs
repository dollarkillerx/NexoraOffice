//! Read a DOCX file, lay it out and write it as a PDF.
//!
//! `cargo run -p aster-surface --example export_docx_pdf -- in.docx out.pdf`

use std::path::Path;

use aster_layout::Layouter;
use aster_surface::HAN_FALLBACK_FAMILIES;
use aster_surface::doc_pdf::{DocPdfInfo, export_doc_pdf};
use aster_text::Shaper;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("a .docx file");
    let out = args.next().expect("an output .pdf file");
    let bytes = std::fs::read(&file).expect("read the file");
    let mut document = match aster_ooxml::docx::read(bytes) {
        Ok(document) => document,
        Err(err) => {
            eprintln!("cannot read the document: {err:?}");
            std::process::exit(2);
        }
    };
    document.renumber();
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let mut layouter = Layouter::with_shaper(shaper);
    let started = std::time::Instant::now();
    let layout = layouter.layout(&document);
    let laid_out = started.elapsed();
    let info = DocPdfInfo {
        title: Path::new(&file)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned()),
        language: document.settings.language.clone(),
    };
    let started = std::time::Instant::now();
    let pdf = match export_doc_pdf(&layout, &document, &info) {
        Ok(pdf) => pdf,
        Err(err) => {
            eprintln!("cannot export: {err}");
            std::process::exit(3);
        }
    };
    println!(
        "{} pages laid out in {:.0} ms, exported in {:.0} ms, {} bytes",
        layout.pages.len(),
        laid_out.as_secs_f64() * 1000.0,
        started.elapsed().as_secs_f64() * 1000.0,
        pdf.len()
    );
    std::fs::write(&out, pdf).expect("write the PDF");
}
