#[cfg(unix)]
mod common;

#[cfg(unix)]
mod binary {
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output};

    use crate::common::{git, ncli, script};
    use ncli::error::Error;
    use tempfile::TempDir;

    const FAKE_AGENT: &str = r#"#!/bin/sh
name=$(basename "$0")
prompt=$(cat)
log="$NCLI_TEST_LOG"
next() {
    count=$(( $(cat "$log/$1.count" 2>/dev/null || echo 0) + 1 ))
    echo "$count" > "$log/$1.count"
    printf '%s' "$prompt" > "$log/$1.$count"
}
answer() {
    case "$name" in
    codex)
        echo '{"type":"thread.started","thread_id":"codex-thread"}'
        echo '{"type":"item.completed","item":{"type":"agent_message","text":"{\"completed\": '"$1"', \"report\": \"'"$2"'\"}"}}' ;;
    *) echo '{"completed": '"$1"', "report": "'"$2"'"}' ;;
    esac
}
case "$prompt" in
*"judging a code review"*)
    next judge
    if [ -n "$FAKE_JUDGE" ]; then
        echo "$FAKE_JUDGE"
    elif [ "$count" -le "${FAKE_FINDING_ROUNDS:-0}" ]; then
        echo '{"change_accessed": true, "findings": [{"severity": "low", "title": "Finding '"$count"'",
            "file": "feature.rs", "line": 1, "explanation": "It is wrong.", "fix": "Fix it."}]}'
    else
        echo '{"change_accessed": true, "findings": []}'
    fi ;;
*"Before changing anything for a comment"*)
    next address
    case "${FAKE_ADDRESS:-fix}" in
    fix) [ "$count" -gt "${FAKE_FIX_ROUNDS:-99}" ] || echo "// fix $count" >> feature.rs ;;
    revert) rm feature.rs ;;
    esac
    answer true "address report $count" ;;
*"Implement this by changing the code"*)
    next implement
    pwd -P > "$log/implement.cwd"
    [ -n "$FAKE_IMPLEMENT" ] || FAKE_IMPLEMENT="echo 'fn f() {}' > feature.rs"
    eval "$FAKE_IMPLEMENT"
    answer "${FAKE_COMPLETED:-true}" "implement report" ;;
*)
    case "$prompt" in
    "You reviewed"*) stage=rebuttal ;;
    *) stage=review ;;
    esac
    printf '%s' "$prompt" > "$log/$name.$stage"
    touch "$log/reviewed"
    echo "$name answer" ;;
esac
"#;

    struct Env {
        bin: TempDir,
        log: TempDir,
        state: TempDir,
        work: TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self::with(&["claude", "codex"])
        }

        fn with(agents: &[&str]) -> Self {
            let env = Self {
                bin: TempDir::new().unwrap(),
                log: TempDir::new().unwrap(),
                state: TempDir::new().unwrap(),
                work: TempDir::new().unwrap(),
            };
            for name in agents {
                script(env.bin.path(), name, FAKE_AGENT);
            }
            env
        }

        fn repo() -> Self {
            let env = Self::new();
            env.git(&["init", "--quiet", "--initial-branch", "main"]);
            fs::write(env.work.path().join("README.md"), "hello\n").unwrap();
            env.git(&["add", "README.md"]);
            env.git(&["commit", "--quiet", "--message", "Start."]);
            env
        }

        fn ncli(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
            ncli(self.state.path())
                .args(args)
                .current_dir(self.work.path())
                .env(
                    "PATH",
                    format!("{}:/bin:/usr/bin", self.bin.path().display()),
                )
                .env("NCLI_TEST_LOG", self.log.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(vars.iter().copied())
                .output()
                .unwrap()
        }

        fn implement(&self, extra: &[&str], vars: &[(&str, &str)]) -> Output {
            let args = [&["code", "implement", "add a feature"][..], extra].concat();
            let output = self.ncli(&args, vars);
            assert_ne!(output.status.code(), Some(Error::USAGE.into()));
            output
        }

        fn succeed(&self, extra: &[&str], vars: &[(&str, &str)]) -> (String, String) {
            let output = self.implement(extra, vars);
            assert!(output.status.success(), "{}", stderr(&output));
            let stderr = stderr(&output);
            (String::from_utf8(output.stdout).unwrap(), stderr)
        }

        fn fail(&self, extra: &[&str], vars: &[(&str, &str)]) -> String {
            let output = self.implement(extra, vars);
            assert_eq!(
                output.status.code(),
                Some(Error::FAILURE.into()),
                "{}",
                stderr(&output)
            );
            assert!(output.stdout.is_empty());
            stderr(&output)
        }

        fn git(&self, args: &[&str]) -> String {
            git(self.work.path(), args)
        }

        fn logged(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.log.path().join(name)).ok()
        }

        fn count(&self, role: &str) -> u32 {
            self.logged(&format!("{role}.count"))
                .map_or(0, |count| count.trim().parse().unwrap())
        }

        fn saved(&self, name: &str) -> bool {
            self.state.path().join(name).exists()
        }

        fn head(&self) -> (String, String) {
            (
                self.git(&["rev-parse", "HEAD"]),
                self.git(&["rev-list", "--count", "HEAD"]),
            )
        }
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn reviewed_trees(prompt: &str) -> (&str, &str) {
        let (_, rest) = prompt.split_once("--no-textconv ").unwrap();
        let (trees, _) = rest.split_once('`').unwrap();
        trees.split_once(' ').unwrap()
    }

    const FINDINGS: (&str, &str) = ("FAKE_FINDING_ROUNDS", "99");

    #[test]
    fn a_clean_first_review_ends_the_run_with_the_changes_left_uncommitted() {
        let env = Env::repo();
        let head = env.head();
        let (stdout, stderr) = env.succeed(&[], &[]);
        assert!(stdout.starts_with("implement report\n\nResume: claude --resume "));
        assert!(stdout.ends_with("\n\n## ncli code implement: no findings\n"));
        assert_eq!((env.count("judge"), env.count("address")), (1, 0));
        assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
        assert_eq!(env.head(), head);
        let prompt = env.logged("implement.1").unwrap();
        assert!(prompt.starts_with("add a feature\n\n"));
        assert!(prompt.contains("Do not commit, do not push"));
        assert_eq!(
            Path::new(env.logged("implement.cwd").unwrap().trim()),
            env.work.path().canonicalize().unwrap()
        );
        for stage in ["implementing...", "round 1/5...", "verdict..."] {
            assert!(
                stderr.contains(&format!("ncli code implement: {stage}")),
                "{stderr}"
            );
        }
        assert!(env.saved("review.json") && env.saved("output.txt"));
    }

    #[test]
    fn the_whole_panel_reviews_the_change_against_the_task() {
        let env = Env::repo();
        env.succeed(&[], &[]);
        let prompts = [
            "claude.review",
            "codex.review",
            "claude.rebuttal",
            "codex.rebuttal",
        ]
        .map(|name| env.logged(name).unwrap());
        for prompt in prompts.iter().chain([&env.logged("judge.1").unwrap()]) {
            assert!(
                prompt.contains("<task>\nadd a feature\n</task>")
                    && prompt.contains("A part of the task that the change does not accomplish"),
                "{prompt}"
            );
        }
    }

    #[test]
    fn findings_are_addressed_and_the_whole_change_is_reviewed_again() {
        let env = Env::repo();
        let head = env.head();
        let (stdout, _) = env.succeed(&[], &[("FAKE_FINDING_ROUNDS", "1")]);
        assert_eq!((env.count("judge"), env.count("address")), (2, 1));
        assert!(stdout.contains("\n\naddress report 1\n\nResume: claude --resume "));
        let address = env.logged("address.1").unwrap();
        assert!(address.contains("Finding 1"));
        assert!(address.contains("<task>\nadd a feature\n</task>"));
        assert!(address.contains("Address it only if it is correct"));
        assert!(!address.contains("<earlier>"));
        let first = env.logged("judge.1").unwrap();
        let second = env.logged("judge.2").unwrap();
        let (base, before) = reviewed_trees(&first);
        let (base_again, after) = reviewed_trees(&second);
        assert_eq!(base, base_again);
        assert_ne!(before, after);
        assert_eq!(env.head(), head);
    }

    #[test]
    fn findings_the_agent_declines_end_the_run_and_stay_listed() {
        let env = Env::repo();
        let (stdout, _) = env.succeed(&[], &[FINDINGS, ("FAKE_ADDRESS", "decline")]);
        assert_eq!((env.count("judge"), env.count("address")), (1, 1));
        assert!(stdout.contains("\n\naddress report 1\n\nResume: "));
        assert!(stdout.contains("## ncli code implement: 1 finding\n\n### 🟡 Low: Finding 1"));
        let prompt = env.logged("judge.1").unwrap();
        let (base, _) = reviewed_trees(&prompt);
        let output = env.ncli(&["code", "publish-comment"], &[]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        let stderr = stderr(&output);
        assert!(
            stderr.contains(&format!(
                " on top of tree {base}, which has nowhere to publish comments"
            )),
            "{stderr}"
        );
    }

    #[test]
    fn findings_declined_on_the_last_round_end_the_run() {
        let env = Env::repo();
        let (stdout, _) = env.succeed(
            &["--max-rounds", "2"],
            &[FINDINGS, ("FAKE_FIX_ROUNDS", "1")],
        );
        assert_eq!((env.count("judge"), env.count("address")), (2, 2));
        assert!(stdout.contains("\n\naddress report 2\n\nResume: "));
        assert!(stdout.contains("## ncli code implement: 1 finding\n\n### 🟡 Low: Finding 2"));
        assert!(env.saved("review.json"));
    }

    #[test]
    fn changes_made_on_the_last_round_are_an_error() {
        let env = Env::repo();
        let head = env.head();
        let stderr = env.fail(&["--max-rounds", "2"], &[FINDINGS]);
        assert!(
            stderr.contains(
                "ncli code implement: error: the review still has 1 finding after 2 round(s), \
                 and the changes made to address them were not reviewed:"
            ),
            "{stderr}"
        );
        assert!(stderr.contains("\n\naddress report 2\n\nResume: claude --resume "));
        assert!(stderr.trim_end().ends_with("**Fix:** Fix it."), "{stderr}");
        assert_eq!(stderr.matches("Resume: ").count(), 3, "{stderr}");
        assert_eq!((env.count("judge"), env.count("address")), (2, 2));
        assert!(env.saved("failure.txt") && !env.saved("output.txt"));
        assert_eq!(env.head(), head);
    }

    #[test]
    fn a_failed_run_drops_the_review_of_an_earlier_run() {
        let env = Env::repo();
        env.succeed(&[], &[]);
        assert!(env.saved("review.json"));
        let implement = ("FAKE_IMPLEMENT", "echo 'fn g() {}' >> feature.rs");
        let failure = env.fail(&["--max-rounds", "1"], &[FINDINGS, implement]);
        assert!(
            failure.contains("the review still has 1 finding"),
            "{failure}"
        );
        assert!(!env.saved("review.json"));
        let output = env.ncli(&["code", "publish-comment"], &[]);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        let stderr = stderr(&output);
        assert!(stderr.contains("there is no review to publish"), "{stderr}");
    }

    #[test]
    fn a_review_that_fails_after_the_changes_is_an_error_that_says_how_to_resume() {
        for judge in ["not json", r#"{"change_accessed": false, "findings": []}"#] {
            let env = Env::repo();
            let stderr = env.fail(&[], &[("FAKE_JUDGE", judge)]);
            assert!(
                stderr.contains("ncli code implement: error: ")
                    && stderr.contains("\nResume: claude --resume "),
                "{stderr}"
            );
            assert_eq!((env.count("judge"), env.count("address")), (1, 0));
            assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
            assert!(!env.saved("review.json"));
        }
    }

    #[test]
    fn the_agent_may_change_what_was_staged_before_the_run() {
        let env = Env::repo();
        let root = env.work.path();
        fs::write(root.join("staged.txt"), "old\n").unwrap();
        fs::write(root.join("README.md"), "staged\n").unwrap();
        env.git(&["add", "staged.txt", "README.md"]);
        let implement = "echo 'fn f() {}' > feature.rs && rm staged.txt && echo hello > README.md";
        env.succeed(&[], &[("FAKE_IMPLEMENT", implement)]);
        let prompt = env.logged("judge.1").unwrap();
        let (base, snapshot) = reviewed_trees(&prompt);
        assert_eq!(
            env.git(&["diff", "--name-only", base, snapshot]),
            "README.md\nfeature.rs\nstaged.txt"
        );
    }

    #[test]
    fn a_partly_staged_file_the_agent_leaves_alone_is_not_an_error() {
        let env = Env::repo();
        let readme = env.work.path().join("README.md");
        fs::write(&readme, "staged\n").unwrap();
        env.git(&["add", "README.md"]);
        fs::write(&readme, "unstaged\n").unwrap();
        env.succeed(&[], &[]);
        let prompt = env.logged("judge.1").unwrap();
        let (base, snapshot) = reviewed_trees(&prompt);
        assert_eq!(
            env.git(&["diff", "--name-only", base, snapshot]),
            "feature.rs"
        );
    }

    #[test]
    fn the_agent_may_stage_edits_made_before_the_run() {
        let env = Env::repo();
        fs::write(env.work.path().join("README.md"), "edited\n").unwrap();
        let implement = "echo 'fn f() {}' > feature.rs && git add -A";
        env.succeed(&[], &[("FAKE_IMPLEMENT", implement)]);
        let prompt = env.logged("judge.1").unwrap();
        let (base, snapshot) = reviewed_trees(&prompt);
        assert_eq!(
            env.git(&["diff", "--name-only", base, snapshot]),
            "feature.rs"
        );
        assert_eq!(
            env.git(&["status", "--porcelain"]),
            "M  README.md\nA  feature.rs"
        );
    }

    #[test]
    fn the_agent_may_unstage_changes_staged_before_the_run() {
        for unstage in [
            "git restore --staged README.md",
            "git stash -q && git stash pop -q",
        ] {
            let env = Env::repo();
            fs::write(env.work.path().join("README.md"), "staged\n").unwrap();
            env.git(&["add", "README.md"]);
            let implement = format!("echo 'fn f() {{}}' > feature.rs && {unstage}");
            env.succeed(&[], &[("FAKE_IMPLEMENT", &implement)]);
            let prompt = env.logged("judge.1").unwrap();
            let (base, snapshot) = reviewed_trees(&prompt);
            assert_eq!(
                env.git(&["diff", "--name-only", base, snapshot]),
                "feature.rs",
                "{unstage}"
            );
        }
    }

    #[test]
    fn staged_changes_the_agent_undoes_are_an_error() {
        let env = Env::repo();
        let implement = "echo 'fn f() {}' > feature.rs && echo x > hidden.rs && \
                         git add hidden.rs && rm hidden.rs";
        let stderr = env.fail(&[], &[("FAKE_IMPLEMENT", implement)]);
        assert!(
            stderr.contains(
                "the working tree undoes staged changes that would be missed (hidden.rs)"
            ),
            "{stderr}"
        );
        assert!(stderr.contains("\nResume: claude --resume "), "{stderr}");
        assert_eq!(env.count("judge"), 0);
    }

    #[test]
    fn later_address_steps_see_the_earlier_decisions() {
        let env = Env::repo();
        env.fail(&[], &[FINDINGS]);
        assert_eq!(env.count("address"), 5);
        assert!(!env.logged("address.1").unwrap().contains("<earlier>"));
        assert!(
            env.logged("address.2")
                .unwrap()
                .contains("<earlier>\naddress report 1\n</earlier>")
        );
    }

    #[test]
    fn an_implementation_that_changes_nothing_is_not_reviewed() {
        let env = Env::repo();
        let stderr = env.fail(&[], &[("FAKE_IMPLEMENT", "true")]);
        assert!(
            stderr.contains("error: claude left no changes in ")
                && stderr.contains(" to review: implement report\nResume: claude --resume "),
            "{stderr}"
        );
        assert!(env.logged("reviewed").is_none());
        assert_eq!(env.count("judge"), 0);
    }

    #[test]
    fn an_address_step_that_reverts_everything_is_an_error() {
        let env = Env::repo();
        let stderr = env.fail(&[], &[FINDINGS, ("FAKE_ADDRESS", "revert")]);
        assert!(
            stderr.contains(" to review: address report 1\nResume: "),
            "{stderr}"
        );
        assert_eq!(env.count("judge"), 1);
    }

    #[test]
    fn uncommitted_changes_made_before_the_run_are_the_baseline() {
        let env = Env::repo();
        let root = env.work.path();
        fs::write(root.join("README.md"), "edited\n").unwrap();
        fs::write(root.join("notes.txt"), "notes\n").unwrap();
        env.succeed(&[], &[]);
        let prompt = env.logged("judge.1").unwrap();
        let (base, snapshot) = reviewed_trees(&prompt);
        assert_ne!(base, env.git(&["rev-parse", "HEAD^{tree}"]));
        assert_eq!(
            env.git(&["diff", "--name-only", base, snapshot]),
            "feature.rs"
        );
        assert!(
            prompt.contains(&format!(" on top of tree {base}. ")),
            "{prompt}"
        );
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "edited\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("notes.txt")).unwrap(),
            "notes\n"
        );
    }

    #[test]
    fn an_agent_that_commits_fails_the_run_before_any_review() {
        let env = Env::repo();
        let commit = "echo 'fn f() {}' > feature.rs && git add -A && \
                      git -c user.name=x -c user.email=x commit -qm x";
        let stderr = env.fail(&[], &[("FAKE_IMPLEMENT", commit)]);
        assert!(
            stderr.contains("error: HEAD in ") && stderr.contains(" moved from "),
            "{stderr}"
        );
        assert!(stderr.contains("\nResume: claude --resume "));
        assert!(env.logged("reviewed").is_none());
    }

    #[test]
    fn an_agent_that_switches_branches_at_the_same_commit_fails_the_run() {
        for (moves, to) in [
            ("git branch other && git switch -q other", " to other at "),
            ("git checkout -q --detach", " to a detached HEAD at "),
        ] {
            let env = Env::repo();
            let implement = format!("{moves} && echo 'fn f() {{}}' > feature.rs");
            let stderr = env.fail(&[], &[("FAKE_IMPLEMENT", &implement)]);
            assert!(
                stderr.contains(" moved from main at ") && stderr.contains(to),
                "{stderr}"
            );
            assert!(env.logged("reviewed").is_none());
        }
    }

    #[test]
    fn the_run_is_checked_before_the_agent_starts() {
        let env = Env::with(&["claude"]);
        env.git(&["init", "--quiet"]);
        let output = env.implement(&[], &[]);
        assert_eq!(output.status.code(), Some(Error::MISSING_EXECUTABLE.into()));
        assert!(env.logged("implement.1").is_none());

        let env = Env::new();
        let stderr = env.fail(&[], &[]);
        assert!(
            stderr.contains(
                "ncli code implement: error: the current directory is not in a git work tree: \
                 git exited with code 128: fatal: not a git repository"
            ),
            "{stderr}"
        );
        assert!(env.logged("implement.1").is_none());

        let env = Env::repo();
        let nested = env.work.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("file"), "x\n").unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&nested)
            .status()
            .unwrap();
        let stderr = env.fail(&[], &[]);
        assert!(
            stderr.contains("untracked git repositories cannot be included (nested)"),
            "{stderr}"
        );
        assert!(env.logged("implement.1").is_none());
    }

    #[test]
    fn an_implementer_that_cannot_finish_is_an_error_that_says_how_to_resume() {
        let env = Env::repo();
        let stderr = env.fail(&[], &[("FAKE_COMPLETED", "false")]);
        assert!(
            stderr.contains(
                "error: claude could not finish the task: implement report\nResume: claude --resume "
            ),
            "{stderr}"
        );
        assert_eq!(env.count("judge"), 0);
    }

    #[test]
    fn an_empty_task_or_no_rounds_is_a_usage_error() {
        let env = Env::repo();
        for task in ["", "   "] {
            let output = env.ncli(&["code", "implement", task], &[]);
            assert_eq!(output.status.code(), Some(Error::USAGE.into()));
            assert!(stderr(&output).contains("the task is empty"));
        }
        let output = env.ncli(&["code", "implement", "add it", "--max-rounds", "0"], &[]);
        assert_eq!(output.status.code(), Some(Error::USAGE.into()));
        assert!(env.logged("implement.1").is_none());
    }

    #[test]
    fn help_needs_no_executables() {
        let env = Env::with(&[]);
        let output = ncli(env.state.path())
            .args(["code", "implement", "--help"])
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("--max-rounds")
        );
    }
}
