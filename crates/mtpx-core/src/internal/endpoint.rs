//! The interface both sides of a transfer implement, and the types that cross it.

use crate::{
    entry::{ModifiedTime, Snapshot},
    error::Result,
    path::RelPath,
    planner::{NameFolding, Partials},
};
use bytes::Bytes;
use futures::Stream;
use mtp_rs::CancelToken;
use serde::{Deserialize, Serialize};
use std::{pin::Pin, sync::Arc};

/// Chunk size for streaming reads. One chunk is also the unit of progress reporting.
pub(crate) const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Which device and storage a transfer talks to; recorded in sidecars so a partial from another phone never resumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    /// Serial number of the device: the MTP `DeviceInfo` one, or the USB descriptor's when
    /// that is empty. Empty when the device reports none anywhere, which disables resume.
    pub device_serial: String,
    /// Storage description as the device names it, e.g. "Internal shared storage".
    pub storage: String,
}

/// A stream of file bytes; every item is one chunk or the error that ended the stream.
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// Everything a destination needs to receive one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteRequest {
    /// Where the file lands, relative to the destination root.
    pub path: RelPath,
    /// Size the source promised; the write fails when the stream delivers anything else.
    pub expected_size: u64,
    /// Bytes the destination already holds, as the planner decided.
    pub resume_from: u64,
    /// Modification time to stamp on the finished file, when the source reports one.
    pub modified: Option<ModifiedTime>,
}

/// One side's full listing plus the partial downloads found there (always empty for MTP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanResult {
    /// Every file and directory under the root.
    pub snapshot: Snapshot,
    /// Resumable partial downloads, keyed by their final path.
    pub partials: Partials,
    /// How this side compares names, so the planner can match what the side would merge.
    pub folding: NameFolding,
}

/// One side of a transfer. Implemented by the local filesystem and by an MTP storage.
pub(crate) trait Endpoint: Send + Sync {
    /// Human-readable name for messages, e.g. the local root or "Moto g52:Internal:/DCIM".
    fn label(&self) -> String;
    /// Recursively lists the root. `on_found` receives the running count and may be
    /// invoked from a blocking worker thread, so it must not touch task-local state.
    async fn scan(
        &self,
        cancel: &CancelToken,
        on_found: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<ScanResult>;
    /// Offset a write for `requested` would actually resume from, re-validated against disk.
    /// The executor probes this before opening the source, so a part that changed since
    /// planning streams from zero instead of from a stale tail.
    async fn resume_offset(&self, path: &RelPath, requested: u64) -> Result<u64>;
    /// Streams a file from `offset` to its end.
    async fn read(&self, path: &RelPath, offset: u64, cancel: &CancelToken) -> Result<ByteStream>;
    /// Receives one file. On error the destination keeps whatever lets a later run resume.
    /// Implementations verify the received length against `expected_size` and return
    /// `Error::LengthMismatch` themselves, keeping the partial; `Ok` therefore means exactly
    /// `expected_size` bytes are committed, and the executor does not re-check.
    async fn write(&self, request: WriteRequest, input: ByteStream) -> Result<()>;
    /// Creates a directory and any missing parents.
    async fn mkdir(&self, path: &RelPath) -> Result<()>;
}
