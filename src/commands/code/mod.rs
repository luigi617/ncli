mod fix;
mod implement;
mod publish_comment;
pub mod review;
mod ship;
mod sync;

use clap::Subcommand;

pub use fix::Fix;
pub use implement::Implement;
pub use publish_comment::PublishComment;
pub use ship::{Ship, Unshipped};
pub use sync::{Sync, Unsynced};

use super::{Context, Output};
use crate::error::Error;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
    PublishComment(PublishComment),
    Fix(Fix),
    Implement(Implement),
    Ship(Ship),
    Sync(Sync),
}

impl CodeCommand {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        match self {
            Self::Review(review) => review.run(context),
            Self::PublishComment(publish) => publish.run(context).map(|text| Output {
                text,
                records: Vec::new(),
            }),
            Self::Fix(fix) => fix.run(context).map(Output::from),
            Self::Implement(implement) => implement.run(context),
            Self::Ship(ship) => ship.run(context).map(Output::from),
            Self::Sync(sync) => sync.run(context).map(Output::from),
        }
    }
}
