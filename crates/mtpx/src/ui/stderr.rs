//! The doors to stderr. Stderr is best effort: a reader that went away must never change the
//! exit code, so nothing here panics the way `eprintln!` does on a closed pipe.

use indicatif::MultiProgress;
use std::io::{self, Write};
use tracing_subscriber::fmt::MakeWriter;

/// Writes `text` and a newline, ignoring any failure.
pub fn line(text: &str) {
    let _ = writeln!(io::stderr().lock(), "{text}");
}

/// The tracing writer: every log line lifts the bars out of the way first, so a warning lands
/// above them instead of being overdrawn. On a hidden draw target it is a plain stderr write.
#[derive(Clone)]
pub struct LogWriter(MultiProgress);

impl LogWriter {
    pub const fn new(progress: MultiProgress) -> Self {
        Self(progress)
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.suspend(|| io::stderr().write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
