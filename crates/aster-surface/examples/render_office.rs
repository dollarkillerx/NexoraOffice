//! Render an .xlsx or .pptx with the read-only views, without a window:
//! the first screen of every sheet, or every slide, as PNG files.
//!
//! `cargo run -p aster-surface --example render_office -- file.pptx out/ [scale]`
//!
//! The scale is 2 unless given; at 1 the 1800 by 1100 pixels show twice as
//! much of a sheet.

use std::path::Path;
use std::sync::Arc;

use aster_ooxml::FormatCodec;
use aster_surface::sheet_view::SheetView;
use aster_surface::slide_view::SlideView;

const WIDTH: u16 = 1800;
const HEIGHT: u16 = 1100;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("an .xlsx or .pptx file");
    let out = args.next().expect("an output folder");
    let scale: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2.0);
    std::fs::create_dir_all(&out).expect("output folder");
    let bytes = std::fs::read(&file).expect("read");
    let write = |name: String, png: Vec<u8>| {
        let path = Path::new(&out).join(name);
        std::fs::write(&path, png).expect("write");
        println!("{}", path.display());
    };
    if file.to_ascii_lowercase().ends_with(".pptx") {
        let presentation = aster_ooxml::Pptx.read(bytes).expect("decode");
        let mut view = SlideView::new(Arc::new(presentation));
        view.resize(WIDTH, HEIGHT, scale);
        for index in 0..view.count() {
            view.go_to(index);
            write(
                format!("slide-{:02}.png", index + 1),
                view.encode_png().unwrap(),
            );
        }
    } else {
        let workbook = aster_ooxml::Xlsx.read(bytes).expect("decode");
        let sheets = workbook.sheets.len();
        let mut view = SheetView::new(Arc::new(workbook));
        view.set_expr_engine(Box::new(aster_calc::ExprProbe::new()));
        view.resize(WIDTH, HEIGHT, scale);
        for index in 0..sheets {
            view.set_sheet(index);
            write(
                format!("sheet-{:02}.png", index + 1),
                view.encode_png().unwrap(),
            );
        }
    }
}
