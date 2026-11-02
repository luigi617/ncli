use clap::Args;
use clap::builder::NonEmptyStringValueParser;

use super::git::{Git, Head, Operation, RemoteBranch};
use crate::agent::{Agent, AgentKind, Performed};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{Cancellation, ProcessError};

const MARKERS: &str = r"^(<{7,}|\|{7,}|>{7,})([[:space:]]|$)";

/// Rebase the current branch onto another branch, resolving conflicts with an agent. If the
/// rebase cannot be finished, the branch is put back where it was.
#[derive(Debug, Args)]
pub struct Sync {
    /// Branch to rebase onto [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Unsynced {
    #[error("a {0} is still in progress")]
    InProgress(Operation),
    #[error("HEAD is now {0}")]
    LeftBranch(Box<Head>),
    #[error("{0} is not in the history of the branch")]
    NotOnto(RemoteBranch),
    #[error("the working tree has uncommitted changes ({0})")]
    Dirty(String),
    #[error("conflict markers are left in {0}")]
    Markers(String),
}

struct Rebase<'a> {
    git: &'a Git<'a>,
    branch: &'a str,
    from: &'a RemoteBranch,
}

impl Sync {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let repo = Git::toplevel(context.cancellation)?;
        let git = Git::new(&repo, context.cancellation);
        if let Some(operation) = git.in_progress()? {
            return Err(Error::OperationInProgress { repo, operation });
        }
        let head = git.head()?;
        let (Some(branch), Some(old)) = (head.branch_name(), &head.commit) else {
            return Err(Error::NotOnABranch {
                repo: repo.clone(),
                head: Box::new(head),
            });
        };
        let dirty = git.uncommitted()?;
        if !dirty.is_empty() {
            return Err(Error::DirtyWorktree {
                repo: repo.clone(),
                paths: dirty.join(", "),
            });
        }
        let from = git.remote_branch(self.from.as_deref())?;
        let agent = AgentKind::first_available()?;
        context.progress(&format!("fetching {from}"));
        git.fetch(&from)?;
        context.progress(&format!("rebasing {branch} onto {from}"));
        let rebase = Rebase {
            git: &git,
            branch,
            from: &from,
        };
        let resolved = rebase
            .run(&agent, context)
            .map_err(|error| rebase.restore(old, error))?;
        let new = git.run(&["rev-parse", "HEAD"])?;
        let summary = if new == *old {
            format!("{branch} is up to date with {from}, at {old}.")
        } else {
            format!("Rebased {branch} onto {from}, from {old} to {new}.")
        };
        Ok(match resolved {
            Some(step) => format!("{summary}\n\nResolved the conflicts:\n\n{step}"),
            None => summary,
        })
    }
}

impl Rebase<'_> {
    fn run(&self, agent: &Agent, context: &Context) -> Result<Option<Performed>, Error> {
        let Err(error) = self.git.run(&["rebase", "--quiet", &self.from.refname()]) else {
            return Ok(None);
        };
        if !matches!(error, ProcessError::Exit { .. })
            || !matches!(self.git.in_progress()?, Some(Operation::Rebase))
        {
            return Err(error.into());
        }
        context.progress("resolving the conflicts");
        let step = agent.perform(&self.prompt(), context.cancellation)?;
        let unsynced = self
            .unsynced()
            .map_err(|error| Error::from(error).resumable(&step.resume))?;
        match unsynced {
            Some(problem) => Err(Error::Unsynced {
                agent: agent.name.clone(),
                branch: self.branch.to_owned(),
                from: Box::new(self.from.clone()),
                problem: Box::new(problem),
                report: step.report,
            }
            .resumable(&step.resume)),
            None => Ok(Some(step)),
        }
    }

    fn unsynced(&self) -> Result<Option<Unsynced>, ProcessError> {
        if let Some(operation) = self.git.in_progress()? {
            return Ok(Some(Unsynced::InProgress(operation)));
        }
        let head = self.git.head()?;
        if head.branch_name() != Some(self.branch) {
            return Ok(Some(Unsynced::LeftBranch(Box::new(head))));
        }
        if !self.git.is_ancestor(&self.from.refname(), "HEAD")? {
            return Ok(Some(Unsynced::NotOnto(self.from.clone())));
        }
        let dirty = self.git.uncommitted()?;
        if !dirty.is_empty() {
            return Ok(Some(Unsynced::Dirty(dirty.join(", "))));
        }
        let marked = self.marked()?;
        Ok((!marked.is_empty()).then(|| Unsynced::Markers(marked.join(", "))))
    }

    // The files the branch changes that have a line starting with a conflict marker.
    fn marked(&self) -> Result<Vec<String>, ProcessError> {
        let changed = self.git.run(&[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-filter=d",
            &self.from.refname(),
            "HEAD",
        ])?;
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let grep: Vec<&str> = [
            "--literal-pathspecs",
            "grep",
            "-I",
            "-l",
            "-z",
            "-E",
            "-e",
            MARKERS,
            "HEAD",
            "--",
        ]
        .into_iter()
        .chain(changed.split_terminator('\0'))
        .collect();
        Ok(self
            .git
            .optional(&grep)?
            .unwrap_or_default()
            .split_terminator('\0')
            .map(|found| found.strip_prefix("HEAD:").unwrap_or(found).to_owned())
            .collect())
    }

    // Not cancellable: an interrupted run still puts the branch back where it was.
    fn restore(&self, old: &str, error: Error) -> Error {
        let uncancellable = Cancellation::default();
        let git = Git::new(self.git.cwd, &uncancellable);
        let Err(restore) = put_back(&git, self.branch, old) else {
            return error;
        };
        let not_restored = |error| Error::NotRestored {
            error: Box::new(error),
            branch: self.branch.to_owned(),
            old: old.to_owned(),
            restore: Box::new(restore),
        };
        match error {
            Error::Resumable { source, resume } => not_restored(*source).resumable(&resume),
            error => not_restored(error),
        }
    }

    fn prompt(&self) -> String {
        format!(
            "A rebase of the branch {branch} onto {from} in the repository at `{repo}` stopped \
             with conflicts. Finish the rebase. At each stop, resolve the conflicts with code \
             that keeps the intent of both sides: what the commit being applied changes, seen \
             with `git show REBASE_HEAD`, and what changed on {from}, seen with `git log -p \
             REBASE_HEAD..{target} -- <path>`. Then stage the resolved files and continue with \
             `GIT_EDITOR=: git rebase --continue`, until the rebase is done. \
             Do not abort the rebase, and do not skip a commit with `git rebase --skip` unless \
             its change is already on {from}. Do not switch branches, reset, stash, or push. \
             In report, say how you resolved each conflict.",
            branch = self.branch,
            from = self.from,
            target = self.from.refname(),
            repo = self.git.cwd.display(),
        )
    }
}

// Works wherever the agent left HEAD, even on another branch.
fn put_back(git: &Git, branch: &str, old: &str) -> Result<(), ProcessError> {
    if let Some(operation) = git.in_progress()? {
        git.run(&[&operation.to_string(), "--abort"])?;
    }
    git.run(&[
        "switch",
        "--quiet",
        "--discard-changes",
        "--force-create",
        branch,
        old,
    ])?;
    Ok(())
}
