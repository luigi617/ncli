use std::io::{self, Write};
use std::process::ExitCode;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::commands::{Command, Context, Output};
use crate::error::Error;
use crate::interrupt;
use crate::process::Cancellation;
use crate::state::{self, Entry};
use crate::stderr;

/// Run workflows that chain other CLIs.
#[derive(Debug, Parser)]
#[command(
    name = "ncli",
    version,
    infer_long_args = true,
    args_override_self = true,
    disable_help_subcommand = true,
    after_help = "A quoted task of several words is given to the first installed of claude and \
                  codex, for example: ncli \"fix the failing test\""
)]
pub struct Ncli {
    #[command(subcommand)]
    pub command: Command,
}

pub fn run() -> ExitCode {
    let matches = Ncli::command().get_matches();
    let ncli = Ncli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    let prog = prog(&matches);
    let cancellation = Cancellation::global();
    let context = Context {
        prog: &prog,
        cancellation,
    };
    let result = interrupt::install()
        .map_err(Error::InterruptHandler)
        .and_then(|()| ncli.command.run(&context))
        .and_then(|output| output.map_or(Ok(()), |output| print(&output)));
    let cancelled = cancellation.is_cancelled();
    if let Err(error) = &result {
        let message = format!("{prog}: error: {error}");
        // An interruption is shown only when it can be resumed, but it is still the last outcome.
        if !cancelled || error.resume().is_some() {
            stderr::line(format_args!("{message}"));
        }
        if ncli.command.records_output() && error.recorded() {
            record_failure(&prog, &message);
        }
    }
    if cancelled {
        interrupt::exit();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => ExitCode::from(error.exit_code()),
    }
}

fn record_failure(prog: &str, message: &str) {
    if let Err(error) = state::save(Entry::Failure, message) {
        stderr::line(format_args!("{prog}: error: {error}"));
    }
}

fn print(output: &Output) -> Result<(), Error> {
    let printed = writeln!(io::stdout(), "{}", output.text).map_err(Error::Output);
    saved(&output.records).and(printed)
}

// A record that fails to save is removed with the ones after it, so none is left from an older run.
fn saved(records: &[(Entry, String)]) -> Result<(), Error> {
    state::remove(Entry::Failure)?;
    for (index, (entry, contents)) in records.iter().enumerate() {
        if let Err(error) = state::save(*entry, contents) {
            let removed: Vec<_> = records[index..]
                .iter()
                .map(|(entry, _)| state::remove(*entry))
                .collect();
            removed.into_iter().collect::<Result<(), _>>()?;
            return Err(error);
        }
    }
    Ok(())
}

fn prog(matches: &ArgMatches) -> String {
    let root = Ncli::command();
    let mut names = vec![root.get_name().to_owned()];
    let (mut command, mut matches) = (&root, matches);
    while let Some((name, sub_matches)) = matches.subcommand() {
        let Some(subcommand) = command.find_subcommand(name) else {
            break;
        };
        names.push(name.to_owned());
        (command, matches) = (subcommand, sub_matches);
    }
    names.join(" ")
}
