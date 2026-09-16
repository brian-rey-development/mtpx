//! What a partial download on the destination remembers about its origin.

use crate::entry::{Entry, ModifiedTime};

/// Identity of the source object a partial download came from. Resume is only allowed when it still matches.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
