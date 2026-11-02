use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;

use clap::{Args, Subcommand};

use super::{Context, Output, count};
use crate::error::Error;
use crate::process::{self, Cli, ProcessError};

const TMUX: &str = "tmux";
const LIST_FORMAT: &str = "#{session_windows}\t#{session_attached}\t#{session_name}";

/// Manage tmux sessions.
#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Create a session in the current directory and enter it.
    Add {
        /// Name of the session
        #[arg(long)]
        name: SessionName,
    },
    /// List the sessions, marking the one you are in.
    Ls,
    /// Enter a session.
    Use(Target),
    /// Detach from the current session.
    Detach,
    /// Kill a session.
    Kill(Target),
    /// Print the recent output of the active pane of a session.
    Logs {
        #[command(flatten)]
        target: Target,
        /// Number of lines of history to include
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
        lines: u32,
    },
    /// Rename a session.
    Rename {
        /// Current name of the session
        old: SessionName,
        /// New name for the session
        new: SessionName,
    },
}

#[derive(Debug, Args)]
pub struct Target {
    /// Name of the session
    name: SessionName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionName(String);

impl SessionName {
    fn parse(name: &str) -> Result<Self, String> {
        if name.trim().is_empty() {
            return Err("a session name cannot be empty".to_owned());
        }
        if name.contains([':', '.', '#', ';']) {
            return Err("a session name cannot contain `:`, `.`, `#` or `;`".to_owned());
        }
        Ok(Self(name.to_owned()))
    }

    fn target(&self) -> String {
        format!("={}", self.0)
    }
}

impl clap::builder::ValueParserFactory for SessionName {
    type Parser = fn(&str) -> Result<Self, String>;

    fn value_parser() -> Self::Parser {
        Self::parse
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

struct Session {
    name: String,
    windows: usize,
    attached: bool,
}

impl Session {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.splitn(3, '\t');
        let windows = fields.next()?.parse().ok()?;
        let attached = fields.next()?.parse::<usize>().ok()? > 0;
        let name = fields.next()?.to_owned();
        Some(Self {
            name,
            windows,
            attached,
        })
    }
}

impl SessionCommand {
    pub fn records_output(&self) -> bool {
        matches!(self, Self::Ls | Self::Logs { .. })
    }

    pub fn run(&self, context: &Context) -> Result<Option<Output>, Error> {
        process::require_executables([OsStr::new(TMUX)])?;
        let server = Server(context);
        match self {
            Self::Add { name } => {
                absent(&server.sessions()?, name)?;
                let cwd = env::current_dir().map_err(Error::CurrentDirectory)?;
                server.run(
                    tmux(&["new-session", "-d", "-s", &name.0, "-c"])
                        .arg(escape_argument(cwd.as_os_str())),
                )?;
                server.enter(name)
            }
            Self::Ls => server.list().map(|text| Some(text.into())),
            Self::Use(Target { name }) => {
                existing(&server.sessions()?, name)?;
                server.enter(name)
            }
            Self::Detach => {
                if !inside_tmux() {
                    return Err(Error::NotInTmux);
                }
                server.run(tmux(&["detach-client"]))?;
                Ok(None)
            }
            Self::Kill(Target { name }) => {
                existing(&server.sessions()?, name)?;
                server.run(tmux(&["kill-session", "-t"]).arg(name.target()))?;
                Ok(None)
            }
            Self::Logs {
                target: Target { name },
                lines,
            } => {
                existing(&server.sessions()?, name)?;
                let pane = format!("{}:", name.target());
                let start = format!("-{lines}");
                let text = server.run(tmux(&["capture-pane", "-p", "-t", &pane, "-S", &start]))?;
                Ok(Some(text.trim_end().to_owned().into()))
            }
            Self::Rename { old, new } => {
                let sessions = server.sessions()?;
                existing(&sessions, old)?;
                absent(&sessions, new)?;
                server.run(
                    tmux(&["rename-session", "-t"])
                        .arg(old.target())
                        .arg("--")
                        .arg(&new.0),
                )?;
                Ok(None)
            }
        }
    }
}

fn exists(sessions: &[Session], name: &SessionName) -> bool {
    sessions.iter().any(|session| session.name == name.0)
}

fn existing(sessions: &[Session], name: &SessionName) -> Result<(), Error> {
    if exists(sessions, name) {
        Ok(())
    } else {
        Err(Error::NoSuchSession(name.clone()))
    }
}

fn absent(sessions: &[Session], name: &SessionName) -> Result<(), Error> {
    if exists(sessions, name) {
        Err(Error::SessionExists(name.clone()))
    } else {
        Ok(())
    }
}

fn tmux(args: &[&str]) -> Cli {
    Cli::new([TMUX].iter().chain(args).copied())
}

// tmux expands `#` as a format, and reads a trailing `;` as a command separator unless it follows
// a backslash, which tmux then drops.
fn escape_argument(text: &OsStr) -> OsString {
    let bytes = text.as_encoded_bytes();
    let mut escaped = Vec::with_capacity(bytes.len() + 1);
    for &byte in bytes {
        if byte == b'#' {
            escaped.push(b'#');
        }
        escaped.push(byte);
    }
    if escaped.last() == Some(&b';') {
        escaped.insert(escaped.len() - 1, b'\\');
    }
    // SAFETY: inserting ASCII bytes only concatenates pieces split around ASCII, which keeps the
    // encoding valid.
    unsafe { OsString::from_encoded_bytes_unchecked(escaped) }
}

fn inside_tmux() -> bool {
    env::var_os("TMUX").is_some_and(|tmux| !tmux.is_empty())
}

// tmux has no exit code for a missing server: it says so on stderr, and when it has never started
// there is no socket to connect to.
fn no_server(error: &ProcessError) -> bool {
    let ProcessError::Exit { stderr_tail, .. } = error else {
        return false;
    };
    stderr_tail.starts_with("no server running on ")
        || (stderr_tail.starts_with("error connecting to ")
            && stderr_tail.ends_with("(No such file or directory)"))
}

struct Server<'a>(&'a Context<'a>);

impl Server<'_> {
    fn run(&self, cli: Cli) -> Result<String, Error> {
        Ok(cli.run(&[], None, None, self.0.cancellation)?)
    }

    fn enter(&self, name: &SessionName) -> Result<Option<Output>, Error> {
        let entered = if inside_tmux() {
            tmux(&["switch-client", "-t"])
        } else {
            tmux(&["attach-session", "-t"]).terminal()
        };
        self.run(entered.arg(name.target()))?;
        Ok(None)
    }

    fn sessions(&self) -> Result<Vec<Session>, Error> {
        let listed = match self.run(tmux(&["list-sessions", "-F", LIST_FORMAT])) {
            Err(Error::Process(error)) if no_server(&error) => return Ok(Vec::new()),
            listed => listed?,
        };
        listed
            .lines()
            .map(|line| Session::parse(line).ok_or_else(|| Error::TmuxSessions(line.to_owned())))
            .collect()
    }

    fn current(&self) -> Result<Option<String>, Error> {
        if !inside_tmux() {
            return Ok(None);
        }
        let name = self.run(tmux(&["display-message", "-p", "#{session_name}"]))?;
        Ok(Some(name.strip_suffix('\n').unwrap_or(&name).to_owned()))
    }

    fn list(&self) -> Result<String, Error> {
        let sessions = self.sessions()?;
        if sessions.is_empty() {
            return Ok(count(0, "session"));
        }
        let current = self.current()?;
        let lines: Vec<String> = sessions
            .iter()
            .map(|session| {
                let marker = if current.as_deref() == Some(&session.name) {
                    '*'
                } else {
                    ' '
                };
                let attached = if session.attached {
                    "attached"
                } else {
                    "not attached"
                };
                let windows = count(session.windows, "window");
                format!("{marker} {}: {windows}, {attached}", session.name)
            })
            .collect();
        Ok(lines.join("\n"))
    }
}
