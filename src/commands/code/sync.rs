use std::path::Path;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;

use super::review::Git;
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;
use crate::process::ProcessError;

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
        let head = git.head()?;
        let (Some(branch), Some(tip)) = (head.branch.clone(), head.commit.clone()) else {
            return Err(Error::NotOnBranch(Box::new(head)));
        };
        if rebasing(&git)? {
            return Err(Error::Rebasing(repo));
        }
        if !git.run(&["status", "--porcelain"])?.is_empty() {
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
            .and_then(|performed| match unsynced(&git, &branch, &onto)? {
                None => Ok(()),
                Some(problem) => Err(Error::Unsynced {
                    agent: agent.name.clone(),
                    problem,
                    report: performed.report,
                }),
            });
        let Err(source) = result else {
            return Ok(format!("Rebased onto {target}."));
        };
        let source = Box::new(source);
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
        if git
            .cwd
            .join(git.run(&["rev-parse", "--git-path", dir])?)
            .exists()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn unsynced(git: &Git, branch: &str, onto: &str) -> Result<Option<Unsynced>, Error> {
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
    if !git
        .run(&["rev-list", "--merges", &format!("{onto}..HEAD")])?
        .is_empty()
    {
        return Ok(Some(Unsynced::Merges));
    }
    if !git.run(&["status", "--porcelain"])?.is_empty() {
        return Ok(Some(Unsynced::Uncommitted));
    }
    // Git finds markers in the lines the branch adds, at the size the attributes give.
    let checked = match git.run(&["diff", "--check", onto, "HEAD"]) {
        Err(error @ ProcessError::Exit { code: 2, .. }) => error.stdout().to_owned(),
        checked => checked?,
    };
    let mut marked: Vec<&str> = checked
        .lines()
        .filter(|line| line.ends_with("leftover conflict marker"))
        .filter_map(|line| line.rsplitn(3, ':').nth(2))
        .collect();
    marked.dedup();
    Ok((!marked.is_empty()).then(|| Unsynced::Markers(marked.join(", "))))
}

// A hard reset, unlike aborting, overwrites files the agent left in the way.
fn restore(git: &Git, branch: &str, tip: &str) -> Result<(), ProcessError> {
    if rebasing(git)? {
        git.run(&["rebase", "--quit"])?;
    }
    git.run(&["symbolic-ref", "HEAD", branch])?;
    git.run(&["reset", "--quiet", "--hard", tip])?;
    git.run(&["clean", "--quiet", "--force", "--force", "-d"])?;
    Ok(())
}

fn prompt(repo: &Path, onto: &str, target: &str) -> String {
    format!(
        "In the repository at `{}`, rebase the current branch onto commit {onto}, which is \
         {target}, with `git rebase {onto}`. Resolve each conflict so that both sides' changes \
         are kept as they were meant, reading the commit being applied with `git show \
         REBASE_HEAD` when that helps, stage the resolution, and continue until the rebase is \
         done. Do not merge, push, or switch branches, and leave nothing uncommitted.",
        repo.display()
    )
}
