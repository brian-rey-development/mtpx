//! The blocking half of a local scan: walks the tree, lists entries, and vets sidecars.

use crate::{
    entry::{Entry, EntryKind, ModifiedTime, SkippedEntry, Snapshot},
    error::{Error, Result},
    internal::{
        endpoint::{Identity, ScanResult},
        partial::{
            PART_SUFFIX, PartialInfo, SIDECAR_SUFFIX, Sidecar, part_path, read_sidecar_blocking,
        },
    },
    path::{PathError, RelPath},
    planner::Partials,
};
use mtp_rs::CancelToken;
use std::{
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
    peer: Identity,
    cancel: CancelToken,
    on_found: Arc<dyn Fn(u64) + Send + Sync>,
}

impl Walker {
    pub(super) const fn new(
        root: PathBuf,
        peer: Identity,
        cancel: CancelToken,
        on_found: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Self {
        Self {
            root,
            peer,
            cancel,
            on_found,
        }
    }

    pub(super) fn run(self) -> Result<ScanResult> {
        let mut found = Found::default();
        if self.root.exists() {
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
        })
    }

    fn walk(&self, found: &mut Found) -> Result<()> {
        let mut items = WalkDir::new(&self.root)
            .min_depth(1)
            .follow_links(false)
            .into_iter();
        while let Some(item) = items.next() {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let item = item.map_err(io::Error::from)?;
            if self.visit(&item, found)? == Visit::Unnameable && item.file_type().is_dir() {
                items.skip_current_dir();
            }
            if found.entries.len() % PROGRESS_EVERY == 0 {
                self.report(found.entries.len());
            }
        }
        Ok(())
    }

    fn report(&self, count: usize) {
        (self.on_found)(count as u64);
    }

    fn visit(&self, item: &DirEntry, found: &mut Found) -> Result<Visit> {
        // Part and sidecar files describe a transfer in progress, not content; listed as
        // entries they would look like phantom files to the planner.
        let name = item.file_name().to_string_lossy();
        if name.ends_with(PART_SUFFIX) {
            return Ok(Visit::Done);
        }
        if name.ends_with(SIDECAR_SUFFIX) {
            self.collect_partial(item.path(), &mut found.partials)?;
            return Ok(Visit::Done);
        }
        let path = match self.relative(item.path()) {
            Ok(path) => path,
            Err(reason) => return Ok(self.skip(item, &reason, found)),
        };
        let meta = item.metadata().map_err(io::Error::from)?;
        if let Some(kind) = kind_of(&meta) {
            found.entries.push(entry_from(path, kind, &meta));
        }
        Ok(Visit::Done)
    }

    /// An entry whose name cannot become a `RelPath` is reported under its parent and left out.
    fn skip(&self, item: &DirEntry, reason: &PathError, found: &mut Found) -> Visit {
        let parent = item
            .path()
            .parent()
            .and_then(|parent| self.relative(parent).ok())
            .unwrap_or_else(RelPath::root);
        tracing::warn!(path = %item.path().display(), %reason, "skipping entry with an unusable name");
        found.skipped.push(SkippedEntry {
            parent,
            reason: reason.to_string(),
        });
        Visit::Unnameable
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

fn part_len(part: &Path) -> Result<Option<u64>> {
    match fs::metadata(part) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Symlinks and special files have no place in a snapshot; only files and directories are described.
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
    Entry {
        path,
        kind,
        size,
        modified: meta.modified().ok().map(ModifiedTime::from_system),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::super::test_support::{endpoint, identity, rel, scan, write_valid_partial};
    use crate::{
        entry::EntryKind,
        error::Error,
        internal::{
            endpoint::Endpoint,
            local::LocalEndpoint,
            partial::{part_path, sidecar_path},
        },
        path::PathError,
    };
    use mtp_rs::CancelToken;
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    #[tokio::test]
    async fn scan_lists_sorted_entries_and_only_valid_partials() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/b.txt"), b"abc").unwrap();
        fs::write(dir.path().join("c.txt"), b"1").unwrap();
        write_valid_partial(&dir, "d.bin", "ZY22", b"1234", 4);
        write_valid_partial(&dir, "e.bin", "OTHER", b"1234", 4);
        write_valid_partial(&dir, "g.bin", "ZY22", b"1234", 2);
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
        assert_eq!(result.partials.len(), 1);
        assert_eq!(result.partials[&rel("d.bin")].bytes, 4);
        assert_eq!(result.partials[&rel("d.bin")].fingerprint.size, 100);
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
        assert_eq!(local.root(), dir.path().join("nope"));
        assert_eq!(local.label(), local.root().display().to_string());
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
        let listed: Vec<_> = result
            .snapshot
            .entries()
            .iter()
            .map(|e| e.path.to_string())
            .collect();
        assert_eq!(listed, vec!["sub", "sub/good.txt", "zed.txt"]);
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
        assert_eq!(
            result.snapshot.skipped()[0].parent,
            crate::path::RelPath::root()
        );
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

    #[tokio::test]
    async fn scan_of_an_empty_root_with_a_set_token_is_cancelled() {
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
    async fn scan_stops_with_cancelled_when_the_token_is_set() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"1").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = endpoint(&dir)
            .scan(&cancel, Arc::new(|_| {}))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
    }
}
