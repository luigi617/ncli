#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use common::{ncli, script};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ncli::error::Error;
use tempfile::TempDir;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
cat > "$NCLI_TEST_LOG/prompt"
pwd -P > "$NCLI_TEST_LOG/cwd"
echo '{"completed": true, "report": "- Shipped the standup command"}'
"#;

struct Env {
    bin: TempDir,
    log: TempDir,
    state: TempDir,
    claude: TempDir,
    codex: TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            bin: TempDir::new().unwrap(),
            log: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            claude: TempDir::new().unwrap(),
            codex: TempDir::new().unwrap(),
        };
        script(env.bin.path(), "claude", FAKE_CLAUDE);
        env
    }

    fn claude(&self, path: &str, cwd: &Path, times: &[Timestamp]) -> PathBuf {
        let entries = times.iter().map(|time| {
            format!(
                r#"{{"type":"user","timestamp":"{time}","cwd":"{}"}}"#,
                cwd.display()
            )
        });
        let lines = [r#"{"type":"mode","mode":"normal"}"#.to_owned()]
            .into_iter()
            .chain(entries);
        write(
            &self.claude.path().join("projects").join(path),
            lines,
            times,
        )
    }

    fn codex(&self, path: &str, cwd: &Path, times: &[Timestamp]) -> PathBuf {
        let meta = format!(
            r#"{{"timestamp":"{}","type":"session_meta","payload":{{"cwd":"{}"}}}}"#,
            times[0],
            cwd.display()
        );
        let entries = times.iter().map(|time| {
            format!(
                r#"{{"timestamp":"{time}","type":"response_item","payload":{{"type":"message"}}}}"#
            )
        });
        let lines = [meta].into_iter().chain(entries);
        write(&self.codex.path().join("sessions").join(path), lines, times)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = ncli(self.state.path());
        command
            .arg("standup")
            .args(args)
            .env(
                "PATH",
                format!("{}:/bin:/usr/bin", self.bin.path().display()),
            )
            .env("NCLI_TEST_LOG", self.log.path())
            .env("CLAUDE_CONFIG_DIR", self.claude.path())
            .env("CODEX_HOME", self.codex.path())
            .env("TZ", "UTC");
        command
    }

    fn prompt(&self) -> Option<String> {
        fs::read_to_string(self.log.path().join("prompt")).ok()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write(path: &Path, lines: impl Iterator<Item = String>, times: &[Timestamp]) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.map(|line| line + "\n").collect();
    fs::write(path, text).unwrap();
    let modified = times
        .iter()
        .max()
        .map_or(SystemTime::now(), |time| SystemTime::from(*time));
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    path.to_owned()
}

fn at(time: &str) -> Timestamp {
    time.parse().unwrap()
}

fn temp() -> PathBuf {
    fs::canonicalize(std::env::temp_dir()).unwrap()
}

#[test]
fn todays_sessions_are_summarized_without_ncli_s_own() {
    let env = Env::new();
    let now = [Timestamp::now()];
    let work = Path::new("/work/app");
    let claude = env.claude("app/today.jsonl", work, &now);
    let codex = env.codex("2026/11/01/rollout.jsonl", work, &now);
    let scratch = env.claude("scratch/today.jsonl", &temp().join("scratch"), &now);
    let old = env.claude(
        "app/old.jsonl",
        work,
        &[Timestamp::now() - SignedDuration::from_hours(72)],
    );
    let subagent = env.claude("app/today/subagents/agent.jsonl", work, &now);
    let reviewer = env.claude("review/today.jsonl", &temp().join("ncli-review-abc"), &now);
    let publisher = env.codex("2026/11/01/publish.jsonl", &temp().join("ncli-xyz"), &now);
    let output = env.run(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "- Shipped the standup command\n"
    );
    assert!(stderr(&output).contains("ncli standup: summarizing 3 sessions..."));
    let prompt = env.prompt().unwrap();
    for included in [&claude, &codex, &scratch] {
        assert!(
            prompt.contains(&format!("- {}\n", included.display())),
            "{prompt}"
        );
    }
    for excluded in [&old, &subagent, &reviewer, &publisher] {
        assert!(
            !prompt.contains(&excluded.display().to_string()),
            "{prompt}"
        );
    }
    let today = Timestamp::now().strftime("%Y-%m-%d").to_string();
    assert!(prompt.contains(&format!("Write my daily standup summary for {today}.")));
    assert!(
        prompt.contains(&format!("from {today}T00:00:00Z up to ")),
        "{prompt}"
    );
    assert!(!prompt.contains("<context>"));
    assert!(prompt.contains("3 to 5 high-level bullets for a daily standup"));
    assert!(prompt.contains(ncli::agent::STYLE), "{prompt}");
    let cwd = fs::read_to_string(env.log.path().join("cwd")).unwrap();
    let cwd = Path::new(cwd.trim());
    assert!(
        cwd.parent() == Some(&temp())
            && cwd
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ncli-"),
        "the summary must run where a later standup leaves its session out: {}",
        cwd.display()
    );
}

#[test]
fn an_earlier_day_keeps_only_sessions_active_that_day() {
    let env = Env::new();
    let work = Path::new("/work/app");
    let active = env.codex(
        "active.jsonl",
        work,
        &[at("2026-10-20T10:00:00Z"), at("2026-10-25T10:00:00Z")],
    );
    let later = env.claude(
        "app/later.jsonl",
        work,
        &[at("2026-10-21T00:00:00Z"), at("2026-10-25T10:00:00Z")],
    );
    let output = env.run(&["--day", "2026-10-20", "--context", "only the ncli work"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let prompt = env.prompt().unwrap();
    assert!(prompt.contains("summary for 2026-10-20."), "{prompt}");
    assert!(prompt.contains("from 2026-10-20T00:00:00Z up to 2026-10-21T00:00:00Z"));
    assert!(
        prompt.contains(&format!("- {}\n", active.display())),
        "{prompt}"
    );
    assert!(!prompt.contains(&later.display().to_string()), "{prompt}");
    assert!(prompt.contains("<context>\nonly the ncli work\n</context>"));
}

#[test]
fn yesterday_is_the_day_before_today() {
    let env = Env::new();
    let today = Timestamp::now().to_zoned(TimeZone::UTC).date();
    let yesterday = today.yesterday().unwrap();
    let noon = yesterday
        .at(12, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp();
    env.claude("app/yesterday.jsonl", Path::new("/work/app"), &[noon]);
    let output = env.run(&["--day", "yesterday"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let prompt = env.prompt().unwrap();
    assert!(prompt.contains(&format!(
        "from {yesterday}T00:00:00Z up to {today}T00:00:00Z"
    )));
}

#[test]
fn a_day_with_only_later_activity_is_an_error_before_any_agent_runs() {
    let env = Env::new();
    env.claude(
        "app/today.jsonl",
        Path::new("/work/app"),
        &[Timestamp::now()],
    );
    let output = env.run(&["--day", "2025-01-01"]);
    assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
    assert_eq!(
        stderr(&output).trim(),
        "ncli standup: error: no claude or codex sessions were active on 2025-01-01"
    );
    assert!(env.prompt().is_none());
}

#[test]
fn a_relative_session_directory_is_given_to_the_agent_as_an_absolute_path() {
    let env = Env::new();
    let session = env.claude(
        "app/today.jsonl",
        Path::new("/work/app"),
        &[Timestamp::now()],
    );
    let parent = env.claude.path().parent().unwrap();
    let name = env.claude.path().file_name().unwrap();
    let output = env
        .command(&[])
        .current_dir(parent)
        .env("CLAUDE_CONFIG_DIR", name)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let listed = fs::canonicalize(&session).unwrap();
    let prompt = env.prompt().unwrap();
    let path = prompt
        .lines()
        .find_map(|line| line.strip_prefix("- "))
        .unwrap();
    assert!(Path::new(path).is_absolute(), "{prompt}");
    assert_eq!(fs::canonicalize(path).unwrap(), listed);
}

#[test]
fn an_unknown_day_is_a_usage_error() {
    let env = Env::new();
    for day in ["tomorrowish", "2026-13-01"] {
        let output = env.run(&["--day", day]);
        assert_eq!(output.status.code(), Some(Error::USAGE.into()), "{day}");
        assert!(
            stderr(&output).contains("expected today, yesterday, or a date such as 2026-11-01"),
            "{}",
            stderr(&output)
        );
    }
    assert!(env.prompt().is_none());
}
