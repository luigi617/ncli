mod common;

use std::ffi::OsStr;
use std::fs;

use common::path_with;
use ncli::error::Error;
use ncli::process::require_executables;

fn require(names: &[&str]) -> Result<(), Error> {
    require_executables(names.iter().map(OsStr::new))
}

#[test]
fn require_executables_reports_every_missing_name_once_in_order() {
    let _path = path_with(&["claude"]);
    let error = require(&["gemini", "claude", "codex", "gemini"]).unwrap_err();
    assert_eq!(error.exit_code(), Error::MISSING_EXECUTABLE);
    assert_eq!(
        error.to_string(),
        "required executables not found on PATH: gemini, codex"
    );
}

#[test]
fn require_executables_treats_non_executable_files_as_missing() {
    let path = path_with(&[]);
    fs::write(path.dir.path().join("claude"), "").unwrap();
    assert!(require(&["claude"]).is_err());
}

#[test]
fn require_executables_passes_when_all_installed() {
    let _path = path_with(&["claude", "codex"]);
    require(&["claude", "codex"]).unwrap();
}
