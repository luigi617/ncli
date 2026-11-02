use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

use clap::Args;
use serde::Deserialize;

use super::review::{self, Assessment, LastDocReview};
use super::{Doc, fix};
use crate::agent::{Agent, AgentKind, Performed, task_schema};
use crate::commands::{Context, Output, parse_link, task};
use crate::error::Error;

static SCHEMA: LazyLock<String> =
    LazyLock::new(|| task_schema(include_str!("written.schema.json")));

static EDITED_SCHEMA: LazyLock<String> =
    LazyLock::new(|| task_schema(include_str!("edited.schema.json")));

/// Write a document, then review and revise it until the review settles.
#[derive(Debug, Args)]
pub struct Write {
    /// What to write; name a URL or a path to put the document there [default: the output, in
    /// Markdown]
    pub task: String,
    /// Most review rounds to run
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_rounds: u32,
}

impl Write {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        let task = task::trimmed(&self.task)?;
        let agent = AgentKind::first_available()?;
        context.progress("writing");
        let (written, document) =
            agent.perform_as::<Written>(&write_prompt(task), &SCHEMA, context.cancellation)?;
        let mut doc = document
            .doc()
            .ok_or_else(|| nothing_written(&agent).resumable(&written.resume))?;
        let mut steps = vec![written];
        let mut round = 1;
        loop {
            context.progress(&format!("round {round}/{}", self.max_rounds));
            let assessment = review::assess(&agent, &review::prompt(&doc, Some(task)), context)
                .map_err(|error| resumable(error, &steps))?;
            if assessment.problems.is_empty() {
                return Ok(finish(context, doc, &steps, &assessment));
            }
            context.progress(&format!("addressing {}", assessment.count()));
            let before = content(&doc).map_err(|error| resumable(error, &steps))?;
            let feedback = feedback(&doc, task, &assessment.report(context.prog), &steps[1..]);
            let (addressed, edited) = address(&agent, &doc, &feedback, context)
                .map_err(|error| resumable(error, &steps))?;
            let revised = match &doc {
                Doc::Text(_) => match addressed.report.trim() {
                    "" => return Err(nothing_written(&agent).resumable(&addressed.resume)),
                    text => Doc::Text(text.to_owned()),
                },
                Doc::Link(_) | Doc::Path(_) => doc.clone(),
            };
            steps.push(addressed);
            let unchanged = match edited {
                Some(edited) => !edited,
                None => before == content(&revised).map_err(|error| resumable(error, &steps))?,
            };
            if unchanged {
                return Ok(finish(context, doc, &steps, &assessment));
            }
            if round == self.max_rounds {
                return Err(Error::Unsettled {
                    rounds: round,
                    report: summary(context, &revised, &steps, &assessment),
                }
                .resumable(&last(&steps).resume));
            }
            doc = revised;
            round += 1;
        }
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "document",
    rename_all = "lowercase",
    deny_unknown_fields
)]
enum Written {
    Markdown(String),
    Path(PathBuf),
    Url(String),
}

impl Written {
    fn doc(self) -> Option<Doc> {
        match self {
            Self::Markdown(text) => Some(text.trim().to_owned())
                .filter(|text| !text.is_empty())
                .map(Doc::Text),
            Self::Path(path) => fs::canonicalize(path)
                .ok()
                .filter(|path| path.is_file())
                .map(Doc::Path),
            Self::Url(link) => parse_link(link.trim()).ok().map(Doc::Link),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edited {
    edited: bool,
}

// A linked document cannot be read here, so the fixer says whether it edited it.
fn address(
    agent: &Agent,
    doc: &Doc,
    feedback: &str,
    context: &Context,
) -> Result<(Performed, Option<bool>), Error> {
    let task = fix::task(doc, feedback);
    match doc {
        Doc::Link(_) => agent
            .perform_as::<Edited>(
                &format!("{task}\n\nSet edited to whether you changed the document."),
                &EDITED_SCHEMA,
                context.cancellation,
            )
            .map(|(addressed, Edited { edited })| (addressed, Some(edited))),
        Doc::Text(_) | Doc::Path(_) => Ok((agent.perform(&task, context.cancellation)?, None)),
    }
}

fn content(doc: &Doc) -> Result<Option<Vec<u8>>, Error> {
    match doc {
        Doc::Text(text) => Ok(Some(text.clone().into_bytes())),
        Doc::Path(path) => fs::read(path).map(Some).map_err(|source| Error::ReadDoc {
            path: path.clone(),
            source,
        }),
        Doc::Link(_) => Ok(None),
    }
}

fn feedback(doc: &Doc, task: &str, review: &str, earlier: &[Performed]) -> String {
    let mut feedback = format!(
        "{}The document was written for this task:\n\n<task>\n{task}\n</task>\n\n",
        fix::given(doc, review)
    );
    if !earlier.is_empty() && !matches!(doc, Doc::Text(_)) {
        let decisions = earlier
            .iter()
            .map(|step| step.report.trim())
            .collect::<Vec<_>>()
            .join("\n\n");
        feedback.push_str(&format!(
            "In earlier rounds you made the decisions below on comments about the same document. \
             Keep a decision to skip a comment that is raised again unless the part of the \
             document it is about has changed or the comment gives new evidence.\n\n\
             <earlier>\n{decisions}\n</earlier>\n\n"
        ));
    }
    feedback
}

fn nothing_written(agent: &Agent) -> Error {
    Error::NothingWritten(agent.name.clone())
}

fn last(steps: &[Performed]) -> &Performed {
    steps.last().expect("the write step is always performed")
}

fn resumable(error: Error, steps: &[Performed]) -> Error {
    error.resumable(&last(steps).resume)
}

fn finish(context: &Context, doc: Doc, steps: &[Performed], assessment: &Assessment) -> Output {
    let text = summary(context, &doc, steps, assessment);
    LastDocReview {
        doc,
        review: assessment.report(context.prog),
    }
    .output(text)
}

// A document given as text is shown instead of the steps, whose reports are earlier drafts.
fn summary(context: &Context, doc: &Doc, steps: &[Performed], assessment: &Assessment) -> String {
    let shown = match doc {
        Doc::Text(text) => vec![
            Performed {
                report: text.clone(),
                resume: last(steps).resume.clone(),
            }
            .to_string(),
        ],
        Doc::Link(link) => located(link, steps),
        Doc::Path(path) => located(&path.display().to_string(), steps),
    };
    shown
        .into_iter()
        .chain([assessment.report(context.prog)])
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn located(location: &str, steps: &[Performed]) -> Vec<String> {
    [location.to_owned()]
        .into_iter()
        .chain(steps.iter().map(ToString::to_string))
        .collect()
}

fn write_prompt(task: &str) -> String {
    format!(
        "{task}\n\n\
         Write this document. If the task says where the document goes, such as a URL or a file \
         path, write it there with the tools you have, set kind to url or path, and put in \
         document only its URL or absolute path. Otherwise do not create or change any file, set \
         kind to markdown, and put in document only the whole document, in Markdown, with \
         nothing before or after it. In report, say in one sentence what you wrote."
    )
}
