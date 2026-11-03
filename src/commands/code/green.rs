use std::ffi::OsStr;
use std::fmt;

use clap::Args;

use super::git::Git;
use super::review::Worktree;
use super::{changed, resumable};
use crate::agent::{AgentKind, Performed};
use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cli, ProcessError, last_lines};

/// Run a check command, and fix the code until the check passes.
#[derive(Debug, Args)]
pub struct Green {
    /// Most rounds of fixes to run
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_rounds: u32,
    /// The check to run, such as a test command, and its arguments
    #[arg(last = true, required = true, value_name = "CHECK")]
    pub check: Vec<String>,
}

impl Green {
    pub fn run(&self, context: &Context) -> Result<String, Error> {
        let check = Check::new(&self.check)?;
        let baseline = Worktree::snapshot(context.cancellation)?;
        let steps = until_green(&check, &baseline, self.max_rounds, context)?;
        if steps.is_empty() {
            return Ok(format!("`{check}` passes; there was nothing to fix."));
        }
        Ok(steps
            .iter()
            .map(ToString::to_string)
            .chain([format!(
                "`{check}` passes after {} round(s) of fixes.",
                steps.len()
            )])
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

struct Check {
    shown: String,
    cli: Cli,
}

impl Check {
    fn new(argv: &[String]) -> Result<Self, Error> {
        process::require_executables([OsStr::new(&argv[0])])?;
        Ok(Self {
            shown: shlex::try_join(argv.iter().map(String::as_str))
                .expect("command-line arguments cannot hold a NUL"),
            cli: Cli::new(argv).echo(),
        })
    }

    // The end of the output of a failing run, or None if the check passes.
    fn run(&self, baseline: &Worktree, context: &Context) -> Result<Option<String>, Error> {
        context.progress(&format!("running `{self}`"));
        let result = self.cli.run(&[], None, None, context.cancellation);
        let head = Git::new(&baseline.repo, context.cancellation).head()?;
        if head != baseline.head {
            return Err(Error::CheckMovedHead {
                check: self.to_string(),
                repo: baseline.repo.clone(),
                from: Box::new(baseline.head.clone()),
                to: Box::new(head),
            });
        }
        match result {
            Ok(_) => Ok(None),
            Err(error @ (ProcessError::Exit { .. } | ProcessError::Signal { .. })) => Ok(Some(
                [last_lines(error.stdout()), error.to_string()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            Err(error) => Err(error.into()),
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.shown)
    }
}

// Runs the check, and while it fails, has the agent fix the code and runs it again. Returns the
// fixing steps, none if the check passed at once.
fn until_green(
    check: &Check,
    baseline: &Worktree,
    max_rounds: u32,
    context: &Context,
) -> Result<Vec<Performed>, Error> {
    let Some(mut failure) = check.run(baseline, context)? else {
        return Ok(Vec::new());
    };
    let agent = AgentKind::first_available()?;
    let mut steps: Vec<Performed> = Vec::new();
    for round in 1..=max_rounds {
        // Taken after the check ran, so that what the check writes is not counted as a fix.
        let before =
            Worktree::snapshot(context.cancellation).map_err(|error| resumable(error, &steps))?;
        context.progress(&format!("fixing, round {round}/{max_rounds}"));
        let step = agent
            .perform(&fix_prompt(check, &failure, &steps), context.cancellation)
            .map_err(|error| resumable(error, &steps))?;
        steps.push(step);
        if changed(&before, &steps, context.cancellation)?.is_none() {
            let error = Error::NothingFixed {
                agent: agent.name.clone(),
                repo: before.repo.clone(),
                report: steps
                    .last()
                    .expect("a step was just performed")
                    .report
                    .clone(),
            };
            return Err(resumable(error, &steps));
        }
        match check
            .run(baseline, context)
            .map_err(|error| resumable(error, &steps))?
        {
            Some(next) => failure = next,
            None => return Ok(steps),
        }
    }
    let error = Error::StillFailing {
        check: check.to_string(),
        rounds: max_rounds,
        report: steps
            .iter()
            .map(ToString::to_string)
            .chain([failure])
            .collect::<Vec<_>>()
            .join("\n\n"),
    };
    Err(resumable(error, &steps))
}

fn fix_prompt(check: &Check, failure: &str, earlier: &[Performed]) -> String {
    let mut prompt = format!(
        "The check `{check}` fails when run in the current directory. The end of its output is \
         below.\n\n<output>\n{failure}\n</output>\n\n\
         Find the root cause of the failure and fix it by changing the code in the current \
         directory, so that the check passes. Do not weaken, skip, or delete any check or test to \
         make it pass, and do not change what the check runs. Keep any uncommitted changes that \
         were already there unless the fix requires changing them. Do not commit, do not push, \
         and do not open a pull request. In report, say what caused the failure and what you \
         changed."
    );
    if !earlier.is_empty() {
        let changes = earlier
            .iter()
            .map(|step| step.report.trim())
            .collect::<Vec<_>>()
            .join("\n\n");
        prompt.push_str(&format!(
            "\n\nIn earlier rounds you made the changes below, and the check still fails.\n\n\
             <earlier>\n{changes}\n</earlier>"
        ));
    }
    prompt
}
