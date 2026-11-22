//! What durability costs: how long one journal append takes at each setting, and
//! how long a checkpoint and a document save take.

use std::time::Instant;

use crate::doc::{Command, Snapshot};
use crate::store::{Durability, Store, write_atomically};

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let index = (p * sorted.len() as f64).ceil() as usize;
    sorted[index.clamp(1, sorted.len()) - 1]
}

fn line(label: &str, samples: &mut [f64], note: &str) {
    samples.sort_by(f64::total_cmp);
    println!(
        "  {label:<34} p50 {:>7.3}  p95 {:>7.3}  p99 {:>7.3}  max {:>7.3} ms  {note}",
        percentile(samples, 0.50),
        percentile(samples, 0.95),
        percentile(samples, 0.99),
        samples[samples.len() - 1],
    );
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "G6 写入延迟 · {} · {}",
        std::env::consts::OS,
        if cfg!(debug_assertions) {
            "DEBUG 构建，请加 --release"
        } else {
            "release 构建"
        }
    );
    let payload = Command::Insert {
        at: 0,
        text: "排版引擎".to_owned(),
    }
    .encode();
    let megabyte = Snapshot {
        rev: 1,
        text: "排版引擎 Aster Office\n".repeat(40_000),
    };
    let megabyte_bytes = megabyte.encode();

    println!("日志追加：每次按键一个事务，{} 字节", payload.len());
    for durability in [Durability::Normal, Durability::Full, Durability::FullFsync] {
        let dir = tempfile::tempdir()?;
        let mut store = Store::open(dir.path(), durability)?;
        let count = if durability == Durability::FullFsync {
            300
        } else {
            3000
        };
        let started = Instant::now();
        let mut samples = Vec::with_capacity(count);
        for rev in 1..=count as u64 {
            let t0 = Instant::now();
            store.append(rev, &payload)?;
            samples.push(t0.elapsed().as_secs_f64() * 1e3);
        }
        let per_second = count as f64 / started.elapsed().as_secs_f64();
        line(
            &format!("synchronous = {}", durability.name()),
            &mut samples,
            &format!("（每秒 {per_second:.0} 次）"),
        );
    }

    println!(
        "检查点与保存：{:.2} MB 的文档",
        megabyte_bytes.len() as f64 / 1e6
    );
    let dir = tempfile::tempdir()?;
    let mut store = Store::open(dir.path(), Durability::Full)?;
    let mut checkpoints = Vec::new();
    for rev in 1..=20 {
        let snapshot = Snapshot {
            rev,
            text: megabyte.text.clone(),
        };
        let t0 = Instant::now();
        store.checkpoint(&snapshot)?;
        checkpoints.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    line("检查点（写文件、改指针、截断日志）", &mut checkpoints, "");
    let mut saves = Vec::new();
    let path = dir.path().join("document.aster");
    for _ in 0..20 {
        let t0 = Instant::now();
        write_atomically(&path, &megabyte_bytes)?;
        saves.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    line("原子保存（临时文件、刷盘、改名）", &mut saves, "");
    Ok(())
}
