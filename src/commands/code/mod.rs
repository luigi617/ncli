pub mod review;

use clap::Subcommand;

use crate::error::Error;
use crate::process::Cancellation;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
}

impl CodeCommand {
    pub fn run(&self, cancellation: &Cancellation) -> Result<(), Error> {
        match self {
            Self::Review(review) => review.run(cancellation),
        }
    }
}
