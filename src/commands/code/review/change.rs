use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::commands::code::git::{Git, Head, Index, RemoteBranch};
use crate::error::Error;
use crate::process::{Cancellation, Cli, ProcessError};

// Keeps the user's diff settings and drivers from hiding parts of the change.
const DIFF: [&str; 4] = [
    "diff",
    "--ignore-submodules=none",
    "--no-ext-diff",
    "--no-textconv",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Link(String),
    Uncommitted {
        repo: PathBuf,
        base: Base,
        snapshot: String,
    },
    Branch {
        repo: PathBuf,
        base: RemoteBranch,
        merge_base: String,
        head: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Base {
    Commit(String),
    Tree(String),
}

impl Base {
    fn id(&self) -> &str {
        match self {
            Self::Commit(id) | Self::Tree(id) => id,
        }
    }
}

impl fmt::Display for Base {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(commit) => f.write_str(commit),
            Self::Tree(tree) => write!(f, "tree {tree}"),
        }
    }
}

impl Git<'_> {
    fn empty_tree(&self) -> Result<String, ProcessError> {
        self.run_with_input(&["hash-object", "-t", "tree", "--stdin"], Some(b""))
    }

    // A staged change is undone when the working tree matches the base there, so the snapshot
    // misses the index entry, and that entry is neither committed nor in the earlier index.
    fn snapshot(
        &self,
        base: &str,
        committed: &str,
        earlier: Option<&Index>,
    ) -> Result<(String, Index), Error> {
        let dir = tempfile::Builder::new()
            .prefix("ncli-snapshot-")
            .tempdir()
            .map_err(Error::Snapshot)?;
        let index = dir.path().join("index");
        let real_index = self
            .cwd
            .join(self.run(&["rev-parse", "--git-path", "index"])?);
        copy_index(&real_index, &index).map_err(Error::Snapshot)?;
        let git = Git {
            cli: Cli::new(["git"]).env("GIT_INDEX_FILE", &index),
            ..*self
        };
        let hidden = git.hidden_edits()?;
        if !hidden.is_empty() {
            return Err(Error::HiddenEdits(hidden.join(", ")));
        }
        let nested = git.nested_repositories()?;
        if !nested.is_empty() {
            return Err(Error::NestedRepositories(nested.join(", ")));
        }
        let index = git.index()?;
        let uncommitted = git.changed_paths(&["--cached", committed])?;
        git.run(&["add", "--all", "--sparse"])?;
        let snapshot = git.run(&["write-tree"])?;
        let snapshotted = git.index()?;
        let missed = index.changed_paths(&snapshotted);
        let restaged = earlier.map(|earlier| index.changed_paths(earlier));
        let kept = git.changed_paths(&[base, &snapshot])?;
        let kept: HashSet<&str> = kept.split('\0').collect();
        let undone: Vec<&str> = uncommitted
            .split('\0')
            .filter(|path| {
                !path.is_empty()
                    && missed.contains(path)
                    && restaged
                        .as_ref()
                        .is_none_or(|restaged| restaged.contains(path))
                    && !kept.contains(path)
            })
            .collect();
        if !undone.is_empty() {
            return Err(Error::UndoneStagedChanges(undone.join(", ")));
        }
        Ok((snapshot, index))
    }

    fn changed_paths(&self, diff: &[&str]) -> Result<String, ProcessError> {
        let args = [&DIFF[..], &["--no-renames", "--name-only", "-z"], diff].concat();
        self.run(&args)
    }

    // Clears the assume-unchanged and skip-worktree bits, so it must only run on a copy of the
    // index. A skip-worktree file missing from disk is left out by a sparse checkout, not edited.
    fn hidden_edits(&self) -> Result<Vec<String>, ProcessError> {
        let index = self.index()?;
        let marked: HashSet<&str> = index
            .hidden()
            .into_iter()
            .filter(|&(path, skip)| !skip || self.cwd.join(path).symlink_metadata().is_ok())
            .map(|(path, _)| path)
            .collect();
        if marked.is_empty() {
            return Ok(Vec::new());
        }
        self.unhide(marked.iter().copied())?;
        let modified = self.changed_paths(&[])?;
        Ok(modified
            .split('\0')
            .filter(|path| marked.contains(path))
            .map(str::to_owned)
            .collect())
    }

    // Untracked files are listed one by one, so only a nested repository is listed as a directory.
    fn nested_repositories(&self) -> Result<Vec<String>, ProcessError> {
        let untracked = self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        Ok(untracked
            .split('\0')
            .filter_map(|path| path.strip_suffix('/'))
            .map(str::to_owned)
            .collect())
    }

    fn dirty_submodules(&self) -> Result<Vec<String>, ProcessError> {
        let status = self.run(&[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=none",
        ])?;
        let mut entries = status.split('\0');
        let mut dirty = Vec::new();
        while let Some(entry) = entries.next() {
            let fields = match entry.as_bytes().first() {
                Some(b'1') => 9,
                Some(b'2') => {
                    entries.next();
                    10
                }
                Some(b'u') => 11,
                _ => continue,
            };
            let fields: Vec<&str> = entry.splitn(fields, ' ').collect();
            let state = fields.get(2).copied().unwrap_or_default();
            if state.starts_with('S') && state.contains('M') {
                dirty.extend(fields.last().map(|&path| path.to_owned()));
            }
        }
        Ok(dirty)
    }
}

// Git distrusts cached stat data newer than the index file, so the copy keeps the original mtime.
fn copy_index(from: &Path, to: &Path) -> io::Result<()> {
    let modified = match fs::metadata(from) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        metadata => metadata?.modified()?,
    };
    fs::copy(from, to)?;
    fs::File::options()
        .write(true)
        .open(to)?
        .set_modified(modified)
}

pub struct Worktree {
    pub repo: PathBuf,
    pub head: Head,
    base: Base,
    pub tree: String,
    index: Index,
}

impl Worktree {
    pub fn snapshot(cancellation: &Cancellation) -> Result<Self, Error> {
        Self::take(None, cancellation)
    }

    // The earlier snapshot is the base, so changes staged before it count as part of the base.
    pub fn since(&self, cancellation: &Cancellation) -> Result<Self, Error> {
        Self::take(Some(self), cancellation)
    }

    pub fn change(&self) -> Change {
        Change::Uncommitted {
            repo: self.repo.clone(),
            base: self.base.clone(),
            snapshot: self.tree.clone(),
        }
    }

    fn take(earlier: Option<&Self>, cancellation: &Cancellation) -> Result<Self, Error> {
        let repo = Git::toplevel(cancellation)?;
        let git = Git::new(&repo, cancellation);
        let head = git.head()?;
        let committed = match &head.commit {
            Some(head) => head.clone(),
            None => git.empty_tree()?,
        };
        let base = match (earlier, &head.commit) {
            (Some(earlier), _) => Base::Tree(earlier.tree.clone()),
            (None, Some(head)) => Base::Commit(head.clone()),
            (None, None) => Base::Tree(committed.clone()),
        };
        let submodules = git.dirty_submodules()?;
        if !submodules.is_empty() {
            return Err(Error::DirtySubmodules {
                repo,
                submodules: submodules.join(", "),
            });
        }
        let (tree, index) =
            git.snapshot(base.id(), &committed, earlier.map(|earlier| &earlier.index))?;
        Ok(Self {
            repo,
            head,
            base,
            tree,
            index,
        })
    }
}

impl Change {
    pub fn local(cancellation: &Cancellation) -> Result<Self, Error> {
        let Worktree {
            repo,
            head,
            base,
            tree,
            ..
        } = Worktree::snapshot(cancellation).map_err(|error| match error {
            Error::NotAWorkTree(source) => Error::NoLocalChange(source),
            error => error,
        })?;
        let git = Git::new(&repo, cancellation);
        if tree != git.run(&["rev-parse", &format!("{}^{{tree}}", base.id())])? {
            return Ok(Self::Uncommitted {
                repo,
                base,
                snapshot: tree,
            });
        }
        let Some(head) = head.commit else {
            return Err(Error::NoCommits(repo));
        };
        let base = git.default_branch()?;
        let Some(merge_base) = git.optional(&["merge-base", &base.0, &head])? else {
            return Err(Error::NoCommonHistory { repo, base });
        };
        if merge_base == head {
            return Err(Error::NoChanges { repo, base });
        }
        Ok(Self::Branch {
            repo,
            base,
            merge_base,
            head,
        })
    }

    pub(super) fn access(&self) -> String {
        let diff = DIFF.join(" ");
        match self {
            Self::Link(_) => "Fetch it yourself, using your own tools and skills.".to_owned(),
            Self::Uncommitted {
                repo,
                base,
                snapshot,
            } => {
                let base = base.id();
                format!(
                    "Read it yourself by running `git {diff} {base} {snapshot}` in the repository at \
                 `{}`, and read files with `git show {snapshot}:<path>`, not from the working tree, \
                 which may have changed since. {}",
                    repo.display(),
                    binary(base, snapshot)
                )
            }
            Self::Branch {
                repo,
                merge_base,
                head,
                ..
            } => format!(
                "Read it yourself by running `git log {merge_base}..{head}` and `git {diff} \
                 {merge_base} {head}` in the repository at `{}`, and read files with `git show \
                 {head}:<path>`, not from the working tree, which may have changed since. {}",
                repo.display(),
                binary(merge_base, head)
            ),
        }
    }
}

// An attribute can make the diff call a text file binary, which would hide its edits.
fn binary(from: &str, to: &str) -> String {
    format!(
        "The diff may call a text file binary; read such a file with `git show {from}:<path>` and \
         `git show {to}:<path>` and compare them."
    )
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(link) => write!(f, "the change at {link}"),
            Self::Uncommitted { repo, base, .. } => {
                write!(
                    f,
                    "the uncommitted changes in {} on top of {base}",
                    repo.display()
                )
            }
            Self::Branch {
                repo, base, head, ..
            } => {
                write!(
                    f,
                    "the commits up to {head} since it branched from {base} in {}",
                    repo.display()
                )
            }
        }
    }
}
