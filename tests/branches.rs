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

    const TOOLS: [&str; 7] = ["git", "sh", "cat", "basename", "touch", "rm", "sleep"];

    const FAKE_AGENT: &str = r#"#!/bin/sh
prompt=$(cat)
log="$NCLI_TEST_LOG"
next() {
    count=$(( $(cat "$log/$1.count" 2>/dev/null || echo 0) + 1 ))
    echo "$count" > "$log/$1.count"
    printf '%s' "$prompt" > "$log/$1.$count"
}
answer() {
    echo '{"completed": true, "report": "'"$1"' report '"$count"'"'"$2"'}'
}
case "$prompt" in
*"Write what is needed to open a pull request"*)
    next ship
    eval "${FAKE_SHIP:-true}"
    answer ship ', "commit_message": "'"${FAKE_MESSAGE-Add a feature.}"'", "branch": "'"${FAKE_BRANCH-add-feature}"'", "title": "Add a feature", "body": "The body."' ;;
*"The check \`"*)
    next fix
    eval "${FAKE_FIX:-touch fixed}"
    answer fix ;;
*"stopped with conflicts"*)
    next resolve
    eval "${FAKE_RESOLVE:-printf 'resolved\n' > README.md}"
    answer resolve ', "kept": ['"${FAKE_KEPT}"']' ;;
esac
"#;

    const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$NCLI_TEST_LOG/gh.log"
case "$1 $2" in
"auth status") [ -z "$FAKE_GH_LOGGED_OUT" ] || { echo "not logged in" >&2; exit 1; } ;;
"pr list") echo "${FAKE_GH_PRS:-[]}" ;;
"pr create") echo "Creating pull request"; echo "https://example.com/pr/1" ;;
esac
"#;

    const FAKE_SSH: &str = r#"#!/bin/sh
if [ "$1" = "-G" ]; then
    printf '%s\n' "$*" >> "$NCLI_TEST_LOG/ssh.log"
    echo "user git"
    echo "hostname github.com"
    exit
fi
for command; do :; done
cd "$FAKE_SSH_ROOT" && eval "git ${command#git-}"
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

        #[test]
        fn uncommitted_changes_on_the_default_branch_become_a_new_pull_request() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.write("feature.rs", "fn f() {}\n");
            let (stdout, _) = env.succeed(&["code", "ship"], &[]);
            assert_eq!(stdout, "https://example.com/pr/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Add a feature.");
            assert_eq!(env.git(&["rev-parse", "HEAD~1"]), start);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert_eq!(env.git(&["rev-parse", "main"]), start);
            assert_eq!(
                env.origin_ref("refs/heads/add-feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
            assert_eq!(
                env.git(&["rev-parse", "--abbrev-ref", "add-feature@{upstream}"]),
                "origin/add-feature"
            );
            let calls = env.gh_calls();
            assert!(
                calls.contains(&format!(
                    "pr create --base main --head add-feature --title Add a feature --body The \
                     body. --repo {}",
                    env.origin.path().display()
                )),
                "{calls:?}"
            );
            let prompt = env.logged("ship.1").unwrap();
            assert!(prompt.contains("against origin/main"), "{prompt}");
            assert!(prompt.contains("the uncommitted changes in "), "{prompt}");
            assert!(
                prompt.contains(
                    "Do not change any file, do not commit, do not push, and do not run gh"
                ),
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
            let (stdout, _) = env.succeed(&["code", "ship", "--into", "develop"], &[]);
            assert_eq!(stdout, "https://example.com/pr/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
            assert_eq!(env.origin_ref("refs/heads/feature"), Some(head));
            let calls = env.gh_calls();
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("pr create --base develop --head feature ")),
                "{calls:?}"
            );
            let prompt = env.logged("ship.1").unwrap();
            assert!(
                prompt.contains("since it branched from origin/develop"),
                "{prompt}"
            );
            assert!(
                prompt.contains("leave the commit message empty"),
                "{prompt}"
            );
        }

        #[test]
        fn a_branch_with_an_open_pull_request_is_pushed_and_reported() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            let prs = (
                "FAKE_GH_PRS",
                r#"[{"url": "https://example.com/pr/7", "isCrossRepository": false}]"#,
            );
            let (stdout, _) = env.succeed(&["code", "ship"], &[prs]);
            assert_eq!(stdout, "https://example.com/pr/7\n");
            assert_eq!(
                env.origin_ref("refs/heads/feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
            let calls = env.gh_calls();
            assert!(
                calls.contains(&format!(
                    "pr list --head feature --base main --state open --json \
                     url,isCrossRepository --repo {}",
                    env.origin.path().display()
                )),
                "{calls:?}"
            );
            assert!(
                !calls.iter().any(|call| call.starts_with("pr create")),
                "{calls:?}"
            );
        }

        #[test]
        fn an_open_pull_request_from_another_repository_is_not_reused() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            let prs = (
                "FAKE_GH_PRS",
                r#"[{"url": "https://example.com/pr/7", "isCrossRepository": true}]"#,
            );
            let (stdout, _) = env.succeed(&["code", "ship"], &[prs]);
            assert_eq!(stdout, "https://example.com/pr/1\n");
            let calls = env.gh_calls();
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("pr create --base main --head feature ")),
                "{calls:?}"
            );
        }

        #[test]
        fn gh_is_pointed_at_the_repository_pushed_to() {
            let env = Env::new();
            let upstream = TempDir::new().unwrap();
            git(upstream.path(), &["init", "--quiet", "--bare"]);
            env.git(&[
                "remote",
                "add",
                "upstream",
                upstream.path().to_str().unwrap(),
            ]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            let repo = format!("--repo {}", env.origin.path().display());
            let calls = env.gh_calls();
            let pr_calls: Vec<_> = calls
                .iter()
                .filter(|call| call.starts_with("pr "))
                .collect();
            assert_eq!(pr_calls.len(), 2, "{calls:?}");
            assert!(
                pr_calls.iter().all(|call| call.ends_with(&repo)),
                "{calls:?}"
            );
        }

        #[test]
        fn gh_is_given_the_host_an_ssh_alias_names() {
            let env = Env::new();
            script(env.bin.path(), "ssh", FAKE_SSH);
            let root = TempDir::new().unwrap();
            let widgets = root.path().join("acme/widgets.git");
            git(
                env.work.path(),
                &[
                    "clone",
                    "--quiet",
                    "--bare",
                    env.origin.path().to_str().unwrap(),
                    widgets.to_str().unwrap(),
                ],
            );
            env.git(&["remote", "set-url", "origin", "work:acme/widgets.git"]);
            env.write("feature.rs", "fn f() {}\n");
            let ssh_root = ("FAKE_SSH_ROOT", root.path().to_str().unwrap());
            env.succeed(&["code", "ship"], &[ssh_root]);
            let calls = env.gh_calls();
            let pr_calls: Vec<_> = calls
                .iter()
                .filter(|call| call.starts_with("pr "))
                .collect();
            assert_eq!(pr_calls.len(), 2, "{calls:?}");
            assert!(
                pr_calls
                    .iter()
                    .all(|call| call.ends_with("--repo github.com/acme/widgets")),
                "{calls:?}"
            );
            assert_eq!(calls[0], "auth status --hostname github.com", "{calls:?}");
            assert_eq!(env.logged("ssh.log").unwrap(), "-G -- work\n-G -- work\n");
        }

        #[test]
        fn a_remote_that_pushes_elsewhere_is_refused_before_committing() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            let fork = TempDir::new().unwrap();
            git(fork.path(), &["init", "--quiet", "--bare"]);
            env.git(&[
                "remote",
                "set-url",
                "--push",
                "origin",
                fork.path().to_str().unwrap(),
            ]);
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(stderr.contains("but pushes to"), "{stderr}");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), start);
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert!(env.logged("ship.1").is_none());
            assert!(
                !env.gh_calls().iter().any(|call| call.starts_with("pr ")),
                "{:?}",
                env.gh_calls()
            );
        }

        #[test]
        fn a_remote_with_any_push_url_elsewhere_is_refused_before_committing() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            let fork = TempDir::new().unwrap();
            git(fork.path(), &["init", "--quiet", "--bare"]);
            let origin = env.origin.path().to_str().unwrap();
            env.git(&["remote", "set-url", "--add", "--push", "origin", origin]);
            let fork_path = fork.path().to_str().unwrap();
            env.git(&["remote", "set-url", "--add", "--push", "origin", fork_path]);
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(stderr.contains("but pushes to"), "{stderr}");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), start);
            assert!(git(fork.path(), &["for-each-ref"]).is_empty());
        }

        #[test]
        fn the_default_branch_is_not_pushed_when_shipping_into_another_branch() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.push_upstream("develop", "dev.txt", "dev\n");
            env.write("feature.rs", "fn f() {}\n");
            let (stdout, _) = env.succeed(&["code", "ship", "--into", "develop"], &[]);
            assert_eq!(stdout, "https://example.com/pr/1\n");
            assert_eq!(env.git(&["branch", "--show-current"]), "add-feature");
            assert_eq!(env.origin_ref("refs/heads/main"), Some(start));
            let calls = env.gh_calls();
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("pr create --base develop --head add-feature ")),
                "{calls:?}"
            );
        }

        #[test]
        fn a_shipped_branch_rebased_by_sync_is_shipped_again() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            env.write("more.rs", "fn g() {}\n");
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(
                env.origin_ref("refs/heads/feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
        }

        #[test]
        fn a_rebased_branch_is_not_pushed_over_commits_it_has_not_seen() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            let theirs = env.push_upstream("feature", "theirs.rs", "theirs\n");
            env.push_upstream("main", "README.md", "upstream\n");
            env.succeed(&["code", "sync"], &[]);
            env.write("more.rs", "fn g() {}\n");
            env.fail(&["code", "ship"], &[]);
            assert_eq!(env.origin_ref("refs/heads/feature"), Some(theirs));
        }

        #[test]
        fn a_fetched_commit_the_branch_never_had_is_not_pushed_over() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            let theirs = env.push_upstream("feature", "theirs.rs", "theirs\n");
            env.git(&["fetch", "--quiet", "origin"]);
            env.write("more.rs", "fn g() {}\n");
            env.fail(&["code", "ship"], &[]);
            assert_eq!(env.origin_ref("refs/heads/feature"), Some(theirs));
        }

        #[test]
        fn a_branch_deleted_on_the_remote_is_pushed_again() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            git(
                env.origin.path(),
                &["branch", "--quiet", "--delete", "--force", "feature"],
            );
            env.write("more.rs", "fn g() {}\n");
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(
                env.origin_ref("refs/heads/feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
        }

        #[test]
        fn a_tag_named_like_the_branch_does_not_block_the_push() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.git(&["tag", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            env.succeed(&["code", "ship"], &[]);
            assert_eq!(
                env.origin_ref("refs/heads/feature"),
                Some(env.git(&["rev-parse", "HEAD"]))
            );
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
            assert_eq!(env.count("ship"), 0);
            assert!(env.gh_calls().is_empty());
        }

        #[test]
        fn an_into_branch_not_yet_fetched_is_found_on_the_remote() {
            let env = Env::new();
            env.push_upstream("develop", "dev.txt", "dev\n");
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("feature.rs", "fn f() {}\n");
            let (stdout, _) = env.succeed(&["code", "ship", "--into", "develop"], &[]);
            assert_eq!(stdout, "https://example.com/pr/1\n");
            let calls = env.gh_calls();
            assert!(
                calls
                    .iter()
                    .any(|call| call.starts_with("pr create --base develop --head feature ")),
                "{calls:?}"
            );
        }

        #[test]
        fn a_branch_name_taken_on_the_remote_changes_nothing() {
            let env = Env::new();
            let start = env.git(&["rev-parse", "HEAD"]);
            env.push_upstream("add-feature", "other.txt", "other\n");
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains(
                    "error: claude wrote an unusable pull request: origin already has a branch \
                     named add-feature\nResume: claude --resume "
                ),
                "{stderr}"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert_eq!(env.git(&["rev-parse", "HEAD"]), start);
            assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
        }

        #[test]
        fn nothing_to_ship_is_rejected_up_front() {
            let env = Env::new();
            let stderr = env.fail(&["code", "ship"], &[]);
            assert!(
                stderr.contains("error: there is nothing to ship in ",)
                    && stderr.contains(
                        ": no uncommitted changes and no commits that are not on origin/main"
                    ),
                "{stderr}"
            );
            assert_eq!(env.count("ship"), 0);
        }

        fn assert_nothing_shipped(env: &Env, head: &str) {
            assert_eq!(env.count("ship"), 0);
            assert_eq!(env.git(&["rev-parse", "HEAD"]), head);
            assert_eq!(env.origin_ref("refs/heads/feature"), None);
            assert!(
                !env.gh_calls().iter().any(|call| call.starts_with("pr ")),
                "{:?}",
                env.gh_calls()
            );
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
            assert_nothing_shipped(&env, &head);
        }

        #[test]
        fn unresolved_conflicts_are_rejected_up_front() {
            let env = Env::new();
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write("README.md", "stashed\n");
            env.git(&["stash", "--quiet"]);
            env.write("README.md", "feature\n");
            let head = env.commit("Feature.");
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
            assert_nothing_shipped(&env, &head);
        }

        #[test]
        fn a_logged_out_gh_is_rejected_up_front() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_GH_LOGGED_OUT", "1")]);
            assert!(
                stderr.contains("error: gh is not logged in; run `gh auth login`"),
                "{stderr}"
            );
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
            assert!(env.gh_calls().is_empty());
        }

        #[test]
        fn an_unusable_branch_name_changes_nothing_and_can_be_resumed() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let stderr = env.fail(&["code", "ship"], &[("FAKE_BRANCH", "bad..name")]);
            assert!(
                stderr.contains(
                    "error: claude wrote an unusable pull request: `bad..name` is not a valid \
                     branch name\nResume: claude --resume "
                ),
                "{stderr}"
            );
            assert_eq!(env.git(&["branch", "--show-current"]), "main");
            assert_eq!(env.git(&["status", "--porcelain"]), "?? feature.rs");
            assert_eq!(env.gh_calls(), ["auth status"]);
        }

        #[test]
        fn an_agent_that_edits_files_ships_nothing() {
            let env = Env::new();
            env.write("feature.rs", "fn f() {}\n");
            let edit = ("FAKE_SHIP", "echo more >> feature.rs");
            let stderr = env.fail(&["code", "ship"], &[edit]);
            assert!(
                stderr.contains("error: claude changed the files in ")
                    && stderr.contains("nothing was committed or pushed"),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-list", "--count", "HEAD"]), "1");
            assert_eq!(env.origin_ref("refs/heads/add-feature"), None);
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
                "`sh -c echo CHECK OUTPUT; test -f fixed` passes; there was nothing to fix.\n"
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
        fn a_check_that_still_fails_after_the_last_round_is_an_error() {
            let env = Env::new();
            let fix = ("FAKE_FIX", "echo more >> notes.txt");
            let stderr = env.fail(&green(&["--max-rounds", "2"]), &[fix]);
            assert!(
                stderr.contains(
                    "error: `sh -c echo CHECK OUTPUT; test -f fixed` still fails after 2 round(s) \
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
                    && stderr.contains(" while `sh -c git commit --allow-empty -qm x` ran;"),
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

        fn feature(env: &Env, path: &str, contents: &str) -> String {
            env.git(&["switch", "--quiet", "--create", "feature"]);
            env.write(path, contents);
            env.commit("Feature.")
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
        fn conflicts_are_resolved_by_the_agent_and_the_rebase_continues() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            let upstream = env.push_upstream("main", "README.md", "upstream\n");
            let (stdout, _) = env.succeed(&["code", "sync"], &[]);
            assert!(
                stdout.starts_with("Rebased feature onto origin/main, "),
                "{stdout}"
            );
            assert!(
                stdout.contains("\n\nResolved the conflicts in README.md while applying ")
                    && stdout.contains(" Feature.:\n\nresolve report 1\n\nResume: "),
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
                prompt.contains("these files by editing them: README.md."),
                "{prompt}"
            );
            assert!(prompt.contains(" Feature."), "{prompt}");
            assert!(
                prompt.contains("Do not stage, commit, or run git rebase"),
                "{prompt}"
            );
        }

        #[test]
        fn conflicts_outside_the_sparse_checkout_are_resolved() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            env.git(&["sparse-checkout", "set", "--no-cone", "/feature.rs"]);
            env.succeed(&["code", "sync"], &[]);
            assert_eq!(env.git(&["show", "HEAD:README.md"]), "resolved");
            assert_eq!(env.git(&["log", "-1", "--format=%s"]), "Feature.");
            assert!(!env.rebasing());
        }

        #[cfg(unix)]
        #[test]
        fn conflicts_in_files_named_like_pathspecs_are_resolved() {
            let env = Env::new();
            feature(&env, ":foo", "feature\n");
            env.push_upstream("main", ":foo", "upstream\n");
            let resolve = ("FAKE_RESOLVE", "printf 'resolved\\n' > ./:foo");
            env.succeed(&["code", "sync"], &[resolve]);
            assert_eq!(
                fs::read_to_string(env.work.path().join(":foo")).unwrap(),
                "resolved\n"
            );
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
        }

        #[test]
        fn unresolved_conflicts_abort_the_rebase() {
            let env = Env::new();
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", "true")]);
            let short = env.git(&["rev-parse", "--short", &old]);
            assert!(
                stderr.contains(&format!(
                    "error: the conflicts in README.md were left unresolved while applying \
                     {short} Feature., so the rebase was aborted: resolve report 1\nResume: \
                     claude --resume "
                )),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
        }

        fn assert_restored(env: &Env, old: &str) {
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
            assert_eq!(env.git(&["branch", "--show-current"]), "feature");
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
        }

        fn delete_upstream_readme(env: &Env) {
            env.change_upstream("main", |clone| {
                fs::remove_file(clone.join("README.md")).unwrap()
            });
        }

        #[test]
        fn a_conflict_without_markers_left_as_it_was_aborts_the_rebase() {
            let env = Env::new();
            let old = feature(&env, "README.md", "feature\n");
            delete_upstream_readme(&env);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", "true")]);
            assert!(
                stderr.contains("error: the conflicts in README.md were left unresolved"),
                "{stderr}"
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn a_conflict_without_markers_is_resolved_by_changing_the_file() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            delete_upstream_readme(&env);
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", "rm README.md")]);
            assert!(!env.work.path().join("README.md").exists());
            assert!(!env.rebasing());
        }

        #[test]
        fn a_conflict_without_markers_is_resolved_by_keeping_the_file() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            delete_upstream_readme(&env);
            env.succeed(
                &["code", "sync"],
                &[("FAKE_RESOLVE", "true"), ("FAKE_KEPT", "\"README.md\"")],
            );
            assert_eq!(
                fs::read_to_string(env.work.path().join("README.md")).unwrap(),
                "feature\n"
            );
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
            let prompt = env.logged("resolve.1").unwrap();
            assert!(prompt.contains("List in kept"), "{prompt}");
        }

        #[test]
        fn a_binary_conflict_is_resolved_by_keeping_the_file() {
            let env = Env::new();
            env.write(".gitattributes", "*.bin binary\n");
            env.write("data.bin", "start\n");
            env.commit("Data.");
            env.git(&["push", "--quiet", "origin", "main"]);
            feature(&env, "data.bin", "feature\n");
            env.push_upstream("main", "data.bin", "upstream\n");
            let kept = [("FAKE_RESOLVE", "true"), ("FAKE_KEPT", "\"data.bin\"")];
            env.succeed(&["code", "sync"], &kept);
            assert_eq!(
                fs::read_to_string(env.work.path().join("data.bin")).unwrap(),
                "upstream\n"
            );
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
        }

        // A submodule the branch moves to one commit while main moves it to another.
        fn submodule_conflict(env: &Env) -> (TempDir, String, String) {
            let sub = TempDir::new().unwrap();
            git(sub.path(), &["init", "--quiet", "--initial-branch", "main"]);
            let commit = |message: &str| {
                git(
                    sub.path(),
                    &["commit", "--quiet", "--allow-empty", "--message", message],
                );
                git(sub.path(), &["rev-parse", "HEAD"])
            };
            commit("Start.");
            let ours = commit("Ours.");
            git(sub.path(), &["reset", "--quiet", "--hard", "HEAD~1"]);
            let theirs = commit("Theirs.");
            git(sub.path(), &["branch", "ours", &ours]);
            git(sub.path(), &["reset", "--quiet", "--hard", "HEAD~1"]);
            let source = sub.path().to_str().unwrap();
            env.git(&[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                source,
                "sub",
            ]);
            env.commit("Submodule.");
            env.git(&["push", "--quiet", "origin", "main"]);
            env.git(&["switch", "--quiet", "--create", "feature"]);
            git(
                &env.work.path().join("sub"),
                &["checkout", "--quiet", &ours],
            );
            env.commit("Feature.");
            env.change_upstream("main", |clone| {
                let gitlink = format!("160000,{theirs},sub");
                git(clone, &["update-index", "--cacheinfo", &gitlink]);
            });
            (sub, ours, theirs)
        }

        #[test]
        fn a_submodule_conflict_is_resolved_by_checking_out_a_commit() {
            let env = Env::new();
            let (_sub, _, theirs) = submodule_conflict(&env);
            let resolve = format!("git -C sub checkout --quiet {theirs}");
            env.succeed(&["code", "sync"], &[("FAKE_RESOLVE", &resolve)]);
            assert_eq!(env.git(&["rev-parse", "HEAD:sub"]), theirs);
            assert_eq!(env.git(&["status", "--porcelain"]), "");
            assert!(!env.rebasing());
            let prompt = env.logged("resolve.1").unwrap();
            assert!(prompt.contains("A conflicted submodule"), "{prompt}");
        }

        #[test]
        fn a_submodule_conflict_is_resolved_by_keeping_its_commit() {
            let env = Env::new();
            let (_sub, ours, _) = submodule_conflict(&env);
            let kept = [("FAKE_RESOLVE", "true"), ("FAKE_KEPT", "\"sub\"")];
            env.succeed(&["code", "sync"], &kept);
            assert_eq!(env.git(&["rev-parse", "HEAD:sub"]), ours);
            assert!(!env.rebasing());
        }

        #[test]
        fn a_submodule_conflict_left_as_it_was_aborts_the_rebase() {
            let env = Env::new();
            let (_sub, _, _) = submodule_conflict(&env);
            let old = env.git(&["rev-parse", "HEAD"]);
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", "true")]);
            assert!(
                stderr.contains("error: the conflicts in sub were left unresolved"),
                "{stderr}"
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn longer_conflict_markers_are_left_unresolved() {
            let env = Env::new();
            env.write(".gitattributes", "README.md conflict-marker-size=10\n");
            env.commit("Attributes.");
            env.git(&["push", "--quiet", "origin", "main"]);
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let marked =
                "printf '<<<<<<<<<< HEAD\\nx\\n==========\\ny\\n>>>>>>>>>> c\\n' > README.md";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", marked)]);
            assert!(
                stderr.contains("error: the conflicts in README.md were left unresolved"),
                "{stderr}"
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn edits_outside_the_conflicts_abort_the_rebase() {
            let env = Env::new();
            env.write("notes.txt", "notes\n");
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'resolved\\n' > README.md; echo edited > notes.txt";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            let short = env.git(&["rev-parse", "--short", &old]);
            assert!(
                stderr.contains(&format!(
                    "error: notes.txt changed outside the conflicts while applying {short} \
                     Feature., so the rebase was aborted;"
                )),
                "{stderr}"
            );
            assert_restored(&env, &old);
        }

        #[test]
        fn staged_edits_outside_the_conflicts_abort_the_rebase() {
            let env = Env::new();
            env.write("notes.txt", "notes\n");
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve =
                "printf 'resolved\\n' > README.md; echo edited > notes.txt; git add notes.txt";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(
                stderr.contains("error: notes.txt changed outside the conflicts while applying "),
                "{stderr}"
            );
            assert_restored(&env, &old);
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
        fn new_files_outside_the_conflicts_abort_the_rebase() {
            let env = Env::new();
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
            let resolve = "printf 'resolved\\n' > README.md; echo scratch > scratch.txt";
            let stderr = env.fail(&["code", "sync"], &[("FAKE_RESOLVE", resolve)]);
            assert!(
                stderr.contains("error: scratch.txt changed outside the conflicts while applying "),
                "{stderr}"
            );
            assert_eq!(env.git(&["rev-parse", "HEAD"]), old);
            assert_eq!(env.git(&["status", "--porcelain"]), "?? scratch.txt");
            assert!(!env.rebasing());
        }

        #[test]
        fn an_interrupted_sync_aborts_the_rebase() {
            use std::process::Stdio;
            use std::thread;
            use std::time::{Duration, Instant};

            let env = Env::new();
            let old = feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
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
        fn a_rebase_in_progress_is_refused() {
            let env = Env::new();
            feature(&env, "README.md", "feature\n");
            env.push_upstream("main", "README.md", "upstream\n");
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
            feature(&env, "README.md", "feature\n");
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
            let stderr = env.fail(&["code", "sync"], &[]);
            assert!(
                stderr.contains("error: a merge is already in progress in "),
                "{stderr}"
            );
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
