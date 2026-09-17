//! The single error type of the crate.

use crate::{
    MtpError,
    device_path::DevicePath,
    discovery::{DeviceSummary, ExclusiveHolder, StorageSummary},
    path::{PathError, RelPath},
};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Everything that can go wrong; each variant is distinct enough to map to its own exit code
/// and message.
///
/// `Display` output may contain device-supplied names, so a front end passes it through
/// [`sanitize_for_display`](crate::sanitize_for_display) before printing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No MTP device is connected, or none is visible to this user.
    #[error("no MTP device found")]
    NoDevice,
    /// More than one device is connected and none was selected. Always carries two or more.
    #[error("{} MTP devices found; none selected", .0.len())]
    AmbiguousDevice(Vec<DeviceSummary>),
    /// A serial or index was given that no attached device has. `available` is never empty:
    /// an empty bus is `NoDevice`.
    #[error("no device matches {selector}; {} attached", .available.len())]
    DeviceNotFound {
        /// The selector as the caller wrote it, for the message.
        selector: String,
        /// Every device on the bus, so a front end can list the choices.
        available: Vec<DeviceSummary>,
    },
    /// More than one storage is exposed and none was selected. Always carries two or more.
    #[error("device exposes {} storages; none selected", .0.len())]
    StorageRequired(Vec<StorageSummary>),
    /// The device answered but listed no storage, as a locked phone does.
    #[error("the device exposes no storage")]
    NoStorage,
    /// `wanted` is the name or index that was asked for; `available` is every storage the
    /// device exposes.
    #[error("storage not found: {wanted}")]
    StorageNotFound {
        /// The selector as the caller wrote it, for the message.
        wanted: String,
        /// Every storage the device exposes, so a front end can list the choices.
        available: Vec<StorageSummary>,
    },
    /// The OS granted access but another process already owns the device session.
    #[error("device is held by another process")]
    ExclusiveAccess {
        /// The holder, when the OS lets us identify it.
        holder: Option<ExclusiveHolder>,
    },
    /// The OS refused the USB device itself; on Linux this is the udev rule case.
    #[error("permission denied opening the device")]
    PermissionDenied,
    /// The first command after opening got no answer: a locked phone, or charging-only mode.
    #[error("the device did not answer")]
    DeviceUnresponsive,
    /// No object on the device has this path; the device described its parent completely.
    #[error("remote path not found: {0}")]
    RemotePathNotFound(DevicePath),
    /// The path was not found, but the device refused to describe `skipped` objects in its
    /// parent, so it may exist among them.
    #[error(
        "remote path not found: {path} ({skipped} objects under its parent could not be described by the device)"
    )]
    RemotePathUndescribed {
        /// The path as the caller wrote it.
        path: DevicePath,
        /// Objects under its parent the device would not describe.
        skipped: usize,
    },
    /// The remote path exists but names a file, or passes through one, where a directory was
    /// required.
    #[error("not a directory: {0}")]
    NotADirectory(DevicePath),
    /// The local destination exists and is a regular file; nothing was created.
    #[error("local path is not a directory: {}", .0.display())]
    LocalNotADirectory(PathBuf),
    /// A path argument failed validation before anything was opened.
    #[error(transparent)]
    InvalidPath(#[from] PathError),
    /// Under `ConflictPolicy::Fail`: files that differ on both sides, and names that collide on
    /// a case-folding destination. Planned before any byte moves, so nothing was written.
    #[error("conflicting files on both sides ({})", .0.len())]
    Conflicts(Vec<RelPath>),
    /// The source disappeared between planning and copying, or between opening and listing.
    #[error("source vanished: {0}")]
    SourceVanished(RelPath),
    /// The stream ended at a different length than the source reported when the plan was
    /// built; the partial is kept for the next run to revalidate.
    #[error("length mismatch for {path}: expected {expected} bytes, got {actual}")]
    LengthMismatch {
        /// The file whose stream ended early or late.
        path: RelPath,
        /// The size from the plan.
        expected: u64,
        /// The bytes that actually arrived.
        actual: u64,
    },
    /// The USB session is gone; nothing further can succeed on this `Device`.
    #[error("device disconnected")]
    Disconnected,
    /// The cancel token fired before an operation could complete; never returned by a transfer
    /// run, which reports an interrupt through its `Report` instead.
    #[error("cancelled")]
    Cancelled,
    /// The device lacks an MTP operation this crate needs; the message names it.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// An MTP failure with no variant of its own; the `From` impl lifts the ones callers react to.
    #[error(transparent)]
    Mtp(MtpError),
    /// A local filesystem operation failed; `op` names it and `path` is the file it touched.
    #[error("{op} {}: {source}", path.display())]
    LocalIo {
        /// A verb such as `open`, `write` or `rename`, for the message.
        op: &'static str,
        /// The file the operation touched.
        path: PathBuf,
        /// The OS error, for callers that inspect `ErrorKind`.
        #[source]
        source: io::Error,
    },
    /// An I/O failure with no path to name, such as a worker thread that could not be joined.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Lifts the MTP variants callers commonly react to into their own variants; the rest stay wrapped.
impl From<MtpError> for Error {
    fn from(e: MtpError) -> Self {
        match e {
            MtpError::Disconnected => Self::Disconnected,
            MtpError::Cancelled => Self::Cancelled,
            MtpError::NoDevice => Self::NoDevice,
            MtpError::ExclusiveAccess => Self::ExclusiveAccess { holder: None },
            MtpError::PermissionDenied => Self::PermissionDenied,
            other => Self::Mtp(other),
        }
    }
}

impl Error {
    /// Builds the `map_err` closure for a local operation `op` on `path`; the path is only
    /// copied when the operation actually fails.
    pub(crate) fn local_io(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        move |source| Self::LocalIo {
            op,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Whether the device re-keyed the object behind a cached handle, so a re-listing would recover.
    pub(crate) fn is_stale_handle(&self) -> bool {
        matches!(self, Self::Mtp(e) if e.is_stale_handle())
    }
}

/// Shorthand for results carrying [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn from_lifts_the_variants_callers_react_to() {
        assert!(matches!(
            Error::from(MtpError::Disconnected),
            Error::Disconnected
        ));
        assert!(matches!(Error::from(MtpError::Cancelled), Error::Cancelled));
        assert!(matches!(Error::from(MtpError::NoDevice), Error::NoDevice));
        assert!(matches!(
            Error::from(MtpError::ExclusiveAccess),
            Error::ExclusiveAccess { holder: None }
        ));
        assert!(matches!(
            Error::from(MtpError::PermissionDenied),
            Error::PermissionDenied
        ));
        assert!(matches!(
            Error::from(MtpError::StaleHandle),
            Error::Mtp(MtpError::StaleHandle)
        ));
        assert!(matches!(
            Error::from(MtpError::Busy),
            Error::Mtp(MtpError::Busy)
        ));
    }

    #[test]
    fn is_stale_handle_only_matches_the_wrapped_mtp_variant() {
        assert!(Error::from(MtpError::StaleHandle).is_stale_handle());
        assert!(!Error::from(MtpError::Busy).is_stale_handle());
        assert!(!Error::Cancelled.is_stale_handle());
    }

    fn storage(index: usize) -> StorageSummary {
        StorageSummary::new(index, format!("Storage {index}"), 1, 2)
    }

    #[test]
    fn list_carrying_variants_render_their_count_without_naming_a_flag() {
        let err = Error::StorageRequired(vec![storage(0), storage(1)]);
        assert_eq!(err.to_string(), "device exposes 2 storages; none selected");
        let err = Error::AmbiguousDevice(vec![]);
        assert_eq!(err.to_string(), "0 MTP devices found; none selected");
        assert!(!Error::NoStorage.to_string().contains('0'));
        let err = Error::StorageNotFound {
            wanted: "sd".into(),
            available: vec![storage(0)],
        };
        assert_eq!(err.to_string(), "storage not found: sd");
        let err = Error::DeviceNotFound {
            selector: "ZY22".into(),
            available: vec![DeviceSummary::new(None, "Pixel".into(), 1, 2, 3, None)],
        };
        assert_eq!(err.to_string(), "no device matches ZY22; 1 attached");
    }

    #[test]
    fn an_undescribed_path_names_the_path_and_the_count() {
        let path: DevicePath = "/DCIM/x.jpg".parse().unwrap();
        let err = Error::RemotePathUndescribed { path, skipped: 3 };
        assert_eq!(
            err.to_string(),
            "remote path not found: /DCIM/x.jpg (3 objects under its parent could not be described by the device)"
        );
    }

    #[test]
    fn conflicts_message_renders_the_count() {
        let one = Error::Conflicts(vec![RelPath::root()]);
        assert_eq!(one.to_string(), "conflicting files on both sides (1)");
        let two = Error::Conflicts(vec![RelPath::root(), RelPath::root()]);
        assert_eq!(two.to_string(), "conflicting files on both sides (2)");
    }

    #[test]
    fn local_io_names_the_operation_the_path_and_the_cause() {
        let err = Error::local_io("open", Path::new("/tmp/a.jpg.mtpx-part"))(io::Error::from(
            io::ErrorKind::NotFound,
        ));
        assert!(matches!(&err, Error::LocalIo { op: "open", .. }), "{err:?}");
        let shown = err.to_string();
        assert!(shown.starts_with("open /tmp/a.jpg.mtpx-part: "), "{shown}");
        assert!(shown.contains("not found"), "{shown}");
    }
}
