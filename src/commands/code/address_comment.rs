use clap::Args;

use super::{Guidance, parse_link};
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;
use crate::state;

/// Change the code in the current directory to address review comments.
#[derive(Debug, Args)]
pub struct AddressComment {
    /// URL of the pull request, or of one of its comments [default: the last output]
    #[arg(value_parser = parse_link)]
    pub link: Option<String>,
    #[command(flatten)]
    pub guidance: Guidance,
}

impl AddressComment {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let feedback = match &self.link {
            Some(link) => linked(link),
            None => given(&state::last_successful_output()?),
        };
        let agent = AgentKind::first_available()?;
        let task = format!(
            "{feedback} Do not commit, do not push, and do not reply to or resolve any comment. In \
             report, summarize the change you made for each comment."
        );
        Ok(agent
            .perform(&self.guidance.apply(task), context.cancellation)?
            .to_string())
    }
}

fn linked(link: &str) -> String {
    format!(
        "Address the review feedback at {link} by changing the code in the current directory. Fetch \
         the feedback yourself, using your own tools and skills. If the link points to a specific \
         comment, address only that comment; otherwise address every unresolved review comment on \
         the change."
    )
}

fn given(output: &str) -> String {
    format!(
        "The output of the last command the user ran is below. Address the review feedback in it \
         by changing the code in the current directory. If it is not review feedback on the code \
         in the current directory, change nothing, set completed to false, and say in report that \
         the last output is not review feedback.\n\n<output>\n{}\n</output>\n\n",
        output.trim_end()
    )
}
