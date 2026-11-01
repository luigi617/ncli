#![cfg(unix)]

mod common;

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{ncli, script};
use tempfile::TempDir;

fn review_command(dir: &Path, reviewer: &str) -> Command {
    for name in ["claude", "codex"] {
        script(dir, name, reviewer);
    }
    let mut command = ncli(&dir.join("state"));
    command
        .args(["code", "review", "https://example.com/pr/1"])
        .env("PATH", format!("{}:/bin:/usr/bin", dir.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn review_with(dir: &Path, reviewer: &str) -> Child {
    review_command(dir, reviewer).spawn().unwrap()
}

// Each fake reviewer marks that it started, so a signal is only sent once both are running.
const SLOW_REVIEWER: &str = r#"#!/bin/sh
touch "$0.started"
cat > /dev/null
case "$*" in
*-schema*) sleep 1; echo '{"change_accessed": true, "findings": []}' ;;
*) sleep 2; touch "$0.survived"; echo answer ;;
esac
"#;

fn wait_until_reviewers_started(dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !["claude", "codex"]
        .iter()
        .all(|name| dir.join(format!("{name}.started")).exists())
    {
        assert!(Instant::now() < deadline, "the reviewers never started");
        thread::sleep(Duration::from_millis(20));
    }
}

fn reviewer_survived(dir: &Path) -> bool {
    ["claude", "codex"]
        .iter()
        .any(|name| dir.join(format!("{name}.survived")).exists())
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
    let mut ncli = review_with(dir.path(), SLOW_REVIEWER);
    wait_until_reviewers_started(dir.path());
    signal(&ncli, signal_number);
    let status = exit_within(&mut ncli, Duration::from_secs(5));
    assert_eq!(status.signal(), Some(signal_number), "{status}");
    thread::sleep(Duration::from_secs(3));
    assert!(!reviewer_survived(dir.path()), "a reviewer outlived ncli");
}

#[test]
fn a_signal_ignored_by_the_parent_stays_ignored() {
    let dir = TempDir::new().unwrap();
    let mut command = review_command(dir.path(), SLOW_REVIEWER);
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut ncli = command.spawn().unwrap();
    wait_until_reviewers_started(dir.path());
    signal(&ncli, libc::SIGHUP);
    let status = exit_within(&mut ncli, Duration::from_secs(15));
    assert!(status.success(), "{status}");
    assert!(reviewer_survived(dir.path()));
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
fn sigquit_kills_reviewers() {
    signal_kills_reviewers(libc::SIGQUIT);
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
