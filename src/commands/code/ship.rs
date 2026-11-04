use std::path::Path;
use std::sync::LazyLock;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::review::Git;
use crate::agent::{AgentKind, task_schema};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

const GH: &str = "gh";

static SCHEMA: LazyLock<String> = LazyLock::new(|| task_schema(r#"{"url": {"type": "string"}}"#));

/// Turn the current work into a pull request: commit it, push it, and open the pull request.
///
/// An agent does the work with git and gh. ncli then checks that the pull request it reports is
/// open against the right branch at the current commit, with nothing left uncommitted.
#[derive(Debug, Args)]
pub struct Ship {
    /// Branch to open the pull request against [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub into: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Unshipped {
    #[error("uncommitted changes are left")]
    Uncommitted,
    #[error("the pull request is {0}, not open")]
    NotOpen(String),
    #[error("the pull request is against {0}")]
    WrongBase(String),
    #[error("the pull request is at {pushed}, but HEAD is at {head}")]
    NotPushed { pushed: String, head: String },
}

#[derive(Deserialize)]
struct Shipped {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    state: String,
    base_ref_name: String,
    head_ref_oid: String,
}

impl Ship {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        process::require_executables([GH.as_ref()])?;
        let agent = AgentKind::first_available()?;
        let repo = Git::toplevel(context.cancellation)?;
        let git = Git::new(&repo, context.cancellation);
        let into = match &self.into {
            Some(into) => into.clone(),
            None => git.default_branch()?.branch().to_owned(),
        };
        context.progress("shipping");
        let (performed, shipped) =
            agent.perform_as::<Shipped>(&prompt(&repo, &into), &SCHEMA, context.cancellation)?;
        match unshipped(&git, &repo, context.cancellation, &shipped.url, &into)? {
            None => Ok(shipped.url),
            Some(problem) => Err(Error::NotShipped {
                agent: agent.name.clone(),
                url: shipped.url,
                problem,
                report: performed.report,
            }
            .resumable(&performed.resume)),
        }
    }
}

fn unshipped(
    git: &Git,
    repo: &Path,
    cancellation: &Cancellation,
    url: &str,
    into: &str,
) -> Result<Option<Unshipped>, Error> {
    if git.uncommitted()? {
        return Ok(Some(Unshipped::Uncommitted));
    }
    let viewed = gh(
        repo,
        cancellation,
        &["pr", "view", url, "--json", "state,baseRefName,headRefOid"],
    )?;
    let pull_request: PullRequest = serde_json::from_str(&viewed).map_err(Error::GhOutput)?;
    let head = git.run(&["rev-parse", "HEAD"])?;
    Ok(if pull_request.state != "OPEN" {
        Some(Unshipped::NotOpen(pull_request.state.to_lowercase()))
    } else if pull_request.base_ref_name != into {
        Some(Unshipped::WrongBase(pull_request.base_ref_name))
    } else if pull_request.head_ref_oid != head {
        Some(Unshipped::NotPushed {
            pushed: pull_request.head_ref_oid,
            head,
        })
    } else {
        None
    })
}

fn gh(repo: &Path, cancellation: &Cancellation, args: &[&str]) -> Result<String, ProcessError> {
    let args: Vec<_> = args.iter().map(Into::into).collect();
    Cli::new([GH]).run(&args, None, Some(repo), cancellation)
}

fn prompt(repo: &Path, into: &str) -> String {
    format!(
        "Ship the work in the repository at `{}` as a pull request into the branch {into}. Commit \
         any uncommitted changes with a message of one short sentence. If HEAD is detached or on \
         {into} or the remote's default branch, first create a branch named in a few lowercase \
         words joined by hyphens. Push the branch, without overwriting commits on the remote that \
         the branch does not have; if the push is rejected for that reason, stop and say so. If a \
         pull request from the branch into {into} is already open, use it; otherwise open one \
         with gh, with a short title and a body that says what the change does and why. Leave \
         nothing uncommitted. Give the pull request's URL as url.",
        repo.display()
    )
}
