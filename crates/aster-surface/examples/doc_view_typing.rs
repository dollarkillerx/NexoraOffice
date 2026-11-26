//! Time typing in the middle of a long document: one key press with the
//! layout it causes and the frame that shows it.
//!
//! `cargo run --release -p aster-surface --example doc_view_typing`

use std::sync::Arc;
use std::time::Instant;

use aster_surface::doc_view::{DocView, Move, Zoom};
use aster_writer::{Block, Document, Paragraph, Run, RunLook, Section, Story};

fn paragraph(text: &str) -> Block {
    let look = RunLook {
        size: 11.0,
        ..RunLook::default()
    };
    Block::paragraph(Paragraph {
        runs: vec![Run::text(text, look.clone())],
        base: look.clone(),
        mark: look,
        ..Paragraph::default()
    })
}

fn document(paragraphs: usize) -> Document {
    let latin =
        "The quick brown fox jumps over the lazy dog while the office suite lays out text. ";
    let chinese = "我们喜欢办公软件，也喜欢把文档排得整整齐齐。";
    let blocks = (0..paragraphs)
        .map(|i| {
            if i % 3 == 2 {
                paragraph(&chinese.repeat(3))
            } else {
                paragraph(&latin.repeat(2))
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

fn stats(mut samples: Vec<f64>) -> String {
    samples.sort_by(|a, b| a.total_cmp(b));
    format!(
        "median {:.1} ms, 90th {:.1} ms, worst {:.1} ms",
        samples[samples.len() / 2],
        samples[samples.len() * 9 / 10],
        samples[samples.len() - 1]
    )
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);
    let started = Instant::now();
    let mut view = DocView::new(document(count));
    println!(
        "{count} paragraphs, {} pages, built in {:.0} ms",
        view.page_count(),
        ms(started)
    );
    view.resize(2800, 1800, 2.0);
    view.set_zoom(Zoom::Percent(100.0));
    view.render();
    // The middle of the document.
    let middle = view.document().paragraphs()[count / 2].0.clone();
    let at = aster_writer::Pos::new(middle, 10);
    view.go_to(&at);
    view.render();
    let mut edit = Vec::new();
    let mut frame = Vec::new();
    let mut check = Vec::new();
    let mut total = Vec::new();
    for n in 0..60 {
        let started = Instant::now();
        view.type_text(if n % 2 == 0 { "a" } else { "中" });
        edit.push(ms(started));
        // The shell asks whether the document is modified after every edit,
        // to mark the tab.
        let asked = Instant::now();
        assert!(view.is_modified());
        check.push(ms(asked));
        let painted = Instant::now();
        view.render();
        frame.push(ms(painted));
        total.push(ms(started));
    }
    println!("edit and layout: {}", stats(edit));
    println!("is_modified after each key: {}", stats(check));
    println!("frame: {}", stats(frame));
    println!("key press, layout and frame: {}", stats(total));
    // Enter and Backspace, which move the paragraphs after the caret.
    let mut total = Vec::new();
    for n in 0..20 {
        let started = Instant::now();
        if n % 2 == 0 {
            view.enter();
        } else {
            view.backspace(aster_surface::doc_view::Unit::Cluster);
        }
        view.render();
        total.push(ms(started));
    }
    println!("Enter or Backspace, layout and frame: {}", stats(total));
    // Cursor keys, for comparison.
    let mut total = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        view.move_caret(Move::Right, false);
        view.render();
        total.push(ms(started));
    }
    println!("arrow key and frame: {}", stats(total));
    let started = Instant::now();
    view.undo();
    view.render();
    println!("undo and frame: {:.1} ms", ms(started));
}
