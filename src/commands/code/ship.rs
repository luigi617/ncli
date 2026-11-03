use std::ffi::{OsStr, OsString};
use std::sync::LazyLock;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::git::{Git, Head, RemoteBranch};
use super::review::{Change, Worktree};
use crate::agent::{AgentKind, task_schema};
use crate::commands::Context;
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

#[derive(Debug, thiserror::Error)]
pub enum Unshipped {
    #[error("the working tree has uncommitted changes ({0})")]
    Dirty(String),
    #[error("HEAD is {0}, which is not a branch to open a pull request from")]
    Unbranched(Box<Head>),
    #[error("the shipped branch no longer contains {0}, the commit it started from")]
    Dropped(String),
    #[error("the shipped commit has tree {found}, not {expected}, the work there was to ship")]
    Altered { expected: String, found: String },
    #[error("the pull request at {url} is not from and into {repository}")]
    Elsewhere { url: String, repository: String },
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
    is_cross_repository: bool,
}

#[derive(Deserialize)]
struct Repository {
    url: String,
}

struct Target<'a> {
    into: &'a RemoteBranch,
    repository: &'a str,
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
        let toplevel = Git::toplevel(cancellation)?;
        let repo = toplevel.as_path();
        let git = Git::new(repo, cancellation);
        git.ensure_idle()?;
        let worktree = Worktree::snapshot(cancellation)?;
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
        let repository = repository(&git, &into.remote)?;
        let default = git.remote_default(&into.remote)?;
        let target = Target {
            into: &into,
            repository: &repository,
            default: default.as_deref(),
        };
        context.progress(&format!("shipping to {into}"));
        let (step, shipment) = agent.perform_as::<Shipment>(
            &prompt(&worktree, &target, uncommitted.as_ref(), commits.as_ref()),
            &SCHEMA,
            cancellation,
        )?;
        let shipped = shipped(&git, &worktree, head, &target, &shipment.url)
            .map_err(|error| error.resumable(&step.resume))?;
        shipped.map_err(|problem| {
            Error::Unshipped {
                agent: agent.name.clone(),
                into: Box::new(into),
                problem: Box::new(problem),
                report: step.report,
            }
            .resumable(&step.resume)
        })
    }
}

// The URL of the pull request the work was shipped as, as gh reports it.
fn shipped(
    git: &Git,
    worktree: &Worktree,
    start: &str,
    target: &Target,
    url: &str,
) -> Result<Result<String, Unshipped>, Error> {
    let dirty = git.uncommitted()?;
    if !dirty.is_empty() {
        return Ok(Err(Unshipped::Dirty(dirty.join(", "))));
    }
    let head = git.head()?;
    let (Some(branch), Some(commit)) = (
        head.branch_name()
            .filter(|&branch| target.ships_from(branch)),
        &head.commit,
    ) else {
        return Ok(Err(Unshipped::Unbranched(Box::new(head))));
    };
    if !git.is_ancestor(start, commit)? {
        return Ok(Err(Unshipped::Dropped(start.to_owned())));
    }
    let tree = git.run(&["rev-parse", &format!("{commit}^{{tree}}")])?;
    if tree != worktree.tree {
        return Ok(Err(Unshipped::Altered {
            expected: worktree.tree.clone(),
            found: tree,
        }));
    }
    let viewed = gh(
        git,
        &[
            "pr",
            "view",
            "--json",
            "url,state,baseRefName,headRefName,headRefOid,isCrossRepository",
            "--",
            url,
        ],
    )?;
    let pr: PullRequest = serde_json::from_str(&viewed).map_err(Error::GhOutput)?;
    if pr.is_cross_repository || !pr.url.starts_with(&format!("{}/pull/", target.repository)) {
        return Ok(Err(Unshipped::Elsewhere {
            url: pr.url,
            repository: target.repository.to_owned(),
        }));
    }
    let expected = [
        ("state", "OPEN", pr.state),
        ("base branch", target.into.branch.as_str(), pr.base_ref_name),
        ("head branch", branch, pr.head_ref_name),
        ("head commit", commit.as_str(), pr.head_ref_oid),
    ];
    let mismatch = expected
        .into_iter()
        .find(|(_, expected, found)| expected != found);
    Ok(match mismatch {
        Some((field, expected, found)) => Err(Unshipped::Mismatch {
            url: pr.url,
            field,
            expected: expected.to_owned(),
            found,
        }),
        None => Ok(pr.url),
    })
}

fn gh(git: &Git, args: &[&str]) -> Result<String, ProcessError> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    Cli::new([GH]).run(&args, None, Some(git.cwd), git.cancellation)
}

// The URL of the repository the remote fetches from, as gh names it, so a pull request's URL can be
// checked against it even after the repository is renamed.
fn repository(git: &Git, remote: &str) -> Result<String, Error> {
    let url = git.run(&["remote", "get-url", remote])?;
    let name = gh_name(git, &url)?;
    let viewed = gh(git, &["repo", "view", &name, "--json", "url"])?;
    let repository: Repository = serde_json::from_str(&viewed).map_err(Error::GhOutput)?;
    Ok(repository.url)
}

// The remote as the `[HOST/]OWNER/REPO` gh takes, with an SSH host alias resolved to the host it
// names. A local repository has no such name and is left for gh to judge.
fn gh_name(git: &Git, url: &str) -> Result<String, Error> {
    let (ssh, authority, path) = match url.split_once("://") {
        Some(("file", _)) => return Ok(url.to_owned()),
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            (scheme.contains("ssh"), authority, path)
        }
        // git reads `host:path` as SSH unless a slash or a drive letter comes before the colon.
        None => match url.split_once(':') {
            Some((authority, path))
                if !(authority.contains('/') || cfg!(windows) && authority.len() == 1) =>
            {
                (true, authority, path)
            }
            _ => return Ok(url.to_owned()),
        },
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let named = path
        .split_once('/')
        .is_some_and(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'));
    if host.is_empty() || !named {
        return Err(Error::NotAGitHubRemote(url.to_owned()));
    }
    if !ssh {
        return Ok(format!("{host}/{path}"));
    }
    let config = Cli::new(["ssh"]).run(
        &["-G".into(), "--".into(), host.into()],
        None,
        Some(git.cwd),
        git.cancellation,
    )?;
    let host = config
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map_or(host, str::trim);
    Ok(format!("{host}/{path}"))
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
         reuse it instead. Ship the work as it is: do not change, drop, or rewrite any of it. \
         Leave the working tree clean and stay on the branch you shipped from. \
         In url, give the pull request's URL. In report, say in one sentence what the change \
         does.",
        remote = into.remote,
        branch = into.branch,
    ));
    prompt
}
