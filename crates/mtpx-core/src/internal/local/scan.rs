//! The blocking half of a local scan: walks the tree, lists entries, and vets sidecars.

use crate::{
    entry::{Entry, EntryKind, ModifiedTime, SkippedEntry, Snapshot},
    error::{Error, Result},
    internal::{
        endpoint::{Identity, ScanResult},
        local::folding,
        partial::{
            PartialInfo, RESERVED_SUFFIXES, SIDECAR_SUFFIX, Sidecar, is_reserved_name, part_path,
            read_sidecar_blocking,
        },
    },
    path::{PathError, RelPath},
    planner::Partials,
};
use mtp_rs::CancelToken;
use std::{
    fmt,
    fs::{self, Metadata},
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use walkdir::{DirEntry, WalkDir};

const PROGRESS_EVERY: usize = 100;

type PathResult<T> = std::result::Result<T, PathError>;

/// Walks a root on a blocking thread, reporting running counts to `on_found` as it goes.
pub(super) struct Walker {
    root: PathBuf,
    /// A single file name to look at, with its resume files, instead of the whole tree.
    only: Option<String>,
    peer: Identity,
    cancel: CancelToken,
    on_found: Arc<dyn Fn(u64) + Send + Sync>,
}

impl Walker {
    pub(super) const fn new(
        root: PathBuf,
        only: Option<String>,
        peer: Identity,
        cancel: CancelToken,
        on_found: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Self {
        Self {
            root,
            only,
            peer,
            cancel,
            on_found,
        }
    }

    pub(super) fn run(self) -> Result<ScanResult> {
        let mut found = Found::default();
        if self.root_is_present()? {
            self.walk(&mut found)?;
        }
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.report(found.entries.len());
        let label = self.root.display().to_string();
        Ok(ScanResult {
            snapshot: Snapshot::new(label, found.entries, found.skipped),
            partials: found.partials,
            folding: folding::detect(&self.root),
        })
    }

    /// A missing root is an empty destination; an unreadable or non-directory root is an error.
    fn root_is_present(&self) -> Result<bool> {
        match fs::metadata(&self.root) {
            Ok(meta) if meta.is_dir() => Ok(true),
            Ok(_) => Err(Error::LocalNotADirectory(self.root.clone())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::local_io("stat", &self.root)(e)),
        }
    }

    fn walk(&self, found: &mut Found) -> Result<()> {
        let mut items = self.walker().into_iter();
        while let Some(item) = items.next() {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    self.skip_unreadable(&e, found);
                    continue;
                }
            };
            if !self.wanted(&item) {
                continue;
            }
            if self.visit(&item, found)? == Visit::Unnameable && item.file_type().is_dir() {
                items.skip_current_dir();
            }
        }
        Ok(())
    }

    /// Links are followed so the snapshot describes what a write would reach through them.
    fn walker(&self) -> WalkDir {
        let walker = WalkDir::new(&self.root).min_depth(1).follow_links(true);
        match self.only {
            Some(_) => walker.max_depth(1),
            None => walker,
        }
    }

    /// In single-file mode only the file itself and its resume files matter.
    fn wanted(&self, item: &DirEntry) -> bool {
        let Some(name) = &self.only else {
            return true;
        };
        item.file_name()
            .to_string_lossy()
            .strip_prefix(name.as_str())
            .is_some_and(|rest| rest.is_empty() || RESERVED_SUFFIXES.contains(&rest))
    }

    fn report(&self, count: usize) {
        (self.on_found)(count as u64);
    }

    /// Part, sidecar and temporary sidecar files are transfers in progress, not content; a
    /// directory that merely carries such a name is listed like any other.
    fn visit(&self, item: &DirEntry, found: &mut Found) -> Result<Visit> {
        let name = item.file_name().to_string_lossy();
        if item.file_type().is_file() && is_reserved_name(&name) {
            if name.ends_with(SIDECAR_SUFFIX) {
                self.collect_partial(item.path(), &mut found.partials)?;
            }
            return Ok(Visit::Done);
        }
        let path = match self.relative(item.path()) {
            Ok(path) => path,
            Err(reason) => {
                self.skip(item.path(), &reason, found);
                return Ok(Visit::Unnameable);
            }
        };
        match item.metadata() {
            Ok(meta) => self.record(path, &meta, found),
            Err(e) if is_vanished(&e) => {}
            Err(e) => self.skip(item.path(), &e, found),
        }
        Ok(Visit::Done)
    }

    fn record(&self, path: RelPath, meta: &Metadata, found: &mut Found) {
        let Some(kind) = kind_of(meta) else {
            return;
        };
        found.entries.push(entry_from(path, kind, meta));
        if found.entries.len() % PROGRESS_EVERY == 0 {
            self.report(found.entries.len());
        }
    }

    /// An entry the walk could not list is reported under its parent and left out.
    fn skip(&self, path: &Path, reason: &dyn fmt::Display, found: &mut Found) {
        let parent = path
            .parent()
            .and_then(|parent| self.relative(parent).ok())
            .unwrap_or_else(RelPath::root);
        tracing::warn!(path = %path.display(), %reason, "skipping unlistable entry");
        found
            .skipped
            .push(SkippedEntry::new(parent, reason.to_string()));
    }

    /// A dangling link, a link loop or an unreadable directory: walkdir moves past it on its own.
    fn skip_unreadable(&self, error: &walkdir::Error, found: &mut Found) {
        let path = error.path().unwrap_or(&self.root);
        self.skip(path, error, found);
    }

    /// Adds the partial a sidecar describes, unless anything about it fails to line up.
    fn collect_partial(&self, sidecar_path: &Path, partials: &mut Partials) -> Result<()> {
        let Some((final_path, sidecar, part_len)) = load_partial(sidecar_path)? else {
            return Ok(());
        };
        let Ok(path) = self.relative(&final_path) else {
            return Ok(());
        };
        if sidecar.is_valid_for(&self.peer, part_len) && sidecar.path == path.to_string() {
            let info = PartialInfo {
                fingerprint: sidecar.fingerprint,
                bytes: sidecar.bytes,
            };
            partials.insert(path, info);
        }
        Ok(())
    }

    fn relative(&self, path: &Path) -> PathResult<RelPath> {
        let rest = path
            .strip_prefix(&self.root)
            .map_err(|_| PathError::InvalidSegment(path.display().to_string()))?;
        let segments = rest
            .components()
            .map(segment_of)
            .collect::<PathResult<Vec<_>>>()?;
        RelPath::new(segments)
    }
}

/// What the tree holds, accumulated as the walk goes.
#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    partials: Partials,
    skipped: Vec<SkippedEntry>,
}

/// Whether an entry was handled, or has a name that makes it and anything below it unlistable.
#[derive(Debug, PartialEq, Eq)]
enum Visit {
    Done,
    Unnameable,
}

fn is_vanished(error: &walkdir::Error) -> bool {
    error
        .io_error()
        .is_some_and(|io| io.kind() == io::ErrorKind::NotFound)
}

fn segment_of(component: Component<'_>) -> PathResult<String> {
    let invalid =
        || PathError::InvalidSegment(component.as_os_str().to_string_lossy().into_owned());
    let Component::Normal(name) = component else {
        return Err(invalid());
    };
    name.to_str().map(str::to_owned).ok_or_else(invalid)
}

/// The sidecar and the length of the part it describes, when both exist on disk.
fn load_partial(sidecar_path: &Path) -> Result<Option<(PathBuf, Sidecar, u64)>> {
    let Some(final_path) = final_path_of(sidecar_path) else {
        return Ok(None);
    };
    let Some(sidecar) = read_sidecar_blocking(&final_path)? else {
        return Ok(None);
    };
    let Some(part_len) = part_len(&part_path(&final_path))? else {
        return Ok(None);
    };
    Ok(Some((final_path, sidecar, part_len)))
}

fn final_path_of(sidecar_path: &Path) -> Option<PathBuf> {
    let name = sidecar_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(SIDECAR_SUFFIX)?;
    (!stem.is_empty()).then(|| sidecar_path.with_file_name(stem))
}

/// The length of a regular file at `part`; anything else there means no part.
fn part_len(part: &Path) -> Result<Option<u64>> {
    match fs::metadata(part) {
        Ok(meta) => Ok(meta.is_file().then_some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::local_io("stat", part)(e)),
    }
}

/// Only files and directories, seen through any link, are described; special files have no
/// place in a snapshot.
fn kind_of(meta: &Metadata) -> Option<EntryKind> {
    if meta.is_dir() {
        Some(EntryKind::Dir)
    } else if meta.is_file() {
        Some(EntryKind::File)
    } else {
        None
    }
}

fn entry_from(path: RelPath, kind: EntryKind, meta: &Metadata) -> Entry {
    let size = match kind {
        EntryKind::File => meta.len(),
        EntryKind::Dir => 0,
    };
    let modified = meta.modified().ok().map(ModifiedTime::from_system);
    Entry::new(path, kind, size, modified)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::super::test_support::{endpoint, identity, rel, scan, write_valid_partial};
    use crate::{
        entry::{EntryKind, Snapshot},
        error::Error,
        internal::{
            endpoint::{Endpoint, ScanResult},
            local::LocalEndpoint,
            partial::{Fingerprint, SIDECAR_TMP_SUFFIX, Sidecar, part_path, sidecar_path},
        },
        path::{PathError, RelPath},
    };
    use mtp_rs::CancelToken;
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    fn listed(snapshot: &Snapshot) -> Vec<String> {
        snapshot
            .entries()
            .iter()
            .map(|e| e.path.to_string())
            .collect()
    }

    async fn scan_result(local: &LocalEndpoint) -> crate::error::Result<ScanResult> {
        local.scan(&CancelToken::new(), Arc::new(|_| {})).await
    }

    /// Whether a file made unreadable with mode 000 actually refuses reads, which it does not
    /// for root.
    fn unreadable(path: &std::path::Path) -> bool {
        fs::read(path).is_err()
    }

    #[tokio::test]
    async fn scan_lists_sorted_entries_and_only_valid_partials() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/b.txt"), b"abc").unwrap();
        fs::write(dir.path().join("c.txt"), b"1").unwrap();
        write_valid_partial(&dir, "d.bin", "ZY22", b"1234", 4);
        write_valid_partial(&dir, "e.bin", "OTHER", b"1234", 4);
        write_valid_partial(&dir, "g.bin", "ZY22", b"12", 4);
        write_valid_partial(&dir, "h.bin", "ZY22", b"123456", 4);
        let found = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&found);
        let on_found = Arc::new(move |count: u64| seen.store(count, Ordering::SeqCst));
        let result = endpoint(&dir)
            .scan(&CancelToken::new(), on_found)
            .await
            .unwrap();
        let listed: Vec<_> = result
            .snapshot
            .entries()
            .iter()
            .map(|e| (e.path.to_string(), e.kind, e.size))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("a".to_owned(), EntryKind::Dir, 0),
                ("a/b.txt".to_owned(), EntryKind::File, 3),
                ("c.txt".to_owned(), EntryKind::File, 1),
            ]
        );
        assert!(
            result
                .snapshot
                .entries()
                .iter()
                .all(|e| e.modified.is_some())
        );
        assert_eq!(found.load(Ordering::SeqCst), 3);
        assert_eq!(result.partials.len(), 2);
        assert_eq!(result.partials[&rel("d.bin")].bytes, 4);
        assert_eq!(result.partials[&rel("d.bin")].fingerprint.size, 100);
        assert_eq!(result.partials[&rel("h.bin")].bytes, 4);
    }

    #[tokio::test]
    async fn scan_ignores_a_sidecar_that_names_another_file() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_partial(&dir, "d.bin", "ZY22", b"1234", 4);
        let moved = dir.path().join("moved.bin");
        fs::rename(part_path(&dir.path().join("d.bin")), part_path(&moved)).unwrap();
        fs::rename(
            sidecar_path(&dir.path().join("d.bin")),
            sidecar_path(&moved),
        )
        .unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert!(result.partials.is_empty());
        assert!(result.snapshot.is_empty());
    }

    #[tokio::test]
    async fn scan_of_a_missing_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalEndpoint::new(dir.path().join("nope"), identity("ZY22"));
        let result = scan(&local).await;
        assert!(result.snapshot.is_empty());
        assert!(result.partials.is_empty());
        assert_eq!(local.label(), dir.path().join("nope").display().to_string());
    }

    #[tokio::test]
    async fn scan_of_a_root_that_is_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        fs::write(&file, b"1").unwrap();
        let err = scan_result(&LocalEndpoint::new(&file, identity("ZY22")))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::LocalNotADirectory(path) if *path == file),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_of_an_unreadable_root_is_an_error_not_an_empty_tree() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir_all(locked.join("root")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let refused = fs::metadata(locked.join("root")).is_err();
        let outcome = scan_result(&LocalEndpoint::new(locked.join("root"), identity("ZY22"))).await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if !refused {
            return;
        }
        let err = outcome.unwrap_err();
        assert!(
            matches!(&err, Error::LocalIo { op: "stat", source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn scan_ignores_a_sidecar_whose_part_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_partial(&dir, "d.bin", "ZY22", b"1234", 4);
        fs::remove_file(part_path(&dir.path().join("d.bin"))).unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert!(result.partials.is_empty());
        assert!(result.snapshot.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_fails_when_a_sidecar_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_valid_partial(&dir, "d.bin", "ZY22", b"1234", 4);
        let sidecar = sidecar_path(&dir.path().join("d.bin"));
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o000)).unwrap();
        if !unreadable(&sidecar) {
            return;
        }
        let err = scan_result(&endpoint(&dir)).await.unwrap_err();
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
    async fn scan_lists_a_directory_named_like_a_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("x.mtpx-part.json")).unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert_eq!(listed(&result.snapshot), ["x.mtpx-part.json"]);
        assert_eq!(result.snapshot.entries()[0].kind, EntryKind::Dir);
        assert!(result.partials.is_empty());
    }

    #[tokio::test]
    async fn scan_lists_a_directory_named_like_a_part_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("y.mtpx-part")).unwrap();
        fs::write(dir.path().join("y.mtpx-part/child"), b"1").unwrap();
        let sidecar = Sidecar::new(
            identity("ZY22"),
            &rel("y"),
            Fingerprint {
                size: 1,
                modified: None,
            },
            0,
        );
        let json = serde_json::to_vec(&sidecar).unwrap();
        fs::write(sidecar_path(&dir.path().join("y")), json).unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert_eq!(
            listed(&result.snapshot),
            ["y.mtpx-part", "y.mtpx-part/child"]
        );
        assert!(result.partials.is_empty(), "{:?}", result.partials);
    }

    #[tokio::test]
    async fn scan_neither_lists_nor_trusts_a_half_written_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("x.bin");
        fs::write(part_path(&final_path), b"12").unwrap();
        let mut tmp = final_path.as_os_str().to_os_string();
        tmp.push(SIDECAR_TMP_SUFFIX);
        fs::write(tmp, b"{ \"version\": 1").unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert!(result.snapshot.is_empty(), "{:?}", result.snapshot);
        assert!(result.partials.is_empty());
    }

    // APFS refuses non-UTF-8 names (EILSEQ), so the fixture can only be created on Linux.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn scan_skips_an_entry_whose_name_is_not_utf8_and_keeps_going() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/good.txt"), b"1").unwrap();
        fs::write(dir.path().join("zed.txt"), b"1").unwrap();
        let bad = dir
            .path()
            .join("sub")
            .join(OsStr::from_bytes(b"bad\xff.txt"));
        fs::write(&bad, b"1").unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert_eq!(
            listed(&result.snapshot),
            vec!["sub", "sub/good.txt", "zed.txt"]
        );
        let skipped = result.snapshot.skipped();
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].parent, rel("sub"));
        assert!(
            skipped[0].reason.contains("invalid path segment"),
            "{}",
            skipped[0].reason
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn scan_prunes_a_directory_whose_name_is_not_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join(OsStr::from_bytes(b"bad\xff"));
        fs::create_dir(&bad).unwrap();
        fs::write(bad.join("inner.txt"), b"1").unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert!(result.snapshot.is_empty());
        assert_eq!(result.snapshot.skipped().len(), 1);
        assert_eq!(result.snapshot.skipped()[0].parent, RelPath::root());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_component_is_an_invalid_segment() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};
        let path = Path::new(OsStr::from_bytes(b"bad\xff.txt"));
        let component = path.components().next().unwrap();
        let err = super::segment_of(component).unwrap_err();
        assert_eq!(err, PathError::InvalidSegment("bad\u{fffd}.txt".into()));
        assert!(err.to_string().contains("invalid path segment"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_follows_a_symlinked_directory() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("inner.txt"), b"12").unwrap();
        let dir = tempfile::tempdir().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        fs::write(dir.path().join("c.txt"), b"1").unwrap();
        symlink("c.txt", dir.path().join("file-link.txt")).unwrap();
        let result = scan(&endpoint(&dir)).await;
        let listed: Vec<_> = result
            .snapshot
            .entries()
            .iter()
            .map(|e| (e.path.to_string(), e.kind, e.size))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("c.txt".to_owned(), EntryKind::File, 1),
                ("file-link.txt".to_owned(), EntryKind::File, 1),
                ("link".to_owned(), EntryKind::Dir, 0),
                ("link/inner.txt".to_owned(), EntryKind::File, 2),
            ]
        );
        assert!(result.snapshot.skipped().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_records_a_dangling_symlink_as_skipped() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        symlink("missing.txt", dir.path().join("sub/gone")).unwrap();
        let result = scan(&endpoint(&dir)).await;
        assert_eq!(listed(&result.snapshot), ["sub"]);
        let skipped = result.snapshot.skipped();
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].parent, rel("sub"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scan_skips_an_unreadable_directory_and_keeps_going() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for sub in ["a", "locked", "b"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
            fs::write(dir.path().join(sub).join("x.txt"), b"1").unwrap();
        }
        let locked = dir.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let refused = fs::read_dir(&locked).is_err();
        let outcome = scan_result(&endpoint(&dir)).await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if !refused {
            return;
        }
        let result = outcome.unwrap();
        assert_eq!(
            listed(&result.snapshot),
            ["a", "a/x.txt", "b", "b/x.txt", "locked"]
        );
        let skipped = result.snapshot.skipped();
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].parent, RelPath::root());
    }

    #[tokio::test]
    async fn scan_reports_progress_once_per_hundred_entries_and_once_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..super::PROGRESS_EVERY {
            fs::write(dir.path().join(format!("{i}.txt")), b"1").unwrap();
        }
        for i in 0..3 {
            fs::write(dir.path().join(format!("stray{i}.mtpx-part")), b"1").unwrap();
        }
        let calls = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&calls);
        let on_found = Arc::new(move |_: u64| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        endpoint(&dir)
            .scan(&CancelToken::new(), on_found)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn scan_with_a_set_token_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = endpoint(&dir)
            .scan(&cancel, Arc::new(|_| {}))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
    }

    #[tokio::test]
    async fn scan_stops_with_cancelled_when_the_token_is_set_mid_walk() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..=super::PROGRESS_EVERY {
            fs::write(dir.path().join(format!("{i}.txt")), b"1").unwrap();
        }
        let cancel = CancelToken::new();
        let calls = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&calls);
        let trigger = cancel.clone();
        let on_found = Arc::new(move |_: u64| {
            seen.fetch_add(1, Ordering::SeqCst);
            trigger.cancel();
        });
        let err = endpoint(&dir).scan(&cancel, on_found).await.unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the walk stops at the next item and never reports the final count"
        );
    }

    #[tokio::test]
    async fn a_single_file_scan_sees_only_that_file_and_its_resume_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.jpg"), b"aaa").unwrap();
        fs::write(dir.path().join("a.jpg.bak"), b"aaa").unwrap();
        fs::write(dir.path().join("b.jpg"), b"b").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.jpg"), b"a").unwrap();
        write_valid_partial(&dir, "a.jpg", "ZY22", b"12", 2);
        write_valid_partial(&dir, "b.jpg", "ZY22", b"12", 2);
        let local = LocalEndpoint::for_file(dir.path(), "a.jpg", identity("ZY22"));
        let result = scan(&local).await;
        assert_eq!(listed(&result.snapshot), ["a.jpg"]);
        assert_eq!(result.partials.len(), 1);
        assert_eq!(result.partials[&rel("a.jpg")].bytes, 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_single_file_scan_never_reads_a_sibling_sidecar() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.jpg"), b"aaa").unwrap();
        write_valid_partial(&dir, "b.jpg", "ZY22", b"12", 2);
        let sibling = sidecar_path(&dir.path().join("b.jpg"));
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o000)).unwrap();
        if !unreadable(&sibling) {
            return;
        }
        assert!(scan_result(&endpoint(&dir)).await.is_err());
        let local = LocalEndpoint::for_file(dir.path(), "a.jpg", identity("ZY22"));
        let result = scan_result(&local).await.unwrap();
        assert_eq!(listed(&result.snapshot), ["a.jpg"]);
    }
}
