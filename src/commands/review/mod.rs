mod prompts;
mod reviewers;

use std::path::Path;
use std::thread;

use clap::{Args, ValueEnum};

pub use reviewers::{Reviewer, ReviewerKind};

use crate::error::Error;
use crate::process::{self, ProcessError};

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
    pub fn run(&self) -> Result<(), Error> {
        let reviewers: Vec<Reviewer> = ReviewerKind::value_variants()
            .iter()
            .map(|kind| kind.reviewer())
            .collect();
        process::require_executables(reviewers.iter().map(|reviewer| reviewer.cli.executable()))?;
        let judge = self.judge.reviewer();
        println!("{}", debate(&self.link, &reviewers, &judge)?);
        Ok(())
    }
}

pub fn debate(link: &str, reviewers: &[Reviewer], judge: &Reviewer) -> Result<String, Error> {
    let tmp = tempfile::Builder::new()
        .prefix("ncli-review-")
        .tempdir()
        .map_err(Error::WorkingDirectory)?;
    let workdir = tmp.path();
    let reviews = ask_all(reviewers, workdir, "review", |_| prompts::review(link))?;
    let rebuttals = ask_all(reviewers, workdir, "rebuttal", |name| {
        let own = reviews
            .iter()
            .find(|(other, _)| *other == name)
            .map(|(_, review)| review.trim())
            .expect("every reviewer has a review");
        let others = sections("Review", reviews.iter().filter(|(other, _)| *other != name));
        prompts::rebuttal(link, own, &transcript(&others))
    })?;
    let mut debate = sections("Review", &reviews);
    debate.extend(sections("Rebuttal", &rebuttals));
    progress("verdict");
    let verdict = judge.ask(&prompts::judge(link, &transcript(&debate)), workdir)?;
    debate.push((format!("Verdict: {}", judge.name), &verdict));
    Ok(transcript(&debate))
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
    workdir: &Path,
    stage: &str,
    prompt_for: impl Fn(&str) -> String + Sync,
) -> Result<Answers<'a>, ProcessError> {
    progress(stage);
    thread::scope(|scope| {
        let handles: Vec<_> = reviewers
            .iter()
            .map(|reviewer| scope.spawn(|| reviewer.ask(&prompt_for(&reviewer.name), workdir)))
            .collect();
        reviewers
            .iter()
            .zip(handles)
            .map(|(reviewer, handle)| {
                let answer = handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
                Ok((reviewer.name.as_str(), answer))
            })
            .collect()
    })
}
