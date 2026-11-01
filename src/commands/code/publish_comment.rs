use clap::Args;

use super::Guidance;
use super::review::{Change, Finding, LastReview};
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;

/// Publish the findings of the last review as comments on the reviewed change.
#[derive(Debug, Args)]
pub struct PublishComment {
    #[command(flatten)]
    pub guidance: Guidance,
}

impl PublishComment {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let LastReview { change, verdict } = LastReview::load()?;
        if verdict.findings.is_empty() {
            return Err(Error::NothingToPublish(change));
        }
        let Change::Link(link) = change else {
            return Err(Error::Unpublishable(change));
        };
        let agent = AgentKind::first_available()?;
        context.progress(&format!("publishing {} to {link}", verdict.count()));
        let task = self.guidance.apply(prompt(&link, &verdict.findings));
        agent.perform_isolated(&task, context.cancellation)
    }
}

fn prompt(link: &str, findings: &[Finding]) -> String {
    let comments = findings
        .iter()
        .enumerate()
        .map(|(index, finding)| {
            format!(
                "## Comment {}\n\nFile: {}\nLine: {}\nBody:\n\n{finding}",
                index + 1,
                finding.file,
                finding.line
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Publish the review comments below on the code change at {link}, using your own tools and \
         skills. Post each comment at most once, as an inline comment on its file and line, with its \
         body verbatim. Skip any that is already posted on the change, and any whose finding no \
         longer applies, since the change may have new commits since it was reviewed. Do not \
         modify any code and do not post anything else. In report, list the comments you posted \
         and the ones you skipped, with why.\n\n{comments}"
    )
}
