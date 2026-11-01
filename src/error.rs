use std::io;
use std::path::PathBuf;

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
    #[error("{agent} could not finish the task: {}\nResume: {resume}", .report.trim())]
    Incomplete {
        agent: String,
        report: String,
        resume: String,
    },
    #[error("the judge could not access the change at {0}")]
    ChangeUnavailable(String),
    #[error("the task is empty")]
    EmptyTask,
    #[error("there is no last output")]
    NoOutput,
    #[error("there is no review to publish; run `ncli code review` first")]
    NoReview,
    #[error("the last review of {0} has no findings to publish")]
    NothingToPublish(String),
    #[error("copying to the clipboard is not supported on this platform")]
    ClipboardUnsupported,
    #[error("could not find your home directory")]
    NoHome,
    #[error("could not update {}: {source}", .path.display())]
    StartupFile { path: PathBuf, source: io::Error },
    #[error("the state directory {0} is not valid UTF-8")]
    NonUtf8StateDir(PathBuf),
    #[error("could not find a directory to save state in; set {DIR_VAR}")]
    NoStateDir,
    #[error("could not access the saved {entry}: {source}")]
    State { entry: Entry, source: io::Error },
    #[error("the saved review is invalid: {0}")]
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

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::MissingExecutables(_) | Self::NoExecutable(_) => Self::MISSING_EXECUTABLE,
            Self::EmptyTask => Self::USAGE,
            Self::WorkingDirectory(_)
            | Self::InvalidAnswer { .. }
            | Self::NoSession(_)
            | Self::Incomplete { .. }
            | Self::ChangeUnavailable(_)
            | Self::NoOutput
            | Self::NoReview
            | Self::NothingToPublish(_)
            | Self::ClipboardUnsupported
            | Self::NoHome
            | Self::StartupFile { .. }
            | Self::NonUtf8StateDir(_)
            | Self::NoStateDir
            | Self::State { .. }
            | Self::SavedReview(_)
            | Self::Output(_)
            | Self::InterruptHandler(_)
            | Self::Process(_) => Self::FAILURE,
        }
    }
}
