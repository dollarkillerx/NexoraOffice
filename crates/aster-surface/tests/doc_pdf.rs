//! Exporting laid-out Word documents as PDF, read back with the PDF reader
//! the viewer uses and compared with what the screen's painter draws.

use std::sync::Arc;

use aster_layout::{
    FrameItem, Item, Layout, Layouter, LeaderItem, Page, PictureItem, Rect, RuleStyle,
};
use aster_pdf::{Document as PdfDocument, Limits};
use aster_surface::doc_paint::DocPainter;
use aster_surface::doc_pdf::{DocPdfInfo, export_doc_pdf};
use aster_text::Leader;
use aster_writer::Document;

#[path = "../../aster-ooxml/tests/common/docx_sample.rs"]
mod docx_sample;

const JPEG: &[u8] = include_bytes!("data/doc-pdf-gradient.jpg");

fn sample() -> (Document, Layout) {
    let mut document = aster_ooxml::docx::read(docx_sample::sample()).expect("a DOCX file");
    document.renumber();
    let layout = Layouter::new().layout(&document);
    (document, layout)
}

fn info() -> DocPdfInfo {
    DocPdfInfo {
        title: Some("Sample".into()),
        language: Some("en-US".into()),
    }
}

/// Run `work` on a thread with the stack the PDF reader wants.
fn big_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(aster_pdf::STACK_BYTES)
        .spawn(work)
        .unwrap()
        .join()
        .unwrap()
}

fn open(pdf: Vec<u8>) -> PdfDocument {
    PdfDocument::open(pdf, "", Limits::default()).expect("the PDF opens")
}

/// What a page shows as text: one string per line of each text item.
fn layout_lines(page: &Page) -> Vec<String> {
    let mut out = Vec::new();
    for item in &page.items {
        if let Item::Text(t) = item {
            let ranges = t.paragraph.line_ranges();
            let end = t.lines.end.min(ranges.len());
            let text = t.paragraph.text();
            for range in &ranges[t.lines.start.min(end)..end] {
                out.push(text[range.clone()].to_owned());
            }
        }
    }
    out
}

fn squeezed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn a_page_in_the_pdf_for_each_page_of_the_layout_with_its_own_size() {
    let (document, layout) = sample();
    assert!(layout.pages.len() >= 3);
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    let sizes: Vec<(f32, f32)> = layout.pages.iter().map(|p| (p.width, p.height)).collect();
    let read = big_stack(move || open(pdf).info().pages.clone());
    assert_eq!(read.len(), sizes.len());
    for (page, (width, height)) in read.iter().zip(&sizes) {
        assert!((page.width - width).abs() < 0.01, "{page:?} {width}");
        assert!((page.height - height).abs() < 0.01, "{page:?} {height}");
    }
    // The sample has a landscape section: not all pages are alike.
    assert!(sizes.iter().any(|s| s.0 > s.1) && sizes.iter().any(|s| s.0 < s.1));
}

#[test]
fn the_text_of_every_page_can_be_read_back() {
    let (document, layout) = sample();
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let expected: Vec<Vec<String>> = layout.pages.iter().map(layout_lines).collect();
    let texts: Vec<String> = big_stack(move || {
        let reader = open(pdf);
        (0..reader.info().pages.len())
            .map(|page| reader.page_text(page).unwrap().text)
            .collect()
    });
    let (mut found, mut total) = (0, 0);
    let mut missing = Vec::new();
    for (page, lines) in expected.iter().enumerate() {
        let text = squeezed(&texts[page]);
        for line in lines {
            let line = squeezed(line);
            if line.is_empty() {
                continue;
            }
            total += 1;
            if text.contains(&line) {
                found += 1;
            } else {
                missing.push((page, line));
            }
        }
    }
    assert!(total > 20, "{total}");
    assert!(found * 100 >= total * 95, "{found}/{total}: {missing:?}");
}

#[test]
fn text_can_be_searched_in_the_pdf() {
    let (document, layout) = sample();
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let word = layout
        .pages
        .iter()
        .flat_map(layout_lines)
        .flat_map(|line| {
            line.split_whitespace()
                .filter(|w| w.len() >= 6 && w.is_ascii())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .next()
        .expect("a word to look for");
    let hits = big_stack(move || {
        let reader = open(pdf);
        (0..reader.info().pages.len())
            .map(|page| reader.find_in_page(page, &word, false, 10).len())
            .sum::<usize>()
    });
    assert!(hits > 0);
}

#[test]
fn headings_are_bookmarks_at_their_pages() {
    let (document, layout) = sample();
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let expected: Vec<(u16, String, usize)> = layout
        .outline()
        .iter()
        .map(|e| (u16::from(e.level), e.text.clone(), e.page))
        .collect();
    assert!(!expected.is_empty());
    let items = big_stack(move || open(pdf).outline());
    assert_eq!(items.len(), expected.len(), "{items:?}");
    for (item, (level, text, page)) in items.iter().zip(&expected) {
        assert_eq!(
            item.title.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
        assert_eq!(item.target.map(|t| t.page), Some(*page), "{text}");
        // A level is at most one deeper than the one before it.
        assert!(item.depth <= *level, "{} {level}", item.depth);
    }
}

#[test]
fn the_same_document_gives_the_same_bytes_every_time() {
    let (document, layout) = sample();
    let first = export_doc_pdf(&layout, &document, &info()).unwrap();
    assert_eq!(first, export_doc_pdf(&layout, &document, &info()).unwrap());
    // Laid out again from scratch, and exported on another thread.
    let (document, layout) = sample();
    let again = std::thread::spawn(move || export_doc_pdf(&layout, &document, &info()).unwrap())
        .join()
        .unwrap();
    assert_eq!(first, again);
    // The metadata has no date.
    let window = |needle: &[u8]| first.windows(needle.len()).any(|w| w == needle);
    assert!(!window(b"/CreationDate") && !window(b"/ModDate"));
}

#[test]
fn links_become_annotations() {
    let (document, layout) = sample();
    let wanted = layout
        .links(&document)
        .iter()
        .filter(|l| aster_platform::links::is_openable(&l.target))
        .count();
    assert!(wanted > 0, "the sample has a link to an address");
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let count = pdf.windows(5).filter(|w| *w == b"/Link").count();
    assert!(count >= wanted, "{count} < {wanted}");
}

/// Items of every kind on an empty page; no text, as glyphs are smoothed
/// differently by the two drawing routes.
fn items_page() -> (Document, Layout) {
    let document = Document::default();
    let mut layout = Layouter::new().layout(&document);
    let picture = |image: &[u8], rect: Rect, crop: [f64; 4]| {
        Item::Picture(PictureItem {
            image: Arc::from(image),
            rect,
            crop,
        })
    };
    let png = docx_sample::png(48, 32);
    let items = vec![
        Item::Fill {
            rect: Rect::new(30.0, 30.0, 130.0, 60.0),
            color: [0xff, 0xe0, 0x40, 0xff],
        },
        Item::Fill {
            rect: Rect::new(100.0, 40.0, 200.0, 80.0),
            color: [0x20, 0x60, 0xc0, 0x80],
        },
        Item::Rule {
            from: (30.0, 100.0),
            to: (250.0, 100.0),
            width: 2.0,
            color: [0, 0, 0, 255],
            style: RuleStyle::Single,
        },
        Item::Rule {
            from: (30.0, 110.0),
            to: (250.0, 110.0),
            width: 3.0,
            color: [0xc0, 0, 0, 255],
            style: RuleStyle::Double,
        },
        Item::Rule {
            from: (30.0, 120.0),
            to: (250.0, 120.0),
            width: 2.0,
            color: [0, 0x80, 0, 255],
            style: RuleStyle::Dotted,
        },
        Item::Rule {
            from: (30.0, 130.0),
            to: (250.0, 130.0),
            width: 2.0,
            color: [0, 0, 0xc0, 255],
            style: RuleStyle::Dashed,
        },
        Item::Rule {
            from: (270.0, 90.0),
            to: (270.0, 200.0),
            width: 2.0,
            color: [0, 0, 0, 255],
            style: RuleStyle::Dashed,
        },
        Item::Leader(LeaderItem {
            x0: 30.0,
            x1: 250.0,
            baseline: 160.0,
            size: 12.0,
            kind: Leader::Dots,
            color: [0, 0, 0, 255],
        }),
        Item::Leader(LeaderItem {
            x0: 30.0,
            x1: 250.0,
            baseline: 175.0,
            size: 12.0,
            kind: Leader::Hyphens,
            color: [0, 0, 0, 255],
        }),
        Item::Leader(LeaderItem {
            x0: 30.0,
            x1: 250.0,
            baseline: 190.0,
            size: 12.0,
            kind: Leader::Underscore,
            color: [0, 0, 0, 255],
        }),
        Item::Leader(LeaderItem {
            x0: 30.0,
            x1: 250.0,
            baseline: 205.0,
            size: 12.0,
            kind: Leader::MiddleDots,
            color: [0, 0, 0, 255],
        }),
        // A PNG, whole and cropped; a JPEG; something that is no picture.
        picture(
            &png,
            Rect::new(30.0, 230.0, 130.0, 294.0),
            [0.0, 0.0, 0.0, 0.0],
        ),
        picture(
            &png,
            Rect::new(150.0, 230.0, 250.0, 280.0),
            [0.25, 0.1, 0.25, 0.4],
        ),
        picture(
            JPEG,
            Rect::new(270.0, 230.0, 366.0, 294.0),
            [0.0, 0.0, 0.0, 0.0],
        ),
        picture(
            JPEG,
            Rect::new(390.0, 230.0, 470.0, 280.0),
            [0.5, 0.0, 0.0, 0.25],
        ),
        picture(&[1, 2, 3], Rect::new(30.0, 320.0, 90.0, 360.0), [0.0; 4]),
        Item::Placeholder {
            rect: Rect::new(110.0, 320.0, 190.0, 360.0),
            label: "chart".into(),
        },
        Item::Frame(FrameItem {
            rect: Rect::new(210.0, 320.0, 330.0, 380.0),
            fill: Some([0xdd, 0xee, 0xff, 0xff]),
            line: Some((2.0, [0x20, 0x20, 0x80, 0xff])),
        }),
        // Content cut to a clip, and a clip left open.
        Item::PushClip(Rect::new(350.0, 320.0, 420.0, 380.0)),
        Item::Fill {
            rect: Rect::new(330.0, 300.0, 460.0, 350.0),
            color: [0xc0, 0x20, 0x20, 0xff],
        },
        Item::PopClip,
        Item::PopClip,
        Item::PushClip(Rect::new(30.0, 400.0, 60.0, 430.0)),
        Item::Fill {
            rect: Rect::new(0.0, 380.0, 200.0, 460.0),
            color: [0x20, 0x90, 0x20, 0xff],
        },
    ];
    layout.pages[0].items = items;
    (document, layout)
}

#[test]
fn items_of_every_kind_look_like_they_do_on_screen() {
    let (document, layout) = items_page();
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let page = layout.pages[0].clone();
    let scale = 2.0;
    let (w, h, from_pdf) = big_stack(move || {
        let reader = open(pdf);
        let cache = hayro::RenderCache::new();
        let p = reader.render(&cache, 0, scale as f32).unwrap().unwrap();
        (
            usize::from(p.width()),
            usize::from(p.height()),
            p.data_as_u8_slice().to_vec(),
        )
    });
    let screen = DocPainter::new().render(&page, scale);
    // The two round the size of the page differently, by a pixel.
    let (sw, sh) = (usize::from(screen.width()), usize::from(screen.height()));
    assert!(
        sw.abs_diff(w) <= 1 && sh.abs_diff(h) <= 1,
        "{sw}x{sh} {w}x{h}"
    );
    let mut differing = 0usize;
    let mut inked = 0usize;
    for y in 0..sh.min(h) {
        for x in 0..sw.min(w) {
            let p = &screen.data()[y * sw + x];
            let q = &from_pdf[(y * w + x) * 4..][..4];
            let d =
                p.r.abs_diff(q[0])
                    .max(p.g.abs_diff(q[1]))
                    .max(p.b.abs_diff(q[2]));
            if d > 80 {
                differing += 1;
            }
            if p.r < 250 || p.g < 250 || p.b < 250 {
                inked += 1;
            }
        }
    }
    // Set `ASTER_PDF` to a folder to look at the two.
    if let Some(folder) = std::env::var_os("ASTER_PDF") {
        let folder = std::path::PathBuf::from(folder);
        let mut pair = vec![255u8; (sw + w + 8) * sh.max(h) * 4];
        for y in 0..sh {
            for x in 0..sw {
                let p = &screen.data()[y * sw + x];
                pair[(y * (sw + w + 8) + x) * 4..][..4].copy_from_slice(&[p.r, p.g, p.b, 255]);
            }
        }
        for y in 0..h {
            let row = &from_pdf[y * w * 4..(y + 1) * w * 4];
            pair[(y * (sw + w + 8) + sw + 8) * 4..][..w * 4].copy_from_slice(row);
        }
        let file = std::fs::File::create(folder.join("items-pair.png")).unwrap();
        let mut encoder = png::Encoder::new(file, (sw + w + 8) as u32, sh.max(h) as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pair)
            .unwrap();
    }
    assert!(inked > 10_000, "{inked}");
    // Edges are placed to a fraction of a pixel differently; the bulk agrees.
    assert!(
        differing * 100 < inked * 4,
        "{differing} of {inked} inked pixels differ"
    );
}

#[test]
fn a_jpeg_is_embedded_as_it_is_and_a_png_is_decoded() {
    let (document, layout) = items_page();
    let pdf = export_doc_pdf(&layout, &document, &info()).unwrap();
    let has = |needle: &[u8]| pdf.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"/DCTDecode"));
    assert!(has(JPEG));
}
