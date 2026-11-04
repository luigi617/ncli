use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::LazyLock;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::git::{Git, RemoteBranch, Upstream};
use super::review::Worktree;
use crate::agent::{AgentKind, task_schema};
use crate::commands::{Context, one_line};
use crate::error::Error;
use crate::process::{self, Cli, ProcessError};

const GH: &str = "gh";

static SCHEMA: LazyLock<String> =
    LazyLock::new(|| task_schema(include_str!("shipment.schema.json")));

/// Turn the current work into a pull request: commit it, push it, and open the pull request.
#[derive(Debug, Args)]
pub struct Ship {
    /// Branch to open the pull request against [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub into: Option<String>,
}

#[derive(Deserialize)]
struct Shipment {
    branch: String,
    commit_message: String,
    title: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    url: String,
    is_cross_repository: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Unusable {
    #[error("the {0} is empty")]
    Empty(&'static str),
    #[error("`{0}` is not a valid branch name")]
    Branch(String),
    #[error("`{branch}` is already a branch on {remote}")]
    Taken { branch: String, remote: String },
}

impl Ship {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        process::require_executables([OsStr::new(GH)])?;
        let agent = AgentKind::first_available()?;
        let work = Worktree::snapshot(context.cancellation)?;
        let repo = &work.repo;
        let git = Git::new(repo, context.cancellation);
        git.ensure_idle()?;
        let default = git.known_default()?.map(|default| default.branch);
        let into = git.upstream(self.into.as_deref())?;
        context.progress(&format!("fetching {into}"));
        let target = git.fetch(&into)?;
        let head = &work.head;
        let merge_base = match &head.commit {
            Some(commit) => git.optional(&["merge-base", &target, commit])?,
            None => None,
        };
        let Some(merge_base) = merge_base else {
            return Err(Error::NoCommonHistory {
                repo: repo.clone(),
                base: into.tracking(),
            });
        };
        let uncommitted = !git.status()?.is_empty();
        if !uncommitted && head.commit.as_ref() == Some(&merge_base) {
            return Err(Error::NothingToShip {
                repo: repo.clone(),
                base: into.tracking(),
            });
        }
        // Without a known default branch, the current branch could be the remote's primary one.
        let current = default.as_deref().and_then(|default| {
            head.branch_name()
                .filter(|&branch| branch != into.branch && branch != default)
                .map(|branch| (branch, default))
        });
        if let Some((branch, default)) = current {
            check_upstream(&git, branch, &into, default)?;
        }
        let current = current.map(|(branch, _)| branch);
        // gh would otherwise pick a repository itself, which need not be the one pushed to.
        let remote_url = git.run(&["remote", "get-url", &into.remote])?;
        gh(&git, &["repo", "view", &remote_url, "--json", "url"], None)?;
        context.progress("describing the change");
        let (_, shipment) = agent.perform_as::<Shipment>(
            &prompt(repo, &into, &merge_base, current, uncommitted),
            &SCHEMA,
            context.cancellation,
        )?;
        let described = Worktree::snapshot(context.cancellation)?;
        if described.head != work.head || described.tree != work.tree {
            return Err(Error::ChangedWhileDescribing {
                agent: agent.name.clone(),
                repo: repo.clone(),
            });
        }
        let new_branch = current.is_none().then_some(into.remote.as_str());
        if let Some(problem) = shipment.problem(&git, new_branch, uncommitted)? {
            return Err(Error::UnusableAnswer {
                agent: agent.name.clone(),
                problem,
            });
        }
        let branch = match current {
            Some(branch) => branch.to_owned(),
            None => {
                git.run(&["switch", "--quiet", "--create", &shipment.branch])?;
                shipment.branch
            }
        };
        if uncommitted {
            git.run(&["add", "--all"])?;
            git.run_with_input(
                &["commit", "--quiet", "--file=-"],
                Some(shipment.commit_message.as_bytes()),
            )?;
        }
        context.progress(&format!("pushing {branch}"));
        // A full refspec, so push config cannot send the branch to another remote branch.
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        git.run(&[
            "push",
            "--quiet",
            "--force-with-lease",
            "--force-if-includes",
            "--set-upstream",
            &into.remote,
            &refspec,
        ])?;
        let listed = gh(
            &git,
            &[
                "pr",
                "list",
                "--head",
                &branch,
                "--base",
                &into.branch,
                "--state",
                "open",
                "--json",
                "url,isCrossRepository",
                "--repo",
                &remote_url,
            ],
            None,
        )?;
        let open: Vec<PullRequest> = serde_json::from_str(&listed).map_err(Error::GhOutput)?;
        if let Some(pull_request) = open.into_iter().find(|open| !open.is_cross_repository) {
            return Ok(pull_request.url);
        }
        context.progress("opening the pull request");
        let created = gh(
            &git,
            &[
                "pr",
                "create",
                "--base",
                &into.branch,
                "--head",
                &branch,
                "--title",
                &one_line(&shipment.title),
                "--body-file",
                "-",
                "--repo",
                &remote_url,
            ],
            Some(shipment.body.as_bytes()),
        )?;
        Ok(created.trim().lines().last().unwrap_or_default().to_owned())
    }
}

impl Shipment {
    fn problem(
        &self,
        git: &Git,
        new_branch: Option<&str>,
        uncommitted: bool,
    ) -> Result<Option<Unusable>, ProcessError> {
        let required = [
            ("commit message", &self.commit_message, uncommitted),
            ("title", &self.title, true),
        ];
        if let Some((field, ..)) = required
            .iter()
            .find(|(_, text, used)| *used && text.trim().is_empty())
        {
            return Ok(Some(Unusable::Empty(field)));
        }
        let Some(remote) = new_branch else {
            return Ok(None);
        };
        // Rejects names git would rewrite, such as @{-1}, as well as invalid ones.
        match git.run(&["check-ref-format", "--branch", &self.branch]) {
            Ok(name) if name == self.branch => {}
            Ok(_) | Err(ProcessError::Exit { .. }) => {
                return Ok(Some(Unusable::Branch(self.branch.clone())));
            }
            Err(error) => return Err(error),
        }
        // Pushing to a branch that exists would add the work to someone else's branch.
        let head = format!("refs/heads/{}", self.branch);
        match git.run(&["ls-remote", "--exit-code", "--heads", remote, &head]) {
            Err(ProcessError::Exit { code: 2, .. }) => Ok(None),
            Ok(_) => Ok(Some(Unusable::Taken {
                branch: self.branch.clone(),
                remote: remote.to_owned(),
            })),
            Err(error) => Err(error),
        }
    }
}

// Pushing a branch that tracks another branch would leave that branch, and its pull request, behind.
fn check_upstream(git: &Git, branch: &str, into: &Upstream, default: &str) -> Result<(), Error> {
    let Some(upstream) = git.optional(&[
        "rev-parse",
        "--verify",
        "--quiet",
        "--symbolic-full-name",
        &format!("{branch}@{{upstream}}"),
    ])?
    else {
        return Ok(());
    };
    if [branch, into.branch.as_str(), default]
        .into_iter()
        .any(|name| upstream == into.named(Some(name)).tracking().0)
    {
        return Ok(());
    }
    Err(Error::OtherUpstream {
        branch: branch.to_owned(),
        upstream: RemoteBranch(upstream),
        pushed: into.named(Some(branch)).tracking(),
    })
}

fn gh(git: &Git, args: &[&str], input: Option<&[u8]>) -> Result<String, ProcessError> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    Cli::new([GH]).run(&args, input, Some(git.cwd), git.cancellation)
}

fn prompt(
    repo: &Path,
    into: &Upstream,
    merge_base: &str,
    current: Option<&str>,
    uncommitted: bool,
) -> String {
    let branch = match current {
        Some(branch) => {
            format!("It is shipped from the current branch, {branch}, so branch is not used")
        }
        None => "It is shipped from a new branch, so give its name, in a few lowercase words \
                 joined by hyphens"
            .to_owned(),
    };
    let commit = if uncommitted {
        "a commit message of one short sentence for the uncommitted changes"
    } else {
        "an empty string, since there are no uncommitted changes"
    };
    format!(
        "Describe the work in the repository at `{}` so it can be shipped as a pull request into \
         {into}. Read it yourself: run `git log {merge_base}..HEAD` for its commits, `git diff \
         {merge_base}` for all of its changes, and `git status --untracked-files=all` for new \
         files, and read the new files. Do not change any file and do not run any command that \
         changes the repository; ncli commits, pushes, and opens the pull request with your \
         answer.\n\n\
         In branch: {branch}. In commit_message: {commit}. In title: a one-line title for the \
         pull request. In body: a description of the whole change for a reviewer, in Markdown. \
         In report, say in one sentence what the change does.",
        repo.display()
    )
}
