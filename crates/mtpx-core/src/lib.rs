//! mtpx-core: incremental MTP transfer engine.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod device_path;
mod discovery;
mod entry;
mod error;
mod event;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the endpoints and sidecar helpers get their first caller in the Phase 5 executor"
    )
)]
mod internal;
mod options;
mod path;
mod plan;
mod planner;

pub use device_path::{DevicePath, StorageSelector};
pub use discovery::{DeviceSummary, ExclusiveHolder, StorageSummary};
pub use entry::{Entry, EntryKind, ModifiedTime, SkippedEntry, Snapshot};
pub use error::{Error, Result};
pub use event::{Hint, ProgressEvent, Report, Side};
pub use mtp_rs::{CancelToken, UsbSpeed};
pub use options::{ConflictPolicy, TransferOptions};
pub use path::{PathError, RelPath, RemotePath};
pub use plan::{Action, CopyReason, Plan, PlanSummary, SkipReason};

/// Exposes the planner and its private inputs to the Criterion benches; not part of the API.
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod __bench {
    pub use crate::{
        internal::partial::{Fingerprint, PartialInfo},
        planner::{Partials, plan},
    };
}
