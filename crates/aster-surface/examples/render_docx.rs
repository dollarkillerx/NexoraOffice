//! Read a DOCX file, lay it out and write one PNG per page.
//!
//! `cargo run -p aster-surface --example render_docx -- file.docx out-dir [scale]`

use std::path::Path;

use aster_layout::Layouter;
use aster_surface::HAN_FALLBACK_FAMILIES;
use aster_surface::doc_paint::DocPainter;
use aster_text::Shaper;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("a .docx file");
    let out = args.next().expect("an output folder");
    let scale: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.5);
    std::fs::create_dir_all(&out).expect("output folder");
    let bytes = std::fs::read(&file).expect("read the file");
    let started = std::time::Instant::now();
    let mut document = aster_ooxml::docx::read(bytes).expect("a DOCX file");
    document.renumber();
    let read = started.elapsed();
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let mut layouter = Layouter::with_shaper(shaper);
    let started = std::time::Instant::now();
    let layout = layouter.layout(&document);
    println!(
        "read in {:.0} ms, {} pages laid out in {:.0} ms",
        read.as_secs_f64() * 1000.0,
        layout.pages.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    let mut painter = DocPainter::new();
    for (index, page) in layout.pages.iter().enumerate() {
        let png = painter.encode_png(page, scale).expect("png");
        let path = Path::new(&out).join(format!("page-{:02}.png", index + 1));
        std::fs::write(&path, png).expect("write");
    }
    for entry in layout.outline() {
        println!("{} {} (page {})", entry.level, entry.text, entry.page + 1);
    }
}
