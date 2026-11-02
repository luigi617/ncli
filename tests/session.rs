#![cfg(unix)]

mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use common::{ncli, script};
use ncli::error::Error;
use tempfile::TempDir;

const FAKE_TMUX: &str = r#"#!/bin/sh
{ for arg in "$@"; do printf '[%s]' "$arg"; done; echo; } >> "$NCLI_TEST_DIR/calls"
case "$1" in
list-sessions)
    if [ -n "$NCLI_TEST_LIST_ERROR" ]; then
        echo "$NCLI_TEST_LIST_ERROR" >&2
        exit 1
    fi
    printf '%b' "$NCLI_TEST_SESSIONS"
    ;;
display-message) echo "$NCLI_TEST_CURRENT" ;;
capture-pane) printf 'line 1\nline 2\n\n\n' ;;
attach-session)
    [ -n "$NCLI_TEST_ATTACH_FAILS" ] && exit 1
    read -r line
    echo "attached: $line"
    ;;
esac
"#;

const NO_SERVER: &str = "no server running on /tmp/tmux-501/default";

struct Env {
    bin: TempDir,
    dir: TempDir,
    state: TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            bin: TempDir::new().unwrap(),
            dir: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
        };
        script(env.bin.path(), "tmux", FAKE_TMUX);
        env
    }

    fn without_tmux() -> Self {
        let env = Self::new();
        fs::remove_file(env.bin.path().join("tmux")).unwrap();
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = ncli(self.state.path());
        command
            .arg("session")
            .args(args)
            .current_dir(self.dir.path())
            .env("PATH", self.bin.path())
            .env("NCLI_TEST_DIR", self.dir.path())
            .env("NCLI_TEST_LIST_ERROR", "")
            .env_remove("TMUX");
        command
    }

    fn sessions(&self, args: &[&str], sessions: &str) -> Output {
        self.command(args)
            .env("NCLI_TEST_SESSIONS", sessions)
            .output()
            .unwrap()
    }

    fn inside(&self, args: &[&str], sessions: &str) -> Output {
        self.command(args)
            .env("NCLI_TEST_SESSIONS", sessions)
            .env("TMUX", "/tmp/tmux-501/default,1,0")
            .env("NCLI_TEST_CURRENT", "work")
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn saved_output(&self) -> Option<String> {
        fs::read_to_string(self.state.path().join("output.txt")).ok()
    }
}

const SESSIONS: &str = r"2\t1\twork\n1\t0\tscratch pad\n";
const LIST: &str = "[list-sessions][-F][#{session_windows}\t#{session_attached}\t#{session_name}]";

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_failed(output: &Output, code: u8, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(code.into()),
        "{}",
        stderr(output)
    );
    assert!(output.stdout.is_empty(), "{}", stdout(output));
    assert_eq!(stderr(output).trim(), message);
}

#[test]
fn add_creates_a_session_in_the_current_directory_and_attaches_outside_tmux() {
    let env = Env::new();
    let mut child = env
        .command(&["add", "--name", "new"])
        .env("NCLI_TEST_SESSIONS", SESSIONS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from the terminal\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "attached: from the terminal\n");
    let cwd = fs::canonicalize(env.dir.path()).unwrap();
    assert_eq!(
        env.calls(),
        [
            LIST.to_owned(),
            format!("[new-session][-d][-s][new][-c][{}]", cwd.display()),
            "[attach-session][-t][=new]".to_owned(),
        ]
    );
}

#[test]
fn add_escapes_tmux_formats_in_the_current_directory() {
    let env = Env::new();
    let dir = env.dir.path().join("w#{pid}");
    fs::create_dir(&dir).unwrap();
    let output = env
        .command(&["add", "--name", "new"])
        .current_dir(&dir)
        .env("NCLI_TEST_SESSIONS", SESSIONS)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let cwd = fs::canonicalize(env.dir.path()).unwrap();
    assert_eq!(
        env.calls()[1],
        format!(
            "[new-session][-d][-s][new][-c][{}/w##{{pid}}]",
            cwd.display()
        )
    );
}

#[test]
fn add_escapes_a_trailing_semicolon_in_the_current_directory() {
    let env = Env::new();
    let dir = env.dir.path().join("dir;");
    fs::create_dir(&dir).unwrap();
    let output = env
        .command(&["add", "--name", "new"])
        .current_dir(&dir)
        .env("NCLI_TEST_SESSIONS", SESSIONS)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let cwd = fs::canonicalize(env.dir.path()).unwrap();
    assert_eq!(
        env.calls()[1],
        format!("[new-session][-d][-s][new][-c][{}/dir\\;]", cwd.display())
    );
}

#[test]
fn add_switches_the_client_inside_tmux() {
    let env = Env::new();
    let output = env.inside(&["add", "--name", "new"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls()[2], "[switch-client][-t][=new]");
    assert_eq!(env.calls().len(), 3);
}

#[test]
fn add_works_when_no_server_is_running() {
    let env = Env::new();
    let output = env
        .command(&["add", "--name", "new"])
        .env("NCLI_TEST_LIST_ERROR", NO_SERVER)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(env.calls()[1].starts_with("[new-session][-d][-s][new][-c]"));
}

#[test]
fn add_refuses_an_existing_name() {
    let env = Env::new();
    let output = env.sessions(&["add", "--name", "work"], SESSIONS);
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session add: error: a tmux session named work already exists",
    );
    assert_eq!(env.calls(), [LIST]);
}

#[test]
fn ls_lists_each_session_and_saves_the_output() {
    let env = Env::new();
    let output = env.sessions(&["ls"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    let listed = "  work: 2 windows, attached\n  scratch pad: 1 window, not attached";
    assert_eq!(stdout(&output), format!("{listed}\n"));
    assert_eq!(env.calls(), [LIST]);
    assert_eq!(env.saved_output().as_deref(), Some(listed));
}

#[test]
fn ls_marks_the_current_session_inside_tmux() {
    let env = Env::new();
    let output = env.inside(&["ls"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "* work: 2 windows, attached\n  scratch pad: 1 window, not attached\n"
    );
    assert_eq!(
        env.calls(),
        [LIST, "[display-message][-p][#{session_name}]"]
    );
}

#[test]
fn ls_without_a_server_says_there_are_no_sessions() {
    for message in [
        NO_SERVER,
        "error connecting to /tmp/tmux-501/default (No such file or directory)",
    ] {
        let env = Env::new();
        let output = env
            .command(&["ls"])
            .env("NCLI_TEST_LIST_ERROR", message)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), "no sessions\n");
    }
}

#[test]
fn ls_reports_any_other_tmux_failure() {
    let env = Env::new();
    let output = env
        .command(&["ls"])
        .env(
            "NCLI_TEST_LIST_ERROR",
            "error connecting to /tmp/tmux-501/default (Permission denied)",
        )
        .output()
        .unwrap();
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session ls: error: tmux exited with code 1: error connecting to /tmp/tmux-501/default (Permission denied)",
    );
}

#[test]
fn use_attaches_outside_tmux_and_switches_inside() {
    let env = Env::new();
    let output = env
        .command(&["use", "scratch pad"])
        .env("NCLI_TEST_SESSIONS", SESSIONS)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls(), [LIST, "[attach-session][-t][=scratch pad]"]);

    let env = Env::new();
    let output = env.inside(&["use", "scratch pad"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls(), [LIST, "[switch-client][-t][=scratch pad]"]);
}

#[test]
fn a_failed_attach_is_an_error() {
    let env = Env::new();
    let output = env
        .command(&["use", "work"])
        .env("NCLI_TEST_SESSIONS", SESSIONS)
        .env("NCLI_TEST_ATTACH_FAILS", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session use: error: tmux exited with code 1",
    );
}

#[test]
fn use_kill_and_logs_refuse_a_missing_session() {
    for command in ["use", "kill", "logs"] {
        let env = Env::new();
        let output = env.sessions(&[command, "wor"], SESSIONS);
        assert_failed(
            &output,
            Error::FAILURE,
            &format!("ncli session {command}: error: there is no tmux session named wor"),
        );
        assert_eq!(env.calls(), [LIST], "{command}");
    }
}

#[test]
fn a_missing_session_without_a_server_is_still_missing() {
    let env = Env::new();
    let output = env
        .command(&["kill", "work"])
        .env("NCLI_TEST_LIST_ERROR", NO_SERVER)
        .output()
        .unwrap();
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session kill: error: there is no tmux session named work",
    );
}

#[test]
fn detach_detaches_the_current_client_inside_tmux() {
    let env = Env::new();
    let output = env.inside(&["detach"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls(), ["[detach-client]"]);
}

#[test]
fn detach_outside_tmux_is_an_error() {
    let env = Env::new();
    let output = env.sessions(&["detach"], SESSIONS);
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session detach: error: not inside a tmux session, so there is no session to detach from",
    );
    assert!(env.calls().is_empty());
}

#[test]
fn kill_targets_the_exact_session() {
    let env = Env::new();
    let output = env.sessions(&["kill", "work"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    assert_eq!(env.calls(), [LIST, "[kill-session][-t][=work]"]);
}

#[test]
fn logs_prints_the_active_pane_with_100_lines_of_history_by_default() {
    let env = Env::new();
    let output = env.sessions(&["logs", "work"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "line 1\nline 2\n");
    assert_eq!(
        env.calls(),
        [LIST, "[capture-pane][-p][-t][=work:][-S][-100]"]
    );
    assert_eq!(env.saved_output().as_deref(), Some("line 1\nline 2"));
}

#[test]
fn logs_includes_the_requested_lines_of_history() {
    let env = Env::new();
    let output = env.sessions(&["logs", "work", "--lines", "5"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls()[1], "[capture-pane][-p][-t][=work:][-S][-5]");
}

#[test]
fn rename_renames_the_exact_session() {
    let env = Env::new();
    let output = env.sessions(&["rename", "work", "play"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(env.calls(), [LIST, "[rename-session][-t][=work][--][play]"]);
}

#[test]
fn rename_passes_a_new_name_starting_with_a_dash_as_a_name() {
    let env = Env::new();
    let output = env.sessions(&["rename", "work", "--", "-dash"], SESSIONS);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        env.calls(),
        [LIST, "[rename-session][-t][=work][--][-dash]"]
    );
}

#[test]
fn rename_refuses_a_missing_session_or_an_existing_name() {
    let env = Env::new();
    let output = env.sessions(&["rename", "nope", "play"], SESSIONS);
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session rename: error: there is no tmux session named nope",
    );

    let env = Env::new();
    let output = env.sessions(&["rename", "work", "scratch pad"], SESSIONS);
    assert_failed(
        &output,
        Error::FAILURE,
        "ncli session rename: error: a tmux session named scratch pad already exists",
    );
    assert!(
        !env.calls()
            .iter()
            .any(|call| call.starts_with("[rename-session]"))
    );
}

#[test]
fn commands_other_than_ls_and_logs_keep_the_saved_output() {
    let env = Env::new();
    fs::write(env.state.path().join("output.txt"), "earlier").unwrap();
    for args in [
        &["kill", "work"][..],
        &["rename", "work", "play"],
        &["kill", "nope"],
        &["detach"],
    ] {
        env.sessions(args, SESSIONS);
    }
    assert_eq!(env.saved_output().as_deref(), Some("earlier"));
    assert!(!env.state.path().join("failure.txt").exists());
}

#[test]
fn invalid_names_are_usage_errors_before_tmux_runs() {
    for args in [
        &["add", "--name", ""][..],
        &["add", "--name", "  "],
        &["add", "--name", "a:b"],
        &["add", "--name", "x#{pid}"],
        &["rename", "work", "r#{session_windows}"],
        &["kill", "work;"],
        &["rename", "other", "play;"],
        &["add", "--name", "a\\b"],
        &["kill", "a\\b"],
        &["use", "a.b"],
        &["kill", ":"],
        &["logs", "a.b"],
        &["rename", "work", "a:b"],
        &["rename", " ", "play"],
        &["logs", "work", "--lines", "0"],
    ] {
        let env = Env::new();
        let output = env.sessions(args, SESSIONS);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(env.calls().is_empty(), "{args:?}");
    }
}

#[test]
fn a_missing_tmux_stops_before_anything_runs() {
    let env = Env::without_tmux();
    for args in [&["ls"][..], &["detach"], &["add", "--name", "new"]] {
        let output = env.command(args).output().unwrap();
        assert_failed(
            &output,
            Error::MISSING_EXECUTABLE,
            &format!(
                "ncli session {}: error: required executables not found on PATH: tmux",
                args[0]
            ),
        );
    }
}
