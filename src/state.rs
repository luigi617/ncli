use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{env, fmt, fs};

use crate::error::Error;

pub const DIR_VAR: &str = "NCLI_STATE_DIR";

#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Output,
    Review,
    Terminal,
    Recording,
}

impl Entry {
    fn file_name(self) -> &'static str {
        match self {
            Self::Output => "output.txt",
            Self::Review => "review.json",
            Self::Terminal => "terminal.txt",
            Self::Recording => "recording.txt",
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Output => "output",
            Self::Review => "review",
            Self::Terminal => "terminal output",
            Self::Recording => "recording",
        })
    }
}

fn dir() -> Result<PathBuf, Error> {
    env::var_os(DIR_VAR)
        .map(PathBuf::from)
        .or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .map(|dir| dir.join("ncli"))
        })
        .ok_or(Error::NoStateDir)
}

pub fn path(entry: Entry) -> Result<PathBuf, Error> {
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

pub fn last_output() -> Result<String, Error> {
    let entry = match newest(&[Entry::Output, Entry::Terminal, Entry::Recording])? {
        Some(Entry::Recording) => {
            let partial = load(Entry::Recording)?.unwrap_or_default();
            if Path::new(String::from_utf8_lossy(&partial).trim_end()).exists() {
                return Err(Error::StillRecording);
            }
            Entry::Terminal
        }
        Some(entry) => entry,
        None => return Err(Error::NoOutput),
    };
    let output = load(entry)?.unwrap_or_default();
    let output = String::from_utf8_lossy(&strip_ansi_escapes::strip(output)).into_owned();
    if output.trim().is_empty() {
        return Err(Error::NoOutput);
    }
    Ok(output)
}

fn newest(entries: &[Entry]) -> Result<Option<Entry>, Error> {
    let mut newest: Option<(SystemTime, Entry)> = None;
    for &entry in entries {
        let modified =
            fs::metadata(dir()?.join(entry.file_name())).and_then(|meta| meta.modified());
        let Some(modified) = found(entry, modified)? else {
            continue;
        };
        if newest.is_none_or(|(time, _)| modified > time) {
            newest = Some((modified, entry));
        }
    }
    Ok(newest.map(|(_, entry)| entry))
}

fn found<T>(entry: Entry, result: io::Result<T>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::State { entry, source }),
    }
}
