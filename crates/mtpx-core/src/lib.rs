//! Incremental, resumable file transfers over MTP.
//!
//! Every transfer scans both sides, plans the difference, and only then executes the plan, so
//! nothing moves until the caller has seen what will happen. Open a [`Device`], turn a remote
//! path plus a local directory into a [`PullJob`], and run it:
//!
//! ```no_run
//! use mtpx_core::{CancelToken, Device, DeviceSelector, DevicePath, TransferOptions};
//! use std::path::Path;
//!
//! # async fn example() -> mtpx_core::Result<()> {
//! let device = Device::open(&DeviceSelector::Only).await?;
//! let cancel = CancelToken::new();
//! let (events, mut rx) = tokio::sync::mpsc::channel(256);
//! tokio::spawn(async move { while let Some(event) = rx.recv().await { println!("{event:?}"); } });
//!
//! let remote: DevicePath = "/DCIM/Camera".parse()?;
//! let job = device.plan_pull(&remote, Path::new("backup"), &TransferOptions::sync(), &cancel, &events).await?;
//! let report = job.run(&cancel, &events).await?;
//! println!("copied {} files", report.copied);
//! device.close().await
//! # }
//! ```
//!
//! The event channel needs a live consumer: every [`ProgressEvent`] except `ScanProgress` and
//! `FileProgress` is awaited, so a full channel stalls the transfer. Those two are best-effort
//! and dropped when the channel is full. Cancellation is cooperative: call
//! [`CancelToken::cancel`] and the run returns `Ok` with `report.interrupted` set, leaving a
//! partial the next plan resumes from.
//!
//! Result types (`*Summary`, [`Entry`], [`SkippedEntry`], [`FailedFile`], [`Report`]) are
//! `#[non_exhaustive]` and gain fields without a major bump; inputs ([`DevicePath`], the
//! selectors, [`ConflictPolicy`]) are exhaustive so callers can build and match them freely.
//!
//! # Public dependencies
//!
//! [`CancelToken`], [`UsbSpeed`], [`MtpError`], [`MtpDateTime`] and the virtual-device configs
//! are re-exported from `mtp-rs` 0.32 and are part of this crate's API, so a minor bump of
//! `mtp-rs` is a breaking change for `mtpx-core`.
//!
//! # Features
//!
//! - `virtual-device`: [`Device::open_virtual`] plus the re-exported [`VirtualDeviceConfig`] and
//!   [`VirtualStorageConfig`], an in-process device backed by local directories so the full
//!   pull, sync and resume path runs in tests without a phone.
//! - `bench-internals`: exposes private planner inputs to the Criterion benches. Not part of
//!   the API.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "unreachable_pub is the convention here: bare pub marks the API, pub(crate) the rest"
)]

mod device;
mod device_path;
mod discovery;
mod display;
mod entry;
mod error;
mod event;
mod internal;
mod options;
mod path;
mod plan;
mod planner;
#[cfg(test)]
pub(crate) mod test_support;

pub use device::{Device, PullJob};
pub use device_path::{DevicePath, StorageSelector};
pub use discovery::{DeviceSelector, DeviceSummary, ExclusiveHolder, StorageSummary, list_devices};
pub use display::sanitize_for_display;
pub use entry::{Entry, EntryKind, ModifiedTime, SkippedEntry, Snapshot};
pub use error::{Error, Result};
pub use event::{FailedFile, Hint, ProgressEvent, Report, Side};
pub use mtp_rs::{CancelToken, DateTime as MtpDateTime, Error as MtpError, UsbSpeed};
#[cfg(feature = "virtual-device")]
pub use mtp_rs::{VirtualDeviceConfig, VirtualStorageConfig};
pub use options::{ConflictPolicy, TransferOptions};
pub use path::{PathError, RelPath, RemotePath};
pub use plan::{Action, CopyReason, Plan, PlanSummary, SkipReason};

/// Exposes the planner and its private inputs to the Criterion benches; not part of the API.
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod __bench {
    pub use crate::{
        internal::partial::{Fingerprint, PartialInfo},
        planner::{NameFolding, Partials, plan},
    };
}
