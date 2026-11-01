use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cli};
use crate::state::{self, Entry};

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
    let output = state::latest(&[Entry::Output, Entry::Terminal])?.unwrap_or_default();
    let output = String::from_utf8_lossy(&strip_ansi_escapes::strip(output)).into_owned();
    if output.trim().is_empty() {
        return Err(Error::NothingToCopy);
    }
    clipboard.run(&[], Some(&output), None, context.cancellation)?;
    Ok(())
}
