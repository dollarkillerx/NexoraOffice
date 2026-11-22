//! A short crash test that runs with the rest of the suite, so every platform CI
//! builds on also kills the process a few dozen times.

use std::process::Command;

fn crash_test(kills: &str, durability: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_g6-storage"))
        .args(["crash-test", kills, durability])
        .output()
        .expect("the binary starts");
    assert!(
        output.status.success(),
        "crash test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn survives_being_killed_with_synced_commits() {
    crash_test("40", "full");
}

#[test]
fn survives_being_killed_with_unsynced_commits() {
    crash_test("40", "normal");
}

#[test]
fn rejects_unknown_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_g6-storage"))
        .args(["crash-test", "3", "sometimes"])
        .output()
        .expect("the binary starts");
    assert!(!output.status.success());
}
