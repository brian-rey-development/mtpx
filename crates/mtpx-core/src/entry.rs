//! Directory entries, second-resolution timestamps and the sorted snapshot that holds them.

use crate::{MtpDateTime, path::RelPath};
use jiff::{civil, tz::TimeZone};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Whether an entry is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file, or on the local side a link resolved to one.
    File,
    /// A directory, or on the local side a link resolved to one.
    Dir,
}

/// Second-resolution timestamp. MTP `DateTime` has no sub-second precision and FAT rounds to 2 s.
///
/// Holds Unix seconds, never negative; pre-epoch times clamp to the epoch.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ModifiedTime(i64);

impl ModifiedTime {
    /// Interprets a device timestamp, or `None` when the fields do not form a valid date.
    #[must_use]
    pub(crate) fn from_mtp(dt: MtpDateTime) -> Option<Self> {
        // MTP carries no zone and Android fills in its local wall clock; the system zone matches what the user saw.
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

    /// The exact second as a `SystemTime`; nothing is lost in the round trip from `from_system`.
    #[must_use]
    pub fn as_system(self) -> SystemTime {
        let seconds = u64::try_from(self.0).unwrap_or(0);
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Seconds since the Unix epoch, never negative.
    #[must_use]
    pub const fn unix_seconds(self) -> i64 {
        self.0
    }
}

/// One file or directory of a scanned side.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
    /// Relative to the scanned root; unique within a snapshot.
    pub path: RelPath,
    /// Files and directories are compared by kind before anything else.
    pub kind: EntryKind,
    /// Zero for directories.
    pub size: u64,
    /// `None` when the side reports none.
    pub modified: Option<ModifiedTime>,
}

impl Entry {
    /// Describes one entry; `size` is what the side reported and is zero for a directory.
    #[must_use]
    pub const fn new(
        path: RelPath,
        kind: EntryKind,
        size: u64,
        modified: Option<ModifiedTime>,
    ) -> Self {
        Self {
            path,
            kind,
            size,
            modified,
        }
    }
}

/// An object the device listed but refused to describe.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkippedEntry {
    /// Directory whose listing was incomplete.
    pub parent: RelPath,
    /// What the device or filesystem said when asked.
    pub reason: String,
}

impl SkippedEntry {
    /// Records that something under `parent` was left out for `reason`.
    #[must_use]
    pub fn new(parent: RelPath, reason: impl Into<String>) -> Self {
        Self {
            parent,
            reason: reason.into(),
        }
    }
}

/// Recursive listing of one root, sorted by path, directories before their children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    label: String,
    entries: Vec<Entry>,
    skipped: Vec<SkippedEntry>,
}

impl Snapshot {
    /// Builds a snapshot, sorting entries so lookups can binary search. Entries sharing a
    /// path keep the first one given, matching the scanners' duplicate rule.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        mut entries: Vec<Entry>,
        skipped: Vec<SkippedEntry>,
    ) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|later, first| later.path == first.path);
        Self {
            label: label.into(),
            entries,
            skipped,
        }
    }

    /// Human-readable name of the scanned side, for messages only; the endpoint label, not
    /// a path (an MTP snapshot reads `SERIAL:STORAGE:/root`).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Every entry, sorted by path with each directory before its children.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Objects the side listed but could not describe; they are absent from `entries()`.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedEntry] {
        &self.skipped
    }

    /// The file entries, in path order.
    pub fn files(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind == EntryKind::File)
    }

    /// The directory entries, in path order, so parents come before children.
    pub fn dirs(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind == EntryKind::Dir)
    }

    /// Exact-path lookup by binary search; no case folding or normalization is applied.
    #[must_use]
    pub fn get(&self, path: &RelPath) -> Option<&Entry> {
        let index = self.entries.binary_search_by(|e| e.path.cmp(path)).ok()?;
        self.entries.get(index)
    }

    /// Number of described entries; skipped objects are not counted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing was described; a root with only refused objects is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::test_support::rel;
    use jiff::{civil, tz::TimeZone};
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(path: &str, kind: EntryKind) -> Entry {
        Entry::new(rel(path), kind, 0, None)
    }

    #[test]
    fn from_mtp_reads_naive_local_time() {
        let dt = MtpDateTime {
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
        let dt = MtpDateTime {
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
        let dt = MtpDateTime {
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
            entry("b.txt", EntryKind::File),
            entry("a/x.txt", EntryKind::File),
            entry("a", EntryKind::Dir),
        ];
        let snapshot = Snapshot::new("/DCIM", entries, vec![]);
        let paths: Vec<_> = snapshot.entries().iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths, vec![rel("a"), rel("a/x.txt"), rel("b.txt")]);
        assert_eq!(snapshot.get(&rel("a/x.txt")).unwrap().kind, EntryKind::File);
        assert!(snapshot.get(&rel("missing")).is_none());
        assert_eq!(snapshot.label(), "/DCIM");
        assert_eq!(snapshot.len(), 3);
        assert!(!snapshot.is_empty());
        assert!(Snapshot::new("/", vec![], vec![]).is_empty());
    }

    #[test]
    fn snapshot_keeps_the_first_of_duplicate_paths() {
        let entries = vec![entry("a", EntryKind::Dir), entry("a", EntryKind::File)];
        let snapshot = Snapshot::new("/", entries, vec![]);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.get(&rel("a")).unwrap().kind, EntryKind::Dir);
    }

    #[test]
    fn snapshot_partitions_files_and_dirs() {
        let entries = vec![
            entry("a", EntryKind::Dir),
            entry("a/x.txt", EntryKind::File),
            entry("b", EntryKind::Dir),
        ];
        let skipped = vec![SkippedEntry::new(rel("b"), "device refused")];
        let snapshot = Snapshot::new("/", entries, skipped);
        assert_eq!(snapshot.files().count(), 1);
        assert_eq!(snapshot.dirs().count(), 2);
        assert_eq!(snapshot.skipped().len(), 1);
    }
}
