//! What a partial download on the destination remembers about its origin, and the on-disk sidecar that stores it.

use crate::{
    entry::{Entry, ModifiedTime},
    error::Result,
    internal::endpoint::Identity,
    path::RelPath,
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Suffix of the file that holds the bytes received so far.
pub const PART_SUFFIX: &str = ".mtpx-part";
/// Suffix of the JSON record that describes a partial file.
pub const SIDECAR_SUFFIX: &str = ".mtpx-part.json";
const SIDECAR_VERSION: u32 = 1;

/// Identity of the source object a partial download came from. Resume is only allowed when it still matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Size the source reported when the download started.
    pub size: u64,
    /// Modification time the source reported when the download started.
    pub modified: Option<ModifiedTime>,
}

impl Fingerprint {
    /// True when `entry` is the same logical object: same size and same modified time.
    ///
    /// `modified` must be equal on both sides, so `None` on one side and `Some` on the other
    /// is a mismatch by design: an unknown time cannot prove identity, and a restart is the
    /// safe outcome.
    ///
    /// The planner only ever sees a partial under the source path it was keyed by, so the
    /// relative path needs no check here. Device serial and storage live in the endpoint
    /// that scans the destination; it drops sidecars from another device before they reach
    /// the planner.
    #[must_use]
    pub fn matches(&self, entry: &Entry) -> bool {
        self.size == entry.size && self.modified == entry.modified
    }
}

/// A partial download found on the destination: how much is on disk and what it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialInfo {
    /// Identity of the source object the bytes were read from.
    pub fingerprint: Fingerprint,
    /// Bytes already written to the destination.
    pub bytes: u64,
}

/// On-disk record next to a partial file: where it came from and how many bytes are valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    /// Format version; a sidecar from a different version is ignored.
    pub version: u32,
    /// Device and storage the bytes were read from.
    pub identity: Identity,
    /// Source path relative to the transfer root, as displayed.
    pub path: String,
    /// Size and modified time of the source when the download started.
    pub fingerprint: Fingerprint,
    /// Bytes the partial file held the last time the record was written.
    pub bytes: u64,
}

impl Sidecar {
    /// Builds a current-version record for one partial download.
    #[must_use]
    pub fn new(identity: Identity, path: &RelPath, fingerprint: Fingerprint, bytes: u64) -> Self {
        Self {
            version: SIDECAR_VERSION,
            identity,
            path: path.to_string(),
            fingerprint,
            bytes,
        }
    }

    /// True when this record describes a resumable partial for `peer` whose file is `part_len` bytes long.
    #[must_use]
    pub fn is_valid_for(&self, peer: &Identity, part_len: u64) -> bool {
        self.version == SIDECAR_VERSION && self.identity == *peer && self.bytes == part_len
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where the bytes of a download in progress live for a given final path.
#[must_use]
pub fn part_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, PART_SUFFIX)
}

/// Where the record of a download in progress lives for a given final path.
#[must_use]
pub fn sidecar_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, SIDECAR_SUFFIX)
}

/// Reads the sidecar for `final_path` on the calling thread; `None` when there is none or it cannot be parsed.
///
/// A missing sidecar and an unreadable one mean the same thing to a scan: nothing to resume.
///
/// # Errors
/// Any I/O failure other than the file being absent.
pub fn read_sidecar_blocking(final_path: &Path) -> Result<Option<Sidecar>> {
    match std::fs::read(sidecar_path(final_path)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Writes or replaces the sidecar for `final_path`.
///
/// # Errors
/// When the record cannot be serialized or written.
pub async fn write_sidecar(final_path: &Path, sidecar: &Sidecar) -> Result<()> {
    let json = serde_json::to_vec_pretty(sidecar).map_err(io::Error::other)?;
    tokio::fs::write(sidecar_path(final_path), json).await?;
    Ok(())
}

async fn remove_if_present(path: PathBuf) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Deletes the partial file and its sidecar for `final_path`; absent files are not an error.
///
/// # Errors
/// Any I/O failure other than a file being absent.
pub async fn remove_partial(final_path: &Path) -> Result<()> {
    remove_if_present(part_path(final_path)).await?;
    remove_if_present(sidecar_path(final_path)).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{entry::EntryKind, path::RelPath};
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(size: u64, modified: Option<ModifiedTime>) -> Entry {
        Entry {
            path: RelPath::new(["a.bin"]).unwrap(),
            kind: EntryKind::File,
            size,
            modified,
        }
    }

    fn at(seconds: u64) -> ModifiedTime {
        ModifiedTime::from_system(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    #[test]
    fn matches_when_size_and_modified_agree() {
        let fp = Fingerprint {
            size: 10,
            modified: Some(at(1)),
        };
        assert!(fp.matches(&entry(10, Some(at(1)))));
    }

    #[test]
    fn does_not_match_when_size_differs() {
        let fp = Fingerprint {
            size: 10,
            modified: Some(at(1)),
        };
        assert!(!fp.matches(&entry(11, Some(at(1)))));
    }

    #[test]
    fn does_not_match_when_modified_differs() {
        let fp = Fingerprint {
            size: 10,
            modified: Some(at(1)),
        };
        assert!(!fp.matches(&entry(10, Some(at(2)))));
        assert!(!fp.matches(&entry(10, None)));
    }

    fn identity(serial: &str, storage: &str) -> Identity {
        Identity {
            device_serial: serial.into(),
            storage: storage.into(),
        }
    }

    fn sidecar(bytes: u64) -> Sidecar {
        let fingerprint = Fingerprint {
            size: 100,
            modified: Some(at(7)),
        };
        let path = RelPath::new(["DCIM", "IMG_1.jpg"]).unwrap();
        Sidecar::new(identity("ZY22", "Internal"), &path, fingerprint, bytes)
    }

    #[test]
    fn part_and_sidecar_paths_append_their_suffixes() {
        assert_eq!(
            part_path(Path::new("a/b.jpg")),
            PathBuf::from("a/b.jpg.mtpx-part")
        );
        assert_eq!(
            sidecar_path(Path::new("a/b.jpg")),
            PathBuf::from("a/b.jpg.mtpx-part.json")
        );
    }

    #[tokio::test]
    async fn sidecar_round_trips_through_json_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        let written = sidecar(42);
        write_sidecar(&final_path, &written).await.unwrap();
        assert_eq!(read_sidecar_blocking(&final_path).unwrap(), Some(written));
        let json = std::fs::read_to_string(sidecar_path(&final_path)).unwrap();
        assert!(json.contains("\"path\": \"DCIM/IMG_1.jpg\""), "{json}");
        assert!(json.contains("\"modified\": 7"), "{json}");
    }

    #[test]
    fn missing_sidecar_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let read = read_sidecar_blocking(&dir.path().join("absent.jpg")).unwrap();
        assert_eq!(read, None);
    }

    #[test]
    fn garbage_sidecar_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        std::fs::write(sidecar_path(&final_path), b"{ not json").unwrap();
        assert_eq!(read_sidecar_blocking(&final_path).unwrap(), None);
    }

    #[tokio::test]
    async fn remove_partial_deletes_both_files_and_tolerates_absence() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        std::fs::write(part_path(&final_path), b"abc").unwrap();
        write_sidecar(&final_path, &sidecar(3)).await.unwrap();
        remove_partial(&final_path).await.unwrap();
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
        remove_partial(&final_path).await.unwrap();
    }

    #[test]
    fn sidecar_is_valid_only_for_the_same_peer_version_and_length() {
        let peer = identity("ZY22", "Internal");
        assert!(sidecar(42).is_valid_for(&peer, 42));
        assert!(!sidecar(42).is_valid_for(&identity("OTHER", "Internal"), 42));
        assert!(!sidecar(42).is_valid_for(&identity("ZY22", "SD card"), 42));
        assert!(!sidecar(42).is_valid_for(&peer, 41));
        let mut future = sidecar(42);
        future.version = 2;
        assert!(!future.is_valid_for(&peer, 42));
    }
}
