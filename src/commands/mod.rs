pub mod code;
mod copy;
mod init;
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
    /// Copy the output of the last command to the clipboard.
    Copy,
    Init(init::Init),
    #[command(external_subcommand)]
    Task(Vec<String>),
}

impl Command {
    pub fn run(&self, context: &Context) -> Result<Option<Output>, Error> {
        match self {
            Self::Code(command) => command.run(context).map(Some),
            Self::Copy => copy::run(context).map(|()| None),
            Self::Init(init) => init.run().map(Some),
            Self::Task(words) => task::run(&words.join(" "), context).map(|text| Some(text.into())),
        }
    }
}
