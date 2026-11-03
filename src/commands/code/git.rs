use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

const PREFERRED_REMOTE: &str = "origin";
const REMOTES: &str = "refs/remotes/";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBranch {
    pub(super) remote: String,
    pub(super) branch: String,
}

impl RemoteBranch {
    // The full ref, since a local branch can share a remote branch's short name.
    pub(super) fn refname(&self) -> String {
        format!("{REMOTES}{}/{}", self.remote, self.branch)
    }
}

impl fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.remote, self.branch)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Sequence,
}

impl Operation {
    // A sequence of picks or reverts stays in progress after one of them is committed by hand.
    const MARKERS: [(&str, Self); 6] = [
        ("rebase-merge", Self::Rebase),
        ("rebase-apply", Self::Rebase),
        ("MERGE_HEAD", Self::Merge),
        ("CHERRY_PICK_HEAD", Self::CherryPick),
        ("REVERT_HEAD", Self::Revert),
        ("sequencer", Self::Sequence),
    ];

    // The git command that aborts the operation; `cherry-pick --abort` also ends a revert sequence.
    pub(super) fn command(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick | Self::Sequence => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sequence => f.write_str("cherry-pick or revert sequence"),
            operation => f.write_str(operation.command()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub(super) branch: Option<String>,
    pub(super) commit: Option<String>,
}

impl Head {
    pub(super) fn branch_name(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(branch))
    }
}

impl fmt::Display for Head {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.branch_name(), &self.commit) {
            (Some(branch), Some(commit)) => write!(f, "{branch} at {commit}"),
            (Some(branch), None) => write!(f, "{branch} with no commits"),
            (None, Some(commit)) => write!(f, "a detached HEAD at {commit}"),
            (None, None) => f.write_str("no commit"),
        }
    }
}

pub(super) struct Git<'a> {
    pub(super) cli: Cli,
    pub(super) cwd: &'a Path,
    pub(super) cancellation: &'a Cancellation,
}

impl<'a> Git<'a> {
    pub(super) fn new(cwd: &'a Path, cancellation: &'a Cancellation) -> Self {
        Self {
            cli: Cli::new(["git"]),
            cwd,
            cancellation,
        }
    }

    pub(super) fn toplevel(cancellation: &Cancellation) -> Result<PathBuf, Error> {
        let git = Git::new(Path::new("."), cancellation);
        process::require_executables([git.cli.executable()])?;
        match git.run(&["rev-parse", "--show-toplevel"]) {
            Err(error @ ProcessError::Exit { .. }) => Err(Error::NotAWorkTree(error)),
            repo => Ok(PathBuf::from(repo?)),
        }
    }

    pub(super) fn run(&self, args: &[&str]) -> Result<String, ProcessError> {
        self.run_with_input(args, None)
    }

    pub(super) fn run_with_input(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<String, ProcessError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let output = self
            .cli
            .run(&args, input, Some(self.cwd), self.cancellation)?;
        Ok(output.strip_suffix('\n').unwrap_or(&output).to_owned())
    }

    pub(super) fn head(&self) -> Result<Head, ProcessError> {
        Ok(Head {
            branch: self.optional(&["symbolic-ref", "--quiet", "HEAD"])?,
            commit: self.commit("HEAD")?,
        })
    }

    pub(super) fn commit(&self, rev: &str) -> Result<Option<String>, ProcessError> {
        self.optional(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])
    }

    pub(super) fn optional(&self, args: &[&str]) -> Result<Option<String>, ProcessError> {
        match self.run(args) {
            Err(ProcessError::Exit { code: 1, .. }) => Ok(None),
            found => found.map(Some),
        }
    }

    pub(super) fn in_progress(&self) -> Result<Option<Operation>, ProcessError> {
        let args: Vec<&str> = Operation::MARKERS
            .iter()
            .flat_map(|(marker, _)| ["--git-path", marker])
            .collect();
        let paths = self.run(&[&["rev-parse"], &args[..]].concat())?;
        Ok(paths
            .lines()
            .zip(Operation::MARKERS)
            .find(|(path, _)| self.cwd.join(path).exists())
            .map(|(_, (_, operation))| operation))
    }

    // Rejects a repository in the middle of an operation or with unresolved conflicts.
    pub(super) fn ensure_idle(&self) -> Result<(), Error> {
        if let Some(operation) = self.in_progress()? {
            return Err(Error::OperationInProgress {
                repo: self.cwd.to_owned(),
                operation,
            });
        }
        let unmerged = self.unmerged()?;
        if !unmerged.is_empty() {
            return Err(Error::UnmergedPaths {
                repo: self.cwd.to_owned(),
                paths: unmerged.join(", "),
            });
        }
        Ok(())
    }

    pub(super) fn is_ancestor(&self, ancestor: &str, rev: &str) -> Result<bool, ProcessError> {
        Ok(self
            .optional(&["merge-base", "--is-ancestor", ancestor, rev])?
            .is_some())
    }

    pub(super) fn unmerged(&self) -> Result<Vec<String>, ProcessError> {
        let entries = self.run(&["ls-files", "--unmerged", "-z"])?;
        let mut paths: Vec<String> = entries
            .split('\0')
            .filter_map(|entry| entry.split_once('\t'))
            .map(|(_, path)| path.to_owned())
            .collect();
        paths.dedup();
        Ok(paths)
    }

    // Each changed or untracked path.
    pub(super) fn uncommitted(&self) -> Result<Vec<String>, ProcessError> {
        let status = self.run(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
        let mut entries = status.split_terminator('\0');
        let mut paths = Vec::new();
        while let Some(entry) = entries.next() {
            let Some((state, path)) = entry.split_at_checked(3) else {
                continue;
            };
            if state.contains(['R', 'C']) {
                entries.next();
            }
            paths.push(path.to_owned());
        }
        Ok(paths)
    }

    // The named branch on whichever remote has it, or the remote's default branch.
    pub(super) fn remote_branch(&self, name: Option<&str>) -> Result<RemoteBranch, Error> {
        let Some(name) = name else {
            return self.default_branch();
        };
        let mut found = Vec::new();
        let mut unreachable = None;
        for remote in self.run(&["remote"])?.lines() {
            match self.remote_has(remote, name) {
                Ok(true) => found.push(RemoteBranch {
                    remote: remote.to_owned(),
                    branch: name.to_owned(),
                }),
                Ok(false) => {}
                Err(error) => {
                    unreachable.get_or_insert(error);
                }
            }
        }
        // An unreachable remote is ignored only when its answer could not change the choice.
        if let Some(error) = unreachable
            && !found.iter().any(|branch| branch.remote == PREFERRED_REMOTE)
        {
            return Err(error.into());
        }
        preferred(found, |branch| &branch.remote).map_err(|found| {
            if found.is_empty() {
                Error::NoRemoteBranch(name.to_owned())
            } else {
                Error::AmbiguousRemoteBranch {
                    branch: name.to_owned(),
                    remotes: listed(&found),
                    preferred: PREFERRED_REMOTE,
                }
            }
        })
    }

    // Asks the remote itself, since its remote-tracking refs may not have been fetched.
    fn remote_has(&self, remote: &str, branch: &str) -> Result<bool, ProcessError> {
        let refname = format!("refs/heads/{branch}");
        let listed = self.run(&["ls-remote", "--heads", remote, &refname])?;
        Ok(listed.lines().any(|line| {
            line.split_once('\t')
                .is_some_and(|(_, name)| name == refname)
        }))
    }

    pub(super) fn remote_default(&self, remote: &str) -> Result<Option<String>, ProcessError> {
        let listed = self.run(&["ls-remote", "--symref", remote, "HEAD"])?;
        Ok(listed.lines().find_map(|line| {
            let target = line.strip_prefix("ref: ")?.strip_suffix("\tHEAD")?;
            target.strip_prefix("refs/heads/").map(str::to_owned)
        }))
    }

    // The fetched commit, which later fetches cannot move as they can the remote-tracking ref.
    pub(super) fn fetch(&self, branch: &RemoteBranch) -> Result<String, ProcessError> {
        let refspec = format!("+refs/heads/{}:{}", branch.branch, branch.refname());
        self.run(&["fetch", "--quiet", &branch.remote, &refspec])?;
        self.run(&[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", branch.refname()),
        ])
    }

    fn default_branch(&self) -> Result<RemoteBranch, Error> {
        let found: Vec<RemoteBranch> = self
            .remote_refs()?
            .into_iter()
            .filter_map(|(name, target)| {
                let remote = name.strip_prefix(REMOTES)?.strip_suffix("/HEAD")?;
                let branch = target.strip_prefix(&format!("{REMOTES}{remote}/"))?;
                Some(RemoteBranch {
                    remote: remote.to_owned(),
                    branch: branch.to_owned(),
                })
            })
            .collect();
        if found.is_empty() {
            return self.reported_default_branch();
        }
        preferred(found, |branch| &branch.remote).map_err(|found| Error::AmbiguousDefaultBranch {
            branches: listed(&found),
            preferred: PREFERRED_REMOTE,
        })
    }

    // A repository not made by `git clone` has no `<remote>/HEAD`, so the remote is asked instead.
    fn reported_default_branch(&self) -> Result<RemoteBranch, Error> {
        let remotes = self.run(&["remote"])?;
        let remote = preferred(remotes.lines().collect(), |remote| remote)
            .map_err(|_| Error::NoDefaultBranch)?;
        let branch = self.remote_default(remote)?.ok_or(Error::NoDefaultBranch)?;
        Ok(RemoteBranch {
            remote: remote.to_owned(),
            branch,
        })
    }

    // Each remote-tracking ref with the ref it points to, if it is symbolic.
    fn remote_refs(&self) -> Result<Vec<(String, String)>, ProcessError> {
        let output = self.run(&["for-each-ref", "--format=%(refname) %(symref)", REMOTES])?;
        Ok(output
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(name, target)| (name.to_owned(), target.to_owned()))
            .collect())
    }
}

fn preferred<T>(mut found: Vec<T>, remote: impl Fn(&T) -> &str) -> Result<T, Vec<T>> {
    if found.len() == 1 {
        return Ok(found.remove(0));
    }
    match found
        .iter()
        .position(|item| remote(item) == PREFERRED_REMOTE)
    {
        Some(index) => Ok(found.swap_remove(index)),
        None => Err(found),
    }
}

fn listed(branches: &[RemoteBranch]) -> String {
    branches
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
