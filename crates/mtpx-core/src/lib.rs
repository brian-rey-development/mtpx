//! mtpx-core: incremental MTP transfer engine.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod device_path;
mod discovery;
mod entry;
mod error;
mod event;
mod options;
mod path;
mod plan;

pub use device_path::{DevicePath, StorageSelector};
pub use discovery::{DeviceSummary, ExclusiveHolder, StorageSummary};
pub use entry::{Entry, EntryKind, ModifiedTime, SkippedEntry, Snapshot};
pub use error::{Error, Result};
pub use event::{Hint, ProgressEvent, Report, Side};
pub use mtp_rs::{CancelToken, UsbSpeed};
pub use options::{ConflictPolicy, TransferOptions};
pub use path::{PathError, RelPath, RemotePath};
pub use plan::{Action, CopyReason, Plan, PlanSummary, SkipReason};
