//! Time what the document view does on a 50-page document: the first
//! layout, the first frame, a scroll step, scrolling onto a page not yet
//! painted, a zoom step and a caret blink.
//!
//! `cargo run --release -p aster-surface --example doc_view_cost`

use std::sync::Arc;
use std::time::Instant;

use aster_surface::doc_view::{DocView, Zoom};
use aster_writer::{Block, Document, Paragraph, Run, RunLook, Section, Story};

fn look() -> RunLook {
    RunLook {
        size: 11.0,
        ..RunLook::default()
    }
}

fn paragraph(text: &str) -> Block {
    let paragraph = Paragraph {
        runs: vec![Run::text(text, look())],
        base: look(),
        mark: look(),
        ..Paragraph::default()
    };
    Block::paragraph(paragraph)
}

fn document(paragraphs: usize) -> Document {
    let latin = "The quick brown fox jumps over the lazy dog while the office suite lays \
                 out another paragraph of ordinary running text for the page. ";
    let chinese = "我们喜欢办公软件，也喜欢把文档排得整整齐齐，让每一页都清晰易读。";
    let blocks = (0..paragraphs)
        .map(|i| {
            if i % 3 == 2 {
                paragraph(&chinese.repeat(4))
            } else {
                paragraph(&latin.repeat(3))
            }
        })
        .collect();
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(Section::default()),
        ..Document::default()
    }
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// The median of `samples`, milliseconds.
fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.total_cmp(b));
    samples[samples.len() / 2]
}

fn main() {
    let mut count = 100;
    let mut view;
    loop {
        let started = Instant::now();
        view = DocView::new(document(count));
        if view.page_count() >= 50 {
            println!(
                "{} paragraphs, {} pages, laid out in {:.0} ms",
                count,
                view.page_count(),
                ms(started)
            );
            break;
        }
        count += 20;
    }
    // A laptop screen at 2x: 1400 by 900 logical pixels.
    view.resize(2800, 1800, 2.0);
    view.set_zoom(Zoom::FitWidth);
    let started = Instant::now();
    view.render();
    println!("first frame (two or three pages painted): {:.1} ms", ms(started));

    // A step of the wheel: the pages are already painted.
    let mut samples = Vec::new();
    for _ in 0..40 {
        view.scroll_by(96.0);
        let started = Instant::now();
        view.render();
        samples.push(ms(started));
    }
    println!(
        "scroll step, pages painted: median {:.1} ms, worst {:.1} ms",
        median(samples.clone()),
        samples.iter().copied().fold(0.0, f64::max)
    );

    // Onto a page that is not painted yet.
    let mut samples = Vec::new();
    for page in (5..45).step_by(5) {
        view.go_to_page(page);
        let started = Instant::now();
        view.render();
        samples.push(ms(started));
    }
    println!(
        "jump to an unpainted place: median {:.1} ms, worst {:.1} ms",
        median(samples.clone()),
        samples.iter().copied().fold(0.0, f64::max)
    );

    // Zoom steps up and down.
    let mut samples = Vec::new();
    view.set_zoom(Zoom::Percent(100.0));
    view.render();
    for _ in 0..3 {
        for up in [true, false, false, true] {
            if up {
                view.zoom_in();
            } else {
                view.zoom_out();
            }
            let started = Instant::now();
            view.render();
            samples.push(ms(started));
        }
    }
    println!(
        "zoom step: median {:.1} ms, worst {:.1} ms",
        median(samples.clone()),
        samples.iter().copied().fold(0.0, f64::max)
    );

    // The caret blinking (in the window), and a selection being dragged.
    if let Some(start) = view.document().start() {
        view.go_to(&start);
        view.render();
    }
    let mut samples = Vec::new();
    for n in 0..20 {
        view.set_caret_on(n % 2 == 0);
        let started = Instant::now();
        view.render();
        samples.push(ms(started));
    }
    println!("caret blink: median {:.1} ms", median(samples));
    let (x, y) = (900.0, 700.0);
    view.press(x, y, 1, false);
    let mut samples = Vec::new();
    for n in 0..20 {
        view.drag(x + 40.0 * f64::from(n), y + 30.0 * f64::from(n));
        let started = Instant::now();
        view.render();
        samples.push(ms(started));
    }
    println!(
        "drag step with a selection: median {:.1} ms",
        median(samples)
    );
    println!("painted pages held: {} MB", view.pixel_bytes() >> 20);

    let started = Instant::now();
    let hits = view.set_find(aster_surface::doc_view::DocQuery {
        pattern: "fox".into(),
        ..Default::default()
    });
    println!("find \"fox\": {hits} matches in {:.1} ms", ms(started));
}
