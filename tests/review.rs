mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::child;
use ncli::commands::Context;
use ncli::commands::code::review::{Reviewer, SchemaArg, Verdict, debate, schema};
use ncli::error::Error;
use ncli::process::Cancellation;
use tempfile::TempDir;

const LINK: &str = "https://github.com/owner/repo/pull/1";
const ONE_FINDING: &str = r#"{"change_accessed": true, "findings": [{"severity": "high", "title": "Bug", "file": "a.rs",
    "line": 3, "explanation": "It breaks.", "fix": "Fix it."}]}"#;

fn reviewer(name: &str, args: &[&str]) -> Reviewer {
    Reviewer {
        name: name.to_owned(),
        cli: child(args),
        schema_arg: SchemaArg::Inline("--schema"),
    }
}

fn judge(dir: &Path, verdict: &str, schema_arg: SchemaArg) -> Reviewer {
    Reviewer {
        schema_arg,
        ..reviewer("judge", &["verdict", dir.to_str().unwrap(), verdict])
    }
}

fn judge_received(dir: &Path, what: &str) -> String {
    fs::read_to_string(dir.join(format!("judge.{what}"))).unwrap()
}

fn assert_judge_received_the_schema(dir: &Path) {
    let received = judge_received(dir, "schema");
    assert!(!received.contains('\n'), "the schema must fit on one line");
    assert_eq!(received, schema());
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

fn run_debate(reviewers: &[Reviewer], judge: &Reviewer) -> Result<Verdict, Error> {
    let context = Context {
        prog: "ncli code review",
        cancellation: &Cancellation::default(),
    };
    debate(LINK, reviewers, judge, &context)
}

#[test]
fn reviewers_review_rebut_and_judge() {
    let dir = TempDir::new().unwrap();
    let reviewers = reviewers(dir.path());
    let judge = judge(dir.path(), ONE_FINDING, SchemaArg::Inline("--schema"));
    let verdict = run_debate(&reviewers, &judge).unwrap();
    assert_eq!(verdict, Verdict::parse(ONE_FINDING).unwrap());

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

    let verdict_prompt = judge_received(dir.path(), "prompt");
    for name in ["a", "b", "c"] {
        assert!(verdict_prompt.contains(&format!("## Review: {name}\n\n{name} answer 0")));
        assert!(verdict_prompt.contains(&format!("## Rebuttal: {name}\n\n{name} answer 1")));
    }
    assert_eq!(prompts(dir.path(), "c").len(), 2);
    assert_judge_received_the_schema(dir.path());
}

#[test]
fn the_schema_can_be_passed_as_a_file() {
    let dir = TempDir::new().unwrap();
    let reviewers = [reviewer("a", &["write", "a answer"])];
    let judge = judge(dir.path(), ONE_FINDING, SchemaArg::File("--schema-file"));
    run_debate(&reviewers, &judge).unwrap();
    assert_judge_received_the_schema(dir.path());
}

#[test]
fn a_change_the_judge_could_not_access_is_an_error() {
    let dir = TempDir::new().unwrap();
    let reviewers = [reviewer("a", &["write", "I could not open the link."])];
    let unavailable = r#"{"change_accessed": false, "findings": []}"#;
    let judge = judge(dir.path(), unavailable, SchemaArg::Inline("--schema"));
    let error = run_debate(&reviewers, &judge).unwrap_err();
    assert!(matches!(error, Error::ChangeUnavailable(_)), "{error}");
    assert_eq!(
        error.to_string(),
        format!("the judge could not access the change at {LINK}")
    );
}

#[test]
fn an_invalid_verdict_is_an_error() {
    let dir = TempDir::new().unwrap();
    let reviewers = [reviewer("a", &["write", "a answer"])];
    let judge = judge(
        dir.path(),
        "Looks good to me!",
        SchemaArg::Inline("--schema"),
    );
    let error = run_debate(&reviewers, &judge).unwrap_err();
    assert!(matches!(error, Error::Verdict { .. }), "{error}");
    assert!(
        error.to_string().ends_with(": Looks good to me!"),
        "{error}"
    );
}

#[test]
fn failing_reviewer_stops_the_others() {
    let reviewers = [
        reviewer("slow", &["sleep", "30"]),
        reviewer("broken", &["fail", "3", "not logged in"]),
    ];
    let started = Instant::now();
    assert!(
        run_debate(&reviewers, &reviewers[0])
            .unwrap_err()
            .to_string()
            .ends_with("exited with code 3: not logged in")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn every_call_runs_in_its_own_working_directory() {
    let dir = TempDir::new().unwrap();
    let reviewers = [reviewer("a", &["cwd"]), reviewer("b", &["cwd"])];
    let judge = judge(dir.path(), ONE_FINDING, SchemaArg::Inline("--schema"));
    run_debate(&reviewers, &judge).unwrap();
    let judge_prompt = judge_received(dir.path(), "prompt");
    let mut dirs: Vec<&str> = judge_prompt
        .lines()
        .filter(|line| line.contains("ncli-review-"))
        .collect();
    assert_eq!(dirs.len(), 4);
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), 4, "reviewers shared a working directory");
}

#[cfg(unix)]
mod binary {
    use std::process::{Command, Output, Stdio};

    use super::LINK;
    use crate::common::script;
    use ncli::error::Error;
    use tempfile::TempDir;

    const FAKE_REVIEWER: &str = r#"#!/bin/sh
cat > /dev/null
case "$*" in
*--output-schema*|*--json-schema*)
    echo '{"change_accessed": true, "findings": [{"severity": "low", "title": "Typo from '"$(basename "$0")"'", "file": "README.md", "line": 1,
        "explanation": "A word is misspelled.", "fix": "Correct the spelling."}]}' ;;
*) echo "$(basename "$0") answer" ;;
esac
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

    fn verdict_of(args: &[&str]) -> String {
        let output = run_ncli(&[("claude", FAKE_REVIEWER), ("codex", FAKE_REVIEWER)], args);
        assert!(output.status.success());
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(err.contains("ncli code review: verdict..."));
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn prints_only_the_verdict_of_the_chosen_judge() {
        assert_eq!(
            verdict_of(&["code", "review", LINK, "--judge", "codex"]),
            "## ncli code review: 1 finding\n\n### 🟡 Low: Typo from codex\n`README.md:1`\n\
             A word is misspelled.\n**Fix:** Correct the spelling.\n"
        );
        assert!(verdict_of(&["code", "review", LINK]).contains("Typo from claude"));
    }

    #[test]
    fn a_closed_stderr_does_not_abort_the_review() {
        let dir = TempDir::new().unwrap();
        for name in ["claude", "codex"] {
            script(dir.path(), name, FAKE_REVIEWER);
        }
        let mut ncli = Command::new(env!("CARGO_BIN_EXE_ncli"))
            .args(["code", "review", LINK])
            .env("PATH", format!("{}:/bin:/usr/bin", dir.path().display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(ncli.stderr.take());
        let output = ncli.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", output.status);
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("Typo from claude")
        );
    }

    #[test]
    fn failing_reviewer_error_is_reported() {
        let broken = "#!/bin/sh\nkill -TERM $$\n";
        let output = run_ncli(
            &[("claude", FAKE_REVIEWER), ("codex", broken)],
            &["code", "review", LINK],
        );
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(output.stdout.is_empty());
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(err.contains("ncli code review: error: codex was killed by signal 15"));
        assert!(err.contains("ncli: codex (attempt 1/3) failed: "));
    }
}
