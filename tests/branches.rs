#[cfg(unix)]
mod common;

#[cfg(unix)]
mod binary {
    use std::fs;
    use std::process::Output;

    use crate::common::{git, ncli, script};
    use ncli::error::Error;
    use tempfile::TempDir;

    const FAKE_AGENT: &str = r#"#!/bin/sh
prompt=$(cat)
log="$NCLI_TEST_LOG"
count=$(( $(cat "$log/agent.count" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$log/agent.count"
printf '%s' "$prompt" > "$log/agent.$count"
SHIP='git switch --quiet --create add-feature 2>/dev/null
git add --all && git commit --quiet --message "Add a feature."
git push --quiet --set-upstream origin HEAD'
RESOLVE='for f in $(git diff --name-only --diff-filter=U); do printf "resolved\n" > "$f"; git add "$f"; done'
case "$prompt" in
*"as a pull request into"*)
    eval "${FAKE_SHIP:-$SHIP}" >&2
    echo '{"completed": true, "report": "Shipped.", "url": "https://example.com/owner/repo/pull/1"}' ;;
*"rebase the current branch onto commit"*)
    onto=$(printf '%s' "$prompt" | sed -n 's/.*onto commit \([0-9a-f]*\).*/\1/p')
    git rebase --quiet "$onto" >&2 2>/dev/null
    while [ -d .git/rebase-merge ]; do
        eval "${FAKE_RESOLVE:-$RESOLVE}" >&2
        [ -n "$FAKE_STOP" ] && break
        GIT_EDITOR=true git rebase --continue >/dev/null 2>&1
    done
    eval "${FAKE_AFTER:-:}" >&2
    echo '{"completed": true, "report": "Rebased."}' ;;
esac
"#;

    const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$NCLI_TEST_LOG/gh.log"
head=$(git rev-parse "${FAKE_PR_HEAD:-HEAD}")
echo "{\"state\": \"${FAKE_PR_STATE:-OPEN}\", \"baseRefName\": \"${FAKE_PR_BASE:-main}\", \"headRefOid\": \"$head\"}"
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
            let env = Self {
                bin: TempDir::new().unwrap(),
                log: TempDir::new().unwrap(),
                state: TempDir::new().unwrap(),
                origin: TempDir::new().unwrap(),
                work: TempDir::new().unwrap(),
            };
            script(env.bin.path(), "claude", FAKE_AGENT);
            script(env.bin.path(), "gh", FAKE_GH);
            git(
                env.origin.path(),
                &["init", "--quiet", "--bare", "--initial-branch", "main"],
            );
            env.git(&["init", "--quiet", "--initial-branch", "main"]);
            env.write("README.md", "hello\n");
            env.commit("Start.");
            env.git(&[
                "remote",
                "add",
                "origin",
                env.origin.path().to_str().unwrap(),
            ]);
            env.git(&["push", "--quiet", "--set-upstream", "origin", "main"]);
            env.git(&["remote", "set-head", "origin", "main"]);
            env
        }

        fn ncli(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
            let output = ncli(self.state.path())
                .args(args)
                .current_dir(self.work.path())
                .env(
                    "PATH",
                    format!("{}:/bin:/usr/bin", self.bin.path().display()),
                )
                .env("NCLI_TEST_LOG", self.log.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "ncli")
                .env("GIT_AUTHOR_EMAIL", "ncli@example.com")
                .env("GIT_COMMITTER_NAME", "ncli")
                .env("GIT_COMMITTER_EMAIL", "ncli@example.com")
                .envs(vars.iter().copied())
                .output()
                .unwrap();
            assert_ne!(output.status.code(), Some(Error::USAGE.into()));
            output
        }

        fn succeed(&self, args: &[&str], vars: &[(&str, &str)]) -> String {
            let output = self.ncli(args, vars);
            assert!(output.status.success(), "{}", stderr(&output));
            String::from_utf8(output.stdout).unwrap()
        }

        fn fail(&self, args: &[&str], vars: &[(&str, &str)]) -> String {
            let output = self.ncli(args, vars);
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

        fn write(&self, path: &str, contents: &str) {
            fs::write(self.work.path().join(path), contents).unwrap();
        }

        fn read(&self, path: &str) -> String {
            fs::read_to_string(self.work.path().join(path)).unwrap()
        }

        fn commit(&self, message: &str) -> String {
            self.git(&["add", "--all"]);
            self.git(&["commit", "--quiet", "--message", message]);
            self.git(&["rev-parse", "HEAD"])
        }

        // Commits to a branch of origin from another clone, as a teammate would.
        fn push_upstream(&self, branch: &str, path: &str, contents: &str) {
            let clone = TempDir::new().unwrap();
            let clone = clone.path();
            git(
                clone,
                &[
                    "clone",
                    "--quiet",
                    self.origin.path().to_str().unwrap(),
                    ".",
                ],
            );
            git(clone, &["switch", "--quiet", branch]);
            fs::write(clone.join(path), contents).unwrap();
            git(clone, &["add", "--all"]);
            git(clone, &["commit", "--quiet", "--message", "Upstream."]);
            git(clone, &["push", "--quiet", "origin", branch]);
        }

        fn origin(&self, name: &str) -> String {
            git(self.origin.path(), &["rev-parse", name])
        }

        fn logged(&self, name: &str) -> String {
            fs::read_to_string(self.log.path().join(name)).unwrap_or_default()
        }

        fn agent_calls(&self) -> u32 {
            self.logged("agent.count").trim().parse().unwrap_or(0)
        }

        fn feature(&self, contents: &[&str]) -> String {
            self.git(&["switch", "--quiet", "--create", "feature"]);
            let mut tip = String::new();
            for (index, content) in contents.iter().enumerate() {
                self.write("README.md", content);
                tip = self.commit(&format!("Feature {index}."));
            }
            tip
        }

        fn assert_put_back(&self, tip: &str) {
            assert_eq!(self.git(&["branch", "--show-current"]), "feature");
            assert_eq!(self.git(&["rev-parse", "HEAD"]), tip);
            assert_eq!(self.git(&["status", "--porcelain"]), "");
            assert!(!self.work.path().join(".git/rebase-merge").exists());
        }
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    mod ship {
        use super::*;

        const URL: &str = "https://example.com/owner/repo/pull/1\n";

        #[test]
        fn the_pull_request_the_agent_opens_is_reported() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            assert_eq!(env.succeed(&["code", "ship"], &[]), URL);
            assert!(env.logged("agent.1").contains("into the branch main"));
            assert_eq!(env.origin("add-feature"), env.git(&["rev-parse", "HEAD"]));
            assert!(
                env.logged("gh.log")
                    .contains("pr view https://example.com/owner/repo/pull/1")
            );
        }

        #[test]
        fn into_names_the_base_branch() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            let stdout = env.succeed(
                &["code", "ship", "--into", "release"],
                &[("FAKE_PR_BASE", "release")],
            );
            assert_eq!(stdout, URL);
            assert!(env.logged("agent.1").contains("into the branch release"));
        }

        fn unshipped(vars: &[(&str, &str)], expected: &str) {
            let env = Env::new();
            env.write("feature.rs", "\n");
            let stderr = env.fail(&["code", "ship"], vars);
            assert!(stderr.contains("but it is not shipped"), "{stderr}");
            assert!(stderr.contains(expected), "{stderr}");
        }

        #[test]
        fn uncommitted_changes_left_by_the_agent_fail() {
            unshipped(&[("FAKE_SHIP", ":")], "uncommitted changes are left");
        }

        #[test]
        fn a_pull_request_that_is_not_open_fails() {
            unshipped(
                &[("FAKE_PR_STATE", "MERGED")],
                "the pull request is merged, not open",
            );
        }

        #[test]
        fn a_pull_request_against_another_branch_fails() {
            unshipped(
                &[("FAKE_PR_BASE", "other")],
                "the pull request is against other",
            );
        }

        #[test]
        fn a_pull_request_behind_head_fails() {
            unshipped(&[("FAKE_PR_HEAD", "HEAD~1")], "but HEAD is at");
        }
    }

    mod sync {
        use super::*;

        #[test]
        fn a_branch_without_conflicts_is_rebased() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "\n");
            env.commit("Feature.");
            env.push_upstream("main", "README.md", "upstream\n");
            let stdout = env.succeed(&["code", "sync"], &[]);
            assert_eq!(stdout, "Rebased onto origin/main.\n");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("main"));
        }

        #[test]
        fn conflicts_the_agent_resolves_are_kept() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(env.read("README.md"), "resolved\n");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("main"));
        }

        #[test]
        fn from_names_the_branch_to_rebase_onto() {
            let env = Env::new();
            env.git(&["push", "--quiet", "origin", "main:release"]);
            env.push_upstream("release", "release.md", "release\n");
            env.feature(&["feature\n"]);
            let stdout = env.succeed(&["code", "sync", "--from", "release"], &[]);
            assert_eq!(stdout, "Rebased onto origin/release.\n");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("release"));
        }

        fn put_back(vars: &[(&str, &str)], expected: &str) {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], vars);
            assert!(stderr.contains("so the branch was put back at"), "{stderr}");
            assert!(stderr.contains(expected), "{stderr}");
            env.assert_put_back(&tip);
        }

        #[test]
        fn an_unfinished_rebase_is_put_back() {
            put_back(&[("FAKE_STOP", "1")], "the rebase is not finished");
        }

        #[test]
        fn committed_conflict_markers_are_put_back() {
            put_back(
                &[("FAKE_RESOLVE", "git add --all")],
                "conflict markers are left in README.md",
            );
        }

        #[test]
        fn a_merge_instead_of_a_rebase_is_put_back() {
            let merge =
                "git rebase --abort; git merge --quiet --strategy-option=ours --no-edit FETCH_HEAD";
            put_back(
                &[("FAKE_RESOLVE", merge), ("FAKE_STOP", "1")],
                "the branch has merge commits",
            );
        }

        #[test]
        fn uncommitted_changes_left_by_the_agent_are_put_back() {
            put_back(
                &[("FAKE_AFTER", "printf 'x\\n' > stray.md")],
                "uncommitted changes are left",
            );
        }

        #[test]
        fn leaving_the_branch_is_put_back() {
            put_back(
                &[("FAKE_AFTER", "git switch --quiet --detach")],
                "HEAD is no longer on the branch",
            );
        }

        #[test]
        fn uncommitted_changes_are_refused() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.write("stray.md", "\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(stderr.contains("has uncommitted changes"), "{stderr}");
            assert_eq!(env.agent_calls(), 0);
        }

        #[test]
        fn a_detached_head_is_refused() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--detach"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(stderr.contains("not a branch to rebase"), "{stderr}");
        }
    }
}
