//! In-window measurements: the same frames, but handed to Slint and shown.
//!
//! What is timed here that the headless benchmark cannot see is the cost of
//! Slint taking a new full-size image every frame and drawing it, and whether the
//! whole loop keeps up with the display.
//!
//! The run is automatic: it scrolls for a fixed number of frames, then types a
//! fixed number of characters, prints the numbers and exits.
//!
//! Observing when Slint has drawn a frame takes a trick, because neither of the
//! obvious hooks works with the Skia renderer on macOS:
//!
//! - `Window::set_rendering_notifier` returns `Ok` and then never calls back. The
//!   Metal surface does not expose a graphics API to hand to the callback.
//! - winit's `RedrawRequested` arrives once, at start-up. After that Slint draws
//!   from a `CADisplayLink` tick and no window event is involved.
//!
//! So the window binds a property to a pure callback that takes the frame number.
//! Slint evaluates that binding while it draws the frame, which calls back into
//! Rust; a zero-delay timer armed there fires as soon as the draw has returned.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Image, Timer, TimerMode};

use crate::bench::{BUDGET_MS, summarize};
use crate::doc::Document;
use crate::frame::{Framer, Strategy};

slint::include_modules!();

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub paragraphs: usize,
    pub strategy: Strategy,
    pub threads: u16,
    pub frames: usize,
    pub edits: usize,
    pub step: i64,
}

/// Frames at the start of the run that are not counted.
const WARMUP: usize = 30;
/// Give up if the window never gets drawn (hidden, occluded, no display).
const WATCHDOG: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Scroll,
    Typing,
    Done,
}

/// Written when a render completes, read by the frame loop.
#[derive(Default)]
struct Marks {
    rendered: Cell<bool>,
    last: Cell<Option<Instant>>,
    edit_started: Cell<Option<Instant>>,
    /// Time between consecutive completed renders.
    intervals: RefCell<Vec<f64>>,
    /// Time Slint spent drawing one frame: image upload, draw, present.
    renders: RefCell<Vec<f64>>,
    /// Time from the start of handling a key press to its frame being rendered.
    latencies: RefCell<Vec<f64>>,
}

impl Marks {
    fn render_completed(&self, redraw_started: Instant) {
        let now = Instant::now();
        let ms = |from: Instant| now.duration_since(from).as_secs_f64() * 1e3;
        self.renders.borrow_mut().push(ms(redraw_started));
        if let Some(previous) = self.last.replace(Some(now)) {
            self.intervals.borrow_mut().push(ms(previous));
        }
        if let Some(started) = self.edit_started.take() {
            self.latencies.borrow_mut().push(ms(started));
        }
        self.rendered.set(true);
    }
}

struct Run {
    config: Config,
    doc: Document,
    framer: Option<Framer>,
    size: (u16, u16),
    top: i64,
    direction: i64,
    phase: Phase,
    frame_no: usize,
    edit_no: usize,
    edit_paragraph: usize,
    produce: Vec<f64>,
    /// Time to handle one key press: re-layout, render, hand the frame to Slint.
    edit_work: Vec<f64>,
    /// How many interval and render samples belong to the scroll phase.
    scroll_samples: usize,
    started: Instant,
}

fn line(label: &str, samples: &[f64], note: &str) {
    let s = summarize(samples);
    println!(
        "  {label:<14}  p50 {:>6.2}  p95 {:>6.2}  p99 {:>6.2}  max {:>6.2} ms  {note}",
        s.p50, s.p95, s.p99, s.max
    );
}

fn report(run: &Run, marks: &Marks) {
    let (width, height) = run.size;
    println!(
        "G2 窗口内测量 · {width}×{height} 物理像素 · {} · {} 线程 · {}",
        run.config.strategy.label(),
        run.config.threads + 1,
        if cfg!(debug_assertions) {
            "DEBUG 构建，数字无参考价值，请加 --release"
        } else {
            "release 构建"
        },
    );
    let intervals = marks.intervals.borrow();
    let renders = marks.renders.borrow();
    let scroll_end = run.scroll_samples.min(intervals.len());
    if scroll_end <= WARMUP {
        println!(
            "  没有采集到足够的帧，窗口可能被遮挡或没有显示（已出 {} 帧，完成渲染 {} 次）",
            run.frame_no,
            renders.len()
        );
        return;
    }

    let scroll = &intervals[WARMUP..scroll_end];
    let s = summarize(scroll);
    let late = scroll.iter().filter(|ms| **ms > s.p50 * 1.5).count();
    line(
        "滚动帧间隔",
        scroll,
        &format!("（{} 帧，约 {:.0} Hz）", s.count, 1000.0 / s.p50),
    );
    println!(
        "  {:<14}  {late} / {}，占 {:.2}%（间隔超过中位数的 1.5 倍）",
        "迟到的帧",
        s.count,
        late as f64 * 100.0 / s.count as f64
    );
    line(
        "出帧耗时",
        &run.produce[WARMUP.min(run.produce.len() - 1)..],
        "（排版加渲染成像素，不含 Slint）",
    );
    line(
        "Slint 上屏耗时",
        &renders[WARMUP.min(renders.len() - 1)..],
        "（Slint 开始绘制到绘制返回，含纹理上传）",
    );

    // A key press is handled right after the previous frame was drawn, then waits
    // for the next display refresh. Its latency therefore sits near one refresh
    // period by construction; what matters is that the work fits inside one period
    // so the character is on screen at the very next refresh.
    let latencies = marks.latencies.borrow();
    if latencies.is_empty() || run.edit_work.is_empty() {
        return;
    }
    let refresh = s.p50;
    let typing_renders = &renders[scroll_end.min(renders.len() - 1)..];
    let work = summarize(&run.edit_work);
    let draw = summarize(typing_renders);
    line(
        "按键处理耗时",
        &run.edit_work,
        "（段落重排、渲染、交给 Slint）",
    );
    line("按键后 Slint 绘制", typing_renders, "");
    let on_time = latencies.iter().filter(|ms| **ms <= refresh * 1.5).count();
    line(
        "按键到帧提交",
        &latencies,
        &format!("（含等待下一次刷新，刷新周期 {refresh:.2} ms）"),
    );
    let fits = work.p95 + draw.p95 <= BUDGET_MS.min(refresh) && on_time == latencies.len();
    println!(
        "  {:<14}  {on_time} / {} 次按键在下一次刷新时上屏；处理加绘制的 p95 合计 {:.2} ms  {}",
        "结论",
        latencies.len(),
        work.p95 + draw.p95,
        if fits { "达标" } else { "超出" }
    );
}

pub fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .select()?;

    let ui = BenchWindow::new()?;
    ui.window()
        .set_size(slint::PhysicalSize::new(config.width, config.height));

    let marks = Rc::new(Marks::default());
    ui.on_frame_drawn({
        let marks = marks.clone();
        move |_frame_no| {
            let started = Instant::now();
            let marks = marks.clone();
            Timer::single_shot(Duration::ZERO, move || marks.render_completed(started));
            0.0
        }
    });

    let run = Rc::new(RefCell::new(Run {
        config,
        doc: Document::generate(config.paragraphs),
        framer: None,
        size: (0, 0),
        top: 0,
        direction: 1,
        phase: Phase::Scroll,
        frame_no: 0,
        edit_no: 0,
        edit_paragraph: 0,
        produce: Vec::new(),
        edit_work: Vec::new(),
        scroll_samples: 0,
        started: Instant::now(),
    }));

    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(1), {
        let (run, marks, ui) = (run.clone(), marks.clone(), ui.as_weak());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut run = run.borrow_mut();
            let run = &mut *run;

            if run.phase == Phase::Done {
                return;
            }
            if run.started.elapsed() > WATCHDOG {
                report(run, &marks);
                run.phase = Phase::Done;
                let _ = slint::quit_event_loop();
                return;
            }

            // Lay the document out once the real window size is known.
            if run.framer.is_none() {
                let size = ui.window().size();
                let scale = ui.window().scale_factor();
                if size.width == 0 || size.height == 0 {
                    return;
                }
                let width = size.width.min(u32::from(u16::MAX)) as u16;
                let height = size.height.min(u32::from(u16::MAX)) as u16;
                run.size = (width, height);
                run.doc.layout_all(width, scale);
                run.framer = Some(Framer::new(
                    run.config.strategy,
                    width,
                    height,
                    run.config.threads,
                ));
            }
            // One frame in flight at a time: wait until Slint drew the last one.
            if !marks.rendered.replace(false) {
                return;
            }
            let framer = run.framer.as_mut().expect("created above");

            match run.phase {
                Phase::Scroll => {
                    if run.frame_no >= run.config.frames + WARMUP {
                        run.scroll_samples = marks.intervals.borrow().len();
                        run.edit_paragraph = run
                            .doc
                            .paragraph_at(run.top as f64 + f64::from(run.size.1) / 2.0);
                        run.phase = Phase::Typing;
                        marks.rendered.set(true);
                        return;
                    }
                    let limit = (run.doc.total_height() as i64 - i64::from(run.size.1)).max(1);
                    run.top += run.direction * run.config.step;
                    if run.top >= limit || run.top <= 0 {
                        run.direction = -run.direction;
                        run.top = run.top.clamp(0, limit);
                    }
                    let t0 = Instant::now();
                    let (buffer, _) = framer.frame(&run.doc, run.top);
                    run.produce.push(t0.elapsed().as_secs_f64() * 1e3);
                    ui.set_frame(Image::from_rgba8_premultiplied(buffer));
                    run.frame_no += 1;
                    ui.set_frame_no(run.frame_no as i32);
                }
                Phase::Typing => {
                    if run.edit_no >= run.config.edits {
                        report(run, &marks);
                        run.phase = Phase::Done;
                        let _ = slint::quit_event_loop();
                        return;
                    }
                    let started = Instant::now();
                    let index = run.edit_paragraph;
                    let (band_top, _) = run.doc.paragraph_band(index);
                    let text = if run.edit_no % 2 == 0 { "字" } else { "a" };
                    if run.doc.append(index, text) {
                        framer.invalidate_from(band_top);
                    } else {
                        let (band_top, band_bottom) = run.doc.paragraph_band(index);
                        framer.invalidate(band_top, band_bottom);
                    }
                    let (buffer, _) = framer.frame(&run.doc, run.top);
                    marks.edit_started.set(Some(started));
                    ui.set_frame(Image::from_rgba8_premultiplied(buffer));
                    run.edit_work.push(started.elapsed().as_secs_f64() * 1e3);
                    run.edit_no += 1;
                    ui.set_frame_no((run.frame_no + run.edit_no) as i32);
                }
                Phase::Done => {}
            }
        }
    });

    ui.run()?;
    Ok(())
}
