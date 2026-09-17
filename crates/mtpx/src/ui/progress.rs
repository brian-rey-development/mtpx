//! Renders progress events on stderr: bars on a terminal, one line per event otherwise, and
//! only failures plus the summary when quiet.

use crate::{
    commands::{Output, UiOptions},
    ui::{bars::Bars, format, theme::Theme},
};
use mtpx_core::{Hint, PlanSummary, ProgressEvent, RelPath, Report, Side, SkipReason};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Receiver;

/// Consumes the event channel until it closes and draws what it sees.
pub struct Renderer {
    output: Drawing,
    theme: Theme,
    /// When the command started, so the summary counts the scans as well as the transfer.
    started: Instant,
    total_files: u64,
    done_files: u64,
    file_base: u64,
    resume_from: u64,
    remaining: u64,
}

enum Drawing {
    Bars(Bars),
    Lines,
    Quiet,
}

impl Renderer {
    /// Drains `rx`, drawing each event; returns once the sender side is dropped. `started` is
    /// when the command began, captured before the scans.
    pub async fn run(mut rx: Receiver<ProgressEvent>, ui: UiOptions, started: Instant) {
        let mut renderer = Self::new(ui, started);
        while let Some(event) = rx.recv().await {
            renderer.handle_batch(event);
        }
        renderer.clear();
    }

    fn new(ui: UiOptions, started: Instant) -> Self {
        let output = match ui.output {
            Output::Bars => Drawing::Bars(Bars::new()),
            Output::Lines => Drawing::Lines,
            Output::Quiet => Drawing::Quiet,
        };
        Self {
            output,
            theme: Theme::new(ui.color),
            started,
            total_files: 0,
            done_files: 0,
            file_base: 0,
            resume_from: 0,
            remaining: 0,
        }
    }

    fn handle_batch(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::ScanStarted { side } => self.scan_started(side),
            ProgressEvent::ScanProgress { side, found } => self.scan_progress(side, found),
            ProgressEvent::ScanFinished {
                side,
                entries,
                skipped,
            } => self.scan_finished(side, entries, skipped),
            ProgressEvent::PlanReady { summary, hints } => self.plan_ready(&summary, &hints),
            ProgressEvent::Interrupted { remaining_files } => self.remaining = remaining_files,
            ProgressEvent::Finished { report } => self.finished(&report),
            other => self.handle_file(other),
        }
    }

    fn handle_file(&mut self, event: ProgressEvent) {
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
            _ => {}
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
            format!(
                ", {} the device refused to describe",
                format::count(skipped)
            )
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
        self.total_files = summary.files_to_copy;
        if let Drawing::Bars(bars) = &self.output {
            bars.scanned();
        }
        self.println(&self.theme.bold(&format::plan_line(summary)));
        for line in hints.iter().filter_map(hint_line) {
            self.println(&self.theme.warn(&line));
        }
        if let Drawing::Bars(bars) = &self.output {
            bars.show_transfer(summary.bytes_to_copy, self.total_files);
        }
    }

    fn file_started(&mut self, path: &RelPath, size: u64, resume_from: u64) {
        self.resume_from = resume_from;
        match &self.output {
            Drawing::Bars(bars) => bars.start_file(path, size, resume_from),
            Drawing::Lines if resume_from > 0 => eprintln!(
                "resuming {path} from {} of {}",
                format::size(resume_from),
                format::size(size)
            ),
            Drawing::Lines => eprintln!("copying {path} ({})", format::size(size)),
            Drawing::Quiet => {}
        }
    }

    fn file_progress(&self, bytes: u64) {
        if let Drawing::Bars(bars) = &self.output {
            let moved = bytes.saturating_sub(self.resume_from);
            bars.advance(bytes, self.file_base + moved);
        }
    }

    fn file_finished(&mut self, path: &RelPath, bytes: u64, elapsed: Duration) {
        self.file_base += bytes;
        self.done_files += 1;
        match &self.output {
            Drawing::Bars(bars) => {
                bars.finish_file(self.file_base, self.done_files, self.total_files);
            }
            Drawing::Lines => eprintln!(
                "copied {path} ({}, {})",
                format::size(bytes),
                format::rate(bytes, elapsed)
            ),
            Drawing::Quiet => {}
        }
    }

    fn file_failed(&self, path: &RelPath, error: &str, will_retry: bool) {
        if will_retry {
            self.println(&self.theme.dim(&format!("retrying {path}: {error}")));
            return;
        }
        self.println_always(&self.theme.err(&format!("failed {path}: {error}")));
    }

    fn skipped(&self, path: &RelPath, reason: SkipReason) {
        let line = format!("skipped {path} ({})", format::skip_reason(reason));
        match &self.output {
            Drawing::Lines => eprintln!("{line}"),
            Drawing::Bars(_) if reason == SkipReason::Conflict => {
                self.println(&self.theme.dim(&line));
            }
            Drawing::Bars(_) | Drawing::Quiet => {}
        }
    }

    fn finished(&self, report: &Report) {
        self.clear();
        let line = format::summary_line(report, self.remaining, self.started.elapsed());
        let painted = if report.interrupted {
            self.theme.warn(&line)
        } else if report.failed.is_empty() {
            self.theme.ok(&line)
        } else {
            self.theme.err(&line)
        };
        self.println_always(&painted);
    }

    fn clear(&self) {
        if let Drawing::Bars(bars) = &self.output {
            bars.clear();
        }
    }

    fn println(&self, line: &str) {
        match &self.output {
            Drawing::Bars(bars) => bars.println(line),
            Drawing::Lines => eprintln!("{line}"),
            Drawing::Quiet => {}
        }
    }

    fn println_always(&self, line: &str) {
        match &self.output {
            Drawing::Bars(bars) => bars.println(line),
            Drawing::Lines | Drawing::Quiet => eprintln!("{line}"),
        }
    }
}

const fn side_name(side: Side) -> &'static str {
    match side {
        Side::Source => "the device",
        Side::Dest => "the local directory",
    }
}

fn hint_line(hint: &Hint) -> Option<String> {
    match hint {
        Hint::SlowLink { bytes } => Some(format!(
            "The USB link is USB 2.0 or slower; {} will take a while",
            format::size(*bytes)
        )),
        Hint::DeviceSkippedObjects { count } => Some(format!(
            "{} the device refused to describe were left out of the plan",
            format::objects(*count as u64)
        )),
        _ => {
            tracing::debug!(?hint, "unhandled hint");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::*;

    #[test]
    fn hints_read_as_one_warning_line() {
        let slow = hint_line(&Hint::SlowLink {
            bytes: 2_000_000_000,
        })
        .unwrap();
        assert!(slow.contains("2 GB"), "{slow}");
        let skipped = hint_line(&Hint::DeviceSkippedObjects { count: 1_500 }).unwrap();
        assert!(skipped.starts_with("1,500 objects"), "{skipped}");
        let one = hint_line(&Hint::DeviceSkippedObjects { count: 1 }).unwrap();
        assert!(one.starts_with("1 object "), "{one}");
    }

    #[test]
    fn renderer_accumulates_bytes_across_files() {
        let ui = UiOptions {
            output: Output::Quiet,
            prompts: false,
            color: false,
        };
        let mut renderer = Renderer::new(ui, Instant::now());
        let path = RelPath::new(["a.jpg"]).unwrap();
        renderer.handle_batch(ProgressEvent::ScanStarted { side: Side::Source });
        renderer.handle_batch(ProgressEvent::FileStarted {
            path: path.clone(),
            size: 10,
            resume_from: 4,
        });
        renderer.handle_batch(ProgressEvent::FileFinished {
            path,
            bytes: 6,
            elapsed: Duration::ZERO,
        });
        renderer.handle_batch(ProgressEvent::Interrupted { remaining_files: 3 });
        assert_eq!(renderer.file_base, 6);
        assert_eq!(renderer.done_files, 1);
        assert_eq!(renderer.resume_from, 4);
        assert_eq!(renderer.remaining, 3);
    }
}
