mod prompts;
mod reviewers;

use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;

use clap::{Args, ValueEnum};

pub use reviewers::{Reviewer, ReviewerKind};

use crate::error::Error;
use crate::process::{self, Cancellation};

type Answers<'a> = Vec<(&'a str, String)>;
type Section<'a> = (String, &'a str);

/// Review a PR/CR with several AI CLIs that debate their findings.
#[derive(Debug, Args)]
pub struct Review {
    /// URL of the pull request or code review
    pub link: String,
    /// Reviewer that writes the final verdict
    #[arg(long, value_enum, default_value_t)]
    pub judge: ReviewerKind,
}

impl Review {
    pub fn run(&self, cancellation: &Cancellation) -> Result<(), Error> {
        let reviewers: Vec<Reviewer> = ReviewerKind::value_variants()
            .iter()
            .map(|kind| kind.reviewer())
            .collect();
        process::require_executables(reviewers.iter().map(|reviewer| reviewer.cli.executable()))?;
        let judge = self.judge.reviewer();
        debate(
            &self.link,
            &reviewers,
            &judge,
            cancellation,
            &mut io::stdout(),
        )
    }
}

pub fn debate(
    link: &str,
    reviewers: &[Reviewer],
    judge: &Reviewer,
    cancellation: &Cancellation,
    out: &mut impl Write,
) -> Result<(), Error> {
    let reviews = ask_all(reviewers, "review", cancellation, |_| prompts::review(link))?;
    let mut debate = sections("Review", &reviews);
    emit(out, "", &debate)?;
    let rebuttals = ask_all(reviewers, "rebuttal", cancellation, |name| {
        let own = reviews
            .iter()
            .find(|(other, _)| *other == name)
            .map(|(_, review)| review.trim())
            .expect("every reviewer has a review");
        let others = sections("Review", reviews.iter().filter(|(other, _)| *other != name));
        prompts::rebuttal(link, own, &transcript(&others))
    })?;
    let rebuttals = sections("Rebuttal", &rebuttals);
    emit(out, "\n", &rebuttals)?;
    debate.extend(rebuttals);
    progress("verdict");
    let verdict = judge.ask(&prompts::judge(link, &transcript(&debate)), cancellation)?;
    emit(
        out,
        "\n",
        &[(format!("Verdict: {}", judge.name), verdict.as_str())],
    )
}

fn emit(out: &mut impl Write, separator: &str, sections: &[Section]) -> Result<(), Error> {
    writeln!(out, "{separator}{}", transcript(sections))
        .and_then(|()| out.flush())
        .map_err(Error::Output)
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

fn progress(stage: &str) {
    eprintln!("ncli review: {stage}...");
}

fn ask_all<'a>(
    reviewers: &'a [Reviewer],
    stage: &str,
    cancellation: &Cancellation,
    prompt_for: impl Fn(&str) -> String + Sync,
) -> Result<Answers<'a>, Error> {
    progress(stage);
    let stage_cancellation = cancellation.child();
    thread::scope(|scope| {
        let (sender, answered) = mpsc::channel();
        let threads: Vec<_> = reviewers
            .iter()
            .enumerate()
            .map(|(index, reviewer)| {
                let sender = sender.clone();
                let (prompt_for, stage_cancellation) = (&prompt_for, &stage_cancellation);
                scope.spawn(move || {
                    let answer = reviewer.ask(&prompt_for(&reviewer.name), stage_cancellation);
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
