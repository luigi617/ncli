mod common;

use std::process::Output;

use common::install;
use ncli::error::Error;
use tempfile::TempDir;

fn run(args: &[&str]) -> Output {
    let state = TempDir::new().unwrap();
    common::ncli(state.path()).args(args).output().unwrap()
}

fn with_path(args: &[&str], installed: &[&str]) -> Output {
    let (dir, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    install(dir.path(), installed);
    common::ncli(state.path())
        .args(args)
        .env("PATH", dir.path())
        .output()
        .unwrap()
}

#[test]
fn version_prints_and_exits_zero() {
    let output = run(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("ncli {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_two() {
    for args in [
        &[][..],
        &["--nope"],
        &["code"],
        &["code", "nope"],
        &["code", "fix", "owner/repo#1"],
        &["code", "fix", "--context", ""],
        &["code", "publish-comment", "--context", ""],
        &["code", "review", "owner/repo#1"],
        &["code", "review", "https://"],
        &["code", "review", "ftp://example.com/pr/1"],
    ] {
        assert_eq!(run(args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn help_works_without_required_executables() {
    let output = with_path(&["code", "review", "--help"], &[]);
    assert!(output.status.success());
}

#[test]
fn missing_executables_stop_before_run_with_the_full_command_path() {
    let output = with_path(&["code", "review", "https://example.com/pr/1"], &["claude"]);
    assert_eq!(output.status.code(), Some(Error::MISSING_EXECUTABLE.into()));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "ncli code review: error: required executables not found on PATH: codex"
    );
}
