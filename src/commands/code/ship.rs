use std::ffi::OsStr;
use std::path::Path;
use std::sync::LazyLock;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::git::{Git, RemoteBranch};
use super::review::{Change, Worktree};
use crate::agent::{AgentKind, task_schema};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

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
pub enum Unshippable {
    #[error("the {0} is empty")]
    Empty(&'static str),
    #[error("the {0} is more than one line")]
    Lines(&'static str),
    #[error("`{0}` is not a valid branch name")]
    BranchName(String),
    #[error("a branch named {0} already exists")]
    BranchExists(String),
    #[error("{remote} already has a branch named {branch}")]
    RemoteBranchExists { remote: String, branch: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shipment {
    commit_message: String,
    branch: String,
    title: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    url: String,
    is_cross_repository: bool,
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
        let gh = Gh {
            cwd: repo,
            cancellation,
        };
        gh.run(&["auth", "status"]).map_err(|error| match error {
            ProcessError::Exit { .. } => Error::GhUnauthenticated(error),
            error => error.into(),
        })?;
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
        // gh picks its own repository from the remotes, which need not be the one pushed to.
        let repository = gh_repository(&git.remote_url(&into.remote)?, repo, cancellation)?;
        for url in git.remote_push_urls(&into.remote)? {
            let pushed_to = gh_repository(&url, repo, cancellation)?;
            if !repository.eq_ignore_ascii_case(&pushed_to) {
                return Err(Error::PushesElsewhere {
                    remote: into.remote.clone(),
                    fetch: repository,
                    push: pushed_to,
                });
            }
        }
        let default = git.remote_default(&into.remote)?;
        let current = worktree
            .head
            .branch_name()
            .filter(|&branch| branch != into.branch && default.as_deref() != Some(branch));
        context.progress("writing the pull request");
        let (written, shipment) = agent.perform_as::<Shipment>(
            &prompt(&into, uncommitted.as_ref(), commits.as_ref()),
            &SCHEMA,
            cancellation,
        )?;
        let resumable = |error: Error| error.resumable(&written.resume);
        let after = worktree.since(cancellation).map_err(resumable)?;
        if after.head != worktree.head {
            return Err(resumable(Error::HeadMoved {
                repo: after.repo,
                from: Box::new(worktree.head.clone()),
                to: Box::new(after.head),
            }));
        }
        if after.tree != worktree.tree {
            return Err(resumable(Error::ChangedWhileShipping {
                agent: agent.name.clone(),
                repo: after.repo,
            }));
        }
        let invalid = |problem| {
            resumable(Error::InvalidShipment {
                agent: agent.name.clone(),
                problem,
            })
        };
        let title = line("pull request title", &shipment.title).map_err(invalid)?;
        let message = uncommitted
            .as_ref()
            .map(|_| line("commit message", &shipment.commit_message))
            .transpose()
            .map_err(invalid)?;
        let branch = match current {
            Some(branch) => branch.to_owned(),
            None => {
                let branch = shipment.branch.trim();
                new_branch(&git, &agent.name, &into.remote, branch).map_err(resumable)?;
                context.progress(&format!("creating {branch}"));
                git.run(&["switch", "--quiet", "--create", branch])
                    .map_err(|error| resumable(error.into()))?;
                branch.to_owned()
            }
        };
        let ship = || -> Result<String, Error> {
            if let Some(message) = message {
                context.progress("committing");
                git.run(&["add", "--all", "--sparse"])?;
                git.run(&["commit", "--quiet", "--message", message])?;
            }
            context.progress(&format!("pushing {branch}"));
            let destination = format!("refs/heads/{branch}");
            let pushed = RemoteBranch {
                remote: into.remote.clone(),
                branch: branch.clone(),
            };
            // Replaces what was pushed before, as after a rebase, but only if it is still there
            // and the branch once held it, so a fetched commit the branch never had is kept.
            // A branch deleted on the remote leaves a stale tracking ref the lease would reject.
            let lease = format!("--force-with-lease={destination}");
            let refspec = format!("{destination}:{destination}");
            let mut push = vec!["push", "--quiet", "--set-upstream"];
            if git.commit(&pushed.refname())?.is_some() && git.remote_has(&into.remote, &branch)? {
                push.extend([lease.as_str(), "--force-if-includes"]);
            }
            push.extend([into.remote.as_str(), &refspec]);
            git.run(&push)?;
            let open = gh.run(&[
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
                &repository,
            ])?;
            let open: Vec<PullRequest> = serde_json::from_str(&open).map_err(Error::GhOutput)?;
            if let Some(PullRequest { url, .. }) =
                open.into_iter().find(|pr| !pr.is_cross_repository)
            {
                context.progress("updated the open pull request");
                return Ok(url);
            }
            context.progress("opening the pull request");
            let created = gh.run(&[
                "pr",
                "create",
                "--base",
                &into.branch,
                "--head",
                &branch,
                "--title",
                title,
                "--body",
                shipment.body.trim(),
                "--repo",
                &repository,
            ])?;
            created
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .map(str::to_owned)
                .ok_or(Error::NoPullRequestUrl)
        };
        ship().map_err(resumable)
    }
}

struct Gh<'a> {
    cwd: &'a Path,
    cancellation: &'a Cancellation,
}

impl Gh<'_> {
    fn run(&self, args: &[&str]) -> Result<String, ProcessError> {
        let args: Vec<_> = args.iter().map(Into::into).collect();
        Cli::new([GH]).run(&args, None, Some(self.cwd), self.cancellation)
    }
}

// The remote as the `[HOST/]OWNER/REPO` gh takes, with an SSH host alias resolved to the host it
// names. A local repository has no such name and is left for gh to judge.
fn gh_repository(url: &str, cwd: &Path, cancellation: &Cancellation) -> Result<String, Error> {
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
        Some(cwd),
        cancellation,
    )?;
    let host = config
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map_or(host, str::trim);
    Ok(format!("{host}/{path}"))
}

fn line<'a>(field: &'static str, text: &'a str) -> Result<&'a str, Unshippable> {
    match text.trim() {
        "" => Err(Unshippable::Empty(field)),
        text if text.contains('\n') => Err(Unshippable::Lines(field)),
        text => Ok(text),
    }
}

fn new_branch(git: &Git, agent: &str, remote: &str, branch: &str) -> Result<(), Error> {
    let invalid = |problem| Error::InvalidShipment {
        agent: agent.to_owned(),
        problem,
    };
    if branch.is_empty() {
        return Err(invalid(Unshippable::Empty("branch name")));
    }
    if let Err(error) = git.run(&["check-ref-format", "--branch", branch]) {
        return Err(match error {
            ProcessError::Exit { .. } => invalid(Unshippable::BranchName(branch.to_owned())),
            error => error.into(),
        });
    }
    if git.commit(&format!("refs/heads/{branch}"))?.is_some() {
        return Err(invalid(Unshippable::BranchExists(branch.to_owned())));
    }
    if git.remote_has(remote, branch)? {
        return Err(invalid(Unshippable::RemoteBranchExists {
            remote: remote.to_owned(),
            branch: branch.to_owned(),
        }));
    }
    Ok(())
}

fn prompt(into: &RemoteBranch, uncommitted: Option<&Change>, commits: Option<&Change>) -> String {
    let mut prompt = format!(
        "Write what is needed to open a pull request against {into}: a commit message, a branch \
         name, a pull request title, and a pull request body."
    );
    if let Some(commits) = commits {
        prompt.push_str(&format!(
            "\n\nThe pull request has {commits}. {}",
            commits.access()
        ));
    }
    match uncommitted {
        Some(uncommitted) => prompt.push_str(&format!(
            "\n\nThe pull request also has {uncommitted}, which will be committed with your commit \
             message. {}\n\nThe commit message describes the uncommitted changes in one short \
             sentence.",
            uncommitted.access()
        )),
        None => prompt
            .push_str("\n\nThere are no uncommitted changes, so leave the commit message empty."),
    }
    prompt.push_str(
        "\n\nThe pull request title is one line and the body describes the whole change, for a \
         reviewer. The branch name is short, lowercase, words joined by hyphens, and a valid git \
         branch name; it is used only if a new branch is needed. Do not change any file, do not \
         commit, do not push, and do not run gh: ncli does all of that with what you write. In \
         report, say in one sentence what the change does.",
    );
    prompt
}
