pub mod review;

use clap::Subcommand;

use crate::error::Error;
use crate::process::Cancellation;

#[derive(Debug, Subcommand)]
pub enum Command {
    Review(review::Review),
}

impl Command {
    pub fn run(&self, cancellation: &Cancellation) -> Result<(), Error> {
        match self {
            Self::Review(review) => review.run(cancellation),
        }
    }
}
