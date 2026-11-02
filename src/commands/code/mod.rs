mod fix;
mod git;
mod green;
mod implement;
mod publish_comment;
pub mod review;
mod ship;
mod sync;

use clap::Subcommand;

pub use fix::Fix;
pub use git::{Head, Operation, RemoteBranch};
pub use green::Green;
pub use implement::Implement;
pub use publish_comment::PublishComment;
pub use ship::{Ship, Unshippable};

use super::{Context, Output};
use crate::agent::Performed;
use crate::error::Error;
use crate::process::Cancellation;
use review::Worktree;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
    PublishComment(PublishComment),
    Fix(Fix),
    Implement(Implement),
    Ship(Ship),
    Green(Green),
    Sync(sync::Sync),
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
            Self::Green(green) => green.run(context).map(Output::from),
            Self::Sync(sync) => sync.run(context).map(Output::from),
        }
    }
}

fn resumable(error: Error, steps: &[Performed]) -> Error {
    match steps.last() {
        Some(step) => error.resumable(&step.resume),
        None => error,
    }
}

// The work tree after the last agent step, or None if the step left it as it was.
fn changed(
    earlier: &Worktree,
    steps: &[Performed],
    cancellation: &Cancellation,
) -> Result<Option<Worktree>, Error> {
    let worktree = earlier
        .since(cancellation)
        .map_err(|error| resumable(error, steps))?;
    if worktree.head != earlier.head {
        return Err(resumable(
            Error::HeadMoved {
                repo: worktree.repo,
                from: Box::new(earlier.head.clone()),
                to: Box::new(worktree.head),
            },
            steps,
        ));
    }
    Ok((worktree.tree != earlier.tree).then_some(worktree))
}
