//! Read a DOCX file, lay it out and print what is on each page: the text
//! fragments with their top and bottom, for finding why a page break is where
//! it is.
//!
//! `cargo run --release -p aster-surface --example dump_pages -- file.docx [first [last]]`

use aster_layout::Layouter;
use aster_layout::Item;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("a .docx file");
    let first: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let last: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let bytes = std::fs::read(&file).expect("read the file");
    let mut document = aster_ooxml::docx::read(bytes).expect("a DOCX file");
    document.renumber();
    let layout = Layouter::new().layout(&document);
    println!("{} pages", layout.pages.len());
    for (index, page) in layout.pages.iter().enumerate() {
        let n = index + 1;
        if n < first || n > last {
            continue;
        }
        println!("--- page {n} ({} x {})", page.width, page.height);
        for item in &page.items {
            if let Item::Text(t) = item {
                let lines = t.paragraph.lines();
                if t.lines.is_empty() {
                    continue;
                }
                let a = &lines[t.lines.start];
                let b = &lines[t.lines.end - 1];
                let text: String = t
                    .paragraph
                    .text()[t.paragraph.line_start_offset(t.lines.start)..]
                    .chars()
                    .take(40)
                    .collect();
                println!(
                    "x={:.0} y={:.1}..{:.1} lines {}..{} {:?}",
                    t.x,
                    t.y + a.top,
                    t.y + b.top + b.height,
                    t.lines.start,
                    t.lines.end,
                    text
                );
            }
        }
    }
}
