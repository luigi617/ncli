mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::child;
use ncli::agent::{Agent, SchemaArg, Session};
use ncli::commands::Context;
use ncli::commands::code::review::{Change, SCHEMA, Verdict, debate};
use ncli::error::Error;
use ncli::process::Cancellation;
use tempfile::TempDir;

const LINK: &str = "https://github.com/owner/repo/pull/1";
const ONE_FINDING: &str = r#"{"change_accessed": true, "findings": [{"severity": "high", "title": "Bug", "file": "a.rs",
    "line": 3, "explanation": "It breaks.", "fix": "Fix it."}]}"#;

fn reviewer(name: &str, args: &[&str]) -> Agent {
    Agent {
        name: name.to_owned(),
        cli: child(args),
        schema_arg: SchemaArg::Inline("--schema"),
        session: Session::Assigned("--session"),
        resume: &[],
    }
}

fn judge(dir: &Path, verdict: &str, schema_arg: SchemaArg) -> Agent {
    Agent {
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
    assert_eq!(received, *SCHEMA);
}

fn reviewers(dir: &Path) -> Vec<Agent> {
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

fn run_debate(reviewers: &[Agent], judge: &Agent) -> Result<Verdict, Error> {
    let context = Context {
        prog: "ncli code review",
        cancellation: &Cancellation::default(),
    };
    debate(&Change::Link(LINK.to_owned()), reviewers, judge, &context)
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
    assert!(matches!(error, Error::InvalidAnswer { .. }), "{error}");
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
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, SystemTime};

    use super::LINK;
    use crate::common::{ncli, script};
    use ncli::error::Error;
    use tempfile::TempDir;

    const FAKE_REVIEWER: &str = r#"#!/bin/sh
prompt=$(cat)
case "$*" in
*--output-schema*|*--json-schema*)
    printf '%s' "$prompt" > "$(dirname "$0")/$(basename "$0").verdict-prompt"
    echo '{"change_accessed": true, "findings": [{"severity": "low", "title": "Typo from '"$(basename "$0")"'", "file": "README.md", "line": 1,
        "explanation": "A word is misspelled.", "fix": "Correct the spelling."}]}' ;;
*) echo "$(basename "$0") answer" ;;
esac
"#;

    fn run_ncli(reviewers: &[(&str, &str)], args: &[&str]) -> Output {
        let dir = TempDir::new().unwrap();
        install_reviewers(dir.path(), reviewers);
        in_path(dir.path(), args).output().unwrap()
    }

    fn install_reviewers(dir: &Path, reviewers: &[(&str, &str)]) {
        for (name, body) in reviewers {
            script(dir, name, body);
        }
    }

    fn in_path(dir: &Path, args: &[&str]) -> Command {
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
        let mut command = ncli(&dir.join("state"));
        command
            .args(args)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
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
        let mut ncli = ncli(&dir.path().join("state"))
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

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=ncli", "-c", "user.email=ncli@example.com"])
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn empty_repo() -> TempDir {
        let repo = TempDir::new().unwrap();
        git(
            repo.path(),
            &["init", "--quiet", "--initial-branch", "main"],
        );
        repo
    }

    fn repo_with_remote(remote: &str) -> TempDir {
        let repo = empty_repo();
        fs::write(repo.path().join("README.md"), "hello\n").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "--quiet", "--message", "Start."]);
        set_default_branch(repo.path(), remote);
        repo
    }

    fn set_default_branch(repo: &Path, remote: &str) {
        let branch = format!("refs/remotes/{remote}/main");
        git(repo, &["update-ref", &branch, "refs/heads/main"]);
        git(
            repo,
            &[
                "symbolic-ref",
                &format!("refs/remotes/{remote}/HEAD"),
                &branch,
            ],
        );
    }

    fn repo() -> TempDir {
        repo_with_remote("origin")
    }

    fn feature_branch(repo: &Path) -> (String, String) {
        let merge_base = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["switch", "--quiet", "--create", "feature"]);
        fs::write(repo.join("README.md"), "hello, world\n").unwrap();
        git(repo, &["commit", "--quiet", "--all", "--message", "Greet."]);
        (merge_base, git(repo, &["rev-parse", "HEAD"]))
    }

    fn snapshot_of_uncommitted_review(prompt: &str) -> &str {
        let (_, rest) = prompt
            .split_once("and read files with `git show ")
            .unwrap_or_else(|| panic!("{prompt}"));
        rest.split_once(":<path>`").unwrap().0
    }

    fn review_locally(dir: &Path, repo: &Path) -> Output {
        in_path(dir, &["code", "review"])
            .current_dir(repo)
            .output()
            .unwrap()
    }

    fn verdict_prompt_of_local_review(repo: &Path) -> String {
        let dir = TempDir::new().unwrap();
        install_reviewers(
            dir.path(),
            &[("claude", FAKE_REVIEWER), ("codex", FAKE_REVIEWER)],
        );
        let output = review_locally(dir.path(), repo);
        assert!(output.status.success(), "{output:?}");
        fs::read_to_string(dir.path().join("claude.verdict-prompt")).unwrap()
    }

    fn local_review_error(repo: &Path) -> String {
        let dir = TempDir::new().unwrap();
        install_reviewers(
            dir.path(),
            &[("claude", FAKE_REVIEWER), ("codex", FAKE_REVIEWER)],
        );
        let output = review_locally(dir.path(), repo);
        assert_eq!(output.status.code(), Some(Error::FAILURE.into()));
        assert!(!dir.path().join("claude.verdict-prompt").exists());
        String::from_utf8(output.stderr).unwrap()
    }

    #[test]
    fn without_a_link_a_snapshot_of_the_uncommitted_changes_is_reviewed() {
        let repo = repo();
        git(repo.path(), &["config", "status.showUntrackedFiles", "no"]);
        fs::write(repo.path().join("new.rs"), "fn main() {}\n").unwrap();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let prompt = verdict_prompt_of_local_review(repo.path());
        let root = repo.path().canonicalize().unwrap();
        assert!(
            prompt.contains(&format!(
                "about the uncommitted changes in {}. Each reviewer",
                root.display()
            )),
            "{prompt}"
        );
        let snapshot = snapshot_of_uncommitted_review(&prompt);
        assert!(prompt.contains(&format!(
            "`git diff {head} {snapshot}` in {}",
            root.display()
        )));
        fs::write(repo.path().join("new.rs"), "edited later\n").unwrap();
        assert_eq!(
            git(repo.path(), &["show", &format!("{snapshot}:new.rs")]),
            "fn main() {}"
        );
    }

    #[test]
    fn an_edit_with_unchanged_stat_data_is_in_the_snapshot() {
        let repo = repo();
        git(repo.path(), &["config", "core.checkStat", "minimal"]);
        git(repo.path(), &["config", "core.trustctime", "false"]);
        let readme = repo.path().join("README.md");
        let past = SystemTime::now() - Duration::from_secs(60);
        let set_mtime = |path: &Path| {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(past)
                .unwrap();
        };
        set_mtime(&readme);
        git(repo.path(), &["add", "README.md"]);
        fs::write(&readme, "jello\n").unwrap();
        set_mtime(&readme);
        set_mtime(&repo.path().join(".git/index"));
        let prompt = verdict_prompt_of_local_review(repo.path());
        let snapshot = snapshot_of_uncommitted_review(&prompt);
        assert_eq!(
            git(repo.path(), &["show", &format!("{snapshot}:README.md")]),
            "jello"
        );
    }

    #[test]
    fn a_review_from_a_subdirectory_snapshots_from_the_real_index() {
        let repo = repo();
        fs::write(repo.path().join(".gitignore"), "README.md\n").unwrap();
        let sub = repo.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let prompt = verdict_prompt_of_local_review(&sub);
        let snapshot = snapshot_of_uncommitted_review(&prompt);
        assert_eq!(
            git(repo.path(), &["show", &format!("{snapshot}:README.md")]),
            "hello"
        );
    }

    #[test]
    fn without_commits_the_uncommitted_changes_are_compared_with_the_empty_tree() {
        let repo = empty_repo();
        fs::write(repo.path().join("staged.rs"), "fn main() {}\n").unwrap();
        git(repo.path(), &["add", "staged.rs"]);
        let empty_tree = git(repo.path(), &["hash-object", "-t", "tree", "/dev/null"]);
        let prompt = verdict_prompt_of_local_review(repo.path());
        let snapshot = snapshot_of_uncommitted_review(&prompt);
        assert!(
            prompt.contains(&format!("`git diff {empty_tree} {snapshot}`")),
            "{prompt}"
        );
        assert_eq!(
            git(repo.path(), &["show", &format!("{snapshot}:staged.rs")]),
            "fn main() {}"
        );
        assert!(!git(repo.path(), &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn without_a_link_or_uncommitted_changes_the_current_branch_is_reviewed() {
        let repo = repo();
        let (merge_base, head) = feature_branch(repo.path());
        let prompt = verdict_prompt_of_local_review(repo.path());
        assert!(
            prompt.contains(&format!("about the commits from origin/main to {head} in ")),
            "{prompt}"
        );
        assert!(prompt.contains(&format!(
            "`git log {merge_base}..{head}` and `git diff {merge_base} {head}`"
        )));
    }

    #[test]
    fn the_default_branch_may_be_on_another_remote() {
        let repo = repo_with_remote("upstream");
        let (_, head) = feature_branch(repo.path());
        set_default_branch(repo.path(), "fork");
        let error = local_review_error(repo.path());
        assert!(
            error.contains(
                "ncli code review: error: the repository has several default branches to compare \
                 the current branch with (fork/main, upstream/main) and none on origin"
            ),
            "{error}"
        );
        git(
            repo.path(),
            &["symbolic-ref", "--delete", "refs/remotes/fork/HEAD"],
        );
        let prompt = verdict_prompt_of_local_review(repo.path());
        assert!(
            prompt.contains(&format!(
                "about the commits from upstream/main to {head} in "
            )),
            "{prompt}"
        );
        set_default_branch(repo.path(), "origin");
        let prompt = verdict_prompt_of_local_review(repo.path());
        assert!(
            prompt.contains(&format!("about the commits from origin/main to {head} in ")),
            "{prompt}"
        );
    }

    #[test]
    fn a_local_review_needs_something_to_review_and_a_branch_to_compare_with() {
        let repo = repo();
        let root = repo.path().canonicalize().unwrap();
        assert!(local_review_error(repo.path()).contains(&format!(
            "ncli code review: error: there is nothing to review in {}: no uncommitted changes \
                 and no commits that are not on origin/main",
            root.display()
        )));
        git(
            repo.path(),
            &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
        );
        assert!(local_review_error(repo.path()).contains(
            "ncli code review: error: the repository has no default branch to compare the current \
             branch with; set one with `git remote set-head <remote> --auto`"
        ));
        let empty = empty_repo();
        let root = empty.path().canonicalize().unwrap();
        assert!(local_review_error(empty.path()).contains(&format!(
            "ncli code review: error: there is nothing to review in {}: no commits and no \
             uncommitted changes",
            root.display()
        )));
    }

    #[test]
    fn a_branch_without_common_history_is_reported() {
        let repo = repo();
        git(repo.path(), &["switch", "--quiet", "--orphan", "unrelated"]);
        fs::write(repo.path().join("other.rs"), "fn main() {}\n").unwrap();
        git(repo.path(), &["add", "other.rs"]);
        git(
            repo.path(),
            &["commit", "--quiet", "--message", "Unrelated."],
        );
        let root = repo.path().canonicalize().unwrap();
        let error = local_review_error(repo.path());
        assert!(
            error.contains(&format!(
                "ncli code review: error: the current branch in {} has no history in common with \
                 origin/main",
                root.display()
            )),
            "{error}"
        );
    }

    fn repo_with_submodule() -> (TempDir, TempDir) {
        let sub = repo();
        let repo = repo();
        git(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                sub.path().to_str().unwrap(),
                "sub",
            ],
        );
        git(repo.path(), &["commit", "--quiet", "--message", "Add sub."]);
        (repo, sub)
    }

    #[test]
    fn uncommitted_changes_inside_a_submodule_are_rejected() {
        let (repo, _sub) = repo_with_submodule();
        fs::write(repo.path().join("sub/README.md"), "changed\n").unwrap();
        let root = repo.path().canonicalize().unwrap();
        let error = local_review_error(repo.path());
        assert!(
            error.contains(&format!(
                "ncli code review: error: the uncommitted changes inside submodules of {} cannot \
                 be reviewed (sub); commit them in the submodules first",
                root.display()
            )),
            "{error}"
        );
    }

    #[test]
    fn untracked_files_inside_a_submodule_do_not_block_a_review() {
        let (repo, _sub) = repo_with_submodule();
        fs::write(repo.path().join("sub/build.out"), "output\n").unwrap();
        let prompt = verdict_prompt_of_local_review(repo.path());
        assert!(
            prompt.contains("about the commits from origin/main to "),
            "{prompt}"
        );
    }

    #[test]
    fn a_local_review_outside_a_work_tree_asks_for_a_link() {
        let outside = TempDir::new().unwrap();
        let error = local_review_error(outside.path());
        assert!(
            error.contains(
                "ncli code review: error: there are no local changes to review outside a git \
                 work tree; give a link instead: git exited with code 128: fatal: not a git \
                 repository"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_local_review_has_nowhere_to_publish_comments() {
        let repo = repo();
        fs::write(repo.path().join("README.md"), "changed\n").unwrap();
        let dir = TempDir::new().unwrap();
        install_reviewers(
            dir.path(),
            &[("claude", FAKE_REVIEWER), ("codex", FAKE_REVIEWER)],
        );
        assert!(review_locally(dir.path(), repo.path()).status.success());
        let publish = in_path(dir.path(), &["code", "publish-comment"])
            .output()
            .unwrap();
        assert_eq!(publish.status.code(), Some(Error::FAILURE.into()));
        let root = repo.path().canonicalize().unwrap();
        assert_eq!(
            String::from_utf8(publish.stderr).unwrap().trim(),
            format!(
                "ncli code publish-comment: error: the last review was of the uncommitted changes \
                 in {}, which has nowhere to publish comments",
                root.display()
            )
        );
    }
}
