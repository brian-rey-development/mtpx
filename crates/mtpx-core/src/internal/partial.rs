//! What a partial download on the destination remembers about its origin, and the on-disk sidecar that stores it.

use crate::{
    entry::{Entry, ModifiedTime},
    error::{Error, Result},
    internal::endpoint::Identity,
    path::RelPath,
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Suffix of the file that holds the bytes received so far.
pub(crate) const PART_SUFFIX: &str = ".mtpx-part";
/// Suffix of the JSON record that describes a partial file.
pub(crate) const SIDECAR_SUFFIX: &str = ".mtpx-part.json";
/// Suffix of the sidecar while it is being replaced; never listed, never read.
pub(crate) const SIDECAR_TMP_SUFFIX: &str = ".mtpx-part.json.tmp";
/// Every suffix a transfer in progress may leave next to its final file.
pub(crate) const RESERVED_SUFFIXES: [&str; 3] = [PART_SUFFIX, SIDECAR_SUFFIX, SIDECAR_TMP_SUFFIX];
const SIDECAR_VERSION: u32 = 1;
/// A genuine sidecar is a few hundred bytes; anything larger under its name was planted or
/// corrupted and is not worth loading into memory.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

/// Whether `name` ends with a suffix mtpx reserves for its own transfer files.
#[must_use]
pub(crate) fn is_reserved_name(name: &str) -> bool {
    RESERVED_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// Identity of the source object a partial download came from; resume is only allowed when it
/// still matches.
///
/// The relative path and the device are not part of it: the planner only sees a partial under
/// the path it was keyed by, and the destination drops sidecars from another device before
/// they reach the planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(not(feature = "bench-internals"), allow(unreachable_pub))]
pub struct Fingerprint {
    /// Size the source reported when the download started.
    pub size: u64,
    /// Modification time the source reported when the download started.
    pub modified: Option<ModifiedTime>,
}

impl Fingerprint {
    /// True when `entry` is the same logical object: same size and same modified time.
    ///
    /// `modified` is compared as-is: two unknown times are equal, so a device that never
    /// reports one still resumes on size alone, while `None` against `Some` is a mismatch
    /// because one side knows a time the other cannot confirm.
    #[must_use]
    pub(crate) fn matches(&self, entry: &Entry) -> bool {
        self.size == entry.size && self.modified == entry.modified
    }
}

/// A partial download found on the destination: how much is on disk and what it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(feature = "bench-internals"), allow(unreachable_pub))]
pub struct PartialInfo {
    /// Identity of the source object the bytes were read from.
    pub fingerprint: Fingerprint,
    /// Bytes already written to the destination.
    pub bytes: u64,
}

/// On-disk record next to a partial file: where it came from and how many bytes are valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Sidecar {
    /// Format version; a sidecar from a different version is ignored.
    pub version: u32,
    /// Device and storage the bytes were read from.
    pub identity: Identity,
    /// Source path relative to the transfer root, as displayed.
    pub path: String,
    /// Size and modified time of the source when the download started.
    pub fingerprint: Fingerprint,
    /// Bytes of the part known to be a correct prefix; anything beyond is discarded on resume.
    pub bytes: u64,
}

impl Sidecar {
    /// Builds a current-version record for one partial download.
    #[must_use]
    pub(crate) fn new(
        identity: Identity,
        path: &RelPath,
        fingerprint: Fingerprint,
        bytes: u64,
    ) -> Self {
        Self {
            version: SIDECAR_VERSION,
            identity,
            path: path.to_string(),
            fingerprint,
            bytes,
        }
    }

    /// True when this record describes a resumable partial for `peer` whose file holds at
    /// least the bytes it vouches for. A peer without a serial never resumes: two phones that
    /// both report none would otherwise splice into one file.
    #[must_use]
    pub(crate) fn is_valid_for(&self, peer: &Identity, part_len: u64) -> bool {
        self.version == SIDECAR_VERSION
            && !peer.device_serial.is_empty()
            && self.identity == *peer
            && part_len >= self.bytes
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where the bytes of a download in progress live for a given final path.
#[must_use]
pub(crate) fn part_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, PART_SUFFIX)
}

/// Where the record of a download in progress lives for a given final path.
#[must_use]
pub(crate) fn sidecar_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, SIDECAR_SUFFIX)
}

fn sidecar_tmp_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, SIDECAR_TMP_SUFFIX)
}

/// Reads the sidecar for `final_path`; `None` when absent, oversized or unparsable, which all
/// mean nothing to resume.
///
/// # Errors
/// Any I/O failure other than the file being absent.
pub(crate) fn read_sidecar_blocking(final_path: &Path) -> Result<Option<Sidecar>> {
    let path = sidecar_path(final_path);
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::local_io("read sidecar", &path)(e)),
    };
    let len = file
        .metadata()
        .map_err(Error::local_io("read sidecar", &path))?
        .len();
    if len > MAX_SIDECAR_BYTES {
        tracing::warn!(path = %path.display(), len, "ignoring oversized sidecar; the partial restarts from zero");
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or_default());
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(Error::local_io("read sidecar", &path))?;
    Ok(parse_sidecar(&path, &bytes))
}

fn parse_sidecar(path: &Path, bytes: &[u8]) -> Option<Sidecar> {
    serde_json::from_slice(bytes)
        .inspect_err(|e| {
            tracing::warn!(path = %path.display(), error = %e, "ignoring unparsable sidecar; the partial restarts from zero");
        })
        .ok()
}

/// Writes or replaces the sidecar for `final_path`, so that a reader sees either the old
/// record or the new one. A run being cancelled rewrites it at the very moment a second
/// Ctrl-C may kill the process, and a half-written record would cost the whole partial.
///
/// # Errors
/// When the record cannot be serialized or written.
pub(crate) async fn write_sidecar(final_path: &Path, sidecar: &Sidecar) -> Result<()> {
    let json = serde_json::to_vec_pretty(sidecar).map_err(io::Error::other)?;
    let tmp = sidecar_tmp_path(final_path);
    tokio::fs::write(&tmp, json)
        .await
        .map_err(Error::local_io("write sidecar", &tmp))?;
    tokio::fs::rename(&tmp, sidecar_path(final_path))
        .await
        .map_err(Error::local_io("rename sidecar", &tmp))
}

async fn remove_if_present(path: PathBuf) -> Result<()> {
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::local_io("remove", &path)(e)),
    }
}

/// Deletes the sidecar for `final_path` and any half-written replacement; absent files are not an error.
///
/// # Errors
/// Any I/O failure other than a file being absent.
pub(crate) async fn remove_sidecar(final_path: &Path) -> Result<()> {
    remove_if_present(sidecar_tmp_path(final_path)).await?;
    remove_if_present(sidecar_path(final_path)).await
}

/// Deletes the partial file and its sidecar for `final_path`; absent files are not an error.
///
/// # Errors
/// Any I/O failure other than a file being absent.
pub(crate) async fn remove_partial(final_path: &Path) -> Result<()> {
    remove_if_present(part_path(final_path)).await?;
    remove_sidecar(final_path).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{
        entry::EntryKind,
        path::RelPath,
        test_support::{at, identity},
    };

    fn entry(size: u64, modified: Option<ModifiedTime>) -> Entry {
        Entry::new(
            RelPath::new(["a.bin"]).unwrap(),
            EntryKind::File,
            size,
            modified,
        )
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

    #[test]
    fn matches_when_neither_side_reports_a_time() {
        let fp = Fingerprint {
            size: 10,
            modified: None,
        };
        assert!(fp.matches(&entry(10, None)));
    }

    #[test]
    fn does_not_match_when_only_the_entry_reports_a_time() {
        let fp = Fingerprint {
            size: 10,
            modified: None,
        };
        assert!(!fp.matches(&entry(10, Some(at(1)))));
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

    #[test]
    fn reserved_names_are_exactly_the_transfer_suffixes() {
        for reserved in [
            "clip.mtpx-part",
            "clip.mtpx-part.json",
            "clip.mtpx-part.json.tmp",
        ] {
            assert!(is_reserved_name(reserved), "{reserved}");
        }
        for plain in ["clip.mp4", "mtpx-part", "clip.mtpx-partial", "clip.json"] {
            assert!(!is_reserved_name(plain), "{plain}");
        }
    }

    #[tokio::test]
    async fn sidecar_round_trips_through_json_on_disk_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        let written = sidecar(42);
        write_sidecar(&final_path, &written).await.unwrap();
        assert_eq!(read_sidecar_blocking(&final_path).unwrap(), Some(written));
        assert!(!sidecar_tmp_path(&final_path).exists());
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

    #[test]
    fn oversized_sidecar_reads_as_none_without_being_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        let file = std::fs::File::create(sidecar_path(&final_path)).unwrap();
        file.set_len(MAX_SIDECAR_BYTES + 1).unwrap();
        assert_eq!(read_sidecar_blocking(&final_path).unwrap(), None);
    }

    #[test]
    fn unreadable_sidecar_is_a_local_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        std::fs::create_dir(sidecar_path(&final_path)).unwrap();
        let err = read_sidecar_blocking(&final_path).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::LocalIo {
                    op: "read sidecar",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn remove_partial_deletes_every_transfer_file_and_tolerates_absence() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("IMG_1.jpg");
        std::fs::write(part_path(&final_path), b"abc").unwrap();
        std::fs::write(sidecar_tmp_path(&final_path), b"{").unwrap();
        write_sidecar(&final_path, &sidecar(3)).await.unwrap();
        remove_partial(&final_path).await.unwrap();
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
        assert!(!sidecar_tmp_path(&final_path).exists());
        remove_partial(&final_path).await.unwrap();
    }

    #[test]
    fn sidecar_is_valid_only_for_the_same_peer_and_version_and_a_part_at_least_as_long() {
        let peer = identity("ZY22", "Internal");
        assert!(sidecar(42).is_valid_for(&peer, 42));
        assert!(sidecar(42).is_valid_for(&peer, 43));
        assert!(!sidecar(42).is_valid_for(&identity("OTHER", "Internal"), 42));
        assert!(!sidecar(42).is_valid_for(&identity("ZY22", "SD card"), 42));
        assert!(!sidecar(42).is_valid_for(&peer, 41));
        let mut future = sidecar(42);
        future.version = 2;
        assert!(!future.is_valid_for(&peer, 42));
    }

    #[test]
    fn a_peer_without_a_serial_never_resumes_even_its_own_sidecar() {
        let anonymous = identity("", "Internal");
        let mut own = sidecar(42);
        own.identity = anonymous.clone();
        assert!(!own.is_valid_for(&anonymous, 42));
    }
}
