use std::iter;
use std::process::ExitCode;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::commands::{Command, Context};
use crate::error::Error;
use crate::interrupt;
use crate::process::Cancellation;
use crate::stderr;

/// Run workflows that chain other CLIs.
#[derive(Debug, Parser)]
#[command(
    name = "ncli",
    version,
    infer_long_args = true,
    args_override_self = true,
    disable_help_subcommand = true
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
        .and_then(|()| ncli.command.run(&context));
    if cancellation.is_cancelled() {
        interrupt::exit();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            stderr::line(format_args!("{prog}: error: {error}"));
            ExitCode::from(error.exit_code())
        }
    }
}

fn prog(matches: &ArgMatches) -> String {
    let root = Ncli::command().get_name().to_owned();
    let subcommands = iter::successors(matches.subcommand(), |(_, sub)| sub.subcommand());
    iter::once(root)
        .chain(subcommands.map(|(name, _)| name.to_owned()))
        .collect::<Vec<_>>()
        .join(" ")
}
