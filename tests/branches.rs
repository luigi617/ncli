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
RESOLVE='for f in $(git diff --name-only --diff-filter=U); do printf "resolved %s\n" "$count" > "$f"; git add "$f"; done'
SHIPMENT='{"completed": true, "report": "Adds a feature.", "branch": "add-feature",
    "commit_message": "Add a feature.", "title": "Add a feature", "body": "Adds it.\n\nIn full."}'
case "$prompt" in
*"shipped as a pull request"*)
    eval "${FAKE_DESCRIBE:-:}" >&2
    printf '%s\n' "${FAKE_SHIPMENT:-$SHIPMENT}" ;;
*"stopped with conflicts"*)
    eval "${FAKE_RESOLVE:-$RESOLVE}" >&2
    echo '{"completed": true, "report": "Resolved."}' ;;
esac
"#;

    const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$NCLI_TEST_LOG/gh.log"
case "$1 $2" in
"repo view")
    [ -z "$FAKE_GH_UNAUTHENTICATED" ] || { echo "gh auth login required" >&2; exit 4; }
    echo '{"url": "https://example.com/owner/repo"}' ;;
"pr list") echo "${FAKE_GH_OPEN:-[]}" ;;
"pr create")
    cat > "$NCLI_TEST_LOG/gh.body"
    echo "Creating pull request"
    echo "https://example.com/owner/repo/pull/1" ;;
esac
"#;

    const FAKE_SSH: &str = r#"#!/bin/sh
[ "$1" = -G ] && { echo "hostname github.com"; exit; }
for last; do :; done
cd "$FAKE_SSH_ROOT" && exec sh -c "$last"
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

        // Adds an initialized submodule `sub`.
        fn submodule(&self) -> TempDir {
            let sub = TempDir::new().unwrap();
            git(sub.path(), &["init", "--quiet", "--initial-branch", "main"]);
            git(
                sub.path(),
                &["commit", "--quiet", "--allow-empty", "--message", "One."],
            );
            self.git(&[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                sub.path().to_str().unwrap(),
                "sub",
            ]);
            self.commit("Submodule.");
            self.git(&["push", "--quiet", "origin", "main"]);
            sub
        }

        fn assert_put_back(&self, tip: &str) {
            assert_eq!(self.git(&["branch", "--show-current"]), "feature");
            assert_eq!(self.git(&["rev-parse", "HEAD"]), tip);
            assert_eq!(
                self.git(&["status", "--porcelain", "--untracked-files=no"]),
                ""
            );
            assert!(!self.work.path().join(".git/rebase-merge").exists());
        }
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    mod ship {
        use super::*;

        #[test]
        fn uncommitted_changes_on_the_default_branch_ship_from_a_new_branch() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.write("feature.rs", "fn f() {}\n");
            let stdout = env.succeed(&["code", "ship"], &[]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), start);
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Add a feature.");
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert_eq!(env.git(&["rev-parse", "main"]), start);
            assert_eq!(env.origin("add-feature"), env.git(&["rev-parse", "HEAD"]));
            assert_eq!(
                env.git(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                "origin/add-feature"
            );
            let origin = env.origin.path().display();
            assert_eq!(
                env.logged("gh.log"),
                format!(
                    "repo view {origin} --json url\n\
                     pr list --head add-feature --base main --state open --json \
                     url,isCrossRepository --repo {origin}\n\
                     pr create --base main --head add-feature --title Add a feature --body-file - \
                     --repo {origin}\n"
                )
            );
            assert_eq!(env.logged("gh.body"), "Adds it.\n\nIn full.");
            let prompt = env.logged("agent.1");
            assert!(prompt.contains(&format!("`git diff {start}`")), "{prompt}");
            assert!(prompt.contains("so give its name"), "{prompt}");
        }

        #[test]
        fn commits_on_a_feature_branch_ship_from_that_branch() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), tip);
            assert_eq!(env.origin("feature"), tip);
            assert!(
                env.logged("gh.log")
                    .contains("pr create --base main --head feature")
            );
            assert!(env.logged("agent.1").contains("branch is not used"));
        }

        #[test]
        fn nothing_to_ship_fails_before_asking_the_agent() {
            let env = Env::new();
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(stderr.contains("there is nothing to ship"), "{stderr}");
            assert!(stderr.contains("origin/main"), "{stderr}");
            assert_eq!(env.agent_calls(), 0);
            assert_eq!(env.logged("gh.log"), "");
        }

        #[test]
        fn an_open_pull_request_is_reported_instead_of_opening_another() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.write("more.rs", "\n");
            let open = r#"[{"url": "https://example.com/fork/repo/pull/6", "isCrossRepository": true},
                {"url": "https://example.com/owner/repo/pull/7", "isCrossRepository": false}]"#;
            let stdout = env.succeed(&["code", "ship"], &[("FAKE_GH_OPEN", open)]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/7\n");
            assert!(!env.logged("gh.log").contains("pr create"));
            assert_eq!(env.origin("feature"), env.git(&["rev-parse", "HEAD"]));
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn an_open_pull_request_from_a_fork_is_not_reported() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            let open =
                r#"[{"url": "https://example.com/fork/repo/pull/6", "isCrossRepository": true}]"#;
            let stdout = env.succeed(&["code", "ship"], &[("FAKE_GH_OPEN", open)]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/1\n");
            assert!(env.logged("gh.log").contains("pr create"));
        }

        #[test]
        fn gh_that_cannot_reach_the_repository_fails_before_the_agent() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_GH_UNAUTHENTICATED", "1")]);
            assert!(stderr.contains("gh auth login required"), "{stderr}");
            assert_eq!(env.agent_calls(), 0);
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
        }

        fn unusable(shipment: &str, expected: &str) {
            unusable_in(Env::new(), shipment, expected);
        }

        fn unusable_in(env: Env, shipment: &str, expected: &str) {
            let start = env.git(&["rev-parse", "HEAD"]);
            env.write("feature.rs", "\n");
            let shipment =
                format!(r#"{{"completed": true, "report": "", "body": "", {shipment}}}"#);
            let stderr = env.fail(&["code", "ship"], &[("FAKE_SHIPMENT", &shipment)]);
            assert!(
                stderr.contains(&format!(
                    "claude gave an answer that cannot be shipped: {expected}"
                )),
                "{stderr}"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), start);
            assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
            assert!(!env.logged("gh.log").contains("pr "));
        }

        #[test]
        fn an_empty_commit_message_changes_nothing() {
            unusable(
                r#""branch": "b", "commit_message": " \n", "title": "T""#,
                "the commit message is empty",
            );
        }

        #[test]
        fn an_empty_title_changes_nothing() {
            unusable(
                r#""branch": "b", "commit_message": "M.", "title": " ""#,
                "the title is empty",
            );
        }

        #[test]
        fn an_invalid_branch_name_changes_nothing() {
            unusable(
                r#""branch": "add feature", "commit_message": "M.", "title": "T""#,
                "`add feature` is not a valid branch name",
            );
            unusable(
                r#""branch": "@{-1}", "commit_message": "M.", "title": "T""#,
                "`@{-1}` is not a valid branch name",
            );
        }

        #[test]
        fn a_branch_name_already_on_the_remote_changes_nothing() {
            unusable(
                r#""branch": "main", "commit_message": "M.", "title": "T""#,
                "`main` is already a branch on origin",
            );
        }

        #[test]
        fn a_branch_changed_on_the_remote_since_fetching_is_refused_before_the_agent() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feat"]);
            env.write("feature.rs", "\n");
            let start = env.commit("Feature.");
            env.git(&["push", "--quiet", "--set-upstream", "origin", "feat"]);
            let main = env.origin("main");
            git(env.origin.path(), &["update-ref", "refs/heads/feat", &main]);
            env.write("more.rs", "\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("feat changed on origin since it was last fetched"),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), start);
        }

        #[test]
        fn a_branch_deleted_on_the_remote_is_pushed_again() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feat"]);
            env.write("feature.rs", "\n");
            env.commit("Feature.");
            env.git(&["push", "--quiet", "--set-upstream", "origin", "feat"]);
            git(
                env.origin.path(),
                &["branch", "--quiet", "--delete", "--force", "feat"],
            );
            env.write("more.rs", "\n");
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(env.origin("feat"), env.git(&["rev-parse", "HEAD"]));
        }

        #[test]
        fn shipping_again_with_nothing_new_skips_the_agent() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            let open =
                r#"[{"url": "https://example.com/owner/repo/pull/7", "isCrossRepository": false}]"#;
            env.succeed(&["code", "ship"], &[("FAKE_GH_OPEN", open)]);
            let stdout = env.succeed(&["code", "ship"], &[("FAKE_GH_OPEN", open)]);
            assert_eq!(stdout, "https://example.com/owner/repo/pull/7\n");
            assert_eq!(env.agent_calls(), 1);
        }

        #[test]
        fn an_ssh_host_alias_is_resolved_for_gh() {
            let env = Env::new();
            let root = TempDir::new().unwrap();
            fs::create_dir(root.path().join("owner")).unwrap();
            std::os::unix::fs::symlink(env.origin.path(), root.path().join("owner/repo.git"))
                .unwrap();
            script(env.bin.path(), "ssh", FAKE_SSH);
            env.git(&["remote", "set-url", "origin", "git@work:owner/repo.git"]);
            env.write("feature.rs", "\n");
            env.succeed(
                &["code", "ship"],
                &[("FAKE_SSH_ROOT", root.path().to_str().unwrap())],
            );
            let calls = env.logged("gh.log");
            assert!(
                calls
                    .lines()
                    .all(|call| call.contains("github.com/owner/repo ")
                        || call.ends_with("github.com/owner/repo")),
                "{calls}"
            );
        }

        #[test]
        fn a_remote_with_two_urls_is_refused_before_the_agent() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            env.git(&["remote", "set-url", "--add", "origin", "/elsewhere"]);
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("the remote origin has more than one URL"),
                "{stderr}"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
        }

        #[test]
        fn a_branch_name_already_local_changes_nothing() {
            let env = Env::new();
            env.git(&["branch", "leftover"]);
            unusable_in(
                env,
                r#""branch": "leftover", "commit_message": "M.", "title": "T""#,
                "`leftover` is already a local branch",
            );
        }

        #[test]
        fn a_remote_that_pushes_elsewhere_is_refused_before_the_agent() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            env.git(&["remote", "set-url", "--push", "origin", "/elsewhere"]);
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("or pushes to another URL than it fetches from"),
                "{stderr}"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
        }

        #[test]
        fn an_agent_that_changes_the_work_while_describing_it_ships_nothing() {
            let env = Env::new();
            env.write("feature.rs", "\n");
            let stderr = env.fail(
                &["code", "ship"],
                &[("FAKE_DESCRIBE", "printf 'x\\n' > scratch.md")],
            );
            assert!(stderr.contains("claude changed the work in"), "{stderr}");
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert_eq!(env.git(&["log", "--format=%s"]), "Start.");
            assert!(!env.logged("gh.log").contains("pr "));
        }

        #[test]
        fn a_branch_that_tracks_its_base_ships_from_that_branch() {
            let env = Env::new();
            let start = env.origin("main");
            let tip = env.feature(&["feature\n"]);
            env.git(&["branch", "--quiet", "--set-upstream-to", "origin/main"]);
            env.git(&["config", "push.default", "upstream"]);
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(env.origin("feature"), tip);
            assert_eq!(env.origin("main"), start);
            assert_eq!(
                env.git(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                "origin/feature"
            );
        }

        #[test]
        fn a_branch_that_tracks_another_name_is_refused_before_the_agent() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.git(&[
                "push",
                "--quiet",
                "--set-upstream",
                "origin",
                "feature:luigi/fix",
            ]);
            env.write("more.rs", "\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains(
                    "feature tracks origin/luigi/fix, but ncli would push it to origin/feature"
                ),
                "{stderr}"
            );
            assert_eq!(env.agent_calls(), 0);
            assert_eq!(env.origin("luigi/fix"), tip);
            assert_eq!(env.git(&["rev-parse", "HEAD"]), tip);
            assert!(!env.logged("gh.log").contains("pr "));
        }

        #[test]
        fn into_needs_no_default_branch() {
            let env = Env::new();
            env.git(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
            env.write("feature.rs", "\n");
            env.succeed(&["code", "ship", "--into", "main"], &[]);
            assert!(
                env.logged("gh.log")
                    .contains("pr create --base main --head add-feature")
            );
        }

        #[test]
        fn without_a_default_branch_the_current_branch_is_not_shipped_from() {
            let env = Env::new();
            env.git(&["push", "--quiet", "origin", "main:release"]);
            env.git(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
            let start = env.git(&["rev-parse", "HEAD"]);
            env.write("feature.rs", "\n");
            env.succeed(&["code", "ship", "--into", "release"], &[]);
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.git(&["rev-parse", "main"]), start);
            assert_eq!(env.origin("main"), start);
            assert_eq!(env.origin("add-feature"), env.git(&["rev-parse", "HEAD"]));
            assert!(
                env.logged("gh.log")
                    .contains("pr create --base release --head add-feature")
            );
            assert!(env.logged("agent.1").contains("so give its name"));
        }

        #[test]
        fn into_opens_the_pull_request_against_another_branch() {
            let env = Env::new();
            env.git(&["push", "--quiet", "origin", "main:release"]);
            env.write("feature.rs", "\n");
            env.succeed(&["code", "ship", "--into", "release"], &[]);
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert!(
                env.logged("gh.log")
                    .contains("pr create --base release --head add-feature")
            );
        }

        #[test]
        fn gh_works_on_the_remote_the_branch_is_pushed_to() {
            let env = Env::new();
            let other = TempDir::new().unwrap();
            git(other.path(), &["init", "--quiet", "--bare"]);
            env.git(&["remote", "add", "upstream", other.path().to_str().unwrap()]);
            env.write("feature.rs", "\n");
            env.succeed(&["code", "ship"], &[]);
            let origin = env.origin.path().to_str().unwrap();
            let calls = env.logged("gh.log");
            assert_eq!(calls.lines().count(), 3, "{calls}");
            assert!(calls.lines().all(|call| call.contains(origin)), "{calls}");
        }
    }

    mod sync {
        use super::*;

        #[test]
        fn without_conflicts_the_branch_is_rebased_without_the_agent() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.push_upstream("main", "other.md", "other\n");
            let stdout = env.succeed(&["code", "sync"], &[]);
            assert_eq!(stdout, "Rebased feature onto origin/main.\n");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("main"));
            assert_eq!(env.agent_calls(), 0);
        }

        #[test]
        fn a_conflict_is_resolved_by_the_agent_and_the_rebase_finishes() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let stdout = env.succeed(&["code", "sync"], &[]);
            assert_eq!(
                stdout,
                "Rebased feature onto origin/main, resolving conflicts in 1 commit.\n"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("main"));
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Feature 0.");
            assert_eq!(env.read("README.md"), "resolved 1\n");
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            let prompt = env.logged("agent.1");
            assert!(
                prompt.contains("exactly these files: README.md."),
                "{prompt}"
            );
        }

        #[test]
        fn each_conflicting_commit_is_resolved_in_turn() {
            let env = Env::new();
            env.feature(&["one\n", "two\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let stdout = env.succeed(&["code", "sync"], &[]);
            assert!(
                stdout.contains("resolving conflicts in 2 commits"),
                "{stdout}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD~2"]), env.origin("main"));
            assert_eq!(env.read("README.md"), "resolved 2\n");
        }

        #[test]
        fn commits_that_become_empty_are_dropped_as_git_drops_them() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("same.md", "same\n");
            env.commit("Already upstream.");
            env.write("README.md", "feature\n");
            env.commit("Resolved away.");
            env.write("kept.md", "kept\n");
            env.commit("Kept.");
            let clone = TempDir::new().unwrap();
            git(
                clone.path(),
                &["clone", "--quiet", env.origin.path().to_str().unwrap(), "."],
            );
            fs::write(clone.path().join("same.md"), "same\n").unwrap();
            fs::write(clone.path().join("README.md"), "upstream\n").unwrap();
            git(clone.path(), &["add", "--all"]);
            git(
                clone.path(),
                &["commit", "--quiet", "--message", "Upstream."],
            );
            git(clone.path(), &["push", "--quiet", "origin", "main"]);
            let ours = "git checkout --ours README.md && git add README.md";
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", ours)]);
            assert_eq!(
                env.git(&["log", "--format=%s", "origin/main..HEAD"]),
                "Kept."
            );
            assert_eq!(env.read("README.md"), "upstream\n");
            assert_eq!(env.agent_calls(), 1);
        }

        fn fails_and_puts_back(resolve: &str, expected: &str) -> Env {
            fails_and_puts_back_in(Env::new(), resolve, expected)
        }

        fn fails_and_puts_back_in(env: Env, resolve: &str, expected: &str) -> Env {
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(stderr.contains("so it was put back at"), "{stderr}");
            assert!(stderr.contains(expected), "{stderr}");
            env.assert_put_back(&tip);
            env
        }

        #[test]
        fn an_agent_that_creates_another_file_is_undone() {
            let env = fails_and_puts_back(
                "printf 'x\\n' > README.md && git add README.md && mkdir new && printf 'x\\n' > new/other.md",
                "changed more than the staged resolution of the conflicted files (new/other.md)",
            );
            assert_eq!(env.git(&["status", "--porcelain"]), "");
        }

        #[test]
        fn an_agent_that_creates_a_file_a_later_commit_adds_is_undone() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.write("later.md", "later\n");
            let tip = env.commit("Later.");
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve =
                "printf 'x\\n' > README.md && git add README.md && printf 'x\\n' > later.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(stderr.contains("so it was put back at"), "{stderr}");
            assert!(stderr.contains("conflicted files (later.md)"), "{stderr}");
            env.assert_put_back(&tip);
            assert_eq!(env.read("later.md"), "later\n");
        }

        #[test]
        fn an_agent_that_hides_a_change_to_another_file_is_undone() {
            let env = Env::new();
            env.write("other.md", "other\n");
            env.commit("Other.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'x\\n' > README.md && git add README.md && \
                 git update-index --skip-worktree other.md && printf 'x\\n' > other.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(stderr.contains("conflicted files (other.md)"), "{stderr}");
            env.assert_put_back(&tip);
            assert_eq!(env.read("other.md"), "other\n");
            assert_eq!(env.git(&["ls-files", "-v", "other.md"]), "H other.md");
        }

        const HIJACK: &str = "printf 'HIJACKED\\n' > other.md";

        fn hidden_edit_is_undone(flag: &str, listed: &str, edit: &str) {
            let env = Env::new();
            env.write("other.md", "other\n");
            env.commit("Other.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["update-index", flag, "other.md"]);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", edit)]);
            assert!(stderr.contains("unmerged paths (README.md)"), "{stderr}");
            env.assert_put_back(&tip);
            assert_eq!(env.read("other.md"), "other\n");
            assert_eq!(env.git(&["ls-files", "-v", "other.md"]), listed);
        }

        #[test]
        fn an_agent_that_edits_an_assume_unchanged_file_is_undone() {
            hidden_edit_is_undone("--assume-unchanged", "h other.md", HIJACK);
        }

        #[test]
        fn an_agent_that_edits_a_skip_worktree_file_is_undone() {
            hidden_edit_is_undone("--skip-worktree", "S other.md", HIJACK);
        }

        #[test]
        fn an_agent_that_deletes_a_skip_worktree_file_is_undone() {
            hidden_edit_is_undone("--skip-worktree", "S other.md", "rm other.md");
        }

        #[test]
        fn hidden_bits_the_rebase_or_the_agent_clears_are_put_back() {
            let env = Env::new();
            env.write("other.md", "other\n");
            env.write("cfg.md", "cfg\n");
            env.commit("Other.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "other.md", "upstream\n");
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["update-index", "--assume-unchanged", "other.md"]);
            env.git(&["update-index", "--skip-worktree", "cfg.md"]);
            let resolve = "git update-index --no-skip-worktree cfg.md";
            env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            env.assert_put_back(&tip);
            assert_eq!(
                env.git(&["ls-files", "-v", "other.md", "cfg.md"]),
                "S cfg.md\nh other.md"
            );
        }

        fn left_out_by_sparse_checkout() -> (Env, String) {
            let env = Env::new();
            env.write("other.md", "other\n");
            env.commit("Other.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["update-index", "--skip-worktree", "other.md"]);
            fs::remove_file(env.work.path().join("other.md")).unwrap();
            (env, tip)
        }

        #[test]
        fn a_skip_worktree_file_left_out_of_the_checkout_stays_out() {
            let (env, _) = left_out_by_sparse_checkout();
            env.succeed(&["code", "sync"], &[]);
            assert!(!env.work.path().join("other.md").exists());
            assert_eq!(env.git(&["ls-files", "-v", "other.md"]), "S other.md");
        }

        #[test]
        fn a_file_left_out_of_the_checkout_that_the_agent_creates_is_removed() {
            let (env, tip) = left_out_by_sparse_checkout();
            env.fail(&["code", "sync"], &[("FAKE_RESOLVE", HIJACK)]);
            env.assert_put_back(&tip);
            assert!(!env.work.path().join("other.md").exists());
            assert_eq!(env.git(&["ls-files", "-v", "other.md"]), "S other.md");
        }

        #[test]
        fn an_agent_that_hides_the_resolution_is_undone() {
            fails_and_puts_back(
                "printf 'x\\n' > README.md && git add README.md && \
                 git update-index --assume-unchanged README.md",
                "conflicted files (README.md)",
            );
        }

        #[test]
        fn an_agent_that_edits_the_rest_of_the_rebase_is_undone() {
            let env = Env::new();
            let tip = env.feature(&["one\n", "two\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'x\\n' > README.md && git add README.md && \
                 : > \"$(git rev-parse --git-path rebase-merge/git-rebase-todo)\"";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(
                stderr.contains("the agent changed the rebase itself"),
                "{stderr}"
            );
            env.assert_put_back(&tip);
        }

        #[test]
        fn an_agent_that_rewrites_the_commit_message_is_undone() {
            fails_and_puts_back(
                "printf 'x\\n' > README.md && git add README.md && \
                 echo rewritten > \"$(git rev-parse --git-path rebase-merge/message)\"",
                "the agent changed the rebase itself",
            );
        }

        #[test]
        fn an_agent_that_edits_another_tracked_file_is_undone() {
            let env = Env::new();
            env.write("other.md", "other\n");
            env.commit("Other.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve =
                "printf 'x\\n' > README.md && git add README.md && printf 'x\\n' > other.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(stderr.contains("conflicted files (other.md)"), "{stderr}");
            env.assert_put_back(&tip);
            assert_eq!(env.read("other.md"), "other\n");
        }

        #[test]
        fn an_agent_that_stages_another_file_is_undone() {
            fails_and_puts_back(
                "printf 'x\\n' > README.md && printf 'x\\n' > other.md && git add README.md other.md",
                "(other.md)",
            );
        }

        #[test]
        fn an_agent_that_continues_the_rebase_itself_is_undone() {
            fails_and_puts_back(
                "printf 'x\\n' > README.md && git add README.md && GIT_EDITOR=true git rebase --continue",
                "the agent changed the rebase itself",
            );
        }

        #[test]
        fn an_agent_that_leaves_markers_is_undone() {
            fails_and_puts_back(
                "git add README.md",
                "the agent left conflict markers in README.md",
            );
        }

        #[test]
        fn an_agent_that_leaves_markers_of_a_configured_size_is_undone() {
            let env = Env::new();
            env.write(".gitattributes", "README.md conflict-marker-size=5\n");
            env.commit("Attributes.");
            env.git(&["push", "--quiet", "origin", "main"]);
            fails_and_puts_back_in(
                env,
                "git add README.md",
                "the agent left conflict markers in README.md",
            );
        }

        #[test]
        fn markers_are_checked_at_the_size_git_wrote_them() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write(".gitattributes", "README.md text\n");
            env.write("README.md", "feature\n");
            let tip = env.commit("Feature.");
            env.push_upstream("main", ".gitattributes", "README.md -text\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'README.md conflict-marker-size=10\\n' > .gitattributes && \
                 git add .gitattributes README.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(
                stderr.contains("the agent left conflict markers in README.md"),
                "{stderr}"
            );
            env.assert_put_back(&tip);
        }

        #[test]
        fn a_marker_size_of_zero_counts_as_the_default() {
            let env = Env::new();
            env.write(".gitattributes", "README.md conflict-marker-size=0\n");
            env.commit("Attributes.");
            env.git(&["push", "--quiet", "origin", "main"]);
            env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'resolved\\n' > README.md && git add README.md";
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
        }

        #[test]
        fn an_agent_that_leaves_unmerged_paths_is_undone() {
            fails_and_puts_back("true", "the agent left unmerged paths (README.md)");
        }

        #[test]
        fn a_stop_that_continuing_cannot_get_past_is_put_back() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "other.md", "other\n");
            env.git(&["config", "commit.gpgsign", "true"]);
            env.git(&["config", "gpg.program", "false"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("could not get past it")
                    && stderr.contains("gpg failed to sign the data"),
                "{stderr}"
            );
            env.assert_put_back(&tip);
            assert_eq!(env.agent_calls(), 0);
        }

        #[test]
        fn markers_already_on_a_side_are_kept() {
            let env = Env::new();
            env.feature(&["feature\n=======\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let keep = "printf 'both\\n=======\\n' > README.md && git add README.md";
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", keep)]);
            assert_eq!(env.read("README.md"), "both\n=======\n");
        }

        #[test]
        fn markers_on_a_side_of_a_renamed_file_are_kept() {
            let env = Env::new();
            let text = "Text.\n".repeat(10);
            env.write("docs.rst", &format!("Title\n\n{text}"));
            env.commit("Docs.");
            env.git(&["push", "--quiet", "origin", "main"]);
            env.feature(&[]);
            env.write(
                "docs.rst",
                &format!("Feature\n\n{text}\nInstall\n=======\n"),
            );
            env.commit("Feature.");
            let clone = TempDir::new().unwrap();
            git(
                clone.path(),
                &["clone", "--quiet", env.origin.path().to_str().unwrap(), "."],
            );
            git(clone.path(), &["mv", "docs.rst", "guide.rst"]);
            fs::write(
                clone.path().join("guide.rst"),
                format!("Upstream\n\n{text}"),
            )
            .unwrap();
            git(
                clone.path(),
                &["commit", "--quiet", "--all", "--message", "Rename."],
            );
            git(clone.path(), &["push", "--quiet", "origin", "main"]);
            let merged = "printf 'Both\\n\\n%s\\nInstall\\n=======\\n' \"$(git show HEAD:guide.rst | tail -n +3)\" > guide.rst && git add guide.rst";
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", merged)]);
            assert!(env.read("guide.rst").ends_with("\nInstall\n=======\n"));
            assert_eq!(env.agent_calls(), 1);
        }

        #[test]
        fn a_dirty_working_tree_is_refused_before_fetching() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.write("untracked.md", "\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("has uncommitted changes or untracked files (untracked.md)"),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), tip);
            assert_eq!(env.agent_calls(), 0);
        }

        #[test]
        fn an_edit_hidden_from_git_is_refused_before_fetching() {
            let env = Env::new();
            env.write("local.md", "base\n");
            env.commit("Local.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "local.md", "upstream\n");
            env.git(&["update-index", "--assume-unchanged", "local.md"]);
            env.write("local.md", "mine\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("assume-unchanged or skip-worktree have edits")
                    && stderr.contains("(local.md)"),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), tip);
            assert_eq!(env.read("local.md"), "mine\n");
            assert_ne!(env.git(&["rev-parse", "origin/main"]), env.origin("main"));
        }

        #[test]
        fn a_conflict_rerere_resolves_is_continued_without_the_agent() {
            let env = Env::new();
            env.git(&["config", "rerere.enabled", "true"]);
            env.git(&["config", "rerere.autoupdate", "true"]);
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            env.git(&["reset", "--quiet", "--hard", &tip]);
            let stdout = env.succeed(&["code", "sync"], &[]);
            assert_eq!(stdout, "Rebased feature onto origin/main.\n");
            assert_eq!(env.agent_calls(), 1);
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("main"));
            assert_eq!(env.read("README.md"), "resolved 1\n");
        }

        #[test]
        fn a_rebase_in_progress_is_refused() {
            let env = Env::new();
            env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["fetch", "--quiet"]);
            let _ = std::process::Command::new("git")
                .args(["-c", "user.name=ncli", "-c", "user.email=ncli@example.com"])
                .args(["rebase", "--quiet", "origin/main"])
                .current_dir(env.work.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(stderr.contains("a rebase is in progress"), "{stderr}");
            assert_eq!(env.agent_calls(), 0);
        }

        #[test]
        fn a_detached_head_is_refused() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--detach"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(stderr.contains("not a branch to rebase"), "{stderr}");
        }

        #[test]
        fn files_and_repositories_the_agent_creates_are_removed() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'x\\n' > new.md && \
                 git init --quiet ref && printf 'x\\n' > ref/new.md && \
                 printf 'x\\n' > README.md && git add README.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(
                stderr.contains("conflicted files (new.md, ref/)"),
                "{stderr}"
            );
            env.assert_put_back(&tip);
            assert_eq!(
                env.git(&["status", "--porcelain", "--untracked-files=all"]),
                ""
            );
            assert!(!env.work.path().join("ref").exists());
        }

        #[test]
        fn ignored_files_survive_a_failed_sync_that_unignores_them() {
            let env = Env::new();
            env.write(".gitignore", "cache/\n");
            env.commit("Ignore.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            fs::create_dir(env.work.path().join("cache")).unwrap();
            env.write("cache/valuable", "keep\n");
            env.push_upstream("main", ".gitignore", "");
            env.push_upstream("main", "README.md", "upstream\n");
            env.fail(&["code", "sync"], &[("FAKE_RESOLVE", "true")]);
            env.assert_put_back(&tip);
            assert_eq!(
                fs::read_to_string(env.work.path().join("cache/valuable")).unwrap(),
                "keep\n"
            );
        }

        #[test]
        fn a_file_the_agent_leaves_where_the_old_tip_tracks_one_is_put_back() {
            let env = Env::new();
            env.write("old.md", "old\n");
            env.commit("Old.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let tip = env.feature(&["feature\n"]);
            let clone = TempDir::new().unwrap();
            git(
                clone.path(),
                &["clone", "--quiet", env.origin.path().to_str().unwrap(), "."],
            );
            git(clone.path(), &["rm", "--quiet", "old.md"]);
            git(
                clone.path(),
                &["commit", "--quiet", "--message", "Drop old."],
            );
            git(clone.path(), &["push", "--quiet", "origin", "main"]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.fail(
                &["code", "sync"],
                &[("FAKE_RESOLVE", "printf 'agent\\n' > old.md")],
            );
            env.assert_put_back(&tip);
            assert_eq!(
                fs::read_to_string(env.work.path().join("old.md")).unwrap(),
                "old\n"
            );
        }

        #[test]
        fn a_branch_with_a_submodule_is_refused() {
            let env = Env::new();
            let _sub = env.submodule();
            let tip = env.feature(&["feature\n"]);
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("does not support submodules, and feature has one at sub"),
                "{stderr}"
            );
            env.assert_put_back(&tip);
        }

        #[test]
        fn a_target_that_adds_a_submodule_is_refused() {
            let env = Env::new();
            let tip = env.feature(&["feature\n"]);
            let clone = TempDir::new().unwrap();
            git(
                clone.path(),
                &["clone", "--quiet", env.origin.path().to_str().unwrap(), "."],
            );
            let info = format!("160000,{},module", "1".repeat(40));
            git(
                clone.path(),
                &["update-index", "--add", "--cacheinfo", &info],
            );
            git(clone.path(), &["commit", "--quiet", "--message", "Module."]);
            git(clone.path(), &["push", "--quiet", "origin", "main"]);
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(stderr.contains("has one at module"), "{stderr}");
            env.assert_put_back(&tip);
        }

        #[test]
        fn from_needs_no_default_branch() {
            let env = Env::new();
            env.git(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
            env.feature(&["feature\n"]);
            env.push_upstream("main", "other.md", "other\n");
            let stdout = env.succeed(&["code", "sync", "--from", "main"], &[]);
            assert_eq!(stdout, "Rebased feature onto origin/main.\n");
        }

        #[test]
        fn from_rebases_onto_another_branch() {
            let env = Env::new();
            env.git(&["push", "--quiet", "origin", "main:release"]);
            env.feature(&["feature\n"]);
            env.push_upstream("release", "release.md", "release\n");
            let stdout = env.succeed(&["code", "sync", "--from", "release"], &[]);
            assert_eq!(stdout, "Rebased feature onto origin/release.\n");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), env.origin("release"));
        }
    }
}
