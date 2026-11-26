//! Bold East Asian text must come out heavier than regular, whichever face
//! is chosen for it and whichever fallback font the glyphs come from.

use std::sync::Arc;

use aster_layout::Layouter;
use aster_surface::HAN_FALLBACK_FAMILIES;
use aster_surface::doc_paint::DocPainter;
use aster_text::Shaper;
use aster_writer::*;

/// The East Asian fonts the layout's font table knows, with a Latin font
/// for the rest of the line.
const FONTS: [&str; 7] = [
    "宋体",
    "黑体",
    "楷体",
    "仿宋",
    "微软雅黑",
    "MS 明朝",
    "MS ゴシック",
];

fn page(font: &str, bold: bool) -> Document {
    let look = RunLook {
        size: 40.0,
        bold,
        font: Some("Times New Roman".into()),
        east_asian_font: Some(font.into()),
        ..RunLook::default()
    };
    let paragraph = Paragraph {
        runs: vec![Run::text("粗体汉字与書體", look.clone())],
        base: look.clone(),
        mark: look,
        ..Paragraph::default()
    };
    Document {
        body: Story {
            blocks: vec![Block::paragraph(paragraph)],
            part: None,
        },
        last_section: Arc::new(Section {
            page: (6_000, 2_000),
            margins: Margins {
                top: 200,
                right: 200,
                bottom: 200,
                left: 200,
                header: 0,
                footer: 0,
                gutter: 0,
            },
            ..Section::default()
        }),
        ..Document::default()
    }
}

/// How much of the page is ink: the sum of how dark each pixel is.
fn ink(layouter: &mut Layouter, painter: &mut DocPainter, document: &Document) -> f64 {
    let layout = layouter.layout(document);
    let bytes = painter.encode_png(&layout.pages[0], 2.0).expect("a png");
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("png header");
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buffer).expect("png data");
    let channels = info.color_type.samples();
    buffer[..info.buffer_size()]
        .chunks(channels)
        .map(|px| {
            let luma = px
                .iter()
                .take(3.min(channels))
                .map(|v| f64::from(*v))
                .sum::<f64>()
                / 3.0f64.min(channels as f64);
            (255.0 - luma) / 255.0
        })
        .sum()
}

#[test]
fn bold_east_asian_text_is_heavier_than_regular_in_every_mapped_family() {
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let mut layouter = Layouter::with_shaper(shaper);
    // One painter for everything, as the app has: whatever it keeps between
    // pages must not make bold text draw as the regular face.
    let mut painter = DocPainter::new();
    for font in FONTS {
        let regular = ink(&mut layouter, &mut painter, &page(font, false));
        let bold = ink(&mut layouter, &mut painter, &page(font, true));
        eprintln!(
            "{font}: regular {regular:.0} bold {bold:.0} ({:.2}x)",
            bold / regular
        );
        assert!(regular > 100.0, "{font}: nothing was drawn");
        assert!(
            bold > regular * 1.25,
            "{font}: bold {bold:.0} is not heavier than regular {regular:.0}"
        );
    }
}

/// A page with the regular line above the bold one, in one drawing.
fn both(font: &str) -> Document {
    let mut document = page(font, false);
    let bold = page(font, true);
    document.body.blocks.extend(bold.body.blocks);
    let mut section = (*document.last_section).clone();
    section.page.1 = 3_600;
    document.last_section = Arc::new(section);
    document
}

#[test]
fn bold_and_regular_on_one_page_do_not_share_glyphs() {
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let mut layouter = Layouter::with_shaper(shaper);
    let mut painter = DocPainter::new();
    for font in FONTS {
        let layout = layouter.layout(&both(font));
        let bytes = painter.encode_png(&layout.pages[0], 2.0).expect("a png");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("png header");
        let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buffer).expect("png data");
        let channels = info.color_type.samples();
        let row = info.width as usize * channels;
        let rows = info.buffer_size() / row;
        // The second paragraph starts at the first line's bottom.
        let split = (layout.caret(&Pos::new(Path::body(1), 0)).unwrap().top * 2.0) as usize;
        let sum = |from: usize, to: usize| -> f64 {
            buffer[from * row..to * row]
                .chunks(channels)
                .map(|px| (255.0 - f64::from(px[0])) / 255.0)
                .sum()
        };
        let (top, bottom) = (sum(0, split), sum(split, rows));
        eprintln!(
            "{font}: same page regular {top:.0} bold {bottom:.0} ({:.2}x)",
            bottom / top
        );
        assert!(bottom > top * 1.25, "{font}: {bottom:.0} against {top:.0}");
    }
}
