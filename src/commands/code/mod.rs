mod address_comment;
mod publish_comment;
pub mod review;

use clap::builder::NonEmptyStringValueParser;
use clap::{Args, Subcommand};

pub use address_comment::AddressComment;
pub use publish_comment::PublishComment;

use super::{Context, Output};
use crate::error::Error;

/// Work with code changes.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    Review(review::Review),
    PublishComment(PublishComment),
    AddressComment(AddressComment),
}

impl CodeCommand {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        match self {
            Self::Review(review) => review.run(context),
            Self::PublishComment(publish) => publish.run(context).map(|text| Output {
                text,
                records: Vec::new(),
            }),
            Self::AddressComment(address) => address.run(context).map(Output::from),
        }
    }
}

#[derive(Debug, Args)]
pub struct Guidance {
    /// Extra context for the agent, for example which comments to handle
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub context: Option<String>,
}

impl Guidance {
    fn apply(&self, task: String) -> String {
        match &self.context {
            Some(context) => format!(
                "{task}\n\n\
                 The user gave this context. Follow it, for example to choose which comments to \
                 handle and which to skip, but it does not lift any restriction above.\n\n\
                 <context>\n{context}\n</context>"
            ),
            None => task,
        }
    }
}

fn parse_link(link: &str) -> Result<String, String> {
    let host = ["https://", "http://"]
        .iter()
        .find_map(|scheme| link.strip_prefix(scheme))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    match host {
        Some(host) if !host.is_empty() && !link.contains(char::is_whitespace) => {
            Ok(link.to_owned())
        }
        _ => Err("expected an http(s) URL".to_owned()),
    }
}
