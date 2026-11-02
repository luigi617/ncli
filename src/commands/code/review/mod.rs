mod change;
mod prompts;
mod verdict;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

pub use change::{Base, Change, Head, RemoteBranch, Worktree};
pub use verdict::{Finding, Report, SCHEMA, Verdict};

use super::parse_link;
use crate::agent::{Agent, AgentKind, WORKDIR};
use crate::commands::{Context, Output};
use crate::error::Error;
use crate::process::{self, Cancellation};
use crate::state::{self, Entry};

type Answers<'a> = Vec<(&'a str, String)>;
type Section<'a> = (String, &'a str);

/// Review a pull request, or the local changes, with several AI CLIs that debate their findings.
#[derive(Debug, Args)]
pub struct Review {
    /// URL of the pull request [default: the uncommitted changes in the current repository, or else
    /// the commits on its current branch that are not on its default branch]
    #[arg(value_parser = parse_link)]
    pub link: Option<String>,
    #[command(flatten)]
    pub panel: Panel,
}

impl Review {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        state::remove(Entry::Review)?;
        let (reviewers, judge) = self.panel.agents()?;
        let change = match &self.link {
            Some(link) => Change::Link(link.clone()),
            None => Change::local(context.cancellation)?,
        };
        let verdict = debate(&change, None, &reviewers, &judge, context)?;
        let text = Report {
            prog: context.prog,
            verdict: &verdict,
        }
        .to_string();
        let mut output = Output::from(text);
        output.records.push(LastReview { change, verdict }.record());
        Ok(output)
    }
}

#[derive(Debug, Args)]
pub struct Panel {
    /// Reviewer that writes the final verdict
    #[arg(long, value_enum, default_value_t)]
    pub judge: AgentKind,
}

impl Panel {
    pub fn agents(&self) -> Result<(Vec<Agent>, Agent), Error> {
        let reviewers: Vec<Agent> = AgentKind::value_variants()
            .iter()
            .map(|&kind| reviewer(kind))
            .collect();
        process::require_executables(reviewers.iter().map(|reviewer| reviewer.cli.executable()))?;
        Ok((reviewers, reviewer(self.judge)))
    }
}

#[derive(Serialize, Deserialize)]
pub struct LastReview {
    pub change: Change,
    pub verdict: Verdict,
}

impl LastReview {
    pub fn load() -> Result<Self, Error> {
        let json = state::load(Entry::Review)?.ok_or(Error::NoReview)?;
        serde_json::from_slice(&json).map_err(Error::SavedReview)
    }

    pub fn record(&self) -> (Entry, String) {
        let json = serde_json::to_string(self).expect("a review serializes to JSON");
        (Entry::Review, json)
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
        .prefix(&format!("{WORKDIR}review-"))
        .tempdir()
        .map_err(Error::WorkingDirectory)?;
    reviewer.ask(prompt, schema, Some(workdir.path()), cancellation)
}

pub fn debate(
    change: &Change,
    task: Option<&str>,
    reviewers: &[Agent],
    judge: &Agent,
    context: &Context,
) -> Result<Verdict, Error> {
    let reviews = ask_all(reviewers, "review", context, |_| {
        prompts::review(change, task)
    })?;
    let mut debate = sections("Review", &reviews);
    let rebuttals = ask_all(reviewers, "rebuttal", context, |name| {
        let own = reviews
            .iter()
            .find(|(other, _)| *other == name)
            .map(|(_, review)| review.trim())
            .expect("every reviewer has a review");
        let others = sections("Review", reviews.iter().filter(|(other, _)| *other != name));
        prompts::rebuttal(change, task, own, &transcript(&others))
    })?;
    debate.extend(sections("Rebuttal", &rebuttals));
    context.progress("verdict");
    let verdict = ask(
        judge,
        &prompts::judge(change, task, &transcript(&debate)),
        Some(&SCHEMA),
        context.cancellation,
    )?;
    let verdict = Verdict::parse(&verdict).map_err(|source| Error::InvalidAnswer {
        agent: judge.name.clone(),
        source,
        output: verdict,
    })?;
    if !verdict.change_accessed {
        return Err(Error::ChangeUnavailable(change.clone()));
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

fn ask_all<'a>(
    reviewers: &'a [Agent],
    stage: &str,
    context: &Context,
    prompt_for: impl Fn(&str) -> String + Sync,
) -> Result<Answers<'a>, Error> {
    context.progress(stage);
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
