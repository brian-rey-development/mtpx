//! Progress events emitted while planning and transferring, plus the final report.

use crate::{
    entry::SkippedEntry,
    path::RelPath,
    plan::{PlanSummary, SkipReason},
};
use std::time::Duration;
use tokio::sync::mpsc;

/// Which end of a transfer an event refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The side files are read from: the device on a pull.
    Source,
    /// The side files are written to: the local directory on a pull.
    Dest,
}

/// Advice attached to a ready plan that a caller may surface before starting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hint {
    /// The USB link is slower than the volume of data warrants.
    SlowLink {
        /// Bytes the plan will move over that link.
        bytes: u64,
    },
    /// The device refused to describe some objects; they are missing from the plan. Each
    /// entry names the folder whose listing was incomplete and what the device answered.
    DeviceSkippedObjects {
        /// One entry per incomplete folder, in scan order.
        skipped: Vec<SkippedEntry>,
    },
    /// Partial downloads on the destination that no planned copy will resume: their source is
    /// gone or already complete. Nothing removes them; `bytes` is what they occupy.
    StalePartials {
        /// Part files left behind, each with or without its sidecar.
        count: u64,
        /// Disk space those part files occupy.
        bytes: u64,
    },
}

/// A file that failed after retries, with the error message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FailedFile {
    /// Relative to the transfer root on both sides.
    pub path: RelPath,
    /// The final error's message; retried errors before it are not kept.
    pub error: String,
}

impl FailedFile {
    /// Records that `path` failed with `error`, the message as it will be shown.
    #[must_use]
    pub fn new(path: RelPath, error: impl Into<String>) -> Self {
        Self {
            path,
            error: error.into(),
        }
    }
}

/// Totals for a finished, interrupted or aborted transfer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Report {
    /// Files that reached their final name in this run.
    pub copied: u64,
    /// Excludes any resumed prefix; only this run's bytes.
    pub bytes: u64,
    /// Files the plan or a conflict policy left untouched.
    pub skipped: u64,
    /// Includes the file in flight when the run aborted.
    pub failed: Vec<FailedFile>,
    /// Wall-clock time from the first action to the last.
    pub elapsed: Duration,
    /// Set when cancellation stopped the run before its last action.
    pub interrupted: bool,
}

/// What a running operation tells its caller, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressEvent {
    /// A side is about to be walked; both sides are scanned concurrently.
    ScanStarted {
        /// Which walk began.
        side: Side,
    },
    /// Best-effort: dropped when the channel is full.
    ScanProgress {
        /// Which walk advanced.
        side: Side,
        /// Entries seen so far on that side, files and directories together.
        found: u64,
    },
    /// A side's walk is complete; the plan needs both.
    ScanFinished {
        /// Which walk ended.
        side: Side,
        /// Entries that made it into the snapshot.
        entries: u64,
        /// Entries the walk could not describe and left out.
        skipped: u64,
    },
    /// Both scans are in and nothing has moved yet; a dry run stops here.
    PlanReady {
        /// Counts and byte totals of what the plan will do.
        summary: PlanSummary,
        /// Advice worth surfacing before the first byte moves.
        hints: Vec<Hint>,
    },
    /// A copy is about to stream; `FileFinished` or `FileFailed` follows for the same path.
    FileStarted {
        /// Relative to the transfer root on both sides.
        path: RelPath,
        /// The size the source reported when the plan was built.
        size: u64,
        /// Zero for a fresh copy.
        resume_from: u64,
    },
    /// Best-effort: dropped when the channel is full.
    FileProgress {
        /// The file in flight.
        path: RelPath,
        /// Includes any resumed prefix.
        bytes: u64,
    },
    /// The file reached its final name.
    FileFinished {
        /// The file that landed.
        path: RelPath,
        /// Only this run's bytes, unlike `FileProgress`.
        bytes: u64,
        /// Time spent streaming this run's bytes; retry backoff before the stream opened is excluded.
        elapsed: Duration,
    },
    /// A copy attempt failed; with `will_retry` the same path starts again after a backoff.
    FileFailed {
        /// The file that failed.
        path: RelPath,
        /// The attempt's error message.
        error: String,
        /// Whether another attempt follows; false means the file counts as failed.
        will_retry: bool,
    },
    /// A planned skip was reached; nothing was read or written for it.
    Skipped {
        /// The path left untouched.
        path: RelPath,
        /// Why the plan left it alone.
        reason: SkipReason,
    },
    /// Cancellation was honoured. Any file in flight has already kept its partial (or dropped
    /// it when it was a stale tail), and the `remaining_files` copies were not attempted.
    /// Always followed by `Finished` with `report.interrupted` set.
    Interrupted {
        /// Planned copies that never started.
        remaining_files: u64,
    },
    /// The run returned `Ok`; `report` equals the one it returns. Always the last event.
    Finished {
        /// The same totals the run returns.
        report: Report,
    },
    /// The run is returning `Err` because no further endpoint call could succeed; `report`
    /// holds the totals reached so far and is not otherwise returned. Always the last event.
    Aborted {
        /// Totals up to the failure, including the file in flight under `failed`.
        report: Report,
        /// Copies not completed, counting the one that was in flight.
        remaining_files: u64,
    },
}

/// Progress has no observer in the plain library path, so a closed receiver is not an error.
pub(crate) async fn emit(events: &mpsc::Sender<ProgressEvent>, event: ProgressEvent) {
    let _ = events.send(event).await;
}
