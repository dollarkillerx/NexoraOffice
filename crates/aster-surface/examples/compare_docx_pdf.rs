//! Export a DOCX file as a PDF, draw its pages once with the painter that
//! the screen uses and once from the PDF, and write for each page the two
//! and a pair side by side (screen left, PDF right), with how far apart
//! they are.
//!
//! `cargo run -p aster-surface --example compare_docx_pdf -- file.docx out-dir [scale] [page ...]`
//! (pages count from 1; all of them when none are named)

use std::path::Path;

use aster_layout::Layouter;
use aster_pdf::{Document, Limits};
use aster_surface::HAN_FALLBACK_FAMILIES;
use aster_surface::doc_paint::DocPainter;
use aster_surface::doc_pdf::{DocPdfInfo, export_doc_pdf};
use aster_text::Shaper;

fn png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("create the PNG");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer.write_image_data(rgba).expect("PNG data");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("a .docx file");
    let out = args.next().expect("an output folder");
    let scale: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.5);
    let wanted: Vec<usize> = args.filter_map(|s| s.parse().ok()).collect();
    std::fs::create_dir_all(&out).expect("output folder");
    let mut document =
        aster_ooxml::docx::read(std::fs::read(&file).expect("read the file")).expect("a DOCX file");
    document.renumber();
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let layout = Layouter::with_shaper(shaper).layout(&document);
    let pdf = export_doc_pdf(&layout, &document, &DocPdfInfo::default()).expect("export");
    std::fs::write(Path::new(&out).join("out.pdf"), &pdf).expect("write the PDF");
    let out = out.clone();
    let pages: Vec<usize> = (0..layout.pages.len())
        .filter(|i| wanted.is_empty() || wanted.contains(&(i + 1)))
        .collect();
    // The PDF reader wants a big stack.
    let rendered = std::thread::Builder::new()
        .stack_size(aster_pdf::STACK_BYTES)
        .spawn(move || {
            let document = Document::open(pdf, "", Limits::default()).expect("open the PDF");
            let cache = hayro::RenderCache::new();
            pages
                .into_iter()
                .map(|page| {
                    let pixmap = document
                        .render(&cache, page, scale)
                        .expect("render")
                        .expect("a page");
                    (
                        page,
                        u32::from(pixmap.width()),
                        u32::from(pixmap.height()),
                        pixmap.data_as_u8_slice().to_vec(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .expect("thread")
        .join()
        .expect("rendering");
    let mut painter = DocPainter::new();
    for (index, width, height, from_pdf) in rendered {
        let screen = painter.render(&layout.pages[index], f64::from(scale));
        let (sw, sh) = (u32::from(screen.width()), u32::from(screen.height()));
        let mut from_screen = Vec::new();
        for p in screen.data() {
            from_screen.extend([p.r, p.g, p.b, p.a]);
        }
        let name = format!("page-{:02}", index + 1);
        let dir = Path::new(&out);
        png(
            &dir.join(format!("{name}-screen.png")),
            sw,
            sh,
            &from_screen,
        );
        png(
            &dir.join(format!("{name}-pdf.png")),
            width,
            height,
            &from_pdf,
        );
        // Side by side, and the mean difference over the common area.
        let (w, h) = (sw.min(width) as usize, sh.min(height) as usize);
        let mut pair = vec![255u8; (w * 2 + 8) * h * 4];
        let mut total = 0u64;
        for y in 0..h {
            for x in 0..w {
                let a = &from_screen[(y * sw as usize + x) * 4..][..4];
                let b = &from_pdf[(y * width as usize + x) * 4..][..4];
                pair[(y * (w * 2 + 8) + x) * 4..][..4].copy_from_slice(a);
                pair[(y * (w * 2 + 8) + w + 8 + x) * 4..][..4].copy_from_slice(b);
                total += (0..3).map(|c| u64::from(a[c].abs_diff(b[c]))).sum::<u64>();
            }
        }
        png(
            &dir.join(format!("{name}-pair.png")),
            (w * 2 + 8) as u32,
            h as u32,
            &pair,
        );
        println!(
            "{name}: screen {sw}x{sh}, pdf {width}x{height}, mean difference {:.2} of 255",
            total as f64 / (w * h * 3) as f64
        );
    }
}
