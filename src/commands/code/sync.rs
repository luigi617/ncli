use std::path::Path;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;

use super::review::Git;
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;
use crate::process::{Cancellation, ProcessError};

/// Rebase the current branch onto another branch, resolving conflicts with an agent.
///
/// An agent runs the rebase and resolves any conflicts. ncli then checks that the rebase finished
/// on the same branch, on top of the other branch, with no merge commits, no conflict markers, and
/// nothing uncommitted. If not, the branch is put back where it was.
#[derive(Debug, Args)]
pub struct Sync {
    /// Branch to rebase onto [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Unsynced {
    #[error("the rebase is not finished")]
    Unfinished,
    #[error("HEAD is no longer on the branch")]
    LeftBranch,
    #[error("the branch is not on top of the commit it was rebased onto")]
    NotOnto,
    #[error("the branch has merge commits")]
    Merges,
    #[error("none of the branch's commits are left")]
    Dropped,
    #[error("uncommitted changes are left")]
    Uncommitted,
    #[error("conflict markers are left in {0}")]
    Markers(String),
}

impl Sync {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let agent = AgentKind::first_available()?;
        let repo = Git::toplevel(context.cancellation)?;
        let git = Git::new(&repo, context.cancellation);
        if rebasing(&git)? {
            return Err(Error::Rebasing(repo));
        }
        let head = git.head()?;
        let (Some(branch), Some(tip)) = (head.branch.clone(), head.commit.clone()) else {
            return Err(Error::NotOnBranch(Box::new(head)));
        };
        // Putting the branch back would lose any uncommitted work.
        if git.uncommitted()? {
            return Err(Error::UncommittedChanges(repo));
        }
        let default = git.default_branch()?;
        let from = self.from.as_deref().unwrap_or(default.branch());
        let target = format!("{}/{from}", default.remote());
        context.progress(&format!("fetching {target}"));
        let refspec = format!("refs/heads/{from}");
        git.run(&["fetch", "--quiet", default.remote(), &refspec])?;
        let onto = git.run(&["rev-parse", "FETCH_HEAD"])?;
        context.progress(&format!("rebasing onto {target}"));
        let result = agent
            .perform(&prompt(&repo, &onto, &target), context.cancellation)
            .and_then(|performed| match unsynced(&git, &branch, &onto, &tip)? {
                None => Ok(()),
                Some(problem) => Err(Error::RebaseUnfinished {
                    agent: agent.name.clone(),
                    problem,
                    report: performed.report,
                }),
            });
        let Err(source) = result else {
            return Ok(format!("Rebased onto {target}; the branch was at {tip}."));
        };
        let source = Box::new(source);
        // An interruption cancels the run's processes, and the branch must still be put back.
        let uncancelled = Cancellation::default();
        let git = Git::new(&repo, &uncancelled);
        Err(match restore(&git, &branch, &tip) {
            Ok(()) => Error::NotSynced {
                target,
                tip,
                source,
            },
            Err(restore) => Error::NotRestored {
                target,
                tip,
                source,
                restore: Box::new(restore),
            },
        })
    }
}

fn rebasing(git: &Git) -> Result<bool, ProcessError> {
    for dir in ["rebase-merge", "rebase-apply"] {
        if git.git_path(dir)?.exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn unsynced(git: &Git, branch: &str, onto: &str, tip: &str) -> Result<Option<Unsynced>, Error> {
    if rebasing(git)? {
        return Ok(Some(Unsynced::Unfinished));
    }
    if git.head()?.branch.as_deref() != Some(branch) {
        return Ok(Some(Unsynced::LeftBranch));
    }
    match git.run(&["merge-base", "--is-ancestor", onto, "HEAD"]) {
        Err(ProcessError::Exit { code: 1, .. }) => return Ok(Some(Unsynced::NotOnto)),
        checked => checked?,
    };
    let range = format!("{onto}..HEAD");
    if !git.run(&["rev-list", "--merges", &range])?.is_empty() {
        return Ok(Some(Unsynced::Merges));
    }
    let unmerged = git.run(&["cherry", onto, tip])?;
    if unmerged.lines().any(|line| line.starts_with('+'))
        && git.run(&["rev-list", &range])?.is_empty()
    {
        return Ok(Some(Unsynced::Dropped));
    }
    update_submodules(git)?;
    if git.uncommitted()? {
        return Ok(Some(Unsynced::Uncommitted));
    }
    // A leftover marker is new against both sides, unlike a marker-like line that the branch or
    // the target already had.
    let against_tip = markers(git, tip)?;
    let against_onto = markers(git, onto)?;
    let mut marked: Vec<&str> = against_onto
        .iter()
        .filter(|&location| against_tip.contains(location))
        .filter_map(|location| location.rsplit_once(':'))
        .map(|(path, _)| path)
        .collect();
    marked.dedup();
    Ok((!marked.is_empty()).then(|| Unsynced::Markers(marked.join(", "))))
}

// Git finds markers in the lines HEAD adds, at the size the attributes give.
fn markers(git: &Git, from: &str) -> Result<Vec<String>, ProcessError> {
    let checked = match git.run(&["diff", "--check", from, "HEAD"]) {
        Err(error @ ProcessError::Exit { code: 2, .. }) => error.stdout().to_owned(),
        checked => checked?,
    };
    Ok(checked
        .lines()
        .filter_map(|line| line.strip_suffix(": leftover conflict marker"))
        .map(str::to_owned)
        .collect())
}

// A hard reset, unlike aborting, overwrites files the agent left in the way.
fn restore(git: &Git, branch: &str, tip: &str) -> Result<(), ProcessError> {
    if rebasing(git)? {
        git.run(&["rebase", "--quit"])?;
    }
    git.run(&["symbolic-ref", "HEAD", branch])?;
    git.run(&["reset", "--quiet", "--hard", tip])?;
    git.run(&["clean", "--quiet", "--force", "--force", "-d"])?;
    update_submodules(git)
}

// Rebasing and resetting move submodule pointers but leave the submodules checked out where they
// were. The checkouts were clean before the sync, so they can follow.
fn update_submodules(git: &Git) -> Result<(), ProcessError> {
    git.run(&["submodule", "update", "--quiet", "--recursive"])?;
    Ok(())
}

fn prompt(repo: &Path, onto: &str, target: &str) -> String {
    format!(
        "In the repository at `{}`, rebase the current branch onto commit {onto}, which is \
         {target}, with `git rebase {onto}`. Resolve each conflict so that both sides' changes \
         are kept as they were meant, reading the commit being applied with `git show \
         REBASE_HEAD` when that helps, stage the resolution, and continue with \
         `GIT_EDITOR=true git rebase --continue`, so that no editor opens, until the rebase is \
         done. Do not merge, push, or switch branches, and leave nothing uncommitted.",
        repo.display()
    )
}
