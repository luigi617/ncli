mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::child;
use ncli::commands::review::{Reviewer, debate};
use ncli::error::Error;
use ncli::process::Cancellation;
use tempfile::TempDir;

const LINK: &str = "https://github.com/owner/repo/pull/1";

fn reviewer(name: &str, args: &[&str]) -> Reviewer {
    Reviewer {
        name: name.to_owned(),
        cli: child(args),
    }
}

fn reviewers(dir: &Path) -> Vec<Reviewer> {
    let dir = dir.to_str().unwrap();
    ["a", "b", "c"]
        .into_iter()
        .map(|name| reviewer(name, &["review", dir, name]))
        .collect()
}

fn prompts(dir: &Path, name: &str) -> Vec<String> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let file = path.file_name().unwrap().to_string_lossy();
            file.starts_with(&format!("{name}.prompt."))
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect()
}

fn run_debate(reviewers: &[Reviewer], judge: &Reviewer) -> (Result<(), Error>, String) {
    let mut out = Vec::new();
    let result = debate(LINK, reviewers, judge, &Cancellation::default(), &mut out);
    (result, String::from_utf8(out).unwrap())
}

#[test]
fn reviewers_review_rebut_and_judge() {
    let dir = TempDir::new().unwrap();
    let reviewers = reviewers(dir.path());
    let (result, transcript) = run_debate(&reviewers, &reviewers[1]);
    result.unwrap();

    let [review, rebuttal] = &prompts(dir.path(), "a")[..] else {
        panic!("a should be asked twice");
    };
    assert!(review.contains(LINK));
    let (own, others) = rebuttal
        .split_once("The other reviewers' findings:")
        .unwrap();
    assert!(own.contains("a answer 0"));
    assert!(others.contains("b answer 0") && others.contains("c answer 0"));
    assert!(!others.contains("a answer 0"));

    let verdict_prompt = &prompts(dir.path(), "b")[2];
    for name in ["a", "b", "c"] {
        assert!(verdict_prompt.contains(&format!("## Review: {name}\n\n{name} answer 0")));
        assert!(verdict_prompt.contains(&format!("## Rebuttal: {name}\n\n{name} answer 1")));
    }
    assert_eq!(prompts(dir.path(), "c").len(), 2);

    let position = |section: &str| transcript.find(section).unwrap();
    assert!(position("## Review: a") < position("## Rebuttal: a"));
    assert!(position("## Rebuttal: c") < position("## Verdict: b"));
    assert!(transcript.ends_with("## Rebuttal: c\n\nc answer 1\n\n## Verdict: b\n\nb answer 2\n"));
}

#[test]
fn failing_reviewer_stops_the_others() {
    let reviewers = [
        reviewer("slow", &["sleep", "30"]),
        reviewer("broken", &["fail", "3", "not logged in"]),
    ];
    let started = Instant::now();
    let (result, transcript) = run_debate(&reviewers, &reviewers[0]);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .ends_with("exited with code 3: not logged in")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(transcript.is_empty());
}

#[test]
fn finished_stages_are_kept_when_the_judge_fails() {
    let dir = TempDir::new().unwrap();
    let reviewers = reviewers(dir.path());
    let judge = reviewer("judge", &["fail", "3", "out of credits"]);
    let (result, transcript) = run_debate(&reviewers, &judge);
    assert!(result.is_err());
    assert!(transcript.contains("## Review: a\n\na answer 0"));
    assert!(transcript.ends_with("## Rebuttal: c\n\nc answer 1\n"));
}

#[test]
fn every_call_runs_in_its_own_working_directory() {
    let reviewers = [reviewer("a", &["cwd"]), reviewer("b", &["cwd"])];
    let (result, transcript) = run_debate(&reviewers, &reviewers[0]);
    result.unwrap();
    let mut dirs: Vec<&str> = transcript
        .lines()
        .filter(|line| line.contains("ncli-review-"))
        .collect();
    assert_eq!(dirs.len(), 5);
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), 5);
}

#[cfg(unix)]
mod binary {
    use std::process::{Command, Output};

    use super::LINK;
    use crate::common::script;
    use ncli::error::Error;
    use tempfile::TempDir;

    const FAKE_REVIEWER: &str = r#"#!/bin/sh
prompt=$(cat)
n=$(ls "$0".prompt.* 2>/dev/null | wc -l | tr -d ' ')
printf '%s' "$prompt" > "$0.prompt.$n"
echo "$(basename "$0") answer $n"
"#;

    fn run_ncli(reviewers: &[(&str, &str)], args: &[&str]) -> Output {
        let dir = TempDir::new().unwrap();
        for (name, body) in reviewers {
            script(dir.path(), name, body);
        }
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(env!("CARGO_BIN_EXE_ncli"))
            .args(args)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    #[test]
    fn prints_the_debate_and_verdict() {
        let output = run_ncli(
            &[("claude", FAKE_REVIEWER), ("codex", FAKE_REVIEWER)],
            &["review", LINK, "--judge", "codex"],
        );
        assert!(output.status.success());
        let out = String::from_utf8(output.stdout).unwrap();
        assert!(out.contains("## Review: claude"));
        assert!(out.ends_with("## Verdict: codex\n\ncodex answer 2\n"));
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(err.contains("ncli review: verdict..."));
    }

    #[test]
    fn failing_reviewer_error_is_reported() {
        let broken = "#!/bin/sh\nkill -TERM $$\n";
        let output = run_ncli(
            &[("claude", FAKE_REVIEWER), ("codex", broken)],
            &["review", LINK],
        );
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(output.stdout.is_empty());
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(err.contains("ncli review: error: codex was killed by signal 15"));
        assert!(err.contains("ncli: codex (attempt 1/3) failed: "));
    }
}
