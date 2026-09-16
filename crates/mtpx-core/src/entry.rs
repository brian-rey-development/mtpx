//! Directory entries, second-resolution timestamps and the sorted snapshot that holds them.

use crate::path::RelPath;
use jiff::{civil, tz::TimeZone};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Whether an entry is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file with a byte size.
    File,
    /// A directory. Scanners report size 0 for directories.
    Dir,
}

/// Second-resolution timestamp. MTP `DateTime` has no sub-second precision and FAT rounds to 2 s.
///
/// Holds Unix seconds, never negative; pre-epoch times clamp to the epoch.
// Serde-transparent so the resume sidecar stores a plain integer rather than a nested object.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ModifiedTime(i64);

impl ModifiedTime {
    /// Interprets a device timestamp, or `None` when the fields do not form a valid date.
    #[must_use]
    pub fn from_mtp(dt: mtp_rs::DateTime) -> Option<Self> {
        // MTP DateTime carries no zone, and Android fills it with the device's local wall
        // clock, so the system zone is the closest match to what the user saw on screen.
        let naive = civil::DateTime::new(
            i16::try_from(dt.year).ok()?,
            i8::try_from(dt.month).ok()?,
            i8::try_from(dt.day).ok()?,
            i8::try_from(dt.hour).ok()?,
            i8::try_from(dt.minute).ok()?,
            i8::try_from(dt.second).ok()?,
            0,
        )
        .ok()?;
        let zoned = naive.to_zoned(TimeZone::system()).ok()?;
        Some(Self(zoned.timestamp().as_second().max(0)))
    }

    /// Truncates a system time to whole seconds.
    #[must_use]
    pub fn from_system(t: SystemTime) -> Self {
        let seconds = t
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        Self(seconds)
    }

    /// The same instant as a `SystemTime`.
    #[must_use]
    pub fn as_system(self) -> SystemTime {
        let seconds = u64::try_from(self.0).unwrap_or(0);
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        self.0
    }
}

/// One file or directory inside a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Location relative to the snapshot root.
    pub path: RelPath,
    /// File or directory.
    pub kind: EntryKind,
    /// Byte size; zero for directories.
    pub size: u64,
    /// Last modification time, when the side reports one.
    pub modified: Option<ModifiedTime>,
}

/// An object the device listed but refused to describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    /// Directory whose listing was incomplete.
    pub parent: RelPath,
    /// What the device or filesystem said when asked.
    pub reason: String,
}

/// Recursive listing of one root, sorted by path, directories before their children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    root: String,
    entries: Vec<Entry>,
    skipped: Vec<SkippedEntry>,
}

impl Snapshot {
    /// Builds a snapshot, sorting entries so lookups can binary search.
    #[must_use]
    pub fn new(
        root: impl Into<String>,
        mut entries: Vec<Entry>,
        skipped: Vec<SkippedEntry>,
    ) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        debug_assert!(
            entries.is_sorted_by(|a, b| a.path < b.path),
            "snapshot entries must be unique by path"
        );
        Self {
            root: root.into(),
            entries,
            skipped,
        }
    }

    /// The root this listing was taken from, as displayed to the user.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Every entry, sorted by path.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Objects that could not be described during the scan.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedEntry] {
        &self.skipped
    }

    /// Only the file entries, in path order.
    pub fn files(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind == EntryKind::File)
    }

    /// Only the directory entries, in path order.
    pub fn dirs(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind == EntryKind::Dir)
    }

    /// Looks up one entry by its relative path.
    #[must_use]
    pub fn get(&self, path: &RelPath) -> Option<&Entry> {
        let index = self.entries.binary_search_by(|e| e.path.cmp(path)).ok()?;
        self.entries.get(index)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the listing has no entries at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use jiff::{civil, tz::TimeZone};
    use std::time::{Duration, UNIX_EPOCH};

    fn rel(segments: &[&str]) -> RelPath {
        RelPath::new(segments.iter().copied()).unwrap()
    }

    fn entry(segments: &[&str], kind: EntryKind) -> Entry {
        Entry {
            path: rel(segments),
            kind,
            size: 0,
            modified: None,
        }
    }

    #[test]
    fn from_mtp_reads_naive_local_time() {
        let dt = mtp_rs::DateTime {
            year: 2026,
            month: 9,
            day: 14,
            hour: 18,
            minute: 22,
            second: 33,
        };
        let expected = civil::date(2026, 9, 14)
            .at(18, 22, 33, 0)
            .to_zoned(TimeZone::system())
            .unwrap()
            .timestamp();
        let from_mtp = ModifiedTime::from_mtp(dt).unwrap();
        let from_system = ModifiedTime::from_system(expected.into());
        assert_eq!(from_mtp, from_system);
        assert_eq!(from_mtp.unix_seconds(), expected.as_second());
    }

    #[test]
    fn from_mtp_rejects_invalid_dates() {
        let dt = mtp_rs::DateTime {
            year: 0,
            month: 0,
            day: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(ModifiedTime::from_mtp(dt), None);
    }

    #[test]
    fn from_system_truncates_sub_seconds() {
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 999_999_999);
        let m = ModifiedTime::from_system(t);
        assert_eq!(m.unix_seconds(), 1_700_000_000);
        assert_eq!(
            m.as_system(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        );
    }

    #[test]
    fn from_system_clamps_pre_epoch_to_zero() {
        let t = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(ModifiedTime::from_system(t).unix_seconds(), 0);
    }

    #[test]
    fn from_mtp_clamps_pre_epoch_to_zero() {
        let dt = mtp_rs::DateTime {
            year: 1960,
            month: 6,
            day: 1,
            hour: 12,
            minute: 0,
            second: 0,
        };
        assert_eq!(ModifiedTime::from_mtp(dt).unwrap().unix_seconds(), 0);
    }

    #[test]
    fn modified_time_serializes_as_plain_integer() {
        let m = ModifiedTime::from_system(UNIX_EPOCH + Duration::from_secs(42));
        assert_eq!(serde_json::to_string(&m).unwrap(), "42");
        assert_eq!(serde_json::from_str::<ModifiedTime>("42").unwrap(), m);
    }

    #[test]
    fn snapshot_sorts_and_finds_by_path() {
        let entries = vec![
            entry(&["b.txt"], EntryKind::File),
            entry(&["a", "x.txt"], EntryKind::File),
            entry(&["a"], EntryKind::Dir),
        ];
        let snapshot = Snapshot::new("/DCIM", entries, vec![]);
        let paths: Vec<_> = snapshot.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            vec![rel(&["a"]), rel(&["a", "x.txt"]), rel(&["b.txt"])]
        );
        assert_eq!(
            snapshot.get(&rel(&["a", "x.txt"])).unwrap().kind,
            EntryKind::File
        );
        assert!(snapshot.get(&rel(&["missing"])).is_none());
        assert_eq!(snapshot.root(), "/DCIM");
        assert_eq!(snapshot.len(), 3);
        assert!(!snapshot.is_empty());
        assert!(Snapshot::new("/", vec![], vec![]).is_empty());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "unique")]
    fn snapshot_rejects_duplicate_paths_in_debug() {
        let entries = vec![
            entry(&["a"], EntryKind::Dir),
            entry(&["a"], EntryKind::File),
        ];
        let _ = Snapshot::new("/", entries, vec![]);
    }

    #[test]
    fn snapshot_partitions_files_and_dirs() {
        let entries = vec![
            entry(&["a"], EntryKind::Dir),
            entry(&["a", "x.txt"], EntryKind::File),
            entry(&["b"], EntryKind::Dir),
        ];
        let skipped = vec![SkippedEntry {
            parent: rel(&["b"]),
            reason: "device refused".into(),
        }];
        let snapshot = Snapshot::new("/", entries, skipped);
        assert_eq!(snapshot.files().count(), 1);
        assert_eq!(snapshot.dirs().count(), 2);
        assert_eq!(snapshot.skipped().len(), 1);
    }
}
