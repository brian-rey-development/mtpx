//! The one `io::Result`-returning door to stdout, so no `println!` can panic on a closed pipe;
//! what to do with a `BrokenPipe` is the caller's call (`commands::quiet_when_gone`).

use std::io::{self, Write};

pub fn write(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}
