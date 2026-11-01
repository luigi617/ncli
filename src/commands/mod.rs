pub mod code;

use clap::Subcommand;

use crate::error::Error;
use crate::process::Cancellation;

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand)]
    Code(code::CodeCommand),
}

impl Command {
    pub fn run(&self, cancellation: &Cancellation) -> Result<(), Error> {
        match self {
            Self::Code(command) => command.run(cancellation),
        }
    }
}
