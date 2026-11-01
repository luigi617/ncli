use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::{Args, ValueEnum};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use super::{Context, Guidance};
use crate::agent::AgentKind;
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
        let since = SystemTime::from(self.day.start);
        let mut files = Vec::new();
        for sessions in AgentKind::value_variants()
            .iter()
            .filter_map(|kind| kind.sessions())
        {
            modified_since(&sessions.dir, sessions.depth, since, &mut files).map_err(|source| {
                Error::Sessions {
                    dir: sessions.dir.clone(),
                    source,
                }
            })?;
        }
        if files.is_empty() {
            return Err(Error::NoSessions(self.day.date));
        }
        files.sort();
        let ncli = fs::canonicalize(env::temp_dir()).map_err(Error::WorkingDirectory)?;
        context.progress(&format!("summarizing {}", sessions(files.len())));
        let task = self.guidance.apply(prompt(&self.day, &files, &ncli));
        agent.perform_isolated(&task, context.cancellation)
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

fn prompt(day: &Day, files: &[PathBuf], ncli: &Path) -> String {
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
         is {date} in my time zone. Skip any session whose working directory is under {ncli}: \
         ncli ran those itself, and they repeat work from the other sessions. Do not modify \
         anything.\n\
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
        ncli = ncli.display(),
    )
}
