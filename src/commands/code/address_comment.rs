use clap::Args;

use crate::agent::AgentKind;
use crate::commands::Context;
use crate::commands::Guidance;
use crate::commands::parse_link;
use crate::error::Error;
use crate::state;

/// Change the code in the current directory to address the review comments that hold up.
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
        Ok(agent
            .perform(&self.guidance.apply(task(&feedback)), context.cancellation)?
            .to_string())
    }
}

pub(super) fn task(feedback: &str) -> String {
    format!(
        "{feedback} Before changing anything for a comment, verify it against the code. Address \
         it only if it is correct and worth the change; leave the code as it is for any comment \
         that is wrong, already handled, or not worth addressing. Do not commit, do not push, \
         and do not reply to or resolve any comment. In report, say for each comment whether \
         you addressed it, with the change you made, or skipped it, and why."
    )
}

fn linked(link: &str) -> String {
    format!(
        "Address the review feedback at {link} by changing the code in the current directory. Fetch \
         the feedback yourself, using your own tools and skills. If the link points to a specific \
         comment, consider only that comment; otherwise consider every unresolved review comment \
         on the change."
    )
}

fn given(output: &str) -> String {
    format!(
        "The output of the last successful ncli command the user ran is below. Address the review \
         feedback in it by changing the code in the current directory. If it is not review feedback \
         on the code in the current directory, change nothing, set completed to false, and say in \
         report that the last output is not review feedback.\n\n<output>\n{}\n</output>\n\n",
        output.trim_end()
    )
}
