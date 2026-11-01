use clap::Args;

use super::address_comment;
use super::review::{Change, LastReview, Panel, Report, Verdict, Worktree, debate};
use crate::agent::{Agent, AgentKind, Performed};
use crate::commands::{Context, Output, task};
use crate::error::Error;
use crate::process::Cancellation;
use crate::state::{self, Entry};

/// Implement a task in the current repository, then review and address the review until it
/// settles, all locally.
#[derive(Debug, Args)]
pub struct Implement {
    /// What to implement
    pub task: String,
    #[command(flatten)]
    pub panel: Panel,
    /// Most review rounds to run
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_rounds: u32,
}

impl Implement {
    pub fn run(&self, context: &Context) -> Result<Output, Error> {
        let task = task::trimmed(&self.task)?;
        state::remove(Entry::Review)?;
        let (reviewers, judge) = self.panel.agents()?;
        let agent = AgentKind::first_available()?;
        let baseline = Worktree::snapshot(context.cancellation)?;
        context.progress("implementing");
        let mut steps = vec![agent.perform(&implement_prompt(task), context.cancellation)?];
        let mut current = settled(&baseline, &agent, &steps, context.cancellation)?;
        let mut round = 1;
        loop {
            let change = current.change();
            context.progress(&format!("round {round}/{}", self.max_rounds));
            let verdict = debate(&change, Some(task), &reviewers, &judge, context)
                .map_err(|error| resumable(error, &steps))?;
            if verdict.findings.is_empty() {
                return Ok(finish(context, &steps, change, verdict));
            }
            context.progress(&format!("addressing {}", verdict.count()));
            let prompt = address_prompt(context, task, &verdict, &steps[1..]);
            let addressed = agent
                .perform(&prompt, context.cancellation)
                .map_err(|error| resumable(error, &steps))?;
            steps.push(addressed);
            let addressed = settled(&baseline, &agent, &steps, context.cancellation)?;
            if addressed.tree == current.tree {
                return Ok(finish(context, &steps, change, verdict));
            }
            if round == self.max_rounds {
                return Err(Error::Unsettled {
                    rounds: round,
                    report: summary(context, &steps, &verdict),
                });
            }
            current = addressed;
            round += 1;
        }
    }
}

fn settled(
    baseline: &Worktree,
    agent: &Agent,
    steps: &[Performed],
    cancellation: &Cancellation,
) -> Result<Worktree, Error> {
    let worktree = baseline
        .since(cancellation)
        .map_err(|error| resumable(error, steps))?;
    let error = if worktree.head != baseline.head {
        Error::HeadMoved {
            repo: worktree.repo,
            from: Box::new(baseline.head.clone()),
            to: Box::new(worktree.head),
        }
    } else if worktree.tree == baseline.tree {
        Error::NothingImplemented {
            agent: agent.name.clone(),
            repo: worktree.repo,
            report: last(steps).report.clone(),
        }
    } else {
        return Ok(worktree);
    };
    Err(resumable(error, steps))
}

fn last(steps: &[Performed]) -> &Performed {
    steps
        .last()
        .expect("the implement step is always performed")
}

fn resumable(error: Error, steps: &[Performed]) -> Error {
    match error {
        Error::Resumable { .. } => error,
        source => Error::Resumable {
            source: Box::new(source),
            resume: last(steps).resume.clone(),
        },
    }
}

fn summary(context: &Context, steps: &[Performed], verdict: &Verdict) -> String {
    let report = Report {
        prog: context.prog,
        verdict,
    };
    steps
        .iter()
        .map(ToString::to_string)
        .chain([report.to_string()])
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn finish(context: &Context, steps: &[Performed], change: Change, verdict: Verdict) -> Output {
    let mut output = Output::from(summary(context, steps, &verdict));
    output.records.push(LastReview { change, verdict }.record());
    output
}

fn implement_prompt(task: &str) -> String {
    format!(
        "{task}\n\n\
         Implement this by changing the code in the repository in the current directory. Keep any \
         uncommitted changes that were already there unless the task requires changing them. Do \
         not commit, do not push, and do not open a pull request."
    )
}

fn address_prompt(
    context: &Context,
    task: &str,
    verdict: &Verdict,
    earlier: &[Performed],
) -> String {
    let report = Report {
        prog: context.prog,
        verdict,
    };
    let mut feedback = format!(
        "The review findings below are about the changes made in the current directory for this \
         task:\n\n<task>\n{task}\n</task>\n\n\
         Address them by changing the code in the current directory, and leave the changes that \
         were already there before the task alone unless a finding requires otherwise.\n\n\
         <findings>\n{report}\n</findings>"
    );
    if !earlier.is_empty() {
        let decisions = earlier
            .iter()
            .map(|step| step.report.trim())
            .collect::<Vec<_>>()
            .join("\n\n");
        feedback.push_str(&format!(
            "\n\nIn earlier rounds you made the decisions below on findings about the same change. \
             Keep a decision to skip a finding that is raised again unless the code it is about \
             has changed or the finding gives new evidence.\n\n<earlier>\n{decisions}\n</earlier>"
        ));
    }
    feedback.push_str("\n\n");
    address_comment::task(&feedback)
}
