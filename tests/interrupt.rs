#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::script;
use ncli::cli::INTERRUPTED;
use tempfile::TempDir;

#[test]
fn interrupt_kills_reviewers_and_exits_130() {
    let dir = TempDir::new().unwrap();
    for name in ["claude", "codex"] {
        script(dir.path(), name, "#!/bin/sh\nsleep 30\n");
    }
    let ncli = Command::new(env!("CARGO_BIN_EXE_ncli"))
        .args(["review", "https://example.com/pr/1"])
        .env("PATH", format!("{}:/bin:/usr/bin", dir.path().display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    unsafe { libc::kill(ncli.id() as libc::pid_t, libc::SIGINT) };
    let output = ncli.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(INTERRUPTED.into()));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("ncli review: error"));
}
