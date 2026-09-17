//! Everything the user sees: progress bars, tables, prompts and error reports.

pub mod bars;
pub mod diagnostic;
pub mod format;
pub mod progress;
pub mod prompt;
pub mod stderr;
pub mod stdout;
pub mod table;
#[cfg(test)]
pub mod test_util;
pub mod theme;
