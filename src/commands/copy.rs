use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cli};
use crate::state;

#[cfg(target_os = "macos")]
const CLIPBOARD: Option<&[&str]> = Some(&["pbcopy"]);
#[cfg(not(target_os = "macos"))]
const CLIPBOARD: Option<&[&str]> = None;

pub fn run(context: &Context) -> Result<(), Error> {
    let clipboard = Cli::new(
        CLIPBOARD
            .ok_or(Error::ClipboardUnsupported)?
            .iter()
            .copied(),
    );
    process::require_executables([clipboard.executable()])?;
    let output = state::last_output()?;
    clipboard.run(&[], Some(&output), None, context.cancellation)?;
    Ok(())
}
