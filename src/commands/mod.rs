pub mod code;
mod copy;
mod task;

use clap::Subcommand;

use crate::error::Error;
use crate::process::Cancellation;
use crate::state::Entry;

pub struct Context<'a> {
    pub prog: &'a str,
    pub cancellation: &'a Cancellation,
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

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand)]
    Code(code::CodeCommand),
    /// Copy the output of the last ncli command to the clipboard.
    Copy,
    #[command(external_subcommand)]
    Task(Vec<String>),
}

impl Command {
    pub fn records_output(&self) -> bool {
        match self {
            Self::Code(_) | Self::Task(_) => true,
            Self::Copy => false,
        }
    }

    pub fn run(&self, context: &Context) -> Result<Option<Output>, Error> {
        match self {
            Self::Code(command) => command.run(context).map(Some),
            Self::Copy => copy::run(context).map(|()| None),
            Self::Task(words) => task::run(words, context).map(|text| Some(text.into())),
        }
    }
}
