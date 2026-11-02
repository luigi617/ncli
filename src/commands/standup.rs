use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{self, Path, PathBuf};
use std::time::SystemTime;

use clap::{Args, ValueEnum};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use serde::Deserialize;

use super::{Context, Guidance};
use crate::agent::{AgentKind, WORKDIR};
use crate::error::Error;

/// Summarize what you did on a day, from your claude and codex sessions.
#[derive(Debug, Args)]
pub struct Standup {
    /// Day to summarize: today, yesterday, or a date such as 2026-11-01
    #[arg(long, default_value = "today", value_parser = parse_day)]
    pub day: Day,
    #[command(flatten)]
    pub guidance: Guidance,
}

#[derive(Clone, Debug)]
pub struct Day {
    pub date: Date,
    pub start: Timestamp,
    pub end: Timestamp,
}

fn parse_day(day: &str) -> Result<Day, String> {
    let today = Zoned::now().date();
    let date = match day {
        "today" => Ok(today),
        "yesterday" => today.yesterday(),
        date => date.parse(),
    }
    .map_err(|_| "expected today, yesterday, or a date such as 2026-11-01".to_owned())?;
    let start_of = |date: Date| {
        date.to_zoned(TimeZone::system())
            .map(|start| start.timestamp())
    };
    let window = date
        .tomorrow()
        .and_then(|tomorrow| Ok((start_of(date)?, start_of(tomorrow)?)));
    let (start, end) = window.map_err(|error| format!("{date} cannot be summarized: {error}"))?;
    Ok(Day { date, start, end })
}

impl Standup {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let agent = AgentKind::first_available()?;
        let temp = fs::canonicalize(env::temp_dir()).map_err(Error::WorkingDirectory)?;
        let mut files = Vec::new();
        for sessions in AgentKind::value_variants()
            .iter()
            .filter_map(|kind| kind.sessions())
        {
            let unreadable = |source| Error::Sessions {
                path: sessions.dir.clone(),
                source,
            };
            let dir = path::absolute(&sessions.dir).map_err(unreadable)?;
            let since = SystemTime::from(self.day.start);
            modified_since(&dir, sessions.depth, since, &mut files).map_err(unreadable)?;
        }
        let mut active = Vec::new();
        for file in files {
            let session = Session::read(&file, &self.day).map_err(|source| Error::Sessions {
                path: file.clone(),
                source,
            })?;
            if session.active && !session.ran_by_ncli(&temp) {
                active.push(file);
            }
        }
        if active.is_empty() {
            return Err(Error::NoSessions(self.day.date));
        }
        active.sort();
        context.progress(&format!("summarizing {}", sessions(active.len())));
        let task = self.guidance.apply(prompt(&self.day, &active));
        agent.perform_isolated(&task, context.cancellation)
    }
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    cwd: Option<PathBuf>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    cwd: Option<PathBuf>,
}

struct Session {
    active: bool,
    cwd: Option<PathBuf>,
}

impl Session {
    // Claude records the working directory on each entry, and codex in the payload of its first.
    fn read(path: &Path, day: &Day) -> io::Result<Self> {
        let mut session = Self {
            active: false,
            cwd: None,
        };
        for line in BufReader::new(File::open(path)?).split(b'\n') {
            let Ok(line) = serde_json::from_slice::<Line>(&line?) else {
                continue;
            };
            session.active |= line
                .timestamp
                .and_then(|time| time.parse::<Timestamp>().ok())
                .is_some_and(|time| (day.start..day.end).contains(&time));
            if session.cwd.is_none() {
                session.cwd = line.cwd.or(line.payload.and_then(|payload| payload.cwd));
            }
            if session.active && session.cwd.is_some() {
                break;
            }
        }
        Ok(session)
    }

    fn ran_by_ncli(&self, temp: &Path) -> bool {
        self.cwd.as_deref().is_some_and(|cwd| {
            cwd.parent() == Some(temp)
                && cwd
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(WORKDIR))
        })
    }
}

fn sessions(count: usize) -> String {
    match count {
        1 => "1 session".to_owned(),
        count => format!("{count} sessions"),
    }
}

fn modified_since(
    dir: &Path,
    depth: Option<usize>,
    since: SystemTime,
    found: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if depth != Some(1) {
                modified_since(&path, depth.map(|depth| depth - 1), since, found)?;
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
            && entry.metadata()?.modified()? >= since
        {
            found.push(path);
        }
    }
    Ok(())
}

fn prompt(day: &Day, files: &[PathBuf]) -> String {
    let files = files
        .iter()
        .map(|file| format!("- {}", file.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Write my daily standup summary for {date}.\n\
         \n\
         My claude and codex sessions are recorded as JSON Lines in the files below. Read them \
         yourself, and consider only entries whose timestamp is from {start} up to {end}, which \
         is {date} in my time zone. Do not modify anything.\n\
         \n\
         {files}\n\
         \n\
         In report, give 3 to 5 high-level bullets for a daily standup, one per main piece of \
         work and one line each: what got built, fixed, reviewed, shipped, or decided, not how. \
         Name the project in a bullet only when there is more than one. If no session has \
         entries from that day, say so.",
        date = day.date,
        start = day.start,
        end = day.end,
    )
}
