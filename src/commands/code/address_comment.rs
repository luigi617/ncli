use clap::Args;

use super::parse_link;
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;

/// Change the code in the current directory to address review comments.
#[derive(Debug, Args)]
pub struct AddressComment {
    /// URL of the pull request, or of one of its comments
    #[arg(value_parser = parse_link)]
    pub link: String,
}

impl AddressComment {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let agent = AgentKind::first_available()?;
        Ok(agent
            .perform(&prompt(&self.link), context.cancellation)?
            .report)
    }
}

fn prompt(link: &str) -> String {
    format!(
        "Address the review feedback at {link} by changing the code in the current directory. Fetch \
         the feedback yourself, using your own tools and skills. If the link points to a specific \
         comment, address only that comment; otherwise address every unresolved review comment on \
         the change. Do not commit, do not push, and do not reply to or resolve any comment. In \
         report, summarize the change you made for each comment."
    )
}
