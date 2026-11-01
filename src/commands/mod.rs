pub mod code;

use clap::Subcommand;

use crate::error::Error;
use crate::process::Cancellation;

pub struct Context<'a> {
    pub prog: &'a str,
    pub cancellation: &'a Cancellation,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand)]
    Code(code::CodeCommand),
}

impl Command {
    pub fn run(&self, context: &Context) -> Result<(), Error> {
        match self {
            Self::Code(command) => command.run(context),
        }
    }
}
