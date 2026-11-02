use std::ffi::OsStr;
use std::sync::LazyLock;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::git::{Git, Head, RemoteBranch};
use super::review::{Change, Worktree};
use crate::agent::{AgentKind, task_schema};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cli};

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

#[derive(Debug, thiserror::Error)]
pub enum Unshipped {
    #[error("the working tree has uncommitted changes ({0})")]
    Dirty(String),
    #[error("HEAD is {0}, which is not a branch to open a pull request from")]
    Unbranched(Box<Head>),
    #[error("the pull request at {url} has {field} {found}, not {expected}")]
    Mismatch {
        url: String,
        field: &'static str,
        expected: String,
        found: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shipment {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    url: String,
    state: String,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
}

struct Target<'a> {
    into: &'a RemoteBranch,
    default: Option<&'a str>,
}

impl Target<'_> {
    fn ships_from(&self, branch: &str) -> bool {
        branch != self.into.branch && self.default != Some(branch)
    }
}

impl Ship {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        process::require_executables([OsStr::new("git"), OsStr::new(GH)])?;
        let agent = AgentKind::first_available()?;
        let cancellation = context.cancellation;
        let worktree = Worktree::snapshot(cancellation)?;
        let repo = worktree.repo.as_path();
        let git = Git::new(repo, cancellation);
        if let Some(operation) = git.in_progress()? {
            return Err(Error::OperationInProgress {
                repo: repo.to_owned(),
                operation,
            });
        }
        let unmerged = git.unmerged()?;
        if !unmerged.is_empty() {
            return Err(Error::UnmergedPaths {
                repo: repo.to_owned(),
                paths: unmerged.join(", "),
            });
        }
        let into = git.remote_branch(self.into.as_deref())?;
        git.fetch(&into)?;
        let Some(head) = &worktree.head.commit else {
            return Err(Error::NoCommonHistory {
                repo: repo.to_owned(),
                base: into,
            });
        };
        let uncommitted = worktree.uncommitted(&git)?.then(|| worktree.change());
        let commits = Change::ahead(&git, repo, head, &into)?;
        if uncommitted.is_none() && commits.is_none() {
            return Err(Error::NothingToShip {
                repo: repo.to_owned(),
                base: into,
            });
        }
        let default = git.remote_default(&into.remote)?;
        let target = Target {
            into: &into,
            default: default.as_deref(),
        };
        context.progress(&format!("shipping to {into}"));
        let (step, shipment) = agent.perform_as::<Shipment>(
            &prompt(&worktree, &target, uncommitted.as_ref(), commits.as_ref()),
            &SCHEMA,
            cancellation,
        )?;
        let unshipped = unshipped(&git, &target, &shipment.url)
            .map_err(|error| error.resumable(&step.resume))?;
        match unshipped {
            Some(problem) => Err(Error::Unshipped {
                agent: agent.name.clone(),
                into: Box::new(into),
                problem: Box::new(problem),
                report: step.report,
            }
            .resumable(&step.resume)),
            None => Ok(shipment.url),
        }
    }
}

fn unshipped(git: &Git, target: &Target, url: &str) -> Result<Option<Unshipped>, Error> {
    let dirty = git.uncommitted()?;
    if !dirty.is_empty() {
        return Ok(Some(Unshipped::Dirty(dirty.join(", "))));
    }
    let head = git.head()?;
    let (Some(branch), Some(commit)) = (
        head.branch_name()
            .filter(|&branch| target.ships_from(branch)),
        &head.commit,
    ) else {
        return Ok(Some(Unshipped::Unbranched(Box::new(head))));
    };
    let viewed = Cli::new([GH]).run(
        &[
            "pr".into(),
            "view".into(),
            url.into(),
            "--json".into(),
            "url,state,baseRefName,headRefName,headRefOid".into(),
        ],
        None,
        Some(git.cwd),
        git.cancellation,
    )?;
    let pr: PullRequest = serde_json::from_str(&viewed).map_err(Error::GhOutput)?;
    let expected = [
        ("state", "OPEN", pr.state),
        ("base branch", target.into.branch.as_str(), pr.base_ref_name),
        ("head branch", branch, pr.head_ref_name),
        ("head commit", commit.as_str(), pr.head_ref_oid),
    ];
    Ok(expected
        .into_iter()
        .find(|(_, expected, found)| expected != found)
        .map(|(field, expected, found)| Unshipped::Mismatch {
            url: pr.url,
            field,
            expected: expected.to_owned(),
            found,
        }))
}

fn prompt(
    worktree: &Worktree,
    target: &Target,
    uncommitted: Option<&Change>,
    commits: Option<&Change>,
) -> String {
    let into = target.into;
    let mut prompt = format!(
        "Ship the work in the repository at `{}` as a pull request against {into}.",
        worktree.repo.display()
    );
    if let Some(commits) = commits {
        prompt.push_str(&format!(
            "\n\nThe pull request has {commits}. {}",
            commits.access()
        ));
    }
    if let Some(uncommitted) = uncommitted {
        prompt.push_str(&format!(
            "\n\nThe pull request also has {uncommitted}. {}\n\nCommit all of the uncommitted \
             changes, untracked files included, with a message of one short sentence.",
            uncommitted.access()
        ));
    }
    match worktree
        .head
        .branch_name()
        .filter(|&branch| target.ships_from(branch))
    {
        Some(branch) => prompt.push_str(&format!("\n\nShip from the current branch, {branch}.")),
        None => prompt.push_str(&format!(
            "\n\nCreate a new branch to ship from before committing, named in a few lowercase \
             words joined by hyphens, since HEAD is {}; never ship from {} or the remote's \
             default branch.",
            worktree.head, into.branch
        )),
    }
    prompt.push_str(&format!(
        "\n\nPush the branch to {remote} and set it as the upstream. If the branch was rewritten, \
         as after a rebase, push with `--force-with-lease --force-if-includes`, and never \
         overwrite commits on {remote} that the branch has not seen. Then open a pull request \
         against {branch} with gh, with a one-line title and a body that describes the whole \
         change for a reviewer; if the branch already has an open pull request against {branch}, \
         reuse it instead. Leave the working tree clean and stay on the branch you shipped from. \
         In url, give the pull request's URL. In report, say in one sentence what the change \
         does.",
        remote = into.remote,
        branch = into.branch,
    ));
    prompt
}
