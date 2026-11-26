//! Time the layout of a large document: 5,000 paragraphs of mixed Chinese
//! and Latin text, about 100 characters each.
//!
//! `cargo run --release -p aster-layout --example bench_layout`
//!
//! Prints the first layout, a layout after changing one paragraph, a layout
//! of the same document again, and the number of pages.

use std::time::Instant;

use aster_layout::Layouter;
use aster_writer::{Block, Document, ParaProps, Paragraph, Path, Run, RunLook, Story};

const WORDS: [&str; 8] = [
    "排版引擎",
    "需要处理",
    "中文和",
    "English",
    "mixed text",
    "段落与页面",
    "Rust 1.95",
    "标点符号，",
];

fn document(paragraphs: usize) -> Document {
    let look = RunLook {
        font: Some("Calibri".into()),
        east_asian_font: Some("宋体".into()),
        size: 10.5,
        ..RunLook::default()
    };
    let mut state = 12_345u32;
    let mut next = move || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (state >> 16) as usize
    };
    let blocks = (0..paragraphs)
        .map(|_| {
            let mut text = String::new();
            while text.chars().count() < 100 {
                text.push_str(WORDS[next() % WORDS.len()]);
                if next() % 3 == 0 {
                    text.push(' ');
                }
            }
            text.push('。');
            Block::paragraph(Paragraph {
                runs: vec![Run::text(text, look.clone())],
                props: ParaProps {
                    space_after: 120,
                    ..ParaProps::default()
                },
                base: look.clone(),
                mark: look.clone(),
                ..Paragraph::default()
            })
        })
        .collect();
    Document {
        body: Story { blocks, part: None },
        ..Document::default()
    }
}

fn main() {
    let paragraphs = 5_000;
    let before = document(paragraphs);
    let mut layouter = Layouter::new();

    let started = Instant::now();
    let layout = layouter.layout(&before);
    let first = started.elapsed();
    println!(
        "first layout: {:.0} ms, {} pages, {} paragraphs shaped",
        first.as_secs_f64() * 1000.0,
        layout.pages.len(),
        layouter.shaped_count()
    );

    let mut after = before.clone();
    after
        .paragraph_mut(&Path::body(paragraphs / 2))
        .expect("paragraph")
        .runs[0]
        .text
        .push_str("，改动了一个段落。");
    let shaped = layouter.shaped_count();
    let started = Instant::now();
    let changed = layouter.layout(&after);
    println!(
        "after changing one paragraph: {:.1} ms, {} shaped again, {} pages",
        started.elapsed().as_secs_f64() * 1000.0,
        layouter.shaped_count() - shaped,
        changed.pages.len()
    );

    let started = Instant::now();
    let again = layouter.layout(&after);
    println!(
        "unchanged: {:.1} ms, {} pages",
        started.elapsed().as_secs_f64() * 1000.0,
        again.pages.len()
    );
}
