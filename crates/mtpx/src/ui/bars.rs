//! The indicatif bars behind the terminal renderer: one spinner while scanning, then a per-file
//! bar and an overall bar.

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use mtpx_core::RelPath;
use std::{env, time::Duration};

const SPINNER_TEMPLATE: &str = "{spinner} {msg}";
const FILE_TEMPLATE: &str = "{msg:<32!} {bar:20} {decimal_bytes:>9} {decimal_bytes_per_sec:>11}";
const OVERALL_TEMPLATE: &str = "Overall {bar:20} {decimal_bytes:>9} / {decimal_total_bytes:<9} \
     {percent:>3}% {decimal_bytes_per_sec:>11} eta {eta} {msg}";
const UNICODE_PROGRESS_CHARS: &str = "█▓░";
const UNICODE_TICK_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";
const ASCII_PROGRESS_CHARS: &str = "#>-";
const ASCII_TICK_CHARS: &str = "|/-\\ ";
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Owns the bars and the `MultiProgress` that keeps them below any printed line.
pub struct Bars {
    multi: MultiProgress,
    spinner: ProgressBar,
    file: ProgressBar,
    overall: ProgressBar,
}

/// The characters the bars draw with, picked once from the locale.
#[derive(Debug, Clone, Copy)]
struct Glyphs {
    progress: &'static str,
    tick: &'static str,
}

impl Bars {
    /// Starts the scan spinner on stderr; the transfer bars stay hidden until `show_transfer`.
    pub fn new() -> Self {
        let glyphs = glyphs(utf8_locale());
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
        let spinner = multi.add(spinner(glyphs));
        spinner.enable_steady_tick(TICK_INTERVAL);
        Self {
            multi,
            spinner,
            file: bar(FILE_TEMPLATE, glyphs),
            overall: bar(OVERALL_TEMPLATE, glyphs),
        }
    }

    /// Updates the spinner text.
    pub fn scanning(&self, message: String) {
        self.spinner.set_message(message);
    }

    /// Removes the spinner once both scans are over.
    pub fn scanned(&self) {
        self.spinner.finish_and_clear();
    }

    /// Reveals the transfer bars, sized to what the plan will move.
    pub fn show_transfer(&self, bytes: u64, files: u64) {
        if files == 0 {
            return;
        }
        self.overall.set_length(bytes);
        self.overall.set_message(file_count(0, files));
        self.multi.add(self.file.clone());
        self.multi.add(self.overall.clone());
    }

    /// Points the file bar at a new file.
    pub fn start_file(&self, path: &RelPath, size: u64, resume_from: u64) {
        self.file.reset();
        self.file.set_length(size);
        self.file.set_position(resume_from);
        let name = path.file_name().unwrap_or_default().to_owned();
        self.file.set_message(name);
    }

    /// Moves both bars; `overall` is the running byte total across files.
    pub fn advance(&self, file_bytes: u64, overall: u64) {
        self.file.set_position(file_bytes);
        self.overall.set_position(overall);
    }

    /// Settles the overall bar after a file and updates its file count.
    pub fn finish_file(&self, overall: u64, done: u64, total: u64) {
        self.overall.set_position(overall);
        self.overall.set_message(file_count(done, total));
    }

    /// Prints a line above the bars.
    pub fn println(&self, line: &str) {
        let _ = self.multi.println(line);
    }

    /// Removes every bar from the terminal.
    pub fn clear(&self) {
        self.spinner.finish_and_clear();
        self.file.finish_and_clear();
        self.overall.finish_and_clear();
        let _ = self.multi.clear();
    }
}

fn spinner(glyphs: Glyphs) -> ProgressBar {
    let bar = ProgressBar::new_spinner();
    bar.set_style(style(SPINNER_TEMPLATE).tick_chars(glyphs.tick));
    bar
}

fn bar(template: &str, glyphs: Glyphs) -> ProgressBar {
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::hidden());
    bar.set_style(style(template).progress_chars(glyphs.progress));
    bar
}

const fn glyphs(utf8: bool) -> Glyphs {
    if utf8 {
        return Glyphs {
            progress: UNICODE_PROGRESS_CHARS,
            tick: UNICODE_TICK_CHARS,
        };
    }
    Glyphs {
        progress: ASCII_PROGRESS_CHARS,
        tick: ASCII_TICK_CHARS,
    }
}

/// Windows consoles take Unicode regardless of the C locale; elsewhere the first locale
/// variable set decides, and an unset locale means a modern default.
fn utf8_locale() -> bool {
    if cfg!(windows) {
        return true;
    }
    LOCALE_VARS
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
        .is_none_or(|value| is_utf8(&value))
}

fn is_utf8(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8")
}

/// The templates are constants checked by the tests, so a parse failure cannot reach a user.
fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template).unwrap_or_else(|_| ProgressStyle::default_bar())
}

fn file_count(done: u64, total: u64) -> String {
    format!("({done}/{total})")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn every_template_parses() {
        for template in [SPINNER_TEMPLATE, FILE_TEMPLATE, OVERALL_TEMPLATE] {
            ProgressStyle::with_template(template).unwrap();
        }
    }

    #[test]
    fn locale_values_are_utf8_when_they_say_so_in_any_spelling() {
        for value in ["en_US.UTF-8", "C.utf8", "es_AR.utf-8", "POSIX.UTF8"] {
            assert!(is_utf8(value), "{value}");
        }
        for value in ["C", "POSIX", "en_US.ISO-8859-1", "ja_JP.eucJP", ""] {
            assert!(!is_utf8(value), "{value}");
        }
    }

    #[test]
    fn both_glyph_sets_build_a_style() {
        for utf8 in [true, false] {
            let picked = glyphs(utf8);
            style(FILE_TEMPLATE).progress_chars(picked.progress);
            style(SPINNER_TEMPLATE).tick_chars(picked.tick);
        }
    }
}
