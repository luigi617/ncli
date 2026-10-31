use std::process::ExitCode;

fn main() -> ExitCode {
    ncli::cli::run(std::env::args_os())
}
