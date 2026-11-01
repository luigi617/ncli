use super::review::{Finding, LastReview};
use crate::agent::AgentKind;
use crate::commands::Context;
use crate::error::Error;

pub fn run(context: &Context) -> Result<String, Error> {
    let LastReview { link, verdict } = LastReview::load()?;
    if verdict.findings.is_empty() {
        return Err(Error::NothingToPublish(link));
    }
    let agent = AgentKind::first_available()?;
    Ok(agent
        .perform(&prompt(&link, &verdict.findings), context.cancellation)?
        .report)
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
         skills. Post each one exactly once, as an inline comment on its file and line, with its \
         body verbatim. Do not modify any code and do not post anything else. In report, list the \
         comments you posted.\n\n{comments}"
    )
}
