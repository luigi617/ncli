use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

const PREFERRED_REMOTE: &str = "origin";
const REMOTES: &str = "refs/remotes/";
// Ignores the user's diff settings, which could otherwise hide parts of the change.
const DIFF: [&str; 3] = ["diff", "--ignore-submodules=none", "--no-ext-diff"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Link(String),
    Uncommitted {
        repo: PathBuf,
        base: String,
        snapshot: String,
    },
    Branch {
        repo: PathBuf,
        base: RemoteBranch,
        merge_base: String,
        head: String,
    },
}

// Kept as the full ref, since a local branch can share a remote branch's short name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteBranch(String);

impl fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.strip_prefix(REMOTES).unwrap_or(&self.0))
    }
}

struct Git<'a> {
    cli: Cli,
    cwd: &'a Path,
    cancellation: &'a Cancellation,
}

impl Git<'_> {
    fn run(&self, args: &[&str]) -> Result<String, ProcessError> {
        self.run_with_input(args, None)
    }

    fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, ProcessError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let output = self
            .cli
            .run(&args, input, Some(self.cwd), self.cancellation)?;
        Ok(output.strip_suffix('\n').unwrap_or(&output).to_owned())
    }

    fn head(&self) -> Result<Option<String>, ProcessError> {
        match self.run(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
            Err(ProcessError::Exit { code: 1, .. }) => Ok(None),
            head => head.map(Some),
        }
    }

    fn empty_tree(&self) -> Result<String, ProcessError> {
        self.run_with_input(&["hash-object", "-t", "tree", "--stdin"], Some(b""))
    }

    fn snapshot(&self, base: &str) -> Result<String, Error> {
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
        let staged = git.changed_paths(&["--cached", base])?;
        git.run(&["add", "--all"])?;
        let snapshot = git.run(&["write-tree"])?;
        let kept = git.changed_paths(&[base, &snapshot])?;
        let kept: HashSet<&str> = kept.split('\0').collect();
        let undone: Vec<&str> = staged
            .split('\0')
            .filter(|path| !path.is_empty() && !kept.contains(path))
            .collect();
        if !undone.is_empty() {
            return Err(Error::UndoneStagedChanges(undone.join(", ")));
        }
        Ok(snapshot)
    }

    fn changed_paths(&self, diff: &[&str]) -> Result<String, ProcessError> {
        let args = [&DIFF[..], &["--no-renames", "--name-only", "-z"], diff].concat();
        self.run(&args)
    }

    // Clears the assume-unchanged and skip-worktree bits, so it must only run on a copy of the
    // index. A skip-worktree file missing from disk is left out by a sparse checkout, not edited.
    fn hidden_edits(&self) -> Result<Vec<String>, ProcessError> {
        let files = self.run(&["ls-files", "-v", "-z"])?;
        let marked: HashSet<&str> = files
            .split('\0')
            .filter_map(|entry| entry.split_once(' '))
            .filter(|&(tag, path)| match tag {
                "S" | "s" => self.cwd.join(path).symlink_metadata().is_ok(),
                tag => tag.bytes().all(|byte| byte.is_ascii_lowercase()),
            })
            .map(|(_, path)| path)
            .collect();
        if marked.is_empty() {
            return Ok(Vec::new());
        }
        let paths: String = marked.iter().map(|path| format!("{path}\0")).collect();
        // With --stdin, update-index applies only one of several flags, so each bit has its own call.
        for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
            self.run_with_input(
                &["update-index", flag, "-z", "--stdin"],
                Some(paths.as_bytes()),
            )?;
        }
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

    fn default_branch(&self) -> Result<RemoteBranch, Error> {
        let output = self.run(&["for-each-ref", "--format=%(refname) %(symref)", REMOTES])?;
        let mut heads: Vec<(&str, RemoteBranch)> = output
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(name, target)| name.ends_with("/HEAD") && !target.is_empty())
            .map(|(name, target)| (name, RemoteBranch(target.to_owned())))
            .collect();
        let preferred = format!("{REMOTES}{PREFERRED_REMOTE}/HEAD");
        match heads.len() {
            0 => Err(Error::NoDefaultBranch),
            1 => Ok(heads.remove(0).1),
            _ => match heads.iter().position(|(name, _)| *name == preferred) {
                Some(index) => Ok(heads.swap_remove(index).1),
                None => Err(Error::AmbiguousDefaultBranch {
                    branches: heads
                        .iter()
                        .map(|(_, branch)| branch.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    preferred: PREFERRED_REMOTE,
                }),
            },
        }
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

impl Change {
    pub fn local(cancellation: &Cancellation) -> Result<Self, Error> {
        let git = Git {
            cli: Cli::new(["git"]),
            cwd: Path::new("."),
            cancellation,
        };
        process::require_executables([git.cli.executable()])?;
        let repo = match git.run(&["rev-parse", "--show-toplevel"]) {
            Err(error @ ProcessError::Exit { .. }) => return Err(Error::NotAWorkTree(error)),
            repo => PathBuf::from(repo?),
        };
        let git = Git { cwd: &repo, ..git };
        let head = git.head()?;
        let base = match &head {
            Some(head) => head.clone(),
            None => git.empty_tree()?,
        };
        let submodules = git.dirty_submodules()?;
        if !submodules.is_empty() {
            return Err(Error::DirtySubmodules {
                repo,
                submodules: submodules.join(", "),
            });
        }
        let snapshot = git.snapshot(&base)?;
        if snapshot != git.run(&["rev-parse", &format!("{base}^{{tree}}")])? {
            return Ok(Self::Uncommitted {
                repo,
                base,
                snapshot,
            });
        }
        let Some(head) = head else {
            return Err(Error::NoCommits(repo));
        };
        let base = git.default_branch()?;
        let merge_base = match git.run(&["merge-base", &base.0, &head]) {
            Err(ProcessError::Exit { code: 1, .. }) => {
                return Err(Error::NoCommonHistory { repo, base });
            }
            merge_base => merge_base?,
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
            } => format!(
                "Read it yourself by running `git {diff} {base} {snapshot}` in the repository at \
                 `{}`, and read files with `git show {snapshot}:<path>`, not from the working tree, \
                 which may have changed since.",
                repo.display()
            ),
            Self::Branch {
                repo,
                merge_base,
                head,
                ..
            } => format!(
                "Read it yourself by running `git log {merge_base}..{head}` and `git {diff} \
                 {merge_base} {head}` in the repository at `{}`, and read files with `git show \
                 {head}:<path>`, not from the working tree, which may have changed since.",
                repo.display()
            ),
        }
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Link(link) => write!(f, "the change at {link}"),
            Self::Uncommitted { repo, .. } => {
                write!(f, "the uncommitted changes in {}", repo.display())
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
