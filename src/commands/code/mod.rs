mod fix;
mod implement;
mod publish_comment;
pub mod review;

use clap::Subcommand;

pub use fix::Fix;
pub use implement::Implement;
pub use publish_comment::PublishComment;

use super::{Context, Output};
use crate::error::Error;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
    PublishComment(PublishComment),
    Fix(Fix),
    Implement(Implement),
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
        }
    }
}
