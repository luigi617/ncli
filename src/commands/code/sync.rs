use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;

use clap::Args;
use clap::builder::NonEmptyStringValueParser;

use super::git::{Git, Head, Operation, Upstream};
use crate::agent::{Agent, AgentKind};
use crate::commands::{Context, count};
use crate::error::Error;
use crate::process::{Cancellation, Cli, ProcessError};

const MARKERS: [&str; 3] = ["<<<<<<<", "|||||||", ">>>>>>>"];

/// Rebase the current branch onto another branch, resolving conflicts with an agent. If the
/// rebase cannot be finished, the branch is put back where it was.
#[derive(Debug, Args)]
pub struct Sync {
    /// Branch to rebase onto [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Unresolved {
    #[error(
        "the rebase stopped at {at} and continuing could not get past it ({stopped}; then {continued})"
    )]
    Stuck {
        at: String,
        stopped: Box<ProcessError>,
        continued: Box<ProcessError>,
    },
    #[error("the agent changed the rebase itself, not just the conflicted files")]
    Moved,
    #[error("the agent left unmerged paths ({0})")]
    Unmerged(String),
    #[error("the agent changed more than the staged resolution of the conflicted files ({0})")]
    Outside(String),
    #[error("the agent left conflict markers in {0}")]
    Markers(String),
}

#[derive(PartialEq, Eq)]
struct Stop {
    head: Head,
    rebase_head: Option<String>,
    state: BTreeMap<OsString, Vec<u8>>,
}

impl Sync {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let agent = AgentKind::first_available()?;
        let repo = Git::toplevel(context.cancellation)?;
        let git = Git {
            cli: Cli::new(["git"]).env("GIT_EDITOR", "true"),
            ..Git::new(&repo, context.cancellation)
        };
        git.ensure_idle()?;
        let head = git.head()?;
        let (Some(branch), Some(tip)) = (head.branch_name(), head.commit.as_deref()) else {
            return Err(Error::NotOnBranch(Box::new(head)));
        };
        let dirty = git.status()?;
        if !dirty.is_empty() {
            return Err(Error::UncommittedChanges {
                repo,
                paths: dirty
                    .into_iter()
                    .map(|(_, path)| path)
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        git.index_copy()?.1.refuse_hidden_edits()?;
        let hidden: HashSet<String> = git
            .index()?
            .hidden()
            .into_iter()
            .map(|(path, _)| path.to_owned())
            .collect();
        let from = git.upstream(self.from.as_deref())?;
        context.progress(&format!("fetching {from}"));
        let onto = git.fetch(&from)?;
        context.progress(&format!("rebasing {branch} onto {from}"));
        let source = match rebase(&git, &agent, &onto, branch, &from, context) {
            Ok(0) => return Ok(format!("Rebased {branch} onto {from}.")),
            Ok(resolved) => {
                return Ok(format!(
                    "Rebased {branch} onto {from}, resolving conflicts in {}.",
                    count(resolved, "commit")
                ));
            }
            Err(source) => Box::new(source),
        };
        let uncancelled = Cancellation::default();
        let git = Git {
            cancellation: &uncancelled,
            ..git
        };
        let (branch, from, tip) = (branch.to_owned(), from.to_string(), tip.to_owned());
        Err(match restore(&git, &branch, &tip, &hidden) {
            Ok(()) => Error::NotSynced {
                branch,
                from,
                tip,
                source,
            },
            Err(restore) => Error::NotRestored {
                branch,
                from,
                tip,
                source,
                restore: Box::new(restore),
            },
        })
    }
}

fn rebase(
    git: &Git,
    agent: &Agent,
    onto: &str,
    branch: &str,
    from: &Upstream,
    context: &Context,
) -> Result<usize, Error> {
    let mut resolved = 0;
    let mut previous = None;
    let mut step = git.run(&["rebase", "--quiet", "--merge", onto]);
    loop {
        let failure = match step {
            Ok(_) => return Ok(resolved),
            Err(error @ ProcessError::Exit { .. })
                if git.in_progress()? == Some(Operation::Rebase) =>
            {
                error
            }
            Err(error) => return Err(error.into()),
        };
        let stop = Stop::read(git)?;
        let conflicted = unmerged(git)?;
        // A stop with nothing unmerged, as when rerere resolved it, needs only continuing,
        // unless continuing already failed there.
        if !conflicted.is_empty() {
            resolve(git, agent, branch, from, context, &stop, &conflicted)?;
            resolved += 1;
        } else if let Some((_, stopped)) = previous.take_if(|(at, _)| *at == stop.rebase_head) {
            return Err(Unresolved::Stuck {
                at: stop.rebase_head.unwrap_or_default(),
                stopped: Box::new(stopped),
                continued: Box::new(failure),
            }
            .into());
        }
        previous = Some((stop.rebase_head, failure));
        step = git.run(&["rebase", "--continue"]);
    }
}

fn resolve(
    git: &Git,
    agent: &Agent,
    branch: &str,
    from: &Upstream,
    context: &Context,
    stop: &Stop,
    conflicted: &[String],
) -> Result<(), Error> {
    let before = git.index()?;
    let worktree = git.status()?;
    context.progress(&format!(
        "resolving {}",
        count(conflicted.len(), "conflict")
    ));
    agent.perform(
        &prompt(git, branch, from, stop, conflicted),
        context.cancellation,
    )?;
    if Stop::read(git)? != *stop {
        return Err(Unresolved::Moved.into());
    }
    let unmerged = unmerged(git)?;
    if !unmerged.is_empty() {
        return Err(Unresolved::Unmerged(unmerged.join(", ")).into());
    }
    let mut outside: Vec<String> = git
        .status()?
        .into_iter()
        .filter(|entry| !entry.0.ends_with(' ') && !worktree.contains(entry))
        .map(|(_, path)| path)
        .collect();
    let after = git.index()?;
    outside.extend(
        before
            .changed_paths(&after)
            .into_iter()
            .filter(|path| !conflicted.iter().any(|conflicted| conflicted == path))
            .chain(
                after
                    .0
                    .iter()
                    .filter(|(path, entry)| conflicted.contains(path) && !entry.starts_with("H "))
                    .map(|(path, _)| path.as_str()),
            )
            .map(str::to_owned),
    );
    outside.sort();
    outside.dedup();
    if !outside.is_empty() {
        return Err(Unresolved::Outside(outside.join(", ")).into());
    }
    let mut marked = Vec::new();
    for path in conflicted {
        let sides: HashSet<String> = [format!("HEAD:{path}"), format!("REBASE_HEAD:{path}")]
            .iter()
            .map(|rev| blob(git, rev))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .flat_map(|side| markers(side))
            .collect();
        let resolved = blob(git, &format!(":0:{path}"))?;
        if markers(&resolved).any(|line| !sides.contains(&line)) {
            marked.push(path.as_str());
        }
    }
    if !marked.is_empty() {
        return Err(Unresolved::Markers(marked.join(", ")).into());
    }
    Ok(())
}

impl Stop {
    fn read(git: &Git) -> Result<Self, Error> {
        let dir = git
            .cwd
            .join(git.run(&["rev-parse", "--git-path", "rebase-merge"])?);
        let state = match fs::read_dir(dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            entries => entries.and_then(|entries| {
                entries
                    .map(|entry| {
                        let entry = entry?;
                        Ok((entry.file_name(), fs::read(entry.path())?))
                    })
                    .collect()
            }),
        }
        .map_err(Error::RebaseState)?;
        Ok(Self {
            head: git.head()?,
            rebase_head: git.optional(&["rev-parse", "--verify", "--quiet", "REBASE_HEAD"])?,
            state,
        })
    }
}

fn unmerged(git: &Git) -> Result<Vec<String>, ProcessError> {
    let paths = git.run(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
    Ok(paths.split_terminator('\0').map(str::to_owned).collect())
}

// A gitlink, as a conflicted submodule has, names a commit with no lines to hold markers.
fn blob(git: &Git, rev: &str) -> Result<String, ProcessError> {
    let Some(id) = git.optional(&["rev-parse", "--verify", "--quiet", rev])? else {
        return Ok(String::new());
    };
    let object =
        git.run_with_input(&["cat-file", "--batch"], Some(format!("{id}\n").as_bytes()))?;
    Ok(match object.split_once('\n') {
        Some((header, contents)) if header.split(' ').nth(1) == Some("blob") => contents.to_owned(),
        _ => String::new(),
    })
}

fn markers(text: &str) -> impl Iterator<Item = String> {
    text.lines()
        .filter(|line| *line == "=======" || MARKERS.iter().any(|marker| line.starts_with(marker)))
        .map(str::to_owned)
}

fn restore(
    git: &Git,
    branch: &str,
    tip: &str,
    hidden: &HashSet<String>,
) -> Result<(), ProcessError> {
    let index = git.index()?;
    git.unhide(
        index
            .hidden()
            .into_iter()
            .map(|(path, _)| path)
            .filter(|path| !hidden.contains(*path)),
    )?;
    // Aborting checks out the old tip, which files the agent left in its way would block.
    git.run(&["reset", "--quiet", "--hard"])?;
    git.run(&["clean", "--quiet", "--force", "-d"])?;
    if git.in_progress()? == Some(Operation::Rebase) {
        git.run(&["rebase", "--abort"])?;
    }
    git.run(&["switch", "--quiet", "--discard-changes", branch])?;
    git.run(&["reset", "--quiet", "--hard", "--recurse-submodules", tip])?;
    git.run(&["clean", "--quiet", "--force", "-d"])?;
    Ok(())
}

fn prompt(git: &Git, branch: &str, from: &Upstream, stop: &Stop, conflicted: &[String]) -> String {
    format!(
        "The rebase of {branch} onto {from} in the repository at `{}` stopped with conflicts \
         while applying commit {}. Resolve the conflicts in exactly these files: {}. Read the \
         commit being applied with `git show REBASE_HEAD`, and keep the intent of both sides. \
         Remove every conflict marker, and stage each resolved file with `git add`, or with `git \
         rm` if the right resolution deletes it. Do not commit, and do not continue, skip, or \
         abort the rebase; ncli does that. Do not change, stage, or create any other file. In \
         report, say how you resolved each file.",
        git.cwd.display(),
        stop.rebase_head.as_deref().unwrap_or("HEAD"),
        conflicted.join(", ")
    )
}
