//! Renders progress events on stderr: bars on a terminal, one line per copy, failure or notable
//! skip otherwise, and only failures plus the summary when quiet.

use crate::{
    commands::{Console, Output},
    ui::{
        bars::Bars,
        format::{self, Omitted},
        stderr,
        theme::Theme,
    },
};
use mtpx_core::{
    Hint, PlanSummary, ProgressEvent, RelPath, Report, Side, SkipReason, SkippedEntry,
    sanitize_for_display,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Receiver;

const INTERRUPTING: &str = "interrupting, finishing the current window...";

/// Consumes the event channel until it closes and draws what it sees.
pub struct Renderer {
    output: Drawing,
    theme: Theme,
    /// When the command started, so the summary counts the scans as well as the transfer.
    started: Instant,
    tally: Tally,
    omitted: Omitted,
    /// Set once the plan is known: only then does a Ctrl-C have a window to finish.
    planned: bool,
}

enum Drawing {
    Bars(Bars),
    Lines,
    Quiet,
}

/// Running file and byte counts across the transfer.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    total_files: u64,
    done_files: u64,
    /// Bytes of every finished file, so the overall bar never rewinds between files.
    file_base: u64,
    /// Where the current file resumed from; its progress events include that prefix.
    resume_from: u64,
}

impl Renderer {
    /// Drains `rx`, drawing each event, and announces the first Ctrl-C above the bars once the
    /// transfer has a window to finish.
    pub async fn run(mut rx: Receiver<ProgressEvent>, console: Console, started: Instant) {
        let mut interrupt = console.interrupt.clone();
        let mut renderer = Self::new(&console, started);
        let mut announced = false;
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(event) => renderer.handle(event),
                    None => break,
                },
                changed = interrupt.changed(), if !announced => {
                    announced = true;
                    if changed.is_ok() {
                        renderer.interrupting();
                    }
                }
            }
        }
        renderer.clear();
    }

    fn new(console: &Console, started: Instant) -> Self {
        let output = match console.ui.output {
            Output::Bars => Drawing::Bars(Bars::new(console.progress.clone())),
            Output::Lines => Drawing::Lines,
            Output::Quiet => Drawing::Quiet,
        };
        Self {
            output,
            theme: Theme::new(console.ui.color),
            started,
            tally: Tally::default(),
            omitted: Omitted::default(),
            planned: false,
        }
    }

    fn handle(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::ScanStarted { side } => self.scan_started(side),
            ProgressEvent::ScanProgress { side, found } => self.scan_progress(side, found),
            ProgressEvent::ScanFinished {
                side,
                entries,
                skipped,
            } => self.scan_finished(side, entries, skipped),
            ProgressEvent::PlanReady { summary, hints } => self.plan_ready(&summary, &hints),
            ProgressEvent::Interrupted { remaining_files } => {
                self.omitted.remaining = remaining_files;
            }
            ProgressEvent::Finished { report } => self.finished(&report),
            ProgressEvent::Aborted {
                report,
                remaining_files,
            } => self.aborted(&report, remaining_files),
            other => self.handle_file_event(other),
        }
    }

    fn handle_file_event(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::FileStarted {
                path,
                size,
                resume_from,
            } => self.file_started(&path, size, resume_from),
            ProgressEvent::FileProgress { bytes, .. } => self.file_progress(bytes),
            ProgressEvent::FileFinished {
                path,
                bytes,
                elapsed,
            } => self.file_finished(&path, bytes, elapsed),
            ProgressEvent::FileFailed {
                path,
                error,
                will_retry,
            } => self.file_failed(&path, &error, will_retry),
            ProgressEvent::Skipped { path, reason } => self.skipped(&path, reason),
            _ => tracing::debug!(?event, "unhandled event"),
        }
    }

    /// Before the plan there is no window to finish: the core stops at once and `main` prints
    /// the plain interrupted line.
    fn interrupting(&self) {
        if self.planned {
            self.println(&self.theme.warn(INTERRUPTING));
        }
    }

    fn scan_started(&self, side: Side) {
        if let Drawing::Bars(bars) = &self.output {
            bars.scanning(format!("Scanning {} ...", side_name(side)));
        }
    }

    fn scan_progress(&self, side: Side, found: u64) {
        if let Drawing::Bars(bars) = &self.output {
            let message = format!(
                "Scanning {} ... {}",
                side_name(side),
                format::objects(found)
            );
            bars.scanning(message);
        }
    }

    fn scan_finished(&self, side: Side, entries: u64, skipped: u64) {
        let note = if skipped > 0 {
            format!(", {} {}", format::count(skipped), skipped_note(side))
        } else {
            String::new()
        };
        self.println(&format!(
            "Scanned {}: {}{note}",
            side_name(side),
            format::objects(entries)
        ));
    }

    fn plan_ready(&mut self, summary: &PlanSummary, hints: &[Hint]) {
        self.tally.total_files = summary.files_to_copy;
        self.planned = true;
        if let Drawing::Bars(bars) = &self.output {
            bars.scanned();
        }
        self.println(&self.theme.bold(&format::plan_line(summary)));
        for hint in hints {
            self.hint(hint);
        }
        if let Drawing::Bars(bars) = &self.output {
            bars.show_transfer(summary.bytes_to_copy, self.tally.total_files);
        }
    }

    /// Objects the device refused to describe are missing from the plan, so that warning is
    /// never silenced and its count reaches the summary; the rest is advice.
    fn hint(&mut self, hint: &Hint) {
        let Some(line) = hint_line(hint) else { return };
        let painted = self.theme.warn(&line);
        if let Hint::DeviceSkippedObjects { skipped } = hint {
            self.omitted.left_out = skipped.len() as u64;
            self.println_always(&painted);
        } else {
            self.println(&painted);
        }
    }

    fn file_started(&mut self, path: &RelPath, size: u64, resume_from: u64) {
        self.tally.resume_from = resume_from;
        match &self.output {
            Drawing::Bars(bars) => bars.start_file(path, size, resume_from),
            Drawing::Lines if resume_from > 0 => stderr::line(&format!(
                "resuming {} from {} of {}",
                format::path(path),
                format::size(resume_from),
                format::size(size)
            )),
            Drawing::Lines => stderr::line(&format!(
                "copying {} ({})",
                format::path(path),
                format::size(size)
            )),
            Drawing::Quiet => {}
        }
    }

    fn file_progress(&self, bytes: u64) {
        if let Drawing::Bars(bars) = &self.output {
            let moved = bytes.saturating_sub(self.tally.resume_from);
            bars.advance(bytes, self.tally.file_base + moved);
        }
    }

    fn file_finished(&mut self, path: &RelPath, bytes: u64, elapsed: Duration) {
        self.tally.file_base += bytes;
        self.tally.done_files += 1;
        match &self.output {
            Drawing::Bars(bars) => {
                let tally = self.tally;
                bars.finish_file(tally.file_base, tally.done_files, tally.total_files);
            }
            Drawing::Lines => stderr::line(&format!(
                "copied {} ({}, {})",
                format::path(path),
                format::size(bytes),
                format::rate(bytes, elapsed)
            )),
            Drawing::Quiet => {}
        }
    }

    fn file_failed(&self, path: &RelPath, error: &str, will_retry: bool) {
        let line = failed_line(path, error, will_retry);
        if will_retry {
            self.println(&self.theme.dim(&line));
            return;
        }
        self.println_always(&self.theme.err(&line));
    }

    /// Identical skips are counted in the plan line and the summary, never listed, in every
    /// mode; a no-op sync piped to a log would otherwise be one line per file.
    fn skipped(&self, path: &RelPath, reason: SkipReason) {
        if !format::skip_is_notable(reason) {
            return;
        }
        let line = format!(
            "skipped {} ({})",
            format::path(path),
            format::skip_reason(reason)
        );
        self.println(&self.theme.dim(&line));
    }

    fn finished(&self, report: &Report) {
        self.clear();
        let line = format::summary_line(report, self.omitted, self.started.elapsed());
        let painted = if report.interrupted {
            self.theme.warn(&line)
        } else if report.failed.is_empty() {
            self.theme.ok(&line)
        } else {
            self.theme.err(&line)
        };
        self.println_always(&painted);
    }

    /// The error itself is not printed here: `main` renders it through the diagnostic path.
    fn aborted(&self, report: &Report, remaining: u64) {
        self.clear();
        let line = format::aborted_line(report, remaining, self.started.elapsed());
        self.println_always(&self.theme.err(&line));
    }

    fn clear(&self) {
        if let Drawing::Bars(bars) = &self.output {
            bars.clear();
        }
    }

    fn println(&self, line: &str) {
        match &self.output {
            Drawing::Bars(bars) => bars.println(line),
            Drawing::Lines => stderr::line(line),
            Drawing::Quiet => {}
        }
    }

    fn println_always(&self, line: &str) {
        match &self.output {
            Drawing::Bars(bars) => bars.println(line),
            Drawing::Lines | Drawing::Quiet => stderr::line(line),
        }
    }
}

const fn side_name(side: Side) -> &'static str {
    match side {
        Side::Source => "the device",
        Side::Dest => "the local directory",
    }
}

/// Why a scan left entries out: the device withholds them; the local walker only drops what it
/// cannot read or name.
const fn skipped_note(side: Side) -> &'static str {
    match side {
        Side::Source => "the device refused to describe",
        Side::Dest => "unreadable or unusably named, left out",
    }
}

/// The path and the error text both come from the device side, so both are sanitized.
fn failed_line(path: &RelPath, error: &str, will_retry: bool) -> String {
    let verb = if will_retry { "retrying" } else { "failed" };
    format!(
        "{verb} {}: {}",
        format::path(path),
        sanitize_for_display(error)
    )
}

fn hint_line(hint: &Hint) -> Option<String> {
    match hint {
        Hint::SlowLink { bytes } => Some(format!(
            "The USB link is USB 2.0 or slower; {} will take a while",
            format::size(*bytes)
        )),
        Hint::DeviceSkippedObjects { skipped } => Some(skipped_objects_lines(skipped)),
        Hint::StalePartials { count, bytes } => Some(format!(
            "{} ({}) will never resume: the file is already complete or no longer on the device",
            partials(*count),
            format::size(*bytes)
        )),
        _ => {
            tracing::debug!(?hint, "unhandled hint");
            None
        }
    }
}

fn skipped_objects_lines(skipped: &[SkippedEntry]) -> String {
    let mut lines = vec![format!(
        "{} the device refused to describe were left out of the plan",
        format::objects(skipped.len() as u64)
    )];
    lines.extend(format::skipped_lines(skipped));
    lines.join("\n")
}

fn partials(count: u64) -> String {
    if count == 1 {
        return "1 partial download".to_owned();
    }
    format!("{} partial downloads", format::count(count))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::*;
    use crate::commands::UiOptions;
    use indicatif::{MultiProgress, ProgressDrawTarget};
    use tokio::sync::{mpsc, watch};

    fn refused(count: usize) -> Vec<SkippedEntry> {
        (0..count)
            .map(|i| SkippedEntry::new(RelPath::root(), format!("refused {i}")))
            .collect()
    }

    /// A quiet console on a hidden draw target, with the Ctrl-C sender handed back.
    fn quiet_console() -> (watch::Sender<bool>, Console) {
        let (interrupt_tx, interrupt) = watch::channel(false);
        let console = Console {
            ui: UiOptions {
                output: Output::Quiet,
                prompts: false,
                color: false,
            },
            progress: MultiProgress::with_draw_target(ProgressDrawTarget::hidden()),
            interrupt,
        };
        (interrupt_tx, console)
    }

    #[test]
    fn hints_read_as_a_warning_line_with_the_refused_objects_beneath() {
        let slow = hint_line(&Hint::SlowLink {
            bytes: 2_000_000_000,
        })
        .unwrap();
        assert!(slow.contains("2 GB"), "{slow}");
        let many = hint_line(&Hint::DeviceSkippedObjects {
            skipped: refused(1_500),
        })
        .unwrap();
        assert!(many.starts_with("1,500 objects"), "{many}");
        assert!(many.contains("\n  .: refused 0\n"), "{many}");
        assert!(many.ends_with("  ... and 1,480 more"), "{many}");
        let one = hint_line(&Hint::DeviceSkippedObjects {
            skipped: refused(1),
        })
        .unwrap();
        assert!(one.starts_with("1 object "), "{one}");
        assert!(one.ends_with("\n  .: refused 0"), "{one}");
    }

    #[test]
    fn stale_partials_hint_counts_them_and_their_size() {
        let one = hint_line(&Hint::StalePartials {
            count: 1,
            bytes: 2_000,
        })
        .unwrap();
        assert!(
            one.starts_with("1 partial download (2 kB) will never resume"),
            "{one}"
        );
        let two = hint_line(&Hint::StalePartials { count: 2, bytes: 0 }).unwrap();
        assert!(two.starts_with("2 partial downloads (0 B)"), "{two}");
    }

    #[test]
    fn renderer_accumulates_bytes_across_files() {
        let (_interrupt, console) = quiet_console();
        let mut renderer = Renderer::new(&console, Instant::now());
        let first = RelPath::new(["a.jpg"]).unwrap();
        renderer.handle(ProgressEvent::ScanStarted { side: Side::Source });
        renderer.handle(ProgressEvent::FileStarted {
            path: first.clone(),
            size: 10,
            resume_from: 4,
        });
        assert_eq!(renderer.tally.resume_from, 4);
        renderer.handle(ProgressEvent::FileFinished {
            path: first,
            bytes: 6,
            elapsed: Duration::ZERO,
        });
        let second = RelPath::new(["b.jpg"]).unwrap();
        renderer.handle(ProgressEvent::FileStarted {
            path: second.clone(),
            size: 4,
            resume_from: 0,
        });
        renderer.handle(ProgressEvent::FileFinished {
            path: second,
            bytes: 4,
            elapsed: Duration::ZERO,
        });
        renderer.handle(ProgressEvent::Interrupted { remaining_files: 3 });
        assert_eq!(renderer.tally.file_base, 10);
        assert_eq!(renderer.tally.done_files, 2);
        assert_eq!(renderer.tally.resume_from, 0);
        assert_eq!(renderer.omitted.remaining, 3);
    }

    #[test]
    fn the_plan_marks_the_transfer_started_and_counts_what_the_device_left_out() {
        let (_interrupt, console) = quiet_console();
        let mut renderer = Renderer::new(&console, Instant::now());
        assert!(!renderer.planned);
        renderer.handle(ProgressEvent::PlanReady {
            summary: PlanSummary::default(),
            hints: vec![Hint::DeviceSkippedObjects {
                skipped: refused(3),
            }],
        });
        assert!(renderer.planned);
        assert_eq!(renderer.omitted.left_out, 3);
    }

    #[test]
    fn a_failed_file_line_is_terminal_safe() {
        let path = RelPath::new(["photo\u{202E}gpj.exe"]).unwrap();
        let line = failed_line(&path, "source vanished: photo\u{202E}gpj.exe", false);
        assert_eq!(
            line,
            "failed photo\u{FFFD}gpj.exe: source vanished: photo\u{FFFD}gpj.exe"
        );
        assert!(failed_line(&path, "timed out", true).starts_with("retrying "));
    }

    #[test]
    fn scan_notes_blame_the_right_side() {
        assert_eq!(skipped_note(Side::Source), "the device refused to describe");
        assert!(!skipped_note(Side::Dest).contains("device"));
    }

    #[tokio::test]
    async fn run_ends_when_the_channel_closes_even_after_an_interrupt() {
        let (events, rx) = mpsc::channel(4);
        let (interrupt, console) = quiet_console();
        let run = tokio::spawn(Renderer::run(rx, console, Instant::now()));
        interrupt.send(true).unwrap();
        drop(interrupt);
        events
            .send(ProgressEvent::Interrupted { remaining_files: 1 })
            .await
            .unwrap();
        drop(events);
        run.await.unwrap();
    }
}
