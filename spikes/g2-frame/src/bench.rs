//! Headless measurements: how long it takes to produce a frame, without a window
//! or a display in the loop. The in-window numbers are in `window.rs`.

use std::time::Instant;

use crate::doc::Document;
use crate::frame::{Framer, Strategy, Timing};

/// The gate's frame budget: one 60 Hz frame.
pub const BUDGET_MS: f64 = 16.0;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub width: u16,
    pub height: u16,
    pub scale: f32,
    pub paragraphs: usize,
    pub frames: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 2560,
            height: 1440,
            scale: 2.0,
            paragraphs: 3000,
            frames: 600,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

/// Nearest-rank percentiles over a set of samples.
pub fn summarize(samples: &[f64]) -> Summary {
    assert!(!samples.is_empty(), "no samples");
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let index = (p * sorted.len() as f64).ceil() as usize;
        sorted[index.clamp(1, sorted.len()) - 1]
    };
    Summary {
        count: sorted.len(),
        p50: rank(0.50),
        p95: rank(0.95),
        p99: rank(0.99),
        max: sorted[sorted.len() - 1],
    }
}

fn verdict(p95: f64) -> &'static str {
    if p95 <= BUDGET_MS { "达标" } else { "超出" }
}

fn row(label: &str, totals: &[f64], parts: &[Timing]) {
    let s = summarize(totals);
    let mean = |f: fn(&Timing) -> f64| parts.iter().map(f).sum::<f64>() / parts.len() as f64;
    println!(
        "  {label:<34} p50 {:>6.2}  p95 {:>6.2}  p99 {:>6.2}  max {:>6.2}  | 均值: 场景 {:>5.2} 栅格 {:>5.2} 合成 {:>5.2}  {}",
        s.p50,
        s.p95,
        s.p99,
        s.max,
        mean(|t| t.scene_ms),
        mean(|t| t.raster_ms),
        mean(|t| t.compose_ms),
        verdict(s.p95),
    );
}

fn laid_out(config: &Config) -> Document {
    let mut doc = Document::generate(config.paragraphs);
    doc.layout_all(config.width, config.scale);
    doc
}

/// Scroll through the document at a constant speed and time every frame.
fn scroll(doc: &Document, config: &Config, strategy: Strategy, threads: u16, step: i64) {
    let mut framer = Framer::new(strategy, config.width, config.height, threads);
    let limit = (doc.total_height() as i64 - i64::from(config.height)).max(1);
    let mut top = 0_i64;
    // Let glyph caches and allocators settle before measuring.
    for _ in 0..20 {
        framer.frame(doc, top);
        top = (top + step) % limit;
    }
    let (mut totals, mut parts) = (Vec::new(), Vec::new());
    for _ in 0..config.frames {
        let (_, timing) = framer.frame(doc, top);
        totals.push(timing.total_ms());
        parts.push(timing);
        top = (top + step) % limit;
    }
    let label = format!("{} · {} 线程 · {step} px/帧", strategy.label(), threads + 1);
    row(&label, &totals, &parts);
}

/// Type into a paragraph in the middle of the viewport: re-lay-out that
/// paragraph, invalidate what it covers, produce the frame.
fn typing(config: &Config, strategy: Strategy, threads: u16) {
    let mut doc = laid_out(config);
    let mut framer = Framer::new(strategy, config.width, config.height, threads);
    let top = (doc.total_height() / 3.0) as i64;
    let index = doc.paragraph_at(top as f64 + f64::from(config.height) / 2.0);
    framer.frame(&doc, top);

    let (mut totals, mut layouts, mut parts) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..200 {
        let text = if i % 2 == 0 { "字" } else { "a" };
        let t0 = Instant::now();
        let (band_top, _) = doc.paragraph_band(index);
        let moved = doc.append(index, text);
        let layout_ms = t0.elapsed().as_secs_f64() * 1e3;
        if moved {
            framer.invalidate_from(band_top);
        } else {
            let (band_top, band_bottom) = doc.paragraph_band(index);
            framer.invalidate(band_top, band_bottom);
        }
        let (_, timing) = framer.frame(&doc, top);
        totals.push(layout_ms + timing.total_ms());
        layouts.push(layout_ms);
        parts.push(timing);
    }
    let label = format!("打字 · {} · {} 线程", strategy.label(), threads + 1);
    row(&label, &totals, &parts);
    println!(
        "  {:<34} 其中段落重排 p50 {:.3} ms，p95 {:.3} ms",
        "",
        summarize(&layouts).p50,
        summarize(&layouts).p95
    );
}

pub fn run(config: Config) {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(1)
        .min(8) as u16;
    println!(
        "G2 无窗口基准 · {}×{} 物理像素 · 缩放 {} · {} 段 · 每项 {} 帧 · {} · {}",
        config.width,
        config.height,
        config.scale,
        config.paragraphs,
        config.frames,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "DEBUG 构建，数字无参考价值，请加 --release"
        } else {
            "release 构建"
        },
    );

    let t0 = Instant::now();
    let doc = laid_out(&config);
    let layout_ms = t0.elapsed().as_secs_f64() * 1e3;
    let screens = doc.total_height() / f64::from(config.height);
    println!(
        "全文排版 {:.0} ms（每段 {:.3} ms），文档高度 {:.0} px，约 {:.0} 屏",
        layout_ms,
        layout_ms / config.paragraphs as f64,
        doc.total_height(),
        screens
    );
    println!("以下耗时单位均为毫秒，预算为 p95 ≤ {BUDGET_MS} ms");

    println!("滚动");
    for threads in [0, workers] {
        scroll(&doc, &config, Strategy::FullRepaint, threads, 60);
    }
    for threads in [0, workers] {
        scroll(&doc, &config, Strategy::Tiled, threads, 60);
    }
    // A flick: more than a tile of new content every frame.
    for threads in [0, workers] {
        scroll(&doc, &config, Strategy::Tiled, threads, 600);
    }

    println!("打字");
    for strategy in [Strategy::FullRepaint, Strategy::Tiled] {
        for threads in [0, workers] {
            typing(&config, strategy, threads);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summarize(&samples);
        assert_eq!((s.p50, s.p95, s.p99, s.max), (50.0, 95.0, 99.0, 100.0));
        assert_eq!(s.count, 100);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let s = summarize(&[7.0]);
        assert_eq!((s.p50, s.p95, s.p99, s.max), (7.0, 7.0, 7.0, 7.0));
    }
}
