use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::{fs, io};

use clap::Args;
use clap::builder::NonEmptyStringValueParser;
use serde::Deserialize;

use super::git::{Git, RemoteBranch};
use crate::agent::{Agent, AgentKind, Performed, task_schema};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{Cancellation, Cli, ProcessError};

const MARKERS: [u8; 3] = [b'<', b'|', b'>'];
const DEFAULT_MARKER_SIZE: usize = 7;
const GITLINK_MODE: &str = "160000";

static SCHEMA: LazyLock<String> =
    LazyLock::new(|| task_schema(include_str!("resolution.schema.json")));

/// Rebase the current branch onto another branch, resolving conflicts with an agent.
#[derive(Debug, Args)]
pub struct Sync {
    /// Branch to rebase onto [default: the remote's default branch]
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub from: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolved {
    kept: Vec<String>,
}

struct Resolution {
    commit: String,
    paths: Vec<String>,
    step: Performed,
}

struct Conflict {
    path: String,
    marker_size: usize,
    submodule: bool,
    before: Option<Entry>,
}

impl Conflict {
    // A conflict git could not mark, such as a deletion or a binary file, is resolved by changing
    // the file, or by the agent saying it keeps the file as git left it.
    fn resolved(&self, after: Option<&Entry>, kept: &[String]) -> bool {
        let before = self.before.as_ref();
        !has_markers(after, self.marker_size)
            && (has_markers(before, self.marker_size)
                || after != before
                || kept.contains(&self.path))
    }
}

#[derive(PartialEq)]
enum Entry {
    File(Vec<u8>),
    Link(PathBuf),
    Submodule(Option<String>),
}

struct Rebase<'a> {
    git: &'a Git<'a>,
    repo: &'a Path,
    branch: &'a str,
    from: &'a RemoteBranch,
    agent: &'a Agent,
    context: &'a Context<'a>,
    resolutions: Vec<Resolution>,
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
        let dirty: Vec<String> = status(&git)?.into_iter().map(|(_, path)| path).collect();
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
        let mut rebase = Rebase {
            git: &git,
            repo: &repo,
            branch,
            from: &from,
            agent: &agent,
            context,
            resolutions: Vec::new(),
        };
        rebase.run()?;
        let new = git.run(&["rev-parse", "HEAD"])?;
        let summary = if new == *old {
            format!("{branch} is up to date with {from}, at {old}.")
        } else {
            format!("Rebased {branch} onto {from}, from {old} to {new}.")
        };
        Ok([summary]
            .into_iter()
            .chain(rebase.resolutions.iter().map(|resolution| {
                format!(
                    "Resolved the conflicts in {} while applying {}:\n\n{}",
                    resolution.paths.join(", "),
                    resolution.commit,
                    resolution.step
                )
            }))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

impl Rebase<'_> {
    fn run(&mut self) -> Result<(), Error> {
        let result = self.rebase();
        let Err(error) = result else {
            return Ok(());
        };
        // Not cancellable: an interrupted run still puts the branch back where it was.
        let uncancellable = Cancellation::default();
        let git = Git::new(self.repo, &uncancellable);
        if git.in_progress()?.is_none() {
            return Err(error);
        }
        match git.run(&["rebase", "--abort"]) {
            Ok(_) => Err(error),
            Err(abort) => Err(Error::RebaseNotAborted {
                error: Box::new(error),
                abort: Box::new(abort),
            }),
        }
    }

    fn rebase(&mut self) -> Result<(), Error> {
        let mut stopped = self.git.run(&["rebase", "--quiet", &self.from.refname()]);
        while let Err(error) = stopped {
            let paths = self.git.unmerged()?;
            if paths.is_empty() {
                return Err(self.resumable(error.into()));
            }
            let commit = self
                .git
                .run(&["log", "-1", "--format=%h %s", "REBASE_HEAD"])
                .map_err(|error| self.resumable(error.into()))?;
            self.resolve(commit, paths)?;
            // Git keeps the commit message without starting an editor when the editor is `:`.
            let continuing = Git {
                cli: Cli::new(["git"]).env("GIT_EDITOR", ":"),
                ..*self.git
            };
            stopped = continuing.run(&["rebase", "--continue"]);
        }
        Ok(())
    }

    fn resolve(&mut self, commit: String, paths: Vec<String>) -> Result<(), Error> {
        self.context
            .progress(&format!("resolving conflicts in {}", paths.join(", ")));
        let conflicts = self
            .conflicts(&paths)
            .map_err(|error| self.resumable(error))?;
        let before = self.git.head()?;
        let index = staged(self.git, &paths).map_err(|error| self.resumable(error.into()))?;
        let (step, Resolved { kept }) = self
            .agent
            .perform_as::<Resolved>(
                &self.prompt(&commit, &paths),
                &SCHEMA,
                self.context.cancellation,
            )
            .map_err(|error| self.resumable(error))?;
        let resumable = |error: Error| error.resumable(&step.resume);
        let after = self.git.head().map_err(|error| resumable(error.into()))?;
        if after != before {
            return Err(resumable(Error::HeadMoved {
                repo: self.repo.to_owned(),
                from: Box::new(before),
                to: Box::new(after),
            }));
        }
        let unresolved = |paths: Vec<String>| {
            resumable(Error::UnresolvedConflicts {
                commit: commit.clone(),
                paths: paths.join(", "),
                report: step.report.clone(),
            })
        };
        let mut marked = Vec::new();
        for conflict in &conflicts {
            let after = self
                .read(&conflict.path, conflict.submodule)
                .map_err(resumable)?;
            if !conflict.resolved(after.as_ref(), &kept) {
                marked.push(conflict.path.clone());
            }
        }
        if !marked.is_empty() {
            return Err(unresolved(marked));
        }
        let add: Vec<&str> = ["--literal-pathspecs", "add", "--all", "--sparse", "--"]
            .into_iter()
            .chain(paths.iter().map(String::as_str))
            .collect();
        self.git
            .run(&add)
            .map_err(|error| resumable(error.into()))?;
        let remaining = self
            .git
            .unmerged()
            .map_err(|error| resumable(error.into()))?;
        if !remaining.is_empty() {
            return Err(unresolved(remaining));
        }
        let reindexed = staged(self.git, &paths).map_err(|error| resumable(error.into()))?;
        let outside: BTreeSet<String> = status(self.git)
            .map_err(|error| resumable(error.into()))?
            .into_iter()
            .filter(|(state, _)| !state.ends_with(' '))
            .map(|(_, path)| path)
            .chain(
                index
                    .symmetric_difference(&reindexed)
                    .map(|(path, _)| path.clone()),
            )
            .collect();
        if !outside.is_empty() {
            return Err(resumable(Error::EditedOutsideConflicts {
                commit,
                paths: outside.into_iter().collect::<Vec<_>>().join(", "),
                report: step.report.clone(),
            }));
        }
        self.resolutions.push(Resolution {
            commit,
            paths,
            step,
        });
        Ok(())
    }

    fn conflicts(&self, paths: &[String]) -> Result<Vec<Conflict>, Error> {
        let check: Vec<&str> = ["check-attr", "-z", "conflict-marker-size", "--"]
            .into_iter()
            .chain(paths.iter().map(String::as_str))
            .collect();
        let attributes = self.git.run(&check)?;
        let submodules: BTreeSet<String> = self
            .git
            .unmerged_entries()?
            .into_iter()
            .filter(|(mode, _)| mode == GITLINK_MODE)
            .map(|(_, path)| path)
            .collect();
        let fields: Vec<&str> = attributes.split_terminator('\0').collect();
        fields
            .chunks_exact(3)
            .map(|fields| {
                let [path, _, size] = fields else {
                    unreachable!("chunks_exact yields three fields")
                };
                let submodule = submodules.contains(*path);
                Ok(Conflict {
                    path: (*path).to_owned(),
                    marker_size: size
                        .parse()
                        .ok()
                        .filter(|&size| size > 0)
                        .unwrap_or(DEFAULT_MARKER_SIZE),
                    submodule,
                    before: self.read(path, submodule)?,
                })
            })
            .collect()
    }

    // A submodule is read as the commit checked out in it, which is what git stages for it.
    fn read(&self, path: &str, submodule: bool) -> Result<Option<Entry>, Error> {
        if !submodule {
            return read(self.repo, path);
        }
        let path = self.repo.join(path);
        let exists = |path: &Path| match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::ReadConflicted {
                path: path.to_owned(),
                source,
            }),
        };
        if !exists(&path)? {
            return Ok(None);
        }
        // Without its own repository, git would look up the enclosing one instead.
        if !exists(&path.join(".git"))? {
            return Ok(Some(Entry::Submodule(None)));
        }
        let commit = Git::new(&path, self.context.cancellation).commit("HEAD")?;
        Ok(Some(Entry::Submodule(commit)))
    }

    fn resumable(&self, error: Error) -> Error {
        match self.resolutions.last() {
            Some(resolution) => error.resumable(&resolution.step.resume),
            None => error,
        }
    }

    fn prompt(&self, commit: &str, paths: &[String]) -> String {
        format!(
            "A rebase of the branch {branch} onto {from} in the repository at `{repo}` stopped \
             with conflicts while applying the commit {commit}. Resolve the conflicts in these \
             files by editing them: {paths}.\n\n\
             In each file, replace every conflict marker and the text between them with code that \
             keeps the intent of both sides: what the commit changes, and what changed on {from}. \
             A conflict without markers, such as a file deleted on one side or a binary file, is \
             resolved by changing or deleting the file, or by keeping it as it is. A conflicted \
             submodule is resolved by checking out, inside the submodule, the commit to keep, or \
             by keeping the commit it has. List in kept each such path you keep as it is, and \
             leave kept empty otherwise. Edit no other files. \
             See the commit with `git show REBASE_HEAD`, and the changes on {from} with `git log \
             -p REBASE_HEAD..HEAD -- <path>`. Do not stage, commit, or run git rebase, merge, \
             checkout, switch, reset, or stash, except to check out a commit inside a conflicted \
             submodule: ncli stages the files and continues the rebase. \
             In report, say for each file how you resolved it.",
            branch = self.branch,
            from = self.from,
            repo = self.repo.display(),
            paths = paths.join(", "),
        )
    }
}

// Each changed or untracked path with its two-letter status: staged, then unstaged.
fn status(git: &Git) -> Result<Vec<(String, String)>, ProcessError> {
    let status = git.run(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
    let mut entries = status.split('\0');
    let mut paths = Vec::new();
    while let Some(entry) = entries.next() {
        let Some((state, path)) = entry.split_at_checked(3) else {
            continue;
        };
        if state.contains(['R', 'C']) {
            entries.next();
        }
        paths.push((state[..2].to_owned(), path.to_owned()));
    }
    Ok(paths)
}

// Each index entry outside the conflicts, as its path with its mode, object, and stage.
fn staged(git: &Git, conflicts: &[String]) -> Result<BTreeSet<(String, String)>, ProcessError> {
    let entries = git.run(&["ls-files", "--stage", "-z"])?;
    Ok(entries
        .split_terminator('\0')
        .filter_map(|entry| entry.split_once('\t'))
        .filter(|(_, path)| !conflicts.iter().any(|conflict| conflict == path))
        .map(|(entry, path)| (path.to_owned(), entry.to_owned()))
        .collect())
}

// None if the path does not exist, as when the conflict deleted it. A symlink is read as the
// link itself, which is what git tracks.
fn read(repo: &Path, path: &str) -> Result<Option<Entry>, Error> {
    let path = repo.join(path);
    let entry = fs::symlink_metadata(&path).and_then(|metadata| {
        if metadata.is_symlink() {
            fs::read_link(&path).map(Entry::Link)
        } else {
            fs::read(&path).map(Entry::File)
        }
    });
    match entry {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::ReadConflicted { path, source }),
    }
}

fn has_markers(entry: Option<&Entry>, size: usize) -> bool {
    let Some(Entry::File(contents)) = entry else {
        return false;
    };
    contents.split(|&byte| byte == b'\n').any(|line| {
        MARKERS.iter().any(|&marker| {
            line.len() >= size
                && line[..size].iter().all(|&byte| byte == marker)
                && matches!(line.get(size), None | Some(b' ' | b'\r'))
        })
    })
}
