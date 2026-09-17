//! The one `io::Result`-returning door to stdout, so no `println!` can panic on a closed pipe;
//! what to do with a `BrokenPipe` is the caller's call (`commands::quiet_when_gone`).

use std::io::{self, Write};

/// Writes `text` to stdout and flushes it.
///
/// # Errors
/// Any I/O failure, `BrokenPipe` included when the reader closed early, as `ls | head` does.
pub fn write(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}
