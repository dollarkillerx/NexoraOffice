//! Phase 0 gate G3: can we do our own line breaking for Chinese and Japanese on
//! top of Parley's shaping, and does it break the same way on every platform?
//!
//! ```text
//! g3-cjk lines [width-em]        how the samples break, with synthetic metrics
//! g3-cjk digest                  one number that must match on every platform
//! g3-cjk compare [width-em]      Parley's own breaking next to ours, real fonts
//! g3-cjk png <file> [width-em]   the same comparison, drawn
//! ```

mod breaker;
mod class;
mod dump;
mod metrics;
mod samples;
mod shape;

use std::process::ExitCode;

use vello_cpu::kurbo::{Affine, Rect};
use vello_cpu::peniko::Color;
use vello_cpu::{Pixmap, RenderContext, Resources};

use breaker::{Fx, Options, break_lines};
use samples::SAMPLES;
use shape::{Paragraph, Shaper};

const USAGE: &str =
    "usage: g3-cjk lines [width-em] | digest | compare [width-em] | png <file> [width-em]";
/// Body size for the real-font commands, in pixels.
const FONT_SIZE: f32 = 32.0;

fn width_arg(arg: Option<&String>, default: Fx) -> Result<Fx, String> {
    match arg {
        None => Ok(default),
        Some(value) => match value.parse::<Fx>() {
            Ok(width) if (2..=200).contains(&width) => Ok(width),
            _ => Err(format!(
                "width must be a whole number of ems from 2 to 200, got {value:?}"
            )),
        },
    }
}

/// Print Parley's lines and ours for every sample, and say where they differ.
fn compare(width_em: Fx) {
    let mut shaper = Shaper::new();
    let width_px = FONT_SIZE * width_em as f32;
    let max = (width_px * 64.0).round() as Fx;
    let (mut same, mut total) = (0, 0);
    for sample in &SAMPLES {
        let parley: Vec<&str> = shaper
            .parley_lines(sample.text, sample.language, FONT_SIZE, width_px)
            .into_iter()
            .map(|range| sample.text[range].trim_end_matches('\n'))
            .collect();
        let shaped = shaper.shape(sample.text, sample.language, FONT_SIZE);
        let lines = break_lines(&shaped.clusters, Options::cjk(max));
        let ours: Vec<String> = lines
            .iter()
            .map(|line| {
                let range = shaped.clusters[line.clusters.start].text.start
                    ..shaped.clusters[line.clusters.end - 1].text.end;
                let text = sample.text[range].trim_end_matches('\n');
                dump::describe(line, (FONT_SIZE * 64.0) as Fx, text)
            })
            .collect();

        println!(
            "== {} · {} em · {} px font",
            sample.name, width_em, FONT_SIZE
        );
        for n in 0..parley.len().max(ours.len()) {
            let theirs = parley.get(n).copied().unwrap_or("");
            let mine = ours.get(n).map(String::as_str).unwrap_or("");
            let mine_text = mine
                .split_once('|')
                .map_or("", |(_, t)| t.trim_end_matches('|'));
            let equal = theirs.trim_end() == mine_text.trim_end();
            total += 1;
            same += usize::from(equal);
            println!("  {} Parley |{theirs}|", if equal { ' ' } else { '≠' });
            println!("    ours   {mine}");
        }
    }
    println!("{same} of {total} lines break at the same place");
}

/// Draw every sample twice: Parley's breaking on the left, ours on the right.
fn png(path: &str, width_em: Fx) -> Result<(), String> {
    let mut shaper = Shaper::new();
    let column = f64::from(FONT_SIZE) * f64::from(width_em);
    let (margin, gutter) = (48.0, 96.0);
    let width = (margin * 2.0 + column * 2.0 + gutter + f64::from(FONT_SIZE)) as u16;
    let height: u16 = 2400;
    let max = (column * 64.0).round() as Fx;

    let mut ctx = RenderContext::new(width, height);
    let mut resources = Resources::new();
    ctx.set_transform(Affine::IDENTITY);
    ctx.set_paint(Color::from_rgb8(0xff, 0xff, 0xff));
    ctx.fill_rect(&Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));

    let right = margin + column + gutter;
    let mut y = margin;
    for (title, x) in [
        ("Parley 自带断行", margin),
        ("自研断行器（全部规则）", right),
    ] {
        let heading = Paragraph {
            text: title,
            language: "zh-Hans",
            font_size: 22.0,
        };
        shaper.paint_parley(&mut ctx, &mut resources, &heading, 2000.0, (x, y));
    }
    y += 56.0;
    for sample in &SAMPLES {
        let top = y;
        let paragraph = Paragraph {
            text: sample.text,
            language: sample.language,
            font_size: FONT_SIZE,
        };
        let left_height = shaper.paint_parley(
            &mut ctx,
            &mut resources,
            &paragraph,
            column as f32,
            (margin, y),
        );
        let shaped = shaper.shape(sample.text, sample.language, FONT_SIZE);
        let lines = break_lines(&shaped.clusters, Options::cjk(max));
        let right_height = shaped.paint(&mut ctx, &mut resources, &lines, FONT_SIZE, (right, y));
        y += left_height.max(right_height);
        shape::paint_rule(&mut ctx, margin + column, top, y);
        shape::paint_rule(&mut ctx, right + column, top, y);
        y += 40.0;
    }

    let mut pixmap = Pixmap::new(width, height);
    ctx.flush();
    ctx.render(&mut pixmap, &mut resources);
    let used = (y.ceil() as u16).min(height);
    let cropped = pixmap.data()[..usize::from(width) * usize::from(used)].to_vec();
    let bytes = Pixmap::from_parts(cropped, width, used)
        .into_png()
        .map_err(|err| err.to_string())?;
    std::fs::write(path, bytes).map_err(|err| format!("cannot write {path}: {err}"))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("lines") => {
            print!("{}", dump::dump(width_arg(args.get(1), 18)?));
            Ok(())
        }
        Some("digest") => {
            println!("{:#018x}", dump::digest());
            Ok(())
        }
        Some("compare") => {
            compare(width_arg(args.get(1), 18)?);
            Ok(())
        }
        Some("png") => {
            let path = args.get(1).ok_or(USAGE)?;
            png(path, width_arg(args.get(2), 18)?)
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("g3-cjk: {err}");
            ExitCode::FAILURE
        }
    }
}
