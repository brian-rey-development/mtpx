//! Progress events emitted while planning and transferring, plus the final report.

use crate::{
    path::RelPath,
    plan::{PlanSummary, SkipReason},
};
use std::time::Duration;

/// Which end of a transfer an event refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Where files are read from.
    Source,
    /// Where files are written to.
    Dest,
}

/// Advice attached to a ready plan that the CLI may surface before starting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hint {
    /// The USB link is slower than the volume of data warrants.
    SlowLink {
        /// Bytes the plan will move.
        bytes: u64,
    },
    /// The device refused to describe some objects; they are missing from the plan.
    DeviceSkippedObjects {
        /// How many objects were skipped.
        count: usize,
    },
}

/// Totals for a finished or interrupted transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Files fully copied and verified.
    pub copied: u64,
    /// Bytes written to the destination.
    pub bytes: u64,
    /// Files left untouched.
    pub skipped: u64,
    /// Files that failed after retries, with the error message.
    pub failed: Vec<(RelPath, String)>,
    /// Wall-clock time from the first action to the last.
    pub elapsed: Duration,
    /// Whether the transfer was cancelled before the plan completed.
    pub interrupted: bool,
}

/// What a running operation tells its caller, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressEvent {
    /// A recursive listing of one side began.
    ScanStarted {
        /// Which side is being listed.
        side: Side,
    },
    /// The listing found more objects.
    ScanProgress {
        /// Which side is being listed.
        side: Side,
        /// Objects found so far.
        found: u64,
    },
    /// The listing completed.
    ScanFinished {
        /// Which side was listed.
        side: Side,
        /// Objects described.
        entries: u64,
        /// Objects the side refused to describe.
        skipped: u64,
    },
    /// Both sides are scanned and the plan is decided.
    PlanReady {
        /// Totals for the plan.
        summary: PlanSummary,
        /// Advice worth showing before the transfer starts.
        hints: Vec<Hint>,
    },
    /// A file copy began.
    FileStarted {
        /// File being copied.
        path: RelPath,
        /// Full size of the file.
        size: u64,
        /// Offset the copy resumes from; zero for a fresh copy.
        resume_from: u64,
    },
    /// More bytes of the current file landed on the destination.
    FileProgress {
        /// File being copied.
        path: RelPath,
        /// Bytes written so far, including any resumed prefix.
        bytes: u64,
    },
    /// A file copy completed and was length-verified.
    FileFinished {
        /// File that was copied.
        path: RelPath,
        /// Bytes written during this run.
        bytes: u64,
        /// Time spent on this file.
        elapsed: Duration,
    },
    /// A file copy failed.
    FileFailed {
        /// File that failed.
        path: RelPath,
        /// Error message.
        error: String,
        /// Whether the executor will try again.
        will_retry: bool,
    },
    /// A file was deliberately left alone.
    Skipped {
        /// File that was skipped.
        path: RelPath,
        /// Why.
        reason: SkipReason,
    },
    /// Cancellation was requested; the current file is being persisted.
    Interrupted {
        /// Files the plan still had left.
        remaining_files: u64,
    },
    /// The operation is over.
    Finished {
        /// Final totals.
        report: Report,
    },
}
