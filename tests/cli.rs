mod common;

use std::process::{Command, Output};

use common::install;
use ncli::error::Error;
use tempfile::TempDir;

fn ncli(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ncli"));
    command.args(args);
    command
}

fn with_path(args: &[&str], installed: &[&str]) -> Output {
    let dir = TempDir::new().unwrap();
    install(dir.path(), installed);
    ncli(args).env("PATH", dir.path()).output().unwrap()
}

#[test]
fn version_prints_and_exits_zero() {
    let output = ncli(&["--version"]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("ncli {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_two() {
    for args in [&[][..], &["nope"], &["review"]] {
        assert_eq!(
            ncli(args).output().unwrap().status.code(),
            Some(2),
            "{args:?}"
        );
    }
}

#[test]
fn help_works_without_required_executables() {
    let output = with_path(&["review", "--help"], &[]);
    assert!(output.status.success());
}

#[test]
fn missing_executables_stop_before_run_with_the_full_command_path() {
    let output = with_path(&["review", "https://example.com/pr/1"], &["claude"]);
    assert_eq!(output.status.code(), Some(Error::MISSING_EXECUTABLE.into()));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "ncli review: error: required executables not found on PATH: codex"
    );
}
