pub mod review;

use clap::Subcommand;

use crate::error::Error;

#[derive(Debug, Subcommand)]
pub enum Command {
    Review(review::Review),
}

impl Command {
    pub fn run(&self) -> Result<(), Error> {
        match self {
            Self::Review(review) => review.run(),
        }
    }
}
