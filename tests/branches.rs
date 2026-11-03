#[cfg(unix)]
mod common;

#[cfg(unix)]
mod binary {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::process::{Command, Output};

    use crate::common::{git, ncli, script};
    use ncli::error::Error;
    use tempfile::TempDir;

    const TOOLS: [&str; 5] = ["git", "sh", "cat", "touch", "sleep"];

    const FAKE_AGENT: &str = r#"#!/bin/sh
prompt=$(cat)
log="$NCLI_TEST_LOG"
next() {
    count=$(( $(cat "$log/$1.count" 2>/dev/null || echo 0) + 1 ))
    echo "$count" > "$log/$1.count"
    printf '%s' "$prompt" > "$log/$1.$count"
}
answer() {
    echo '{"completed": '"${FAKE_COMPLETED:-true}"', "report": "'"$1"' report '"$count"'"'"$2"'}'
}
RESOLVE="printf 'resolved\\n' > README.md && git add README.md \
&& git commit --quiet --no-edit --no-verify --cleanup=verbatim -C REBASE_HEAD && GIT_EDITOR=: git rebase --continue"
SHIP='case "$prompt" in *"Create a new branch"*) git switch --quiet --create add-feature ;; esac
git add --all && { git diff --cached --quiet || git commit --quiet --message "Add a feature."; } &&
git push --quiet --set-upstream origin HEAD'
case "$prompt" in
*"as a pull request against"*)
    next ship
    eval "${FAKE_SHIP:-$SHIP}" >&2
    answer ship ', "url": "'"${FAKE_SHIP_URL-https://example.com/owner/repo/pull/1}"'"' ;;
*"The check \`"*)
    next fix
    eval "${FAKE_FIX:-touch fixed}"
    answer fix ;;
*"stopped with conflicts"*)
    next resolve
    eval "${FAKE_RESOLVE:-$RESOLVE}" >&2
    answer resolve ;;
esac
"#;

    const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$NCLI_TEST_LOG/gh.log"
case "$1 $2" in
"repo view")
    [ -z "$FAKE_GH_REPO_FAILS" ] || { echo "not logged in" >&2; exit 1; }
    echo '{"url": "https://example.com/owner/repo"}' ;;
"pr view")
    [ -z "$FAKE_GH_PR_FAILS" ] || { echo "not logged in" >&2; exit 1; }
    branch=$(git branch --show-current)
    oid=$(git rev-parse --verify --quiet "refs/remotes/origin/$branch")
    eval "selector=\${$#}"
    echo '{"url": "'"${FAKE_GH_URL:-$selector}"'", "state": "'"${FAKE_GH_STATE:-OPEN}"'", "baseRefName": "'"${FAKE_GH_BASE:-main}"'", "headRefName": "'"${FAKE_GH_HEAD:-$branch}"'", "headRefOid": "'"$oid"'", "isCrossRepository": '"${FAKE_GH_CROSS:-false}"'}' ;;
esac
"#;

    struct Env {
        bin: TempDir,
        log: TempDir,
        state: TempDir,
        origin: TempDir,
        work: TempDir,
    }

    impl Env {
        fn new() -> Self {
            Self::with(&["claude", "gh"])
        }

        fn with(stubs: &[&str]) -> Self {
            let env = Self {
                bin: TempDir::new().unwrap(),
                log: TempDir::new().unwrap(),
                state: TempDir::new().unwrap(),
                origin: TempDir::new().unwrap(),
                work: TempDir::new().unwrap(),
            };
            for tool in TOOLS {
                let found = Command::new("sh")
                    .args(["-c", &format!("command -v {tool}")])
                    .output()
                    .unwrap();
                let path = String::from_utf8(found.stdout).unwrap();
                symlink(path.trim(), env.bin.path().join(tool)).unwrap();
            }
            for stub in stubs {
                let body = if *stub == "gh" { FAKE_GH } else { FAKE_AGENT };
                script(env.bin.path(), stub, body);
            }
            git(
                env.origin.path(),
                &["init", "--quiet", "--bare", "--initial-branch", "main"],
            );
            env.git(&["init", "--quiet", "--initial-branch", "main"]);
            env.write("README.md", "hello\n");
            env.commit("Start.");
            let origin = env.origin.path().to_str().unwrap();
            env.git(&["remote", "add", "origin", origin]);
            env.git(&["push", "--quiet", "--set-upstream", "origin", "main"]);
            env.git(&["remote", "set-head", "origin", "main"]);
            env
        }

        fn ncli(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
            self.ncli_in(self.work.path(), args, vars)
        }

        fn ncli_in(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
            let output = self.command(dir, args, vars).output().unwrap();
            assert_ne!(output.status.code(), Some(Error::USAGE.into()));
            output
        }

        fn command(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> Command {
            let mut command = ncli(self.state.path());
            command
                .args(args)
                .current_dir(dir)
                .env("PATH", self.bin.path())
                .env("NCLI_TEST_LOG", self.log.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "ncli")
                .env("GIT_AUTHOR_EMAIL", "ncli@example.com")
                .env("GIT_COMMITTER_NAME", "ncli")
                .env("GIT_COMMITTER_EMAIL", "ncli@example.com")
                .envs(vars.iter().copied());
            command
        }

        fn succeed(&self, args: &[&str], vars: &[(&str, &str)]) -> (String, String) {
            let output = self.ncli(args, vars);
            assert!(output.status.success(), "{}", stderr(&output));
            let stderr = stderr(&output);
            (String::from_utf8(output.stdout).unwrap(), stderr)
        }

        fn fail(&self, args: &[&str], vars: &[(&str, &str)]) -> String {
            failed(&self.ncli(args, vars), Error::FAILURE)
        }

        fn git(&self, args: &[&str]) -> String {
            git(self.work.path(), args)
        }

        fn write(&self, path: &str, contents: &str) {
            fs::write(self.work.path().join(path), contents).unwrap();
        }

        fn commit(&self, message: &str) -> String {
            self.git(&["add", "--all"]);
            self.git(&["commit", "--quiet", "--message", message]);
            self.git(&["rev-parse", "HEAD"])
        }

        // Pushes a commit to a branch of origin from another clone, as a teammate would.
        fn push_upstream(&self, branch: &str, path: &str, contents: &str) -> String {
            self.change_upstream(branch, |clone| {
                fs::write(clone.join(path), contents).unwrap()
            })
        }

        fn change_upstream(&self, branch: &str, change: impl FnOnce(&Path)) -> String {
            let clone = TempDir::new().unwrap();
            let origin = self.origin.path().to_str().unwrap();
            git(clone.path(), &["clone", "--quiet", origin, "."]);
            if git(clone.path(), &["ls-remote", "--heads", "origin", branch]).is_empty() {
                git(clone.path(), &["switch", "--quiet", "--create", branch]);
            } else {
                git(clone.path(), &["switch", "--quiet", branch]);
            }
            change(clone.path());
            git(clone.path(), &["add", "--all"]);
            git(
                clone.path(),
                &["commit", "--quiet", "--message", "Upstream."],
            );
            git(clone.path(), &["push", "--quiet", "origin", branch]);
            git(clone.path(), &["rev-parse", "HEAD"])
        }

        fn origin_ref(&self, name: &str) -> Option<String> {
            let output = Command::new("git")
                .args(["rev-parse", "--verify", "--quiet", name])
                .current_dir(self.origin.path())
                .output()
                .unwrap();
            output
                .status
                .success()
                .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
        }

        fn logged(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.log.path().join(name)).ok()
        }

        fn count(&self, role: &str) -> u32 {
            self.logged(&format!("{role}.count"))
                .map_or(0, |count| count.trim().parse().unwrap())
        }

        fn gh_calls(&self) -> Vec<String> {
            self.logged("gh.log")
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn viewed_repo(&self) -> String {
            format!("repo view {} --json url", self.origin.path().display())
        }

        fn rebasing(&self) -> bool {
            self.work.path().join(".git/rebase-merge").exists()
                || self.work.path().join(".git/rebase-apply").exists()
        }
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn failed(output: &Output, code: u8) -> String {
        assert_eq!(
            output.status.code(),
            Some(code.into()),
            "{}",
            stderr(output)
        );
        assert!(output.stdout.is_empty());
        stderr(output)
    }

    mod ship {
        use super::*;

        const VIEW: &str = "pr view --json \
            url,state,baseRefName,headRefName,headRefOid,isCrossRepository -- \
            https://example.com/owner/repo/pull/1";

        #[test]
        fn uncommitted_changes_on_the_default_branch_become_a_new_pull_request() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.write("feature.rs", "fn f() {}\n");
            let (stdout, _) = env.succeed(&["code", "ship"], &[]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), start);
            assert_eq!(env.git(&["rev-parse", "main"]), start);
            assert_eq!(
                env.origin_ref("refs/heads/add-feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo(), VIEW.to_owned()]);
            let prompt = env.logged("ship.1").unwrap();
            assert!(
                prompt.contains("as a pull request against origin/main"),
                "{prompt}"
            );
            assert!(prompt.contains("the uncommitted changes in "), "{prompt}");
            assert!(
                prompt.contains("with a message of one short sentence"),
                "{prompt}"
            );
            assert!(prompt.contains("Create a new branch"), "{prompt}");
            assert!(
                prompt.contains("`--force-with-lease --force-if-includes`"),
                "{prompt}"
            );
            assert!(
                prompt.contains("already has an open pull request against main"),
                "{prompt}"
            );
        }

        #[test]
        fn commits_on_a_branch_are_shipped_into_the_named_branch() {
            let env = Env::new();
            env.push_upstream("develop", "dev.txt", "dev\n");
            env.git(&["fetch", "--quiet", "origin"]);
            env.git(&["switch", "--quiet", "--create", "feature", "origin/develop"]);
            env.write("feature.rs", "fn f() {}\n");
            let head = env.commit("Add the feature.");
            let base = ("FAKE_GH_BASE", "develop");
            let (stdout, _) = env.succeed(&["code", "ship", "--into", "develop"], &[base]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.origin_ref("refs/heads/feature"), Some(head));
            let prompt = env.logged("ship.1").unwrap();
            assert!(
                prompt.contains("since it branched from origin/develop"),
                "{prompt}"
            );
            assert!(
                prompt.contains("Ship from the current branch, feature."),
                "{prompt}"
            );
            assert!(!prompt.contains("uncommitted"), "{prompt}");
        }

        #[test]
        fn the_default_branch_is_not_shipped_from_when_shipping_into_another_branch() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.push_upstream("develop", "dev.txt", "dev\n");
            env.write("feature.rs", "fn f() {}\n");
            let base = ("FAKE_GH_BASE", "develop");
            env.succeed(&["code", "ship", "--into", "develop"], &[base]);
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.origin_ref("refs/heads/main"), Some(start));
            let prompt = env.logged("ship.1").unwrap();
            assert!(prompt.contains("Create a new branch"), "{prompt}");
        }

        fn assert_unshipped(env: &Env, vars: &[(&str, &str)], problem: &str) {
            let stderr = env.fail(&["code", "ship"], vars);
            assert!(
                stderr.contains(&format!(
                    "error: claude did not finish shipping to origin/main: {problem}: ship report 1\nResume: claude --resume "
                )),
                "{stderr}"
            );
        }

        #[test]
        fn uncommitted_changes_left_by_the_agent_are_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let ship = ("FAKE_SHIP", "true");
            assert_unshipped(
                &env,
                &[ship],
                "the working tree has uncommitted changes (feature.rs)",
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        #[test]
        fn shipping_from_the_target_branch_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let ship = (
                "FAKE_SHIP",
                "git add --all && git commit --quiet --message x && git push --quiet origin HEAD",
            );
            let stderr = env.fail(&["code", "ship"], &[ship]);
            assert!(
                stderr.contains("did not finish shipping to origin/main: HEAD is main at ")
                    && stderr.contains(
                        ", which is not a branch to open a pull request from: ship report 1\nResume: "
                    ),
                "{stderr}"
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        #[test]
        fn shipping_from_the_default_branch_into_another_branch_is_an_error() {
            let env = Env::new();
            env.push_upstream("develop", "dev.txt", "dev\n");
            env.write("feature.rs", "fn f() {}\n");
            let ship = (
                "FAKE_SHIP",
                "git add --all && git commit --quiet --message x && git push --quiet origin HEAD",
            );
            let base = ("FAKE_GH_BASE", "develop");
            let stderr = env.fail(&["code", "ship", "--into", "develop"], &[ship, base]);
            assert!(
                stderr.contains("did not finish shipping to origin/develop: HEAD is main at ")
                    && stderr.contains(
                        ", which is not a branch to open a pull request from: ship report 1\nResume: "
                    ),
                "{stderr}"
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        #[test]
        fn a_detached_head_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let ship = ("FAKE_SHIP", "eval \"$SHIP\" && git switch --quiet --detach");
            let stderr = env.fail(&["code", "ship"], &[ship]);
            assert!(stderr.contains("HEAD is a detached HEAD at "), "{stderr}");
        }

        #[test]
        fn an_unpushed_branch_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let ship = (
                "FAKE_SHIP",
                "git switch --quiet --create add-feature && git add --all && git commit --quiet --message x",
            );
            let stderr = env.fail(&["code", "ship"], &[ship]);
            let head = env.git(&["rev-parse", "HEAD"]);
            assert!(
                stderr.contains(&format!(
                    "the pull request at https://example.com/owner/repo/pull/1 has head commit , not {head}: \
                     ship report 1\nResume: "
                )),
                "{stderr}"
            );
        }

        #[test]
        fn a_pull_request_that_is_not_open_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            assert_unshipped(
                &env,
                &[("FAKE_GH_STATE", "MERGED")],
                "the pull request at https://example.com/owner/repo/pull/1 has state MERGED, not OPEN",
            );
        }

        #[test]
        fn a_pull_request_against_another_branch_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            assert_unshipped(
                &env,
                &[("FAKE_GH_BASE", "develop")],
                "the pull request at https://example.com/owner/repo/pull/1 has base branch develop, not main",
            );
        }

        #[test]
        fn a_pull_request_from_another_branch_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            assert_unshipped(
                &env,
                &[("FAKE_GH_HEAD", "other")],
                "the pull request at https://example.com/owner/repo/pull/1 has head branch other, not \
                 add-feature",
            );
        }

        #[test]
        fn uncommitted_changes_the_agent_does_not_ship_are_an_error() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.commit("Add the feature.");
            env.write("more.rs", "fn g() {}\n");
            let stash = (
                "FAKE_SHIP",
                "git stash --quiet --include-untracked && git push --quiet --set-upstream origin HEAD",
            );
            let stderr = env.fail(&["code", "ship"], &[stash]);
            assert!(
                stderr.contains(
                    "did not finish shipping to origin/main: the shipped commit has tree "
                ) && stderr.contains(", the work there was to ship: ship report 1\nResume: "),
                "{stderr}"
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        #[test]
        fn local_commits_the_agent_does_not_ship_are_an_error() {
            let env = Env::new();
            env.write("local.rs", "fn l() {}\n");
            let start = env.commit("Local.");
            env.write("feature.rs", "fn f() {}\n");
            let fresh = (
                "FAKE_SHIP",
                "git stash --quiet --include-untracked && \
                 git switch --quiet --create add-feature origin/main && \
                 git stash pop --quiet && git add --all && git commit --quiet --message x && \
                 git push --quiet --set-upstream origin HEAD",
            );
            assert_unshipped(
                &env,
                &[fresh],
                &format!(
                    "the shipped branch no longer contains {start}, the commit it started from"
                ),
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        #[test]
        fn a_pull_request_in_another_repository_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let url = "https://example.com/other/repo/pull/1";
            assert_unshipped(
                &env,
                &[("FAKE_GH_URL", url)],
                &format!(
                    "the pull request at {url} is not from and into https://example.com/owner/repo"
                ),
            );
        }

        #[test]
        fn the_url_shown_is_the_one_gh_reports() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let url = "https://example.com/owner/repo/pull/7";
            let (stdout, _) = env.succeed(
                &["code", "ship"],
                &[("FAKE_SHIP_URL", ""), ("FAKE_GH_URL", url)],
            );
            assert_eq!(stdout, format!("{url}\n"));
        }

        #[test]
        fn a_pull_request_from_another_repository_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            assert_unshipped(
                &env,
                &[("FAKE_GH_CROSS", "true")],
                "the pull request at https://example.com/owner/repo/pull/1 is not from and into \
                 https://example.com/owner/repo",
            );
        }

        #[test]
        fn a_failing_gh_after_the_agent_can_be_resumed() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_GH_PR_FAILS", "1")]);
            assert!(stderr.contains("not logged in"), "{stderr}");
            assert!(
                stderr
                    .trim_end()
                    .lines()
                    .last()
                    .unwrap()
                    .starts_with("Resume: claude --resume "),
                "{stderr}"
            );
        }

        #[test]
        fn an_agent_that_could_not_finish_is_an_error() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_COMPLETED", "false")]);
            assert!(
                stderr.contains(
                    "error: claude could not finish the task: ship report 1\nResume: claude --resume "
                ),
                "{stderr}"
            );
            assert_eq!(env.gh_calls(), [env.viewed_repo()]);
        }

        fn assert_rejected(env: &Env) {
            assert_eq!(env.count("ship"), 0);
            assert!(env.gh_calls().is_empty());
        }

        #[test]
        fn an_into_branch_missing_from_the_remote_is_rejected_up_front() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship", "--into", "nope"], &[]);
            assert!(
                stderr.contains("error: no remote has a branch named nope"),
                "{stderr}"
            );
            assert_rejected(&env);
        }

        #[test]
        fn an_unreachable_origin_is_rejected_up_front() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let missing = env.origin.path().join("missing");
            env.git(&["remote", "set-url", "origin", missing.to_str().unwrap()]);
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(stderr.contains("missing"), "{stderr}");
            assert_rejected(&env);
        }

        #[test]
        fn a_branch_without_commits_is_rejected_up_front() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--orphan", "fresh"]);
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("error: the current branch in ")
                    && stderr.contains(" has no history in common with origin/main"),
                "{stderr}"
            );
            assert_rejected(&env);
        }

        #[test]
        fn nothing_to_ship_is_rejected_up_front() {
            let env = Env::new();
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("error: there is nothing to ship in ")
                    && stderr.contains(
                        ": no uncommitted changes and no commits that are not on origin/main"
                    ),
                "{stderr}"
            );
            assert_rejected(&env);
        }

        #[test]
        fn a_merge_in_progress_is_rejected_up_front() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "feature\n");
            let head = env.commit("Feature.");
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["fetch", "--quiet", "origin"]);
            let merge = Command::new("git")
                .args([
                    "-c",
                    "user.name=x",
                    "-c",
                    "user.email=x",
                    "merge",
                    "origin/main",
                ])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!merge.status.success());
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("error: a merge is already in progress in "),
                "{stderr}"
            );
            assert_rejected(&env);
            assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
        }

        #[test]
        fn unresolved_conflicts_are_rejected_up_front() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "stashed\n");
            env.git(&["stash", "--quiet"]);
            env.write("README.md", "feature\n");
            env.commit("Feature.");
            let pop = Command::new("git")
                .args(["stash", "pop"])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!pop.status.success());
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains(
                    "has unresolved conflicts in README.md; resolve and stage them first"
                ),
                "{stderr}"
            );
            assert_rejected(&env);
        }

        #[test]
        fn a_repository_gh_cannot_find_is_rejected_up_front() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_GH_REPO_FAILS", "1")]);
            assert!(stderr.contains("not logged in"), "{stderr}");
            assert_eq!(env.count("ship"), 0);
        }

        #[test]
        fn a_missing_gh_is_rejected_up_front() {
            let env = Env::with(&["claude"]);
            env.write("feature.rs", "fn f() {}\n");
            let stderr = failed(&env.ncli(&["code", "ship"], &[]), Error::MISSING_EXECUTABLE);
            assert!(
                stderr.contains("required executables not found on PATH: gh"),
                "{stderr}"
            );
            assert_eq!(env.count("ship"), 0);
        }

        #[test]
        fn a_missing_agent_is_rejected_up_front() {
            let env = Env::with(&["gh"]);
            env.write("feature.rs", "fn f() {}\n");
            failed(&env.ncli(&["code", "ship"], &[]), Error::MISSING_EXECUTABLE);
            assert!(env.gh_calls().is_empty());
        }

        #[test]
        fn outside_a_git_repository_is_rejected_up_front() {
            let env = Env::new();
            let elsewhere = TempDir::new().unwrap();
            let stderr = failed(
                &env.ncli_in(elsewhere.path(), &["code", "ship"], &[]),
                Error::FAILURE,
            );
            assert!(
                stderr.contains("error: the current directory is not in a git work tree"),
                "{stderr}"
            );
            assert_rejected(&env);
        }
    }

    mod green {
        use super::*;

        const CHECK: [&str; 4] = ["--", "sh", "-c", "echo CHECK OUTPUT; test -f fixed"];

        fn green<'a>(extra: &[&'a str]) -> Vec<&'a str> {
            [&["code", "green"][..], extra, &CHECK].concat()
        }

        #[test]
        fn a_passing_check_needs_no_agent() {
            let env = Env::with(&[]);
            env.write("fixed", "");
            let (stdout, stderr) = env.succeed(&green(&[]), &[]);
            assert_eq!(
                stdout,
                "`sh -c 'echo CHECK OUTPUT; test -f fixed'` passes; there was nothing to fix.\n"
            );
            assert!(stderr.contains("CHECK OUTPUT"), "{stderr}");
            assert_eq!(env.count("fix"), 0);
        }

        #[test]
        fn a_failing_check_is_fixed_and_run_again() {
            let env = Env::new();
            let (stdout, stderr) = env.succeed(&green(&[]), &[]);
            assert!(
                stdout.starts_with("fix report 1\n\nResume: claude --resume "),
                "{stdout}"
            );
            assert!(
                stdout.ends_with(" passes after 1 round(s) of fixes.\n"),
                "{stdout}"
            );
            assert_eq!(stderr.matches("\nCHECK OUTPUT\n").count(), 2, "{stderr}");
            assert_eq!(env.count("fix"), 1);
            let prompt = env.logged("fix.1").unwrap();
            assert!(
                prompt.contains("<output>\nCHECK OUTPUT\nsh exited with code 1"),
                "{prompt}"
            );
            assert!(prompt.contains("Do not weaken, skip, or delete any check or test"));
            assert!(prompt.contains("Do not commit"));
            assert_eq!(env.git(&["rev-list", "--count", "HEAD"]), "1");
        }

        #[test]
        fn the_agent_sees_what_a_check_killed_by_a_signal_wrote_to_stderr() {
            let env = Env::new();
            let check = [
                "code",
                "green",
                "--",
                "sh",
                "-c",
                "test -f fixed || { echo BOOM >&2; kill -ABRT $$; }",
            ];
            env.succeed(&check, &[]);
            let prompt = env.logged("fix.1").unwrap();
            assert!(
                prompt.contains("sh was killed by signal 6: BOOM\n</output>"),
                "{prompt}"
            );
        }

        #[test]
        fn a_check_that_still_fails_after_the_last_round_is_an_error() {
            let env = Env::new();
            let fix = ("FAKE_FIX", "echo more >> notes.txt");
            let stderr = env.fail(&green(&["--max-rounds", "2"]), &[fix]);
            assert!(
                stderr.contains(
                    "error: `sh -c 'echo CHECK OUTPUT; test -f fixed'` still fails after 2 round(s) \
                     of fixes:\n\nfix report 1\n\nResume: "
                ),
                "{stderr}"
            );
            assert!(
                stderr.contains("sh exited with code 1\nResume: claude --resume "),
                "{stderr}"
            );
            assert_eq!(env.count("fix"), 2);
            assert!(
                env.logged("fix.2")
                    .unwrap()
                    .contains("<earlier>\nfix report 1\n</earlier>")
            );
        }

        #[test]
        fn an_agent_that_changes_nothing_is_an_error() {
            let env = Env::new();
            let stderr = env.fail(&green(&[]), &[("FAKE_FIX", "true")]);
            assert!(
                stderr.contains("error: claude left no changes in ")
                    && stderr.contains(" to fix the check: fix report 1\nResume: claude --resume "),
                "{stderr}"
            );
            assert_eq!(env.count("fix"), 1);
        }

        #[test]
        fn what_the_check_writes_is_not_counted_as_a_fix() {
            let env = Env::new();
            let check = [
                "code",
                "green",
                "--",
                "sh",
                "-c",
                "echo run >> check.log; test -f fixed",
            ];
            let stderr = env.fail(&check, &[("FAKE_FIX", "true")]);
            assert!(
                stderr.contains("error: claude left no changes in "),
                "{stderr}"
            );
            assert_eq!(env.count("fix"), 1);
        }

        #[test]
        fn an_agent_that_commits_is_an_error() {
            let env = Env::new();
            let commit = ("FAKE_FIX", "touch fixed && git add -A && git commit -qm x");
            let stderr = env.fail(&green(&[]), &[commit]);
            assert!(
                stderr.contains("error: HEAD in ") && stderr.contains(" moved from main at "),
                "{stderr}"
            );
        }

        #[test]
        fn a_check_that_moves_head_is_an_error() {
            let env = Env::new();
            let check = [
                "code",
                "green",
                "--",
                "sh",
                "-c",
                "git commit --allow-empty -qm x",
            ];
            let stderr = env.fail(&check, &[]);
            assert!(
                stderr.contains("error: HEAD in ")
                    && stderr.contains(" while `sh -c 'git commit --allow-empty -qm x'` ran;"),
                "{stderr}"
            );
            assert_eq!(env.count("fix"), 0);
        }

        #[test]
        fn a_missing_check_command_is_rejected_up_front() {
            let env = Env::new();
            let output = env.ncli(&["code", "green", "--", "no-such-check"], &[]);
            let stderr = failed(&output, Error::MISSING_EXECUTABLE);
            assert!(stderr.contains("no-such-check"), "{stderr}");
            assert_eq!(env.count("fix"), 0);
        }
    }

    mod sync {
        use super::*;

        const UNFINISHED: &str = "error: claude did not finish rebasing feature onto origin/main: ";

        fn feature(env: &Env, path: &str, contents: &str) -> String {
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write(path, contents);
            env.commit("Feature.")
        }

        // A branch whose commit changes README.md, which main changes too.
        fn conflicting(env: &Env) -> (String, String) {
            let old = feature(env, "README.md", "feature\n");
            let upstream = env.push_upstream("main", "README.md", "upstream\n");
            (old, upstream)
        }

        fn assert_restored(env: &Env, old: &str) {
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
            assert_eq!(env.git(&["rev-parse", "feature"]), old);
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert!(!env.rebasing());
        }

        fn assert_unfinished(stderr: &str, problem: &str) {
            assert!(
                stderr.contains(&format!(
                    "{UNFINISHED}{problem}: resolve report 1\nResume: claude --resume "
                )),
                "{stderr}"
            );
        }

        #[test]
        fn a_branch_without_conflicts_is_rebased_onto_the_default_branch() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            let upstream = env.push_upstream("main", "README.md", "upstream\n");
            let (stdout, _) = env.succeed(&["code", "sync"], &[]);
            let new = env.git(&["rev-parse", "HEAD"]);
            assert_eq!(
                stdout,
                format!("Rebased feature onto origin/main, from {old} to {new}.\n")
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), upstream);
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.count("resolve"), 0);
            assert_eq!(env.origin_ref("refs/heads/feature"), None);
        }

        #[test]
        fn an_up_to_date_branch_is_left_as_it_is() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            let (stdout, _) = env.succeed(&["code", "sync"], &[]);
            assert_eq!(
                stdout,
                format!("feature is up to date with origin/main, at {old}.\n")
            );
            assert_eq!(env.count("resolve"), 0);
        }

        #[test]
        fn the_default_branch_is_asked_of_the_remote_when_it_is_not_recorded_locally() {
            let env = Env::new();
            env.git(&["remote", "set-head", "origin", "--delete"]);
            feature(&env, "feature.rs", "fn f() {}\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let trunk = env.push_upstream("trunk", "trunk.txt", "trunk\n");
            git(
                env.origin.path(),
                &["symbolic-ref", "HEAD", "refs/heads/trunk"],
            );
            let (stdout, _) = env.succeed(&["code", "sync"], &[]);
            assert!(
                stdout.starts_with("Rebased feature onto origin/trunk, "),
                "{stdout}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), trunk);
        }

        #[test]
        fn a_branch_is_rebased_onto_the_named_branch() {
            let env = Env::new();
            feature(&env, "feature.rs", "fn f() {}\n");
            let develop = env.push_upstream("develop", "dev.txt", "dev\n");
            let (stdout, _) = env.succeed(&["code", "sync", "--from", "develop"], &[]);
            assert!(
                stdout.starts_with("Rebased feature onto origin/develop, "),
                "{stdout}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), develop);
        }

        #[test]
        fn a_rebase_that_fails_without_conflicts_is_an_error_without_the_agent() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            env.push_upstream("main", "README.md", "upstream\n");
            script(
                &env.work.path().join(".git/hooks"),
                "pre-rebase",
                "#!/bin/sh\nexit 1\n",
            );
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("The pre-rebase hook refused to rebase."),
                "{stderr}"
            );
            assert_eq!(env.count("resolve"), 0);
            assert_restored(&env, &old);
        }

        #[test]
        fn conflicts_are_resolved_by_the_agent_which_finishes_the_rebase() {
            let env = Env::new();
            let (_, upstream) = conflicting(&env);
            let (stdout, _) = env.succeed(&["code", "sync"], &[]);
            assert!(
                stdout.starts_with("Rebased feature onto origin/main, "),
                "{stdout}"
            );
            assert!(
                stdout.contains(".\n\nResolved the conflicts:\n\nresolve report 1\n\nResume: "),
                "{stdout}"
            );
            assert_eq!(
                fs::read_to_string(env.work.path().join("README.md")).unwrap(),
                "resolved\n"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), upstream);
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Feature.");
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
            let prompt = env.logged("resolve.1").unwrap();
            assert!(
                prompt.contains(
                    "A rebase of the branch feature onto origin/main in the repository at "
                ),
                "{prompt}"
            );
            assert!(
                prompt.contains(
                    "`git commit --no-edit --no-verify --cleanup=verbatim -C REBASE_HEAD`"
                ),
                "{prompt}"
            );
            assert!(
                prompt.contains("`GIT_EDITOR=: git rebase --continue`, until the rebase is done"),
                "{prompt}"
            );
            assert!(
                prompt.contains("Do not switch branches, reset, stash, or push."),
                "{prompt}"
            );
        }

        #[test]
        fn every_conflicting_commit_is_resolved_in_one_agent_run() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            env.write("README.md", "more\n");
            env.commit("More.");
            let upstream = env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "for _ in 1 2 3; do [ -d .git/rebase-merge ] || break; \
                git rev-parse REBASE_HEAD > README.md; git add README.md; \
                GIT_EDITOR=: git rebase --continue; done";
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert_eq!(env.count("resolve"), 1);
            assert_eq!(env.git(&["rev-parse", "HEAD~2"]), upstream);
            assert_eq!(env.git(&["log", "-2", "--format=%s"]), "More.\nFeature.");
            assert!(!env.rebasing());
        }

        #[test]
        fn marker_like_lines_in_files_the_branch_does_not_change_are_allowed() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            env.change_upstream("main", |clone| {
                fs::write(clone.join("README.md"), "upstream\n").unwrap();
                fs::write(clone.join("sample.txt"), "<<<<<<< sample\n").unwrap();
            });
            env.succeed(&["code", "sync"], &[]);
            assert!(!env.rebasing());
        }

        #[test]
        fn a_rebase_the_agent_leaves_unfinished_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", "true")]);
            assert_unfinished(&stderr, "a rebase is still in progress");
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn a_rebase_the_agent_aborts_is_an_error() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let abort = ("FAKE_RESOLVE", "git rebase --abort");
            let stderr = env.fail(&["code", "sync"], &[abort]);
            assert_unfinished(&stderr, "origin/main is not in the history of the branch");
            assert_restored(&env, &old);
        }

        #[test]
        fn a_commit_the_agent_skips_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let skip = ("FAKE_RESOLVE", "git rebase --skip");
            let stderr = env.fail(&["code", "sync"], &[skip]);
            assert_unfinished(
                &stderr,
                &format!("the changes of {old} are no longer on the branch"),
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn a_skipped_commit_is_found_when_a_kept_commit_shares_its_identity() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            let date = "--date=2000-01-01T00:00:00Z";
            env.write("a.txt", "a\n");
            env.git(&["add", "--all"]);
            env.git(&["commit", "--quiet", date, "--message", "wip"]);
            env.write("README.md", "feature\n");
            env.git(&["add", "--all"]);
            env.git(&["commit", "--quiet", date, "--message", "wip"]);
            let old = env.git(&["rev-parse", "HEAD"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let skip = ("FAKE_RESOLVE", "git rebase --skip");
            let stderr = env.fail(&["code", "sync"], &[skip]);
            assert_unfinished(
                &stderr,
                &format!("the changes of {old} are no longer on the branch"),
            );
            assert_restored(&env, &old);
        }

        const THEIRS: &str = "git checkout --ours README.md && git add README.md && \
            git commit --quiet --allow-empty --no-edit --no-verify -C REBASE_HEAD && \
            GIT_EDITOR=: git rebase --continue";

        #[test]
        fn a_commit_the_agent_empties_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", THEIRS)]);
            assert_unfinished(
                &stderr,
                &format!("the changes of {old} are no longer on the branch"),
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn a_change_the_agent_drops_from_a_kept_commit_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let resolve =
                format!("echo unrelated > unrelated.txt && git add unrelated.txt && {THEIRS}");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", &resolve)]);
            assert_unfinished(
                &stderr,
                &format!("the changes of {old} are no longer on the branch"),
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn a_kept_commit_may_leave_out_a_change_already_upstream() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "feature\n");
            env.write("same.txt", "same\n");
            env.commit("Feature.");
            env.change_upstream("main", |clone| {
                fs::write(clone.join("same.txt"), "same\n").unwrap();
                fs::write(clone.join("README.md"), "upstream\n").unwrap();
            });
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Feature.");
            assert!(!env.rebasing());
        }

        #[test]
        fn a_branch_with_merge_commits_is_rejected_up_front() {
            let env = Env::new();
            feature(&env, "feature.rs", "fn f() {}\n");
            env.git(&["switch", "--quiet", "--create", "side", "main"]);
            env.write("side.txt", "side\n");
            env.commit("Side.");
            env.git(&["switch", "--quiet", "feature"]);
            env.git(&["merge", "--quiet", "--no-commit", "side"]);
            env.write("merge-only", "merge\n");
            let old = env.commit("Merge.");
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains(&format!(
                    "cannot sync feature: its merge commits since origin/main ({old}) would be \
                     flattened by a rebase"
                )),
                "{stderr}"
            );
            assert_eq!(env.count("resolve"), 0);
            assert_restored(&env, &old);
        }

        #[test]
        fn marker_like_lines_already_in_a_file_the_branch_changes_are_allowed() {
            let env = Env::new();
            env.write("fixture.txt", "<<<<<<< ours\n");
            env.commit("Fixture.");
            env.git(&["push", "--quiet", "origin", "main"]);
            feature(&env, "README.md", "feature\n");
            env.write("fixture.txt", "<<<<<<< ours\nmore\n");
            env.commit("More.");
            env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            assert!(!env.rebasing());
        }

        #[test]
        fn a_commit_git_drops_because_its_change_is_already_upstream_is_allowed() {
            let env = Env::new();
            feature(&env, "same.txt", "same\n");
            env.write("README.md", "feature\n");
            env.commit("More.");
            let upstream = env.change_upstream("main", |clone| {
                fs::write(clone.join("same.txt"), "same\n").unwrap();
                fs::write(clone.join("README.md"), "upstream\n").unwrap();
            });
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), upstream);
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "More.");
            assert!(!env.rebasing());
        }

        #[test]
        fn a_commit_already_upstream_and_edited_there_since_is_allowed() {
            let env = Env::new();
            feature(&env, "same.txt", "same\n");
            env.write("README.md", "feature\n");
            env.commit("More.");
            env.push_upstream("main", "same.txt", "same\n");
            env.push_upstream("main", "same.txt", "edited\n");
            let upstream = env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), upstream);
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "More.");
            assert!(!env.rebasing());
        }

        #[test]
        fn hidden_edits_are_rejected_up_front() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            env.git(&["update-index", "--assume-unchanged", "README.md"]);
            env.write("README.md", "hidden\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("files marked assume-unchanged or skip-worktree have edits"),
                "{stderr}"
            );
            assert_eq!(env.count("resolve"), 0);
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
            assert_eq!(
                fs::read_to_string(env.work.path().join("README.md")).unwrap(),
                "hidden\n"
            );
        }

        #[test]
        fn an_agent_that_leaves_the_branch_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let detach = (
                "FAKE_RESOLVE",
                "eval \"$RESOLVE\" && git switch --quiet --detach",
            );
            let stderr = env.fail(&["code", "sync"], &[detach]);
            assert!(
                stderr.contains(&format!("{UNFINISHED}HEAD is now a detached HEAD at ")),
                "{stderr}"
            );
            assert!(stderr.contains("Resume: claude --resume "), "{stderr}");
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn uncommitted_changes_left_by_the_agent_put_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let scratch = (
                "FAKE_RESOLVE",
                "eval \"$RESOLVE\" && echo scratch > scratch.txt",
            );
            let stderr = env.fail(&["code", "sync"], &[scratch]);
            assert_unfinished(
                &stderr,
                "the working tree has uncommitted changes (scratch.txt)",
            );
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "?? scratch.txt");
        }

        #[test]
        fn a_failed_sync_leaves_other_branches_in_the_rebased_range_alone() {
            let env = Env::new();
            conflicting(&env);
            env.git(&["branch", "part"]);
            env.write("more.rs", "fn g() {}\n");
            let old = env.commit("More.");
            env.git(&["config", "rebase.updateRefs", "true"]);
            let part = env.git(&["rev-parse", "part"]);
            let junk = ("FAKE_RESOLVE", "eval \"$RESOLVE\" && echo junk > junk");
            env.fail(&["code", "sync"], &[junk]);
            assert_restored(&env, &old);
            assert_eq!(env.git(&["rev-parse", "part"]), part);
        }

        #[test]
        fn a_branch_that_changes_many_files_is_checked_for_conflict_markers() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            let dir = env.work.path().join("vendor").join("x".repeat(200));
            fs::create_dir_all(&dir).unwrap();
            for index in 0..8_000 {
                fs::write(dir.join(format!("{index:0>200}")), "").unwrap();
            }
            env.write("README.md", "feature\n");
            let old = env.commit("Feature.");
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", MARKED)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
        }

        const MARKED: &str = "printf '<<<<<<< HEAD\\nx\\n=======\\ny\\n>>>>>>> c\\n' > README.md \
            && git add README.md && GIT_EDITOR=: git rebase --continue";

        #[test]
        fn committed_conflict_markers_put_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", MARKED)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        fn configure_grep(env: &Env) {
            for (key, value) in [
                ("color.ui", "always"),
                ("grep.lineNumber", "true"),
                ("grep.column", "true"),
            ] {
                env.git(&["config", key, value]);
            }
        }

        #[test]
        fn conflict_markers_are_found_whatever_the_grep_config() {
            let env = Env::new();
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "<<<<<<< upstream\n");
            configure_grep(&env);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", MARKED)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
        }

        #[test]
        fn marker_like_lines_that_moved_are_allowed_whatever_the_grep_config() {
            let env = Env::new();
            env.write("fixture.txt", "1\n2\n3\n4\n<<<<<<< fixture\n");
            env.commit("Fixture.");
            env.git(&["push", "--quiet", "origin", "main"]);
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "feature\n");
            env.write("fixture.txt", "0\n1\n2\n3\n4\n<<<<<<< fixture\n");
            env.commit("Feature.");
            env.change_upstream("main", |clone| {
                fs::write(clone.join("README.md"), "upstream\n").unwrap();
                fs::write(
                    clone.join("fixture.txt"),
                    "1\n2\n3\n4\n4b\n<<<<<<< fixture\n",
                )
                .unwrap();
            });
            configure_grep(&env);
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(
                fs::read_to_string(env.work.path().join("fixture.txt")).unwrap(),
                "0\n1\n2\n3\n4\n4b\n<<<<<<< fixture\n"
            );
        }

        #[test]
        fn a_rebase_backend_set_in_config_does_not_stop_the_agent_committing() {
            let env = Env::new();
            conflicting(&env);
            env.git(&["config", "rebase.backend", "apply"]);
            env.succeed(&["code", "sync"], &[]);
            assert!(!env.rebasing());
        }

        #[test]
        fn a_target_fetched_again_during_the_resolution_does_not_fail_the_sync() {
            let env = Env::new();
            let (_, upstream) = conflicting(&env);
            let moved = "eval \"$RESOLVE\" && git update-ref refs/remotes/origin/main \
                $(git commit-tree -p HEAD -m later HEAD^{tree})";
            let (stdout, _) = env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", moved)]);
            assert!(
                stdout.starts_with("Rebased feature onto origin/main, "),
                "{stdout}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), upstream);
            let prompt = env.logged("resolve.1").unwrap();
            assert!(
                prompt.contains(&format!("REBASE_HEAD..{upstream} -- <path>")),
                "{prompt}"
            );
        }

        #[test]
        fn conflict_markers_are_judged_against_the_fetched_target() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let moved = format!(
                "{MARKED} && git update-ref refs/remotes/origin/main \
                 $(git commit-tree -p HEAD -m later HEAD^{{tree}})"
            );
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", &moved)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
        }

        #[test]
        fn a_sequence_the_agent_leaves_in_progress_is_ended_when_putting_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            env.git(&["switch", "--quiet", "--create", "side", "main"]);
            env.write("README.md", "one\n");
            env.commit("One.");
            env.write("README.md", "two\n");
            env.commit("Two.");
            env.git(&["switch", "--quiet", "feature"]);
            let sequence = "eval \"$RESOLVE\" && { git cherry-pick side~2..side; \
                printf 'x\\n' > README.md && git add README.md && git commit --quiet -m x; }";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", sequence)]);
            assert_unfinished(
                &stderr,
                "a cherry-pick or revert sequence is still in progress",
            );
            assert_restored(&env, &old);
            assert!(!env.work.path().join(".git/sequencer").exists());
        }

        #[test]
        fn longer_conflict_markers_are_found() {
            let env = Env::new();
            env.write(".gitattributes", "README.md conflict-marker-size=10\n");
            env.commit("Attributes.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let (old, _) = conflicting(&env);
            let marked = "printf '<<<<<<<<<< HEAD\\nx\\n==========\\ny\\n>>>>>>>>>> c\\n' \
                > README.md && git add README.md && GIT_EDITOR=: git rebase --continue";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", marked)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
        }

        #[test]
        fn conflict_markers_outside_the_sparse_checkout_are_found() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            env.git(&["sparse-checkout", "set", "--no-cone", "/feature.rs"]);
            let marked = MARKED.replace("git add", "git add --sparse");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", &marked)]);
            assert_unfinished(&stderr, "conflict markers are left in README.md");
            assert_restored(&env, &old);
        }

        #[test]
        fn conflicts_in_files_named_like_pathspecs_are_checked() {
            let env = Env::new();
            let old = feature(&env, ":foo", "feature\n");
            env.push_upstream("main", ":foo", "upstream\n");
            let marked = MARKED.replace("README.md", "./:foo");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", &marked)]);
            assert_unfinished(&stderr, "conflict markers are left in :foo");
            assert_restored(&env, &old);
        }

        #[test]
        fn an_agent_that_cannot_finish_puts_the_branch_back() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let stderr = env.fail(
                &["code", "sync"],
                &[("FAKE_RESOLVE", "true"), ("FAKE_COMPLETED", "false")],
            );
            assert!(
                stderr.contains(
                    "error: claude could not finish the task: resolve report 1\nResume: claude \
                     --resume "
                ),
                "{stderr}"
            );
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn a_branch_that_cannot_be_put_back_is_reported_with_the_resume_hint() {
            let env = Env::new();
            let (old, _) = conflicting(&env);
            let lock = ("FAKE_RESOLVE", "touch .git/index.lock");
            let stderr = env.fail(&["code", "sync"], &[lock]);
            assert!(
                stderr.contains(&format!(
                    "{UNFINISHED}a rebase is still in progress: resolve report 1\nPutting \
                     feature back at {old} then failed: "
                )),
                "{stderr}"
            );
            assert!(
                stderr
                    .trim_end()
                    .lines()
                    .last()
                    .unwrap()
                    .starts_with("Resume: claude --resume "),
                "{stderr}"
            );
            assert!(env.rebasing());
        }

        #[test]
        fn an_interrupted_sync_puts_the_branch_back() {
            use std::process::Stdio;
            use std::thread;
            use std::time::{Duration, Instant};

            let env = Env::new();
            let (old, _) = conflicting(&env);
            let hang = ("FAKE_RESOLVE", "touch \"$log/resolve.started\"; sleep 30");
            let sync = env
                .command(env.work.path(), &["code", "sync"], &[hang])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while env.logged("resolve.started").is_none() {
                assert!(Instant::now() < deadline, "the agent never started");
                thread::sleep(Duration::from_millis(20));
            }
            unsafe { libc::kill(sync.id() as libc::pid_t, libc::SIGTERM) };
            let output = sync.wait_with_output().unwrap();
            assert!(!output.status.success());
            assert_restored(&env, &old);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn a_from_branch_missing_from_the_remote_is_rejected_up_front() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "sync", "--from", "nope"], &[]);
            assert!(
                stderr.contains("error: no remote has a branch named nope"),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
        }

        #[test]
        fn an_unreachable_origin_is_an_error_even_when_another_remote_has_the_branch() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            let other = TempDir::new().unwrap();
            git(other.path(), &["init", "--quiet", "--bare"]);
            env.git(&["remote", "add", "other", other.path().to_str().unwrap()]);
            env.git(&["push", "--quiet", "other", "main:develop"]);
            let missing = env.origin.path().join("missing");
            env.git(&["remote", "set-url", "origin", missing.to_str().unwrap()]);
            let stderr = env.fail(&["code", "sync", "--from", "develop"], &[]);
            assert!(!stderr.contains("no remote has a branch"), "{stderr}");
            assert!(stderr.contains("missing"), "{stderr}");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
        }

        #[test]
        fn a_missing_agent_is_rejected_up_front() {
            let env = Env::with(&["gh"]);
            let (old, _) = conflicting(&env);
            failed(&env.ncli(&["code", "sync"], &[]), Error::MISSING_EXECUTABLE);
            assert_restored(&env, &old);
            assert_eq!(
                env.git(&["rev-parse", "origin/main"]),
                env.git(&["rev-parse", "main"])
            );
        }

        #[test]
        fn a_dirty_working_tree_is_refused() {
            let env = Env::new();
            feature(&env, "feature.rs", "fn f() {}\n");
            env.write("README.md", "edited\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains(" has uncommitted changes (README.md); commit or stash them first"),
                "{stderr}"
            );
            assert_eq!(
                fs::read_to_string(env.work.path().join("README.md")).unwrap(),
                "edited\n"
            );
        }

        #[test]
        fn untracked_files_are_refused() {
            let env = Env::new();
            let old = feature(&env, "feature.rs", "fn f() {}\n");
            env.push_upstream("main", "README.md", "upstream\n");
            env.write("untracked.txt", "untracked\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains(
                    " has uncommitted changes (untracked.txt); commit or stash them first"
                ),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
        }

        #[test]
        fn a_detached_head_is_refused() {
            let env = Env::new();
            env.git(&["checkout", "--quiet", "--detach"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("error: cannot sync a detached HEAD at ")
                    && stderr.contains("switch to a branch with commits first"),
                "{stderr}"
            );
        }

        #[test]
        fn a_branch_without_commits_is_refused() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--orphan", "fresh"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("error: cannot sync fresh with no commits in "),
                "{stderr}"
            );
        }

        #[test]
        fn a_rebase_in_progress_is_refused() {
            let env = Env::new();
            conflicting(&env);
            env.git(&["fetch", "--quiet", "origin"]);
            let rebase = Command::new("git")
                .args(["rebase", "origin/main"])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!rebase.status.success());
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("error: a rebase is already in progress in "),
                "{stderr}"
            );
            assert!(env.rebasing());
        }

        #[test]
        fn a_merge_in_progress_is_refused() {
            let env = Env::new();
            conflicting(&env);
            env.git(&["fetch", "--quiet", "origin"]);
            let merge = Command::new("git")
                .args([
                    "-c",
                    "user.name=x",
                    "-c",
                    "user.email=x",
                    "merge",
                    "origin/main",
                ])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!merge.status.success());
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("error: a merge is already in progress in "),
                "{stderr}"
            );
        }

        #[test]
        fn unresolved_conflicts_are_refused() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "stashed\n");
            env.git(&["stash", "--quiet"]);
            env.write("README.md", "feature\n");
            env.commit("Feature.");
            let pop = Command::new("git")
                .args(["stash", "pop"])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!pop.status.success());
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains(
                    "has unresolved conflicts in README.md; resolve and stage them first"
                ),
                "{stderr}"
            );
            assert_eq!(env.count("resolve"), 0);
        }

        #[test]
        fn a_cherry_pick_sequence_still_in_progress_is_refused() {
            let env = Env::new();
            let old = feature(&env, "README.md", "one\n");
            env.write("README.md", "two\n");
            env.commit("Two.");
            env.git(&["switch", "--quiet", "main"]);
            env.write("README.md", "main\n");
            env.commit("Main.");
            env.git(&["switch", "--quiet", "--create", "picked"]);
            let pick = Command::new("git")
                .args(["cherry-pick", &format!("{old}~1..feature")])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!pick.status.success());
            env.write("README.md", "resolved\n");
            env.commit("Resolved.");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr
                    .contains("error: a cherry-pick or revert sequence is already in progress in "),
                "{stderr}"
            );
            assert_eq!(env.count("resolve"), 0);
        }

        #[test]
        fn an_operation_in_progress_is_reported_before_the_working_tree_is_judged() {
            let env = Env::new();
            conflicting(&env);
            env.git(&["fetch", "--quiet", "origin"]);
            let merge = Command::new("git")
                .args([
                    "-c",
                    "user.name=x",
                    "-c",
                    "user.email=x",
                    "merge",
                    "origin/main",
                ])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(!merge.status.success());
            env.write("README.md", "feature\n");
            for command in ["sync", "ship"] {
                let stderr = env.fail(&["code", command], &[]);
                assert!(
                    stderr.contains("error: a merge is already in progress in "),
                    "{stderr}"
                );
            }
        }

        #[test]
        fn outside_a_git_repository_is_refused() {
            let env = Env::new();
            let elsewhere = TempDir::new().unwrap();
            let stderr = failed(
                &env.ncli_in(elsewhere.path(), &["code", "sync"], &[]),
                Error::FAILURE,
            );
            assert!(
                stderr.contains("error: the current directory is not in a git work tree"),
                "{stderr}"
            );
        }
    }
}
