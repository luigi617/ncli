use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

const PREFERRED_REMOTE: &str = "origin";
const REMOTES: &str = "refs/remotes/";
const OPERATIONS: [(&str, Operation); 6] = [
    ("rebase-merge", Operation::Rebase),
    ("rebase-apply", Operation::Rebase),
    ("MERGE_HEAD", Operation::Merge),
    ("CHERRY_PICK_HEAD", Operation::CherryPick),
    ("REVERT_HEAD", Operation::Revert),
    ("sequencer", Operation::Sequencer),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Rebase,
    Merge,
    CherryPick,
    Revert,
    Sequencer,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Sequencer => "cherry-pick or revert",
        })
    }
}

// Kept as the full ref, since a local branch can share a remote branch's short name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteBranch(pub(super) String);

impl fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.strip_prefix(REMOTES).unwrap_or(&self.0))
    }
}

#[derive(Clone, Debug)]
pub(super) struct Upstream {
    pub(super) remote: String,
    pub(super) branch: String,
}

impl Upstream {
    pub(super) fn named(&self, branch: Option<&str>) -> Self {
        Self {
            remote: self.remote.clone(),
            branch: branch.unwrap_or(&self.branch).to_owned(),
        }
    }

    pub(super) fn tracking(&self) -> RemoteBranch {
        RemoteBranch(format!("{REMOTES}{}/{}", self.remote, self.branch))
    }
}

impl fmt::Display for Upstream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.remote, self.branch)
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

// Each path with its index entry, tagged to show the skip-worktree and assume-unchanged flags.
pub(super) struct Index(pub(super) HashSet<(String, String)>);

impl Index {
    pub(super) fn changed_paths<'a>(&'a self, other: &'a Self) -> HashSet<&'a str> {
        self.0
            .symmetric_difference(&other.0)
            .map(|(path, _)| path.as_str())
            .collect()
    }

    // Each path git does not compare with the working tree, and whether it is skip-worktree.
    pub(super) fn hidden(&self) -> HashSet<(&str, bool)> {
        self.0
            .iter()
            .filter_map(|(path, entry)| {
                let tag = entry.chars().next()?;
                (tag == 'S' || tag.is_ascii_lowercase())
                    .then_some((path.as_str(), tag.eq_ignore_ascii_case(&'s')))
            })
            .collect()
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

    pub(super) fn optional(&self, args: &[&str]) -> Result<Option<String>, ProcessError> {
        match self.run(args) {
            Err(ProcessError::Exit { code: 1, .. }) => Ok(None),
            found => found.map(Some),
        }
    }

    pub(super) fn head(&self) -> Result<Head, ProcessError> {
        Ok(Head {
            branch: self.optional(&["symbolic-ref", "--quiet", "HEAD"])?,
            commit: self.optional(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?,
        })
    }

    pub(super) fn index(&self) -> Result<Index, ProcessError> {
        let entries = self.run(&["ls-files", "--stage", "-v", "-z"])?;
        Ok(Index(
            entries
                .split_terminator('\0')
                .filter_map(|entry| entry.split_once('\t'))
                .map(|(entry, path)| (path.to_owned(), entry.to_owned()))
                .collect(),
        ))
    }

    pub(super) fn unhide<'p>(
        &self,
        paths: impl IntoIterator<Item = &'p str>,
    ) -> Result<(), ProcessError> {
        let paths: String = paths.into_iter().map(|path| format!("{path}\0")).collect();
        if paths.is_empty() {
            return Ok(());
        }
        // With --stdin, update-index applies only one of several flags, so each has its own call.
        for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
            self.run_with_input(
                &["update-index", flag, "-z", "--stdin"],
                Some(paths.as_bytes()),
            )?;
        }
        Ok(())
    }

    // Each changed or untracked path, with its two-letter status.
    pub(super) fn status(&self) -> Result<Vec<(String, String)>, ProcessError> {
        let status = self.run(&[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ])?;
        Ok(status
            .split_terminator('\0')
            .filter_map(|entry| entry.split_at_checked(3))
            .map(|(code, path)| (code[..2].to_owned(), path.to_owned()))
            .collect())
    }

    pub(super) fn in_progress(&self) -> Result<Option<Operation>, ProcessError> {
        let args: Vec<&str> = ["rev-parse"]
            .into_iter()
            .chain(
                OPERATIONS
                    .iter()
                    .flat_map(|(marker, _)| ["--git-path", marker]),
            )
            .collect();
        let paths = self.run(&args)?;
        Ok(paths
            .lines()
            .zip(OPERATIONS)
            .find(|(path, _)| self.cwd.join(path).exists())
            .map(|(_, (_, operation))| operation))
    }

    pub(super) fn ensure_idle(&self) -> Result<(), Error> {
        match self.in_progress()? {
            Some(operation) => Err(Error::OperationInProgress {
                repo: self.cwd.to_owned(),
                operation,
            }),
            None => Ok(()),
        }
    }

    // The fetched commit, which later fetches cannot move as they can the remote-tracking ref.
    pub(super) fn fetch(&self, upstream: &Upstream) -> Result<String, ProcessError> {
        let tracking = upstream.tracking().0;
        let refspec = format!("+refs/heads/{}:{tracking}", upstream.branch);
        self.run(&["fetch", "--quiet", &upstream.remote, &refspec])?;
        self.run(&["rev-parse", "--verify", &format!("{tracking}^{{commit}}")])
    }

    pub(super) fn default_branch(&self) -> Result<RemoteBranch, Error> {
        self.default_upstream().map(|upstream| upstream.tracking())
    }

    // The default branch, or None when the remotes do not settle on one.
    pub(super) fn known_default(&self) -> Result<Option<Upstream>, Error> {
        match self.default_upstream() {
            Err(Error::NoDefaultBranch | Error::AmbiguousDefaultBranch { .. }) => Ok(None),
            default => default.map(Some),
        }
    }

    // A named branch needs no default branch when the repository has one remote, or origin.
    pub(super) fn upstream(&self, branch: Option<&str>) -> Result<Upstream, Error> {
        let branch = match (self.known_default()?, branch) {
            (Some(default), branch) => return Ok(default.named(branch)),
            (None, None) => return self.default_upstream(),
            (None, Some(branch)) => branch,
        };
        let remotes = self.run(&["remote"])?;
        let remotes: Vec<&str> = remotes.lines().collect();
        let remote = match remotes[..] {
            [remote] => remote,
            _ if remotes.contains(&PREFERRED_REMOTE) => PREFERRED_REMOTE,
            _ => return self.default_upstream(),
        };
        Ok(Upstream {
            remote: remote.to_owned(),
            branch: branch.to_owned(),
        })
    }

    fn default_upstream(&self) -> Result<Upstream, Error> {
        let output = self.run(&["for-each-ref", "--format=%(refname) %(symref)", REMOTES])?;
        let mut heads: Vec<Upstream> = output
            .lines()
            .filter_map(|line| {
                let (name, target) = line.split_once(' ')?;
                let remote = name.strip_prefix(REMOTES)?.strip_suffix("/HEAD")?;
                let branch = target
                    .strip_prefix(REMOTES)?
                    .strip_prefix(remote)?
                    .strip_prefix('/')?;
                Some(Upstream {
                    remote: remote.to_owned(),
                    branch: branch.to_owned(),
                })
            })
            .collect();
        match heads.len() {
            0 => Err(Error::NoDefaultBranch),
            1 => Ok(heads.remove(0)),
            _ => match heads
                .iter()
                .position(|head| head.remote == PREFERRED_REMOTE)
            {
                Some(index) => Ok(heads.swap_remove(index)),
                None => Err(Error::AmbiguousDefaultBranch {
                    branches: heads
                        .iter()
                        .map(Upstream::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    preferred: PREFERRED_REMOTE,
                }),
            },
        }
    }
}
