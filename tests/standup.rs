#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use common::{ncli, script};
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

    fn session(&self, root: &TempDir, path: &str, age: Duration) -> PathBuf {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        ncli(self.state.path())
            .arg("standup")
            .args(args)
            .env(
                "PATH",
                format!("{}:/bin:/usr/bin", self.bin.path().display()),
            )
            .env("NCLI_TEST_LOG", self.log.path())
            .env("CLAUDE_CONFIG_DIR", self.claude.path())
            .env("CODEX_HOME", self.codex.path())
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    fn prompt(&self) -> Option<String> {
        fs::read_to_string(self.log.path().join("prompt")).ok()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const NOW: Duration = Duration::ZERO;
const DAYS_AGO: Duration = Duration::from_secs(3 * 24 * 60 * 60);

#[test]
fn todays_claude_and_codex_sessions_are_summarized() {
    let env = Env::new();
    let claude = env.session(&env.claude, "projects/app/today.jsonl", NOW);
    let codex = env.session(&env.codex, "sessions/2026/11/01/rollout.jsonl", NOW);
    let old = env.session(&env.claude, "projects/app/old.jsonl", DAYS_AGO);
    let subagent = env.session(&env.claude, "projects/app/today/subagents/agent.jsonl", NOW);
    let output = env.run(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "- Shipped the standup command\n"
    );
    assert!(stderr(&output).contains("ncli standup: summarizing 2 sessions..."));
    let prompt = env.prompt().unwrap();
    for included in [&claude, &codex] {
        assert!(
            prompt.contains(&format!("- {}\n", included.display())),
            "{prompt}"
        );
    }
    for excluded in [&old, &subagent] {
        assert!(
            !prompt.contains(&excluded.display().to_string()),
            "{prompt}"
        );
    }
    let today = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    assert!(prompt.contains(&format!("Write my daily standup summary for {today}.")));
    assert!(
        prompt.contains(&format!("from {today}T00:00:00Z up to ")),
        "{prompt}"
    );
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(prompt.contains(&format!("working directory is under {}:", temp.display())));
    assert!(!prompt.contains("<context>"));
    assert!(prompt.contains("3 to 5 high-level bullets for a daily standup"));
    assert!(prompt.contains(ncli::agent::STYLE), "{prompt}");
    let cwd = fs::read_to_string(env.log.path().join("cwd")).unwrap();
    assert!(
        Path::new(cwd.trim()).starts_with(&temp),
        "the summary must not run in a directory whose session would appear in a later standup"
    );
}

#[test]
fn an_earlier_day_covers_its_window_and_the_context_is_given() {
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let two_days_ago = today.checked_sub(jiff::Span::new().days(2)).unwrap();
    for (day, date) in [
        ("yesterday".to_owned(), today.yesterday().unwrap()),
        (two_days_ago.to_string(), two_days_ago),
    ] {
        let env = Env::new();
        env.session(
            &env.codex,
            "sessions/day.jsonl",
            Duration::from_secs(60 * 60),
        );
        let output = env.run(&["--day", &day, "--context", "only the ncli work"]);
        assert!(output.status.success(), "{day}: {}", stderr(&output));
        let prompt = env.prompt().unwrap();
        let next = date.tomorrow().unwrap();
        assert!(prompt.contains(&format!("summary for {date}.")), "{prompt}");
        assert!(
            prompt.contains(&format!("from {date}T00:00:00Z up to {next}T00:00:00Z")),
            "{prompt}"
        );
        assert!(prompt.contains("<context>\nonly the ncli work\n</context>"));
    }
}

#[test]
fn a_day_without_sessions_is_an_error_before_any_agent_runs() {
    let env = Env::new();
    env.session(&env.claude, "projects/app/old.jsonl", DAYS_AGO);
    let output = env.run(&["--day", "today"]);
    assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
    let today = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    assert_eq!(
        stderr(&output).trim(),
        format!("ncli standup: error: no claude or codex sessions were active on {today}")
    );
    assert!(env.prompt().is_none());
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
