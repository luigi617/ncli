mod prompts;
mod reviewers;
mod verdict;

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;

use clap::{Args, ValueEnum};

pub use reviewers::{Reviewer, ReviewerKind, SchemaArg};
pub use verdict::{Report, SCHEMA, Verdict};

use crate::commands::Context;
use crate::error::Error;
use crate::process;
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
    pub judge: ReviewerKind,
}

impl Review {
    pub fn run(&self, context: &Context) -> Result<(), Error> {
        let reviewers: Vec<Reviewer> = ReviewerKind::value_variants()
            .iter()
            .map(|kind| kind.reviewer())
            .collect();
        process::require_executables(reviewers.iter().map(|reviewer| reviewer.cli.executable()))?;
        let judge = self.judge.reviewer();
        let verdict = debate(&self.link, &reviewers, &judge, context)?;
        let report = Report {
            prog: context.prog,
            verdict: &verdict,
        };
        writeln!(io::stdout(), "{report}").map_err(Error::Output)
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

pub fn debate(
    link: &str,
    reviewers: &[Reviewer],
    judge: &Reviewer,
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
    let verdict = judge.ask(
        &prompts::judge(link, &transcript(&debate)),
        Some(&SCHEMA),
        context.cancellation,
    )?;
    let verdict = Verdict::parse(&verdict).map_err(|source| Error::Verdict {
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
    reviewers: &'a [Reviewer],
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
                    let answer =
                        reviewer.ask(&prompt_for(&reviewer.name), None, stage_cancellation);
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
