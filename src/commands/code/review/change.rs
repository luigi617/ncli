use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::process::{self, Cancellation, Cli, ProcessError};

const PREFERRED_REMOTE: &str = "origin";

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
        base: String,
        merge_base: String,
        head: String,
    },
}

struct Git<'a> {
    cli: Cli,
    cancellation: &'a Cancellation,
}

impl Git<'_> {
    fn run(&self, args: &[&str]) -> Result<String, ProcessError> {
        self.run_with_input(args, None)
    }

    fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, ProcessError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let output = self.cli.run(&args, input, None, self.cancellation)?;
        Ok(output.trim().to_owned())
    }

    fn head(&self) -> Result<Option<String>, ProcessError> {
        match self.run(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
            Err(ProcessError::Exit { code: 1, .. }) => Ok(None),
            head => head.map(Some),
        }
    }

    fn empty_tree(&self) -> Result<String, ProcessError> {
        self.run_with_input(&["hash-object", "-w", "-t", "tree", "--stdin"], Some(b""))
    }

    fn snapshot(&self) -> Result<String, Error> {
        let dir = tempfile::Builder::new()
            .prefix("ncli-snapshot-")
            .tempdir()
            .map_err(Error::Snapshot)?;
        let index = dir.path().join("index");
        let real_index = PathBuf::from(self.run(&["rev-parse", "--git-path", "index"])?);
        copy_index(&real_index, &index).map_err(Error::Snapshot)?;
        let git = Git {
            cli: Cli::new(["git"]).env("GIT_INDEX_FILE", &index),
            cancellation: self.cancellation,
        };
        git.run(&["add", "--all"])?;
        Ok(git.run(&["write-tree"])?)
    }

    fn dirty_submodules(&self) -> Result<Vec<String>, ProcessError> {
        let status = self.run(&["--no-optional-locks", "status", "--porcelain=v2", "-z"])?;
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

    fn default_branch(&self) -> Result<String, Error> {
        let output = self.run(&[
            "for-each-ref",
            "--format=%(symref:short)",
            "refs/remotes/*/HEAD",
        ])?;
        let branches: Vec<&str> = output.lines().filter(|line| !line.is_empty()).collect();
        let preferred = format!("{PREFERRED_REMOTE}/");
        match branches[..] {
            [] => Err(Error::NoDefaultBranch),
            [branch] => Ok(branch.to_owned()),
            _ => branches
                .iter()
                .find(|branch| branch.starts_with(&preferred))
                .map(|&branch| branch.to_owned())
                .ok_or_else(|| Error::AmbiguousDefaultBranch(branches.join(", "))),
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
            cancellation,
        };
        process::require_executables([git.cli.executable()])?;
        let repo = match git.run(&["rev-parse", "--show-toplevel"]) {
            Err(error @ ProcessError::Exit { .. }) => return Err(Error::NotAWorkTree(error)),
            repo => PathBuf::from(repo?),
        };
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
        let snapshot = git.snapshot()?;
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
        let merge_base = match git.run(&["merge-base", &base, &head]) {
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
        match self {
            Self::Link(_) => "Fetch it yourself, using your own tools and skills.".to_owned(),
            Self::Uncommitted {
                repo,
                base,
                snapshot,
            } => format!(
                "Read it yourself by running `git diff {base} {snapshot}` in {}, and read files with \
                 `git show {snapshot}:<path>`, not from the working tree, which may have changed since.",
                repo.display()
            ),
            Self::Branch {
                repo,
                merge_base,
                head,
                ..
            } => format!(
                "Read it yourself by running `git log {merge_base}..{head}` and `git diff {merge_base} \
                 {head}` in {}, and read files with `git show {head}:<path>`, not from the working \
                 tree, which may have changed since.",
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
                write!(f, "the commits from {base} to {head} in {}", repo.display())
            }
        }
    }
}
