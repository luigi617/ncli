use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, ValueEnum};

use super::Output;
use crate::error::Error;
use crate::state::{self, Entry};

/// Print shell code that records the output of chosen commands for `ncli copy`.
///
/// Add `eval "$(ncli init zsh)"` to ~/.zshrc, or run `ncli init zsh --install` to add it.
/// Only commands named in the ncli_captured array are recorded, for example `ncli_captured=(gh)`.
/// A recorded command writes to a pipe instead of the terminal, with stderr merged into stdout,
/// so do not add interactive programs such as editors and pagers.
#[derive(Debug, Args)]
pub struct Init {
    #[arg(value_enum)]
    pub shell: Shell,
    /// Add the shell code to the shell's startup file instead of printing it
    #[arg(long)]
    pub install: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Shell {
    Zsh,
}

impl Shell {
    fn startup_line(self) -> &'static str {
        match self {
            Self::Zsh => r#"eval "$(ncli init zsh)""#,
        }
    }

    fn startup_file(self) -> Result<PathBuf, Error> {
        match self {
            Self::Zsh => env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .or_else(dirs::home_dir)
                .map(|dir| dir.join(".zshrc"))
                .ok_or(Error::NoHome),
        }
    }
}

impl Init {
    pub fn run(&self) -> Result<Output, Error> {
        let text = if self.install {
            install(self.shell)?
        } else {
            script(self.shell)?
        };
        Ok(Output {
            text,
            records: Vec::new(),
        })
    }
}

fn script(shell: Shell) -> Result<String, Error> {
    let capture = state::path(Entry::Terminal)?;
    let capture = capture
        .to_str()
        .ok_or_else(|| Error::NonUtf8StateDir(capture.clone()))?;
    Ok(match shell {
        Shell::Zsh => include_str!("init/init.zsh").replace("NCLI_CAPTURE", &quote(capture)),
    })
}

fn install(shell: Shell) -> Result<String, Error> {
    let path = shell.startup_file()?;
    let line = shell.startup_line();
    let error = |source| Error::StartupFile {
        path: path.clone(),
        source,
    };
    let existing = match fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(source) if source.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(error(source)),
    };
    if existing.lines().any(|existing| existing.trim() == line) {
        return Ok(format!("{} already runs ncli", path.display()));
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{separator}{line}"))
        .map_err(error)?;
    Ok(format!(
        "Added {line} to {}; open a new terminal to start recording",
        path.display()
    ))
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}
