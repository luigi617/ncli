use std::io;

use crate::process::{MissingExecutables, ProcessError, last_lines};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    MissingExecutables(#[from] MissingExecutables),
    #[error("could not prepare a working directory: {0}")]
    WorkingDirectory(#[source] io::Error),
    #[error("the judge returned an invalid verdict ({source}): {}", last_lines(.output))]
    Verdict {
        source: serde_json::Error,
        output: String,
    },
    #[error("the judge could not access the change at {0}")]
    ChangeUnavailable(String),
    #[error("could not write the output: {0}")]
    Output(#[source] io::Error),
    #[error("could not install the interrupt handler: {0}")]
    InterruptHandler(#[source] io::Error),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

impl Error {
    pub const FAILURE: u8 = 1;
    pub const MISSING_EXECUTABLE: u8 = 127;

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::MissingExecutables(_) => Self::MISSING_EXECUTABLE,
            Self::WorkingDirectory(_)
            | Self::Verdict { .. }
            | Self::ChangeUnavailable(_)
            | Self::Output(_)
            | Self::InterruptHandler(_)
            | Self::Process(_) => Self::FAILURE,
        }
    }
}
