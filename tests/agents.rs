mod common;

use common::child;
use ncli::agent::{Agent, SchemaArg, Session};
use ncli::error::Error;
use ncli::process::{Cancellation, Cli};

fn agent(answer: &str, session: Session) -> Agent {
    Agent {
        name: "agent".to_owned(),
        cli: child(&["perform", answer]),
        schema_arg: SchemaArg::Inline("--schema"),
        session,
        resume: &["agent", "resume"],
    }
}

fn assigned(answer: &str) -> Agent {
    agent(answer, Session::Assigned("--session"))
}

fn perform(agent: &Agent) -> Result<String, Error> {
    agent
        .perform("Do it.", &Cancellation::default())
        .map(|performed| performed.to_string())
}

#[test]
fn an_assigned_session_is_the_one_to_resume() {
    let performed = perform(&assigned(r#"{"completed": true, "report": " Done. "}"#)).unwrap();
    let (report, id) = performed.split_once("\n\nResume: agent resume ").unwrap();
    assert_eq!(report, "Done.");
    uuid::Uuid::parse_str(id).unwrap();
}

#[test]
fn a_session_reported_in_events_is_the_one_to_resume() {
    let agent = agent(
        r#"{"completed": true, "report": "Done."}"#,
        Session::FromEvents("--json"),
    );
    assert_eq!(
        perform(&agent).unwrap(),
        "Done.\n\nResume: agent resume thread-1"
    );
}

#[test]
fn an_agent_that_reports_no_session_is_an_error() {
    let agent = Agent {
        cli: child(&[
            "perform-anonymously",
            r#"{"completed": true, "report": "Done."}"#,
        ]),
        ..agent("", Session::FromEvents("--json"))
    };
    let error = perform(&agent).unwrap_err();
    assert!(matches!(error, Error::NoSession(_)), "{error}");
    assert_eq!(error.to_string(), "agent did not report its session");
}

#[test]
fn an_unfinished_task_is_an_error_that_says_how_to_resume() {
    let error = perform(&assigned(r#"{"completed": false, "report": "No access."}"#)).unwrap_err();
    assert!(
        resumable(&error, |source| matches!(source, Error::Incomplete { .. })),
        "{error}"
    );
    assert_eq!(error.exit_code(), Error::FAILURE);
    let message = error.to_string();
    assert!(
        message.starts_with("agent could not finish the task: No access.\nResume: agent resume "),
        "{message}"
    );
}

#[test]
fn an_answer_that_does_not_match_the_schema_is_an_error() {
    for answer in [
        "Done!",
        r#"{"completed": true}"#,
        r#"{"completed": true, "report": "", "extra": 1}"#,
    ] {
        let error = perform(&assigned(answer)).unwrap_err();
        assert!(
            resumable(&error, |source| matches!(
                source,
                Error::InvalidAnswer { .. }
            )),
            "{answer}: {error}"
        );
        assert!(
            error.to_string().contains("\nResume: agent resume "),
            "{error}"
        );
    }
}

fn resumable(error: &Error, source: impl Fn(&Error) -> bool) -> bool {
    matches!(error, Error::Resumable { source: inner, .. } if source(inner))
}

#[test]
fn an_agent_that_fails_after_its_session_starts_says_how_to_resume() {
    let claude = Agent {
        cli: child(&["fail", "3", "boom"]),
        ..assigned("")
    };
    let codex = Agent {
        cli: child(&["start-and-fail"]),
        ..agent("", Session::FromEvents("--json"))
    };
    for (agent, resume) in [(claude, "agent resume "), (codex, "agent resume thread-1")] {
        let error = perform(&agent).unwrap_err();
        assert!(
            resumable(&error, |source| matches!(source, Error::Process(_))),
            "{error}"
        );
        assert_eq!(
            error
                .resume()
                .map(|resume| resume.starts_with("agent resume ")),
            Some(true)
        );
        assert!(
            error.to_string().contains(&format!("\nResume: {resume}")),
            "{error}"
        );
    }
}

#[test]
fn an_agent_that_never_started_has_nothing_to_resume() {
    let agent = Agent {
        cli: Cli::new(["ncli-no-such-agent"]),
        ..assigned("")
    };
    let error = perform(&agent).unwrap_err();
    assert!(matches!(error, Error::Process(_)), "{error}");
    assert!(error.resume().is_none());
}

#[cfg(unix)]
mod binary {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use crate::common::{ncli, script};
    use ncli::error::Error;
    use tempfile::TempDir;

    const LINK: &str = "https://github.com/owner/repo/pull/1";

    const FAKE_AGENT: &str = r#"#!/bin/sh
name=$(basename "$0")
prompt=$(cat)
case "$prompt" in
*"judging a code review"*)
    echo '{"change_accessed": true, "findings": [{"severity": "low", "title": "Typo", "file": "README.md", "line": 1,
        "explanation": "A word is misspelled.", "fix": "Correct the spelling."}]}' ;;
*"When you are done"*)
    printf '%s' "$prompt" > "$NCLI_TEST_LOG/$name.prompt"
    pwd -P > "$NCLI_TEST_LOG/$name.cwd"
    printf '%s' "$*" > "$NCLI_TEST_LOG/$name.args"
    if [ -n "$FAKE_HANG" ]; then
        [ "$name" = codex ] && echo '{"type":"thread.started","thread_id":"codex-thread"}'
        touch "$NCLI_TEST_LOG/$name.started"
        sleep 30
    fi
    case "$name" in
    codex)
        echo '{"type":"thread.started","thread_id":"codex-thread"}'
        echo '{"type":"item.completed","item":{"type":"agent_message","text":"{\"completed\": '"${FAKE_COMPLETED:-true}"', \"report\": \"'"$name"' did it\"}"}}' ;;
    *) echo '{"completed": '"${FAKE_COMPLETED:-true}"', "report": "'"$name"' did it"}' ;;
    esac ;;
*) echo "$name answer" ;;
esac
"#;

    #[cfg(target_os = "macos")]
    const CLIPBOARD: &str = "pbcopy";
    #[cfg(not(target_os = "macos"))]
    const CLIPBOARD: &str = "wl-copy";

    const FAKE_CLIPBOARD: &str = "#!/bin/sh\ncat > \"$NCLI_TEST_LOG/clipboard\"\n";

    struct Env {
        bin: TempDir,
        log: TempDir,
        state: TempDir,
        work: TempDir,
    }

    impl Env {
        fn new(installed: &[&str]) -> Self {
            let env = Self {
                bin: TempDir::new().unwrap(),
                log: TempDir::new().unwrap(),
                state: TempDir::new().unwrap(),
                work: TempDir::new().unwrap(),
            };
            for name in installed {
                let body = if ["pbcopy", "wl-copy", "xclip", "xsel"].contains(name) {
                    FAKE_CLIPBOARD
                } else {
                    FAKE_AGENT
                };
                script(env.bin.path(), name, body);
            }
            env
        }

        fn ncli(&self, args: &[&str]) -> Command {
            let mut command = ncli(self.state.path());
            command
                .args(args)
                .current_dir(self.work.path())
                .env(
                    "PATH",
                    format!("{}:/bin:/usr/bin", self.bin.path().display()),
                )
                .env("NCLI_TEST_LOG", self.log.path())
                .env("WAYLAND_DISPLAY", "ncli-test");
            command
        }

        fn run(&self, args: &[&str]) -> Output {
            self.ncli(args).output().unwrap()
        }

        fn succeed(&self, args: &[&str]) -> String {
            let output = self.run(args);
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }

        fn logged(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.log.path().join(name)).ok()
        }

        fn work_dir(&self) -> PathBuf {
            self.work.path().canonicalize().unwrap()
        }
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn session_id(args: &str) -> &str {
        args.split_once("--session-id ")
            .unwrap()
            .1
            .split(' ')
            .next()
            .unwrap()
    }

    #[test]
    fn a_task_runs_on_claude_in_the_current_directory_and_says_how_to_resume() {
        let env = Env::new(&["claude", "codex"]);
        let output = env.succeed(&["fix the bug"]);
        let args = env.logged("claude.args").unwrap();
        assert_eq!(
            output,
            format!(
                "claude did it\n\nResume: claude --resume {}\n",
                session_id(&args)
            )
        );
        assert!(
            env.logged("claude.prompt")
                .unwrap()
                .starts_with("fix the bug\n\n")
        );
        assert_eq!(
            Path::new(env.logged("claude.cwd").unwrap().trim()),
            env.work_dir()
        );
        assert!(env.logged("codex.prompt").is_none());
    }

    #[test]
    fn a_task_falls_back_to_codex() {
        let env = Env::new(&["codex"]);
        assert_eq!(
            env.succeed(&["fix the bug"]),
            "codex did it\n\nResume: codex resume codex-thread\n"
        );
    }

    #[test]
    fn a_task_needs_an_agent() {
        let output = Env::new(&[]).run(&["fix the bug"]);
        assert_eq!(output.status.code(), Some(Error::MISSING_EXECUTABLE.into()));
        assert_eq!(
            stderr(&output).trim(),
            "ncli: error: none of these executables were found on PATH: claude, codex"
        );
    }

    #[test]
    fn a_task_must_be_one_quoted_argument_of_several_words() {
        let env = Env::new(&["claude"]);
        for (args, word) in [
            (&["help"][..], "help"),
            (&["cdoe", "review"], "cdoe review"),
            (&["review", LINK], &format!("review {LINK}")),
            (&["fix", "the bug"], "fix the bug"),
        ] {
            let output = env.run(args);
            assert_eq!(output.status.code(), Some(Error::USAGE.into()), "{args:?}");
            assert!(
                stderr(&output)
                    .starts_with(&format!("ncli: error: unrecognized command `{word}`;")),
                "{}",
                stderr(&output)
            );
        }
        assert!(env.logged("claude.prompt").is_none());
    }

    #[test]
    fn an_interrupted_task_says_how_to_resume_and_records_it() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::Stdio;
        use std::thread;
        use std::time::{Duration, Instant};

        for (installed, resume) in [
            ("claude", "claude --resume "),
            ("codex", "codex resume codex-thread"),
        ] {
            let env = Env::new(&[installed]);
            let task = env
                .ncli(&["fix the bug"])
                .env("FAKE_HANG", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while env.logged(&format!("{installed}.started")).is_none() {
                assert!(Instant::now() < deadline, "{installed} never started");
                thread::sleep(Duration::from_millis(20));
            }
            unsafe { libc::kill(task.id() as libc::pid_t, libc::SIGINT) };
            let output = task.wait_with_output().unwrap();
            assert_eq!(output.status.signal(), Some(libc::SIGINT));
            let expected = format!("ncli: error: {installed} was cancelled\nResume: {resume}");
            assert!(stderr(&output).contains(&expected), "{}", stderr(&output));
            let saved = fs::read_to_string(env.state.path().join("failure.txt")).unwrap();
            assert!(saved.starts_with(&expected), "{saved}");
        }
    }

    #[test]
    fn a_failure_is_recorded_apart_from_the_last_successful_output() {
        let env = Env::new(&["claude"]);
        let saved = || fs::read_to_string(env.state.path().join("failure.txt")).ok();
        env.run(&["copy"]);
        assert_eq!(saved(), None);
        let succeeded = env.succeed(&["fix the bug"]);
        let output = env
            .ncli(&["fix the bug"])
            .env("FAKE_COMPLETED", "false")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(output.stdout.is_empty());
        let error = saved().unwrap();
        assert!(error.starts_with(
            "ncli: error: claude could not finish the task: claude did it\nResume: claude --resume "
        ));
        assert!(stderr(&output).trim_end().ends_with(&error), "{error}");
        env.run(&["copy"]);
        env.run(&["cpy"]);
        assert_eq!(saved().unwrap(), error);

        env.succeed(&["code", "address-comment"]);
        let prompt = env.logged("claude.prompt").unwrap();
        assert!(
            prompt.contains(&format!("<output>\n{succeeded}</output>")),
            "{prompt}"
        );
        assert_eq!(saved(), None, "a success clears the saved failure");
    }

    #[test]
    fn the_output_is_saved_even_when_it_cannot_be_printed() {
        use std::process::Stdio;

        let env = Env::new(&["claude"]);
        let mut task = env
            .ncli(&["fix the bug"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(task.stdout.take());
        let output = task.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(stderr(&output).contains("could not write the output"));
        let saved = fs::read_to_string(env.state.path().join("output.txt")).unwrap();
        assert!(saved.starts_with("claude did it\n\nResume: "), "{saved}");
        assert!(
            !env.state.path().join("failure.txt").exists(),
            "the write error would replace the saved output for copy"
        );
    }

    #[test]
    fn a_state_error_after_a_finished_review_keeps_the_result_for_copy() {
        let env = Env::new(&["claude", "codex"]);
        fs::create_dir(env.state.path().join("review.json")).unwrap();
        let output = env.run(&["code", "review", LINK]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(stderr(&output).contains("could not access the saved review"));
        let saved = fs::read_to_string(env.state.path().join("output.txt")).unwrap();
        assert!(
            saved.starts_with("## ncli code review: 1 finding"),
            "{saved}"
        );
        assert!(!env.state.path().join("failure.txt").exists());
    }

    #[test]
    fn a_failed_save_leaves_no_older_result_or_review_behind() {
        let env = Env::new(&["claude", "codex"]);
        env.succeed(&["code", "review", LINK]);
        fs::write(env.state.path().join("failure.txt"), "ncli: error: older").unwrap();
        fs::remove_file(env.state.path().join("output.txt")).unwrap();
        fs::create_dir(env.state.path().join("output.txt")).unwrap();
        let output = env.run(&["code", "review", LINK]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(!env.state.path().join("failure.txt").exists());
        assert!(!env.state.path().join("review.json").exists());
        let publish = env.run(&["code", "publish-comment"]);
        assert!(stderr(&publish).contains("there is no review to publish"));
    }

    #[test]
    fn an_empty_state_directory_variable_is_ignored() {
        let env = Env::new(&["claude"]);
        let home = TempDir::new().unwrap();
        let output = env
            .ncli(&["fix the bug"])
            .env(ncli::state::DIR_VAR, "")
            .env("HOME", home.path())
            .env_remove("XDG_STATE_HOME")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(!env.work.path().join("output.txt").exists());
    }

    #[test]
    fn an_empty_task_is_a_usage_error() {
        let output = Env::new(&["claude"]).run(&[" "]);
        assert_eq!(output.status.code(), Some(Error::USAGE.into()));
        assert_eq!(stderr(&output).trim(), "ncli: error: the task is empty");
    }

    #[test]
    fn address_comment_changes_the_current_directory_without_committing() {
        let env = Env::new(&["claude", "codex"]);
        let link = format!("{LINK}#discussion_r1");
        let output = env.succeed(&["code", "address-comment", &link]);
        let args = env.logged("claude.args").unwrap();
        assert_eq!(
            output,
            format!(
                "claude did it\n\nResume: claude --resume {}\n",
                session_id(&args)
            )
        );
        let prompt = env.logged("claude.prompt").unwrap();
        assert!(prompt.contains(&link));
        assert!(prompt.contains("Do not commit, do not push"));
        assert!(prompt.contains("Address it only if it is correct and worth the change"));
        assert!(!prompt.contains("<context>"));
        assert_eq!(
            Path::new(env.logged("claude.cwd").unwrap().trim()),
            env.work_dir()
        );
    }

    #[test]
    fn address_comment_without_a_link_addresses_the_last_output() {
        let env = Env::new(&["claude", "codex"]);
        let output = env.run(&["code", "address-comment"]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert_eq!(
            stderr(&output).trim(),
            "ncli code address-comment: error: there is no last output"
        );
        assert!(env.logged("claude.prompt").is_none());

        let review = env.succeed(&["code", "review", LINK]);
        assert!(
            env.succeed(&["code", "address-comment", "--context", "skip README.md"])
                .starts_with("claude did it\n\nResume: claude --resume ")
        );
        let prompt = env.logged("claude.prompt").unwrap();
        assert!(prompt.contains(&format!("<output>\n{}</output>", review)));
        assert!(prompt.contains("<context>\nskip README.md\n</context>"));
        assert!(prompt.contains("Do not commit, do not push"));

        let unrelated = env
            .ncli(&["code", "address-comment"])
            .env("FAKE_COMPLETED", "false")
            .output()
            .unwrap();
        assert_eq!(unrelated.status.code(), Some(Error::FAILURE.into()));
        assert!(
            stderr(&unrelated)
                .contains("ncli code address-comment: error: claude could not finish the task: "),
            "{}",
            stderr(&unrelated)
        );
    }

    #[test]
    fn a_new_state_directory_is_private_and_an_existing_one_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;

        let env = Env::new(&["claude"]);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let new = env.state.path().join("new");
        let output = env
            .ncli(&["fix the bug"])
            .env(ncli::state::DIR_VAR, &new)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(mode(&new), 0o700);
        fs::set_permissions(env.state.path(), fs::Permissions::from_mode(0o755)).unwrap();
        env.succeed(&["fix the bug"]);
        assert_eq!(mode(env.state.path()), 0o755);
    }

    #[test]
    fn publish_comment_posts_the_findings_of_the_last_review() {
        let env = Env::new(&["claude", "codex"]);
        let output = env.run(&["code", "publish-comment"]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert_eq!(
            stderr(&output).trim(),
            "ncli code publish-comment: error: there is no review to publish; run `ncli code review` first"
        );

        env.succeed(&["code", "review", LINK]);
        assert_eq!(
            env.succeed(&["code", "publish-comment", "--context", "only the typos"]),
            "claude did it\n"
        );
        let cwd = env.logged("claude.cwd").unwrap();
        assert_ne!(
            Path::new(cwd.trim()),
            env.work_dir(),
            "publishing ran in the repository"
        );
        assert!(
            !Path::new(cwd.trim()).exists(),
            "the publishing directory was left behind"
        );
        let prompt = env.logged("claude.prompt").unwrap();
        assert!(prompt.contains(LINK));
        assert!(prompt.contains("<context>\nonly the typos\n</context>"));
        assert!(prompt.contains("Skip any that is already posted on the change"));
        assert!(prompt.contains("any whose finding no longer applies"));
        assert!(prompt.contains(
            "File: README.md\nLine: 1\nBody:\n\n### 🟡 Low: Typo\n`README.md:1`\nA word is misspelled.\n\
             **Fix:** Correct the spelling."
        ));
    }

    #[test]
    fn publish_comment_rejects_a_review_without_findings_or_a_corrupt_one() {
        let env = Env::new(&["claude"]);
        let review = env.state.path().join("review.json");
        for (saved, message) in [
            (
                format!(
                    r#"{{"link": "{LINK}", "verdict": {{"change_accessed": true, "findings": []}}}}"#
                ),
                format!("the last review of {LINK} has no findings to publish"),
            ),
            ("{".to_owned(), "the saved review is invalid: ".to_owned()),
        ] {
            fs::write(&review, saved).unwrap();
            let output = env.run(&["code", "publish-comment"]);
            assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
            assert!(
                stderr(&output)
                    .starts_with(&format!("ncli code publish-comment: error: {message}")),
                "{}",
                stderr(&output)
            );
        }
        assert!(env.logged("claude.prompt").is_none());
    }

    #[test]
    fn copy_puts_the_last_output_on_the_clipboard() {
        let env = Env::new(&["claude", CLIPBOARD]);
        let output = env.run(&["copy"]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert_eq!(
            stderr(&output).trim(),
            "ncli copy: error: there is no last output"
        );

        let printed = env.succeed(&["fix the bug"]);
        for _ in 0..2 {
            assert_eq!(env.succeed(&["copy"]), "");
            assert_eq!(
                env.logged("clipboard").unwrap(),
                printed.strip_suffix('\n').unwrap()
            );
        }

        env.ncli(&["fix the bug"])
            .env("FAKE_COMPLETED", "false")
            .output()
            .unwrap();
        env.succeed(&["copy"]);
        assert!(
            env.logged("clipboard")
                .unwrap()
                .starts_with("ncli: error: claude could not finish the task: ")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn copy_uses_the_clipboard_of_the_running_display() {
        let env = Env::new(&["claude", "wl-copy", "xclip"]);
        let printed = env.succeed(&["fix the bug"]);
        let copy = |display: Option<&str>| {
            let mut copy = env.ncli(&["copy"]);
            copy.env_remove("WAYLAND_DISPLAY").env_remove("DISPLAY");
            if let Some(display) = display {
                copy.env(display, "ncli-test");
            }
            copy.output().unwrap()
        };
        let output = copy(None);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert_eq!(
            stderr(&output).trim(),
            "ncli copy: error: there is no display to copy to; set WAYLAND_DISPLAY or DISPLAY"
        );
        for display in ["WAYLAND_DISPLAY", "DISPLAY"] {
            fs::remove_file(env.log.path().join("clipboard")).ok();
            assert!(copy(Some(display)).status.success(), "{display}");
            assert_eq!(
                env.logged("clipboard").unwrap(),
                printed.strip_suffix('\n').unwrap()
            );
        }
    }
}
