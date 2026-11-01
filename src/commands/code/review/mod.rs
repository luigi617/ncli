mod prompts;
mod verdict;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

pub use verdict::{Finding, Report, SCHEMA, Verdict};

use super::parse_link;
use crate::agent::{Agent, AgentKind};
use crate::commands::{Context, Output};
use crate::error::Error;
use crate::process::{self, Cancellation};
use crate::state::{self, Entry};
use crate::stderr;

type Answers<'a> = Vec<(&'a str, String)>;
type Section<'a> = (String, &'a str);

/// Review a pull request with several AI CLIs that debate their findings.
#[derive(Debug, Args)]
pub struct Review {
    /// URL of the pull request
    #[arg(value_parser = parse_link)]
    pub link: String,
    /// Reviewer that writes the final verdict
    #[arg(long, value_enum, default_value_t)]
    pub judge: AgentKind,
}

impl Review {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        let reviewers: Vec<Agent> = AgentKind::value_variants()
            .iter()
            .map(|&kind| reviewer(kind))
            .collect();
        process::require_executables(reviewers.iter().map(|reviewer| reviewer.cli.executable()))?;
        let judge = reviewer(self.judge);
        let verdict = debate(&self.link, &reviewers, &judge, context)?;
        let text = Report {
            prog: context.prog,
            verdict: &verdict,
        }
        .to_string();
        let review = LastReview {
            link: self.link.clone(),
            verdict,
        };
        let json = serde_json::to_string(&review).expect("a review serializes to JSON");
        let mut output = Output::from(text);
        output.records.push((Entry::Review, json));
        Ok(output)
    }
}

#[derive(Serialize, Deserialize)]
pub struct LastReview {
    pub link: String,
    pub verdict: Verdict,
}

impl LastReview {
    pub fn load() -> Result<Self, Error> {
        let json = state::load(Entry::Review)?.ok_or(Error::NoReview)?;
        serde_json::from_slice(&json).map_err(Error::SavedReview)
    }
}

fn reviewer(kind: AgentKind) -> Agent {
    let agent = kind.agent();
    Agent {
        cli: agent
            .cli
            .timeout(Duration::from_secs(30 * 60))
            .attempts(3)
            .retry_delay(Duration::from_secs(10)),
        ..agent
    }
}

fn ask(
    reviewer: &Agent,
    prompt: &str,
    schema: Option<&str>,
    cancellation: &Cancellation,
) -> Result<String, Error> {
    let workdir = tempfile::Builder::new()
        .prefix("ncli-review-")
        .tempdir()
        .map_err(Error::WorkingDirectory)?;
    reviewer.ask(prompt, schema, Some(workdir.path()), cancellation)
}

pub fn debate(
    link: &str,
    reviewers: &[Agent],
    judge: &Agent,
    context: &Context,
) -> Result<Verdict, Error> {
    let reviews = ask_all(reviewers, "review", context, |_| prompts::review(link))?;
    let mut debate = sections("Review", &reviews);
    let rebuttals = ask_all(reviewers, "rebuttal", context, |name| {
        let own = reviews
            .iter()
            .find(|(other, _)| *other == name)
            .map(|(_, review)| review.trim())
            .expect("every reviewer has a review");
        let others = sections("Review", reviews.iter().filter(|(other, _)| *other != name));
        prompts::rebuttal(link, own, &transcript(&others))
    })?;
    debate.extend(sections("Rebuttal", &rebuttals));
    progress(context, "verdict");
    let verdict = ask(
        judge,
        &prompts::judge(link, &transcript(&debate)),
        Some(&SCHEMA),
        context.cancellation,
    )?;
    let verdict = Verdict::parse(&verdict).map_err(|source| Error::InvalidAnswer {
        agent: judge.name.clone(),
        source,
        output: verdict,
    })?;
    if !verdict.change_accessed {
        return Err(Error::ChangeUnavailable(link.to_owned()));
    }
    Ok(verdict)
}

fn transcript(sections: &[Section]) -> String {
    sections
        .iter()
        .map(|(title, body)| format!("## {title}\n\n{}", body.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn sections<'a>(
    stage: &str,
    answers: impl IntoIterator<Item = &'a (&'a str, String)>,
) -> Vec<Section<'a>> {
    answers
        .into_iter()
        .map(|(name, answer)| (format!("{stage}: {name}"), answer.as_str()))
        .collect()
}

fn progress(context: &Context, stage: &str) {
    stderr::line(format_args!("{}: {stage}...", context.prog));
}

fn ask_all<'a>(
    reviewers: &'a [Agent],
    stage: &str,
    context: &Context,
    prompt_for: impl Fn(&str) -> String + Sync,
) -> Result<Answers<'a>, Error> {
    progress(context, stage);
    let stage_cancellation = context.cancellation.child();
    thread::scope(|scope| {
        let (sender, answered) = mpsc::channel();
        let threads: Vec<_> = reviewers
            .iter()
            .enumerate()
            .map(|(index, reviewer)| {
                let sender = sender.clone();
                let (prompt_for, stage_cancellation) = (&prompt_for, &stage_cancellation);
                scope.spawn(move || {
                    let answer = ask(
                        reviewer,
                        &prompt_for(&reviewer.name),
                        None,
                        stage_cancellation,
                    );
                    let _ = sender.send((index, answer));
                })
            })
            .collect();
        drop(sender);
        let mut answers: Vec<Option<String>> = reviewers.iter().map(|_| None).collect();
        let mut failure = None;
        for (index, answer) in answered {
            match answer {
                Ok(answer) => answers[index] = Some(answer),
                Err(error) if failure.is_none() => {
                    stage_cancellation.cancel();
                    failure = Some(error);
                }
                Err(_) => {}
            }
        }
        for thread in threads {
            thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(reviewers
            .iter()
            .zip(answers)
            .map(|(reviewer, answer)| {
                (
                    reviewer.name.as_str(),
                    answer.expect("every reviewer answered"),
                )
            })
            .collect())
    })
}
