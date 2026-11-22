//! Phase 0 gate G6: does the command journal bring the document back, intact,
//! after the process is killed at an arbitrary moment?
//!
//! ```text
//! g6-storage crash-test [kills] [normal|full|fullfsync]   default 1000, full
//! g6-storage bench                                        what durability costs
//! ```
//!
//! `worker` is an internal mode: the crash test starts this binary with it.

mod bench;
mod crash;
mod doc;
mod store;

use std::path::Path;
use std::process::ExitCode;

use store::Durability;

const USAGE: &str =
    "usage: g6-storage crash-test [kills] [normal|full|fullfsync] | g6-storage bench";
const SEED: u64 = 0x0a57_e12f;

fn durability_arg(arg: Option<&String>) -> Result<Durability, String> {
    match arg {
        None => Ok(Durability::Full),
        Some(name) => {
            Durability::parse(name).ok_or_else(|| format!("unknown durability {name:?}\n{USAGE}"))
        }
    }
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("crash-test") => {
            let kills = match args.get(1) {
                None => 1000,
                Some(value) => value
                    .parse()
                    .map_err(|_| format!("kills must be a number, got {value:?}"))?,
            };
            let durability = durability_arg(args.get(2))?;
            let outcome = crash::run(kills, durability, SEED)?;
            println!(
                "G6 强杀测试 · {} · synchronous = {}",
                std::env::consts::OS,
                durability.name()
            );
            println!("  强杀次数                {}", outcome.kills);
            println!(
                "  其中发生在首次确认之前  {}",
                outcome.kills_before_first_ack
            );
            println!(
                "  已提交但未及确认的事务  {}（全部恢复）",
                outcome.unacknowledged_commits
            );
            println!("  已确认却丢失的事务      0");
            println!(
                "  已保存文件的校验次数    {}（全部完整）",
                outcome.saved_file_checks
            );
            println!("  最终修订号              {}", outcome.final_rev);
            println!("  最终文档长度            {} 字节", outcome.final_len);
            println!(
                "  单次恢复最多重放        {} 条日志",
                outcome.longest_replay
            );
            println!(
                "  用时                    {:.1} 秒",
                outcome.elapsed.as_secs_f64()
            );
            println!("  结果                    通过");
            Ok(())
        }
        Some("bench") => bench::run(),
        Some("worker") => {
            let (Some(dir), Some(seed)) = (args.get(1), args.get(2)) else {
                return Err(USAGE.into());
            };
            let seed = seed.parse().map_err(|_| "seed must be a number")?;
            crash::worker(Path::new(dir), seed, durability_arg(args.get(3))?)
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("g6-storage: {err}");
            ExitCode::FAILURE
        }
    }
}
