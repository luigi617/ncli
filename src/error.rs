use std::io;
use std::path::PathBuf;

use crate::commands::code::review::{Change, Head, RemoteBranch};
use crate::process::{MissingExecutables, NoExecutable, ProcessError, last_lines};
use crate::state::{DIR_VAR, Entry};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    MissingExecutables(#[from] MissingExecutables),
    #[error(transparent)]
    NoExecutable(#[from] NoExecutable),
    #[error("could not prepare a working directory: {0}")]
    WorkingDirectory(#[source] io::Error),
    #[error("{agent} returned an invalid answer ({source}): {}", last_lines(.output))]
    InvalidAnswer {
        agent: String,
        source: serde_json::Error,
        output: String,
    },
    #[error("{0} did not report its session")]
    NoSession(String),
    #[error("{agent} could not finish the task: {}", .report.trim())]
    Incomplete { agent: String, report: String },
    #[error("{source}\nResume: {resume}")]
    Resumable { source: Box<Error>, resume: String },
    #[error("the judge could not access {0}")]
    ChangeUnavailable(Change),
    #[error(
        "the repository has no default branch to compare the current branch with; set one with `git remote set-head <remote> --auto`"
    )]
    NoDefaultBranch,
    #[error(
        "the repository has several default branches to compare the current branch with ({branches}) and none on {preferred}"
    )]
    AmbiguousDefaultBranch {
        branches: String,
        preferred: &'static str,
    },
    #[error("there is nothing to review in {}: no commits and no uncommitted changes", .0.display())]
    NoCommits(PathBuf),
    #[error("the current directory is not in a git work tree: {0}")]
    NotAWorkTree(#[source] ProcessError),
    #[error(
        "could not find a git work tree to review the local changes of; give a link instead: {0}"
    )]
    NoLocalChange(#[source] ProcessError),
    #[error("could not snapshot the uncommitted changes: {0}")]
    Snapshot(#[source] io::Error),
    #[error(
        "there is nothing to review in {}: no uncommitted changes and no commits that are not on {base}", .repo.display()
    )]
    NoChanges { repo: PathBuf, base: RemoteBranch },
    #[error(
        "the current branch in {} has no history in common with {base}", .repo.display()
    )]
    NoCommonHistory { repo: PathBuf, base: RemoteBranch },
    #[error(
        "the uncommitted changes inside submodules of {} cannot be reviewed ({submodules}); commit them in the submodules first", .repo.display()
    )]
    DirtySubmodules { repo: PathBuf, submodules: String },
    #[error(
        "files marked assume-unchanged or skip-worktree have edits the review would miss ({0}); commit or revert them, or clear the marks with `git update-index --no-assume-unchanged <paths>` and then `git update-index --no-skip-worktree <paths>`"
    )]
    HiddenEdits(String),
    #[error(
        "untracked git repositories cannot be reviewed ({0}); ignore them or add them as submodules"
    )]
    NestedRepositories(String),
    #[error(
        "the working tree undoes staged changes the review would miss ({0}); commit them, or make the working tree match the index, for example by ignoring files removed with `git rm --cached`"
    )]
    UndoneStagedChanges(String),
    #[error("{agent} left no changes in {} to review: {}", .repo.display(), .report.trim())]
    NothingImplemented {
        agent: String,
        repo: PathBuf,
        report: String,
    },
    #[error(
        "HEAD in {} moved from {from} to {to} during the run; agents must not commit or switch branches, so the changes were not reviewed",
        .repo.display()
    )]
    HeadMoved {
        repo: PathBuf,
        from: Box<Head>,
        to: Box<Head>,
    },
    #[error(
        "the review still has findings after {rounds} round(s), and the changes made to address them were not reviewed:\n\n{report}"
    )]
    Unsettled { rounds: u32, report: String },
    #[error("the task is empty")]
    EmptyTask,
    #[error(
        "unrecognized command `{0}`; quote a task to give it to an agent, for example: ncli \"fix the failing test\""
    )]
    NotATask(String),
    #[error("there is no last output")]
    NoOutput,
    #[error("no claude or codex sessions were active on {0}")]
    NoSessions(jiff::civil::Date),
    #[error("could not read the sessions in {}: {source}", .path.display())]
    Sessions { path: PathBuf, source: io::Error },
    #[error("there is no review to publish; run `ncli code review` first")]
    NoReview,
    #[error("the last review of {0} has no findings to publish")]
    NothingToPublish(Change),
    #[error("the last review was of {0}, which has nowhere to publish comments")]
    Unpublishable(Change),
    #[error("there is no display to copy to; set WAYLAND_DISPLAY or DISPLAY")]
    NoClipboard,
    #[error("could not find a directory to save state in; set {DIR_VAR}")]
    NoStateDir,
    #[error("could not access the saved {entry}: {source}")]
    State { entry: Entry, source: io::Error },
    #[error("the saved review is invalid ({0}); run `ncli code review` again")]
    SavedReview(#[source] serde_json::Error),
    #[error("could not write the output: {0}")]
    Output(#[source] io::Error),
    #[error("could not install the interrupt handler: {0}")]
    InterruptHandler(#[source] io::Error),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

impl Error {
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const MISSING_EXECUTABLE: u8 = 127;

    pub fn resume(&self) -> Option<&str> {
        match self {
            Self::Resumable { resume, .. } => Some(resume),
            _ => None,
        }
    }

    // A usage error is not a result, an unprintable output was already saved, and a state error
    // would replace the result it failed to save or read.
    pub fn recorded(&self) -> bool {
        !matches!(
            self,
            Self::Output(_) | Self::State { .. } | Self::NoStateDir
        ) && self.exit_code() != Self::USAGE
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::MissingExecutables(_) | Self::NoExecutable(_) => Self::MISSING_EXECUTABLE,
            Self::EmptyTask | Self::NotATask(_) => Self::USAGE,
            Self::Resumable { source, .. } => source.exit_code(),
            Self::WorkingDirectory(_)
            | Self::InvalidAnswer { .. }
            | Self::NoSession(_)
            | Self::Incomplete { .. }
            | Self::ChangeUnavailable(_)
            | Self::NoDefaultBranch
            | Self::AmbiguousDefaultBranch { .. }
            | Self::NoCommits(_)
            | Self::NotAWorkTree(_)
            | Self::NoLocalChange(_)
            | Self::NoChanges { .. }
            | Self::NoCommonHistory { .. }
            | Self::DirtySubmodules { .. }
            | Self::HiddenEdits(_)
            | Self::NestedRepositories(_)
            | Self::UndoneStagedChanges(_)
            | Self::NothingImplemented { .. }
            | Self::HeadMoved { .. }
            | Self::Unsettled { .. }
            | Self::NoOutput
            | Self::NoSessions(_)
            | Self::Sessions { .. }
            | Self::NoReview
            | Self::NothingToPublish(_)
            | Self::Unpublishable(_)
            | Self::NoClipboard
            | Self::NoStateDir
            | Self::State { .. }
            | Self::SavedReview(_)
            | Self::Snapshot(_)
            | Self::Output(_)
            | Self::InterruptHandler(_)
            | Self::Process(_) => Self::FAILURE,
        }
    }
}
