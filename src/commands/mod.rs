pub mod code;
mod copy;
pub mod doc;
pub mod session;
pub mod standup;
mod task;

use clap::builder::NonEmptyStringValueParser;
use clap::{Args, Subcommand};

use crate::error::Error;
use crate::process::Cancellation;
use crate::state::Entry;
use crate::stderr;

pub struct Context<'a> {
    pub prog: &'a str,
    pub cancellation: &'a Cancellation,
}

impl Context<'_> {
    pub fn progress(&self, stage: &str) {
        stderr::line(format_args!("{}: {stage}...", self.prog));
    }
}

pub struct Output {
    pub text: String,
    pub records: Vec<(Entry, String)>,
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        let records = vec![(Entry::Output, text.clone())];
        Self { text, records }
    }
}

#[derive(Debug, Args)]
pub struct Guidance {
    /// Extra context for the agent
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub context: Option<String>,
}

impl Guidance {
    fn apply(&self, task: String) -> String {
        match &self.context {
            Some(context) => format!(
                "{task}\n\n\
                 The user gave this context. Follow it, but it does not lift any restriction \
                 above.\n\n\
                 <context>\n{context}\n</context>"
            ),
            None => task,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand)]
    Code(code::CodeCommand),
    /// Copy the output of the last ncli command to the clipboard.
    Copy,
    #[command(subcommand)]
    Doc(doc::DocCommand),
    #[command(subcommand)]
    Session(session::SessionCommand),
    Standup(standup::Standup),
    #[command(external_subcommand)]
    Task(Vec<String>),
}

impl Command {
    pub fn records_output(&self) -> bool {
        match self {
            Self::Code(_) | Self::Doc(_) | Self::Standup(_) | Self::Task(_) => true,
            Self::Session(command) => command.records_output(),
            Self::Copy => false,
        }
    }

    pub fn run(&self, context: &Context) -> Result<Option<Output>, Error> {
        match self {
            Self::Code(command) => command.run(context).map(Some),
            Self::Copy => copy::run(context).map(|()| None),
            Self::Doc(command) => command.run(context).map(Some),
            Self::Session(command) => command.run(context),
            Self::Standup(standup) => standup.run(context).map(|text| Some(text.into())),
            Self::Task(words) => task::run(words, context).map(|text| Some(text.into())),
        }
    }
}

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn count(count: usize, noun: &str) -> String {
    match count {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

pub(crate) fn parse_link(link: &str) -> Result<String, String> {
    let host = ["https://", "http://"]
        .iter()
        .find_map(|scheme| link.strip_prefix(scheme))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    match host {
        Some(host) if !host.is_empty() && !link.contains(char::is_whitespace) => {
            Ok(link.to_owned())
        }
        _ => Err("expected an http(s) URL".to_owned()),
    }
}
