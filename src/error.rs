use std::ffi::OsString;
use std::io;

use crate::process::ProcessError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("required executables not found on PATH: {}", join_lossy(.0))]
    MissingExecutables(Vec<OsString>),
    #[error("could not create a working directory: {0}")]
    WorkingDirectory(#[source] io::Error),
    #[error(transparent)]
    Process(#[from] ProcessError),
}

impl Error {
    pub const FAILURE: u8 = 1;
    pub const MISSING_EXECUTABLE: u8 = 127;

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::MissingExecutables(_) => Self::MISSING_EXECUTABLE,
            Self::WorkingDirectory(_) | Self::Process(_) => Self::FAILURE,
        }
    }
}

fn join_lossy(names: &[OsString]) -> String {
    names
        .iter()
        .map(|name| name.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ")
}
