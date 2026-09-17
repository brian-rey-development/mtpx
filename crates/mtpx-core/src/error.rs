//! The single error type of the crate.

use crate::{
    device_path::DevicePath,
    discovery::{DeviceSummary, ExclusiveHolder, StorageSummary},
    path::{PathError, RelPath},
};

/// Everything that can go wrong; the CLI maps each variant to an exit code and message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No MTP device is connected, or none is visible to this user.
    #[error("no MTP device found")]
    NoDevice,
    /// More than one device is connected and none was selected. Always carries two or more.
    #[error("{} MTP devices found, pass --device", .0.len())]
    AmbiguousDevice(Vec<DeviceSummary>),
    /// The device has several storages and none was selected. Carries every storage the device
    /// exposes; empty when it exposes none, as a locked phone does.
    #[error("device has {} storages, pass --storage", .0.len())]
    StorageRequired(Vec<StorageSummary>),
    /// The selected storage name or index matched nothing.
    #[error("storage not found: {0}")]
    StorageNotFound(String),
    /// Another process has the USB interface open.
    #[error("device is held by another process")]
    ExclusiveAccess {
        /// The holder, when the OS lets us identify it.
        holder: Option<ExclusiveHolder>,
    },
    /// The OS refused to open the device for this user.
    #[error("permission denied opening the device")]
    PermissionDenied,
    /// The phone is attached but its MTP side is not talking: the first command after opening
    /// got no answer, which is what a locked phone or one in charging-only mode does.
    #[error("the device did not answer")]
    DeviceUnresponsive,
    /// The remote path does not exist on the selected storage.
    #[error("remote path not found: {0}")]
    RemotePathNotFound(DevicePath),
    /// A directory operation was attempted on a file.
    #[error("not a directory: {0}")]
    NotADirectory(DevicePath),
    /// A path argument could not be parsed.
    #[error(transparent)]
    InvalidPath(#[from] PathError),
    /// Same-path files differ and the conflict policy is `Fail`.
    #[error("conflicting files on both sides ({}); pass --overwrite or --skip-existing", .0.len())]
    Conflicts(Vec<RelPath>),
    /// A source file disappeared after it was named: between planning and copying, or, for a
    /// single-file pull, between opening the path and listing it.
    #[error("source vanished: {0}")]
    SourceVanished(RelPath),
    /// The destination received a different number of bytes than the source promised.
    #[error("length mismatch for {path}: expected {expected} bytes, got {actual}")]
    LengthMismatch {
        /// File that was being copied.
        path: RelPath,
        /// Size the source reported.
        expected: u64,
        /// Bytes actually written.
        actual: u64,
    },
    /// The USB link dropped mid-operation.
    #[error("device disconnected")]
    Disconnected,
    /// The operation was cancelled by the caller.
    #[error("cancelled")]
    Cancelled,
    /// The device or this build does not support the requested operation.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// Any other error from the MTP layer.
    #[error(transparent)]
    Mtp(#[from] mtp_rs::Error),
    /// Any other local filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Lifts the MTP variants the CLI reacts to into their own variants; the rest stay wrapped.
    #[must_use]
    pub fn from_mtp(e: mtp_rs::Error) -> Self {
        match e {
            mtp_rs::Error::Disconnected => Self::Disconnected,
            mtp_rs::Error::Cancelled => Self::Cancelled,
            mtp_rs::Error::NoDevice => Self::NoDevice,
            mtp_rs::Error::ExclusiveAccess => Self::ExclusiveAccess { holder: None },
            mtp_rs::Error::PermissionDenied => Self::PermissionDenied,
            other => Self::Mtp(other),
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
    use super::*;

    #[test]
    fn from_mtp_maps_the_variants_the_cli_reacts_to() {
        let cases = [
            (mtp_rs::Error::Disconnected, "Disconnected"),
            (mtp_rs::Error::Cancelled, "Cancelled"),
            (mtp_rs::Error::NoDevice, "NoDevice"),
            (
                mtp_rs::Error::ExclusiveAccess,
                "ExclusiveAccess { holder: None }",
            ),
            (mtp_rs::Error::PermissionDenied, "PermissionDenied"),
            (mtp_rs::Error::StaleHandle, "Mtp(StaleHandle)"),
            (mtp_rs::Error::Busy, "Mtp(Busy)"),
        ];
        for (input, expected) in cases {
            assert_eq!(format!("{:?}", Error::from_mtp(input)), expected);
        }
    }

    #[test]
    fn is_stale_handle_only_matches_the_wrapped_mtp_variant() {
        assert!(Error::from_mtp(mtp_rs::Error::StaleHandle).is_stale_handle());
        assert!(!Error::from_mtp(mtp_rs::Error::Busy).is_stale_handle());
        assert!(!Error::Cancelled.is_stale_handle());
    }

    #[test]
    fn list_carrying_variants_render_their_count() {
        let err = Error::StorageRequired(vec![]);
        assert_eq!(err.to_string(), "device has 0 storages, pass --storage");
        let err = Error::AmbiguousDevice(vec![]);
        assert_eq!(err.to_string(), "0 MTP devices found, pass --device");
    }

    #[test]
    fn conflicts_message_reads_the_same_for_one_and_many() {
        let one = Error::Conflicts(vec![RelPath::root()]);
        assert_eq!(
            one.to_string(),
            "conflicting files on both sides (1); pass --overwrite or --skip-existing"
        );
    }
}
