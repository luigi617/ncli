use std::fmt;
use std::io::{self, Write};

// Diagnostics are best effort: a closed stderr must not abort the run.
pub fn line(message: fmt::Arguments) {
    let _ = writeln!(io::stderr().lock(), "{message}");
}
