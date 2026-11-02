use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fmt, fs};

use crate::error::Error;

pub const DIR_VAR: &str = "NCLI_STATE_DIR";

#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Output,
    Review,
    DocReview,
    Failure,
}

impl Entry {
    fn file_name(self) -> &'static str {
        match self {
            Self::Output => "output.txt",
            Self::Review => "review.json",
            Self::DocReview => "doc-review.json",
            Self::Failure => "failure.txt",
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Output => "output",
            Self::Review => "review",
            Self::DocReview => "document review",
            Self::Failure => "failure",
        })
    }
}

fn dir() -> Result<PathBuf, Error> {
    env::var_os(DIR_VAR)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .map(|dir| dir.join("ncli"))
        })
        .ok_or(Error::NoStateDir)
}

fn path(entry: Entry) -> Result<PathBuf, Error> {
    let dir = dir()?;
    create_private(&dir).map_err(|source| Error::State { entry, source })?;
    Ok(dir.join(entry.file_name()))
}

#[cfg(unix)]
fn create_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

pub fn save(entry: Entry, contents: &str) -> Result<(), Error> {
    let error = |source| Error::State { entry, source };
    let path = path(entry)?;
    let dir = path
        .parent()
        .expect("an entry lives in the state directory");
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(error)?;
    file.write_all(contents.as_bytes()).map_err(error)?;
    file.persist(path).map_err(|persist| error(persist.error))?;
    Ok(())
}

pub fn load(entry: Entry) -> Result<Option<Vec<u8>>, Error> {
    found(entry, fs::read(dir()?.join(entry.file_name())))
}

pub fn remove(entry: Entry) -> Result<(), Error> {
    found(entry, fs::remove_file(dir()?.join(entry.file_name()))).map(|_| ())
}

// A success removes the saved failure, so a failure that is still saved is the newest.
pub fn last_output() -> Result<String, Error> {
    match load(Entry::Failure)? {
        Some(failure) => text(failure),
        None => last_successful_output(),
    }
}

pub fn last_successful_output() -> Result<String, Error> {
    text(load(Entry::Output)?.unwrap_or_default())
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        return Err(Error::NoOutput);
    }
    Ok(text)
}

fn found<T>(entry: Entry, result: io::Result<T>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::State { entry, source }),
    }
}
