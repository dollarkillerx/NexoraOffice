//! The macOS sandbox and the closing of inherited descriptors, through the
//! child binary's hidden `--sandbox-selftest` mode.

#![cfg(target_os = "macos")]

#[path = "../src/builder.rs"]
mod builder;

use std::os::fd::AsRawFd;
use std::process::Command;

use builder::{Builder, one_page};

/// Run the self-test on `bytes`; the lines it printed.
fn selftest(bytes: &[u8]) -> Vec<String> {
    run(bytes, "--sandbox-selftest")
}

fn run(bytes: &[u8], mode: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.pdf");
    std::fs::write(&path, bytes).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_aster-pdf-worker"));
    command.arg(mode).arg(&path);
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "failed: {text}");
    println!("{text}");
    text.lines().map(str::to_owned).collect()
}

/// The pixels that are not white, from the "render ok" line.
fn ink(lines: &[String]) -> usize {
    let line = lines.iter().find(|l| l.starts_with("render ok")).unwrap();
    line.rsplit(' ').next().unwrap().parse().unwrap()
}

fn assert_contained(lines: &[String]) {
    let has = |s: &str| lines.iter().any(|l| l.starts_with(s));
    assert!(has("sandbox on"), "{lines:?}");
    assert!(has("render ok"), "{lines:?}");
    for denied in [
        "read denied",
        "write denied",
        "connect denied",
        "resolve denied",
        "spawn denied",
    ] {
        assert!(has(denied), "{denied} missing: {lines:?}");
    }
}

#[test]
fn a_simple_page_renders_inside_the_sandbox() {
    let bytes = Builder::document(&[b"1 0 0 rg 0 0 200 100 re f"]);
    let lines = selftest(&bytes);
    assert_contained(&lines);
    assert!(ink(&lines) > 10_000, "{lines:?}");
}

#[test]
fn a_system_font_is_read_inside_the_sandbox() {
    let bytes = one_page(
        |b| {
            let descendant = b.add(
                "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >> \
                 /DW 1000 >>",
            );
            let font = b.add(format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light \
                 /Encoding /UniGB-UCS2-H /DescendantFonts [{descendant} 0 R] >>"
            ));
            format!("<< /Font << /F1 {font} 0 R >> >>")
        },
        "",
        b"BT /F1 40 Tf 10 30 Td <4F60597D4E16754C> Tj ET",
    );
    let lines = selftest(&bytes);
    assert_contained(&lines);
    // The glyphs come from the system font read inside the sandbox.
    assert!(ink(&lines) > 500, "{lines:?}");
    // Control: without the font directories the fallback cannot be read, so
    // the page must come out clearly different.
    let control = run(&bytes, "--sandbox-selftest-no-fonts");
    println!("ink with fonts {}, without {}", ink(&lines), ink(&control));
    assert!(
        control.iter().any(|l| l.starts_with("render ok")),
        "{control:?}"
    );
    assert!(ink(&control).abs_diff(ink(&lines)) > 500, "{control:?}");
}

#[test]
fn inherited_descriptors_are_closed() {
    let file = std::fs::File::open("/etc/hosts").unwrap();
    let fd = file.as_raw_fd();
    // Make it survive exec: clear close-on-exec.
    // SAFETY: plain fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }
    let bytes = Builder::document(&[b"0 g 0 0 10 10 re f"]);
    let lines = selftest(&bytes);
    let before = lines.iter().find(|l| l.starts_with("fds before")).unwrap();
    let after = lines.iter().find(|l| l.starts_with("fds after")).unwrap();
    assert!(before.contains(&fd.to_string()), "{lines:?}");
    assert_eq!(after, "fds after []", "{lines:?}");
    drop(file);
}
