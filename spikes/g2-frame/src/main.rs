//! Phase 0 gate G2: is the path from "the document changed" to "the frame is on
//! screen" fast enough when we draw the document ourselves on the CPU and hand
//! Slint a full-size image every frame?
//!
//! ```text
//! g2-frame bench [width height]           headless numbers, 2560x1440 by default
//! g2-frame window [full|tiled] [threads]  the same frames shown in a Slint window
//! ```
//!
//! Always run with `--release`; debug numbers mean nothing.

mod bench;
mod doc;
mod frame;
mod window;

use std::process::ExitCode;

use frame::Strategy;

const USAGE: &str = "usage: g2-frame bench [width height] | g2-frame window [full|tiled] [threads]";

fn default_threads() -> u16 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(1)
        .min(8) as u16
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        None | Some("bench") => {
            let mut config = bench::Config::default();
            if let (Some(width), Some(height)) = (args.get(1), args.get(2)) {
                config.width = width
                    .parse()
                    .map_err(|_| format!("width must be a number, got {width:?}"))?;
                config.height = height
                    .parse()
                    .map_err(|_| format!("height must be a number, got {height:?}"))?;
            }
            bench::run(config);
            Ok(())
        }
        Some("window") => {
            let strategy = match args.get(1).map(String::as_str) {
                None | Some("tiled") => Strategy::Tiled,
                Some("full") => Strategy::FullRepaint,
                Some(other) => return Err(format!("unknown strategy {other:?}\n{USAGE}").into()),
            };
            let threads = match args.get(2) {
                Some(value) => value
                    .parse()
                    .map_err(|_| format!("threads must be a number, got {value:?}"))?,
                None => default_threads(),
            };
            window::run(window::Config {
                width: 2560,
                height: 1440,
                paragraphs: 3000,
                strategy,
                threads,
                frames: 600,
                edits: 200,
                step: 60,
            })
        }
        Some(_) => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("g2-frame: {err}");
            ExitCode::FAILURE
        }
    }
}
