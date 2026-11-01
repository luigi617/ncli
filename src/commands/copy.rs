use std::env;

use crate::commands::Context;
use crate::error::Error;
use crate::process::{self, Cli};
use crate::state;

struct Clipboard {
    display: Option<&'static str>,
    argv: &'static [&'static str],
}

#[cfg(target_os = "macos")]
const CLIPBOARDS: &[Clipboard] = &[Clipboard {
    display: None,
    argv: &["pbcopy"],
}];

#[cfg(all(unix, not(target_os = "macos")))]
const CLIPBOARDS: &[Clipboard] = &[
    Clipboard {
        display: Some("WAYLAND_DISPLAY"),
        argv: &["wl-copy"],
    },
    Clipboard {
        display: Some("DISPLAY"),
        argv: &["xclip", "-selection", "clipboard"],
    },
    Clipboard {
        display: Some("DISPLAY"),
        argv: &["xsel", "--clipboard", "--input"],
    },
];

#[cfg(windows)]
const CLIPBOARDS: &[Clipboard] = &[Clipboard {
    display: None,
    argv: &["clip"],
}];

impl Clipboard {
    fn usable(&self) -> bool {
        self.display
            .is_none_or(|display| env::var_os(display).is_some())
    }

    // X11 and Wayland clipboards are served by the process that copied, so it must keep running.
    fn cli(&self) -> Cli {
        let cli = Cli::new(self.argv.iter().copied());
        if self.display.is_some() {
            #[cfg(unix)]
            return cli.leave_running();
        }
        cli
    }
}

// clip reads UTF-16 when the input starts with a byte order mark, and the console code page
// otherwise.
#[cfg(windows)]
fn encode(text: &str) -> Vec<u8> {
    std::iter::once('\u{feff}' as u16)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(windows))]
fn encode(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

pub fn run(context: &Context) -> Result<(), Error> {
    let usable: Vec<Cli> = CLIPBOARDS
        .iter()
        .filter(|clipboard| clipboard.usable())
        .map(Clipboard::cli)
        .collect();
    if usable.is_empty() {
        return Err(Error::NoClipboard);
    }
    let clipboard = process::first_available(usable, Cli::executable)?;
    let output = state::last_output()?;
    clipboard.run(&[], Some(&encode(&output)), None, context.cancellation)?;
    Ok(())
}
