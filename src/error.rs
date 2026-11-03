use std::io;
use std::path::PathBuf;

use crate::commands::code::review::Change;
use crate::commands::code::{Head, Operation, RemoteBranch, Unshipped, Unsynced};
use crate::commands::session::SessionName;
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
    ChangeUnavailable(Box<Change>),
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
    #[error(
        "no remote has a branch named {0}; check the name, or fetch it with `git fetch <remote>`"
    )]
    NoRemoteBranch(String),
    #[error(
        "several remotes have a branch named {branch} ({remotes}) and none of them is {preferred}"
    )]
    AmbiguousRemoteBranch {
        branch: String,
        remotes: String,
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
    #[error("{base} has not been fetched; fetch it first")]
    Unfetched { base: RemoteBranch },
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
        "HEAD in {} moved from {from} to {to} during the run; agents must not commit or switch branches",
        .repo.display()
    )]
    HeadMoved {
        repo: PathBuf,
        from: Box<Head>,
        to: Box<Head>,
    },
    #[error(
        "the review still has {remaining} after {rounds} round(s), and the changes made to address them were not reviewed:\n\n{report}"
    )]
    Unsettled {
        remaining: String,
        rounds: u32,
        report: String,
    },
    #[error(
        "there is nothing to ship in {}: no uncommitted changes and no commits that are not on {base}", .repo.display()
    )]
    NothingToShip { repo: PathBuf, base: RemoteBranch },
    #[error("{agent} did not finish shipping to {into}: {problem}: {}", .report.trim())]
    Unshipped {
        agent: String,
        into: Box<RemoteBranch>,
        #[source]
        problem: Box<Unshipped>,
        report: String,
    },
    #[error("{0} does not name a GitHub repository")]
    NotAGitHubRemote(String),
    #[error("gh described the repository or pull request in an unexpected format: {0}")]
    GhOutput(#[source] serde_json::Error),
    #[error("{agent} left no changes in {} to fix the check: {}", .repo.display(), .report.trim())]
    NothingFixed {
        agent: String,
        repo: PathBuf,
        report: String,
    },
    #[error(
        "HEAD in {} moved from {from} to {to} while `{check}` ran; the check must not commit or switch branches",
        .repo.display()
    )]
    CheckMovedHead {
        check: String,
        repo: PathBuf,
        from: Box<Head>,
        to: Box<Head>,
    },
    #[error("`{check}` still fails after {rounds} round(s) of fixes:\n\n{report}")]
    StillFailing {
        check: String,
        rounds: u32,
        report: String,
    },
    #[error("a {operation} is already in progress in {}; finish or abort it first", .repo.display())]
    OperationInProgress { repo: PathBuf, operation: Operation },
    #[error(
        "the working tree in {} has unresolved conflicts in {paths}; resolve and stage them first", .repo.display()
    )]
    UnmergedPaths { repo: PathBuf, paths: String },
    #[error("cannot sync {head} in {}; switch to a branch with commits first", .repo.display())]
    NotOnABranch { repo: PathBuf, head: Box<Head> },
    #[error(
        "cannot sync {branch}: its merge commits since {from} ({commits}) would be flattened by a rebase, losing the changes made in them; rebase it by hand"
    )]
    MergeCommits {
        branch: String,
        from: Box<RemoteBranch>,
        commits: String,
    },
    #[error(
        "the working tree in {} has uncommitted changes ({paths}); commit or stash them first", .repo.display()
    )]
    DirtyWorktree { repo: PathBuf, paths: String },
    #[error("{agent} did not finish rebasing {branch} onto {from}: {problem}: {}", .report.trim())]
    Unsynced {
        agent: String,
        branch: String,
        from: Box<RemoteBranch>,
        #[source]
        problem: Box<Unsynced>,
        report: String,
    },
    #[error("{error}\nPutting {branch} back at {old} then failed: {restore}")]
    NotRestored {
        error: Box<Error>,
        branch: String,
        old: String,
        restore: Box<ProcessError>,
    },
    #[error("{0} wrote no document")]
    NothingWritten(String),
    #[error("could not read the document at {}: {source}", .path.display())]
    ReadDoc { path: PathBuf, source: io::Error },
    #[error("the last output is not a document review; run `ncli doc review` first")]
    NoDocReview,
    #[error("the saved document review is invalid ({0}); run `ncli doc review` again")]
    SavedDocReview(#[source] serde_json::Error),
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
    NothingToPublish(Box<Change>),
    #[error("the last review was of {0}, which has nowhere to publish comments")]
    Unpublishable(Box<Change>),
    #[error("there is no display to copy to; set WAYLAND_DISPLAY or DISPLAY")]
    NoClipboard,
    #[error("could not find a directory to save state in; set {DIR_VAR}")]
    NoStateDir,
    #[error("could not access the saved {entry}: {source}")]
    State { entry: Entry, source: io::Error },
    #[error("the saved review is invalid ({0}); run `ncli code review` again")]
    SavedReview(#[source] serde_json::Error),
    #[error("a tmux session named {0} already exists")]
    SessionExists(SessionName),
    #[error("there is no tmux session named {0}")]
    NoSuchSession(SessionName),
    #[error("not inside a tmux session, so there is no session to detach from")]
    NotInTmux,
    #[error("tmux listed a session in an unexpected format: {0}")]
    TmuxSessions(String),
    #[error("could not read the current directory: {0}")]
    CurrentDirectory(#[source] io::Error),
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

    pub fn resumable(self, resume: &str) -> Self {
        match self {
            Self::Resumable { .. } => self,
            source => Self::Resumable {
                source: Box::new(source),
                resume: resume.to_owned(),
            },
        }
    }

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
            Self::Resumable { source, .. } | Self::NotRestored { error: source, .. } => {
                source.exit_code()
            }
            Self::WorkingDirectory(_)
            | Self::InvalidAnswer { .. }
            | Self::NoSession(_)
            | Self::Incomplete { .. }
            | Self::ChangeUnavailable(_)
            | Self::NoDefaultBranch
            | Self::AmbiguousDefaultBranch { .. }
            | Self::NoRemoteBranch(_)
            | Self::AmbiguousRemoteBranch { .. }
            | Self::NoCommits(_)
            | Self::NotAWorkTree(_)
            | Self::NoLocalChange(_)
            | Self::NoChanges { .. }
            | Self::NoCommonHistory { .. }
            | Self::Unfetched { .. }
            | Self::DirtySubmodules { .. }
            | Self::HiddenEdits(_)
            | Self::NestedRepositories(_)
            | Self::UndoneStagedChanges(_)
            | Self::NothingImplemented { .. }
            | Self::HeadMoved { .. }
            | Self::Unsettled { .. }
            | Self::NothingToShip { .. }
            | Self::Unshipped { .. }
            | Self::NotAGitHubRemote(_)
            | Self::GhOutput(_)
            | Self::NothingFixed { .. }
            | Self::StillFailing { .. }
            | Self::CheckMovedHead { .. }
            | Self::OperationInProgress { .. }
            | Self::UnmergedPaths { .. }
            | Self::NotOnABranch { .. }
            | Self::MergeCommits { .. }
            | Self::DirtyWorktree { .. }
            | Self::Unsynced { .. }
            | Self::NothingWritten(_)
            | Self::ReadDoc { .. }
            | Self::NoDocReview
            | Self::SavedDocReview(_)
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
            | Self::SessionExists(_)
            | Self::NoSuchSession(_)
            | Self::NotInTmux
            | Self::TmuxSessions(_)
            | Self::CurrentDirectory(_)
            | Self::Snapshot(_)
            | Self::Output(_)
            | Self::InterruptHandler(_)
            | Self::Process(_) => Self::FAILURE,
        }
    }
}
