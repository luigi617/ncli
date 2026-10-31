use std::ffi::OsString;
use std::iter;
use std::process::ExitCode;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::commands::Command;
use crate::process;

pub const INTERRUPTED: u8 = 130;

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

pub fn run<I, T>(argv: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Ncli::command().get_matches_from(argv);
    let ncli = Ncli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    ctrlc::set_handler(process::cancel).expect("failed to install the interrupt handler");
    let result = ncli.command.run();
    if process::is_cancelled() {
        return ExitCode::from(INTERRUPTED);
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: error: {error}", prog(&matches));
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
