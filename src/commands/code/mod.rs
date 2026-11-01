pub mod review;

use clap::Subcommand;

use super::Context;
use crate::error::Error;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
}

impl CodeCommand {
    pub fn run(&self, context: &Context) -> Result<(), Error> {
        match self {
            Self::Review(review) => review.run(context),
        }
    }
}
