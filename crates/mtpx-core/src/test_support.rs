//! Fixtures shared by the unit tests of several modules.

#![allow(clippy::unwrap_used)]

use crate::{entry::ModifiedTime, internal::endpoint::Identity, path::RelPath};
use std::time::{Duration, UNIX_EPOCH};

pub(crate) fn rel(path: &str) -> RelPath {
    RelPath::new(path.split('/')).unwrap()
}

pub(crate) fn at(seconds: u64) -> ModifiedTime {
    ModifiedTime::from_system(UNIX_EPOCH + Duration::from_secs(seconds))
}

pub(crate) fn identity(serial: &str, storage: &str) -> Identity {
    Identity {
        device_serial: serial.into(),
        storage: storage.into(),
    }
}

/// Deterministic bytes that do not compress or repeat, so a truncated or shifted copy differs.
#[cfg(feature = "virtual-device")]
pub(crate) fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9E37_79B9;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xFF) as u8
        })
        .collect()
}

#[cfg(feature = "virtual-device")]
pub(crate) const VIRTUAL_CAPACITY: u64 = 1024 * 1024 * 1024;

#[cfg(feature = "virtual-device")]
pub(crate) fn virtual_storage(
    description: &str,
    backing_dir: &std::path::Path,
) -> mtp_rs::VirtualStorageConfig {
    mtp_rs::VirtualStorageConfig {
        description: description.to_owned(),
        capacity: VIRTUAL_CAPACITY,
        backing_dir: backing_dir.to_path_buf(),
        read_only: false,
    }
}

/// A virtual phone that polls nothing and watches nothing, so tests never wait on it.
#[cfg(feature = "virtual-device")]
pub(crate) fn virtual_config(
    serial: &str,
    storages: Vec<mtp_rs::VirtualStorageConfig>,
) -> mtp_rs::VirtualDeviceConfig {
    mtp_rs::VirtualDeviceConfig {
        manufacturer: "mtpx".into(),
        model: "Virtual Phone".into(),
        serial: serial.to_owned(),
        storages,
        event_poll_interval: Duration::ZERO,
        watch_backing_dirs: false,
        ..Default::default()
    }
}
