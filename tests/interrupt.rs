#![cfg(unix)]

mod common;

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::script;
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
    signal(child, libc::SIGINT);
}

fn signal(child: &Child, signal: libc::c_int) {
    unsafe { libc::kill(child.id() as libc::pid_t, signal) };
}

fn signal_kills_reviewers(signal_number: libc::c_int) {
    let dir = TempDir::new().unwrap();
    let survived = dir.path().join("survived");
    let reviewer = format!("#!/bin/sh\nsleep 2\ntouch '{}'\n", survived.display());
    let mut ncli = review_with(dir.path(), &reviewer);
    thread::sleep(Duration::from_millis(500));
    signal(&ncli, signal_number);
    let status = exit_within(&mut ncli, Duration::from_secs(5));
    assert_eq!(status.signal(), Some(signal_number), "{status}");
    thread::sleep(Duration::from_secs(3));
    assert!(!survived.exists(), "a reviewer outlived ncli");
}

#[test]
fn sigint_kills_reviewers() {
    signal_kills_reviewers(libc::SIGINT);
}

#[test]
fn sigterm_kills_reviewers() {
    signal_kills_reviewers(libc::SIGTERM);
}

#[test]
fn sighup_kills_reviewers() {
    signal_kills_reviewers(libc::SIGHUP);
}

#[test]
fn second_interrupt_exits_immediately() {
    let dir = TempDir::new().unwrap();
    let huge_verdict = r#"#!/bin/sh
cat > /dev/null
case "$*" in
*-schema*)
    printf '{"change_accessed": true, "findings": [{"severity": "low", "title": "t", "file": "f", "line": 1, "explanation": "'
    head -c 4000000 /dev/zero | tr '\0' x
    printf '", "fix": "x"}]}' ;;
*) echo answer ;;
esac
"#;
    let mut ncli = review_with(dir.path(), huge_verdict);
    let mut first_byte = [0];
    ncli.stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut first_byte)
        .unwrap();
    interrupt(&ncli);
    thread::sleep(Duration::from_millis(300));
    assert!(
        ncli.try_wait().unwrap().is_none(),
        "ncli should still be blocked writing to the unread stdout"
    );
    interrupt(&ncli);
    let status = exit_within(&mut ncli, Duration::from_secs(5));
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status}");
}
