#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::script;
use ncli::cli::INTERRUPTED;
use tempfile::TempDir;

fn review_with(dir: &Path, reviewer: &str) -> Child {
    for name in ["claude", "codex"] {
        script(dir, name, reviewer);
    }
    Command::new(env!("CARGO_BIN_EXE_ncli"))
        .args(["review", "https://example.com/pr/1"])
        .env("PATH", format!("{}:/bin:/usr/bin", dir.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn exit_within(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    panic!("ncli did not exit within {limit:?}");
}

fn interrupt(child: &Child) {
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
}

#[test]
fn interrupt_kills_reviewers_and_exits_130() {
    let dir = TempDir::new().unwrap();
    let mut ncli = review_with(dir.path(), "#!/bin/sh\nsleep 30\n");
    thread::sleep(Duration::from_millis(500));
    interrupt(&ncli);
    let status = exit_within(&mut ncli, Duration::from_secs(5));
    let output = ncli.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(INTERRUPTED.into()));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("ncli review: error"));
}

#[test]
fn second_interrupt_exits_immediately() {
    let dir = TempDir::new().unwrap();
    let huge_answer = "#!/bin/sh\nhead -c 4000000 /dev/zero | tr '\\0' x\n";
    let mut ncli = review_with(dir.path(), huge_answer);
    thread::sleep(Duration::from_secs(1));
    interrupt(&ncli);
    thread::sleep(Duration::from_millis(300));
    assert!(
        ncli.try_wait().unwrap().is_none(),
        "ncli should still be blocked writing to the unread stdout"
    );
    interrupt(&ncli);
    let status = exit_within(&mut ncli, Duration::from_secs(5));
    assert_eq!(status.code(), Some(INTERRUPTED.into()));
}
