use crate::process::Cancellation;

#[cfg(unix)]
pub use unix::{exit, install};
#[cfg(windows)]
pub use windows::{exit, install};

#[cfg(unix)]
mod unix {
    use std::io;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::thread;

    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    use signal_hook::low_level::emulate_default_handler;

    use super::interrupted;

    static RECEIVED: AtomicI32 = AtomicI32::new(SIGINT);

    pub fn install() -> io::Result<()> {
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        thread::spawn(move || {
            for signal in signals.forever() {
                RECEIVED.store(signal, Ordering::SeqCst);
                interrupted(|| terminate(signal));
            }
        });
        Ok(())
    }

    // Dying from the signal, rather than exiting with a code, is what tells a calling shell that the
    // command was interrupted, so that a loop running it stops too.
    pub fn exit() -> ! {
        terminate(RECEIVED.load(Ordering::SeqCst))
    }

    fn terminate(signal: i32) -> ! {
        let _ = emulate_default_handler(signal);
        std::process::exit(128 + signal)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::Foundation::STATUS_CONTROL_C_EXIT;

    use super::interrupted;

    pub fn install() -> io::Result<()> {
        ctrlc::set_handler(|| interrupted(|| exit())).map_err(io::Error::other)
    }

    pub fn exit() -> ! {
        std::process::exit(STATUS_CONTROL_C_EXIT)
    }
}

fn interrupted(again: impl FnOnce()) {
    let cancellation = Cancellation::global();
    if cancellation.is_cancelled() {
        again();
    }
    cancellation.cancel();
}
