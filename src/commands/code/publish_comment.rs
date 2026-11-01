use clap::Args;

use super::Guidance;
use super::review::{Finding, LastReview};
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
        let LastReview { link, verdict } = LastReview::load()?;
        if verdict.findings.is_empty() {
            return Err(Error::NothingToPublish(link));
        }
        let agent = AgentKind::first_available()?;
        let task = self.guidance.apply(prompt(&link, &verdict.findings));
        Ok(agent.perform(&task, context.cancellation)?.to_string())
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
         body verbatim. Do not modify any code and do not post anything else. In report, list the \
         comments you posted.\n\n{comments}"
    )
}
