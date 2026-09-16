//! Lists device folders one at a time, naming each child under its parent, and walks a subtree with them.

use crate::{
    entry::{Entry, EntryKind, ModifiedTime, SkippedEntry},
    error::{Error, Result},
    path::RelPath,
};
use mtp_rs::{CancelToken, ObjectHandle, ObjectInfo, Storage};
use std::collections::HashSet;

const DUPLICATE_NAME: &str = "duplicate name in folder";
const LISTED_TWICE: &str = "folder listed twice";

/// One folder's children with their paths, plus every object that could not be named under it.
#[derive(Debug, Default)]
pub(super) struct Listing {
    pub children: Vec<(RelPath, ObjectInfo)>,
    pub skipped: Vec<SkippedEntry>,
}

/// Everything a walk learned: entries for the snapshot, the handles behind them, and what was left out.
#[derive(Debug, Default)]
pub(super) struct Walked {
    pub entries: Vec<Entry>,
    pub handles: Vec<(RelPath, ObjectHandle)>,
    pub skipped: Vec<SkippedEntry>,
}

/// Lists the folder `parent` (`None` for the storage root) whose path is `parent_path`.
///
/// Device-side skips and unnameable or duplicate names land in `skipped` under `parent_path`.
pub(super) async fn list_folder(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    parent_path: &RelPath,
    cancel: Option<&CancelToken>,
) -> Result<Listing> {
    let collection = storage
        .collect_objects_with_cancel(parent, cancel)
        .await
        .map_err(Error::from_mtp)?;
    let mut listing = classify(parent_path, &collection.objects);
    listing
        .skipped
        .extend(collection.skipped.iter().map(|skipped| SkippedEntry {
            parent: parent_path.clone(),
            reason: skipped.error.to_string(),
        }));
    Ok(listing)
}

/// Names every object under `parent_path`; the first object with a given name wins.
fn classify(parent_path: &RelPath, objects: &[ObjectInfo]) -> Listing {
    let mut listing = Listing::default();
    let mut seen = HashSet::with_capacity(objects.len());
    for info in objects {
        match child_path(parent_path, info, &mut seen) {
            Ok(path) => listing.children.push((path, info.clone())),
            Err(reason) => {
                tracing::warn!(parent = %parent_path, filename = %info.filename, %reason, "skipping object");
                listing.skipped.push(SkippedEntry {
                    parent: parent_path.clone(),
                    reason,
                });
            }
        }
    }
    listing
}

fn child_path<'a>(
    parent: &RelPath,
    info: &'a ObjectInfo,
    seen: &mut HashSet<&'a str>,
) -> std::result::Result<RelPath, String> {
    let path = parent
        .join(&info.filename)
        .map_err(|reason| reason.to_string())?;
    // Duplicate names: first wins. Real devices never produce them; this guards broken firmware.
    if !seen.insert(&info.filename) {
        return Err(DUPLICATE_NAME.to_owned());
    }
    Ok(path)
}

/// Lists every folder beneath `root` depth first, checking `cancel` before each folder.
///
/// # Errors
/// `Cancelled` once the token is set, or the first listing error.
pub(super) async fn walk(
    storage: &Storage,
    root: Option<ObjectHandle>,
    cancel: &CancelToken,
    on_found: &(dyn Fn(u64) + Send + Sync),
) -> Result<Walked> {
    let mut walked = Walked::default();
    let mut pending = vec![(RelPath::root(), root)];
    let mut visited = HashSet::from([root.unwrap_or(ObjectHandle::ROOT)]);
    while let Some((path, handle)) = pending.pop() {
        ensure_live(cancel)?;
        let listing = list_folder(storage, handle, &path, Some(cancel)).await?;
        walked.skipped.extend(listing.skipped);
        for (child, info) in listing.children {
            if info.is_folder() && !visited.insert(info.handle) {
                walked.skip(&path, LISTED_TWICE);
            } else {
                walked.record(child, &info, &mut pending);
                on_found(walked.entries.len() as u64);
            }
        }
    }
    Ok(walked)
}

/// Lists `parent` once and keeps only the file named `name`, as a walk of one entry.
///
/// # Errors
/// `SourceVanished` when `name` is missing or a folder: the file went away between open and
/// scan. `Cancelled` once the token is set, or the listing error.
pub(super) async fn pick_file(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    name: &str,
    cancel: &CancelToken,
    on_found: &(dyn Fn(u64) + Send + Sync),
) -> Result<Walked> {
    ensure_live(cancel)?;
    let listing = list_folder(storage, parent, &RelPath::root(), Some(cancel)).await?;
    // Sibling skips are deliberately dropped: a single-file pull only cares about this file's fate.
    let found = listing
        .children
        .into_iter()
        .find(|(_, info)| info.filename == name && !info.is_folder());
    let Some((path, info)) = found else {
        return Err(Error::SourceVanished(RelPath::new([name])?));
    };
    let mut walked = Walked::default();
    walked.handles.push((path.clone(), info.handle));
    walked.entries.push(entry_from(path, &info));
    on_found(1);
    Ok(walked)
}

fn ensure_live(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

impl Walked {
    fn record(
        &mut self,
        path: RelPath,
        info: &ObjectInfo,
        pending: &mut Vec<(RelPath, Option<ObjectHandle>)>,
    ) {
        if info.is_folder() {
            pending.push((path.clone(), Some(info.handle)));
        }
        self.handles.push((path.clone(), info.handle));
        self.entries.push(entry_from(path, info));
    }

    fn skip(&mut self, parent: &RelPath, reason: &str) {
        tracing::warn!(%parent, reason, "skipping folder");
        self.skipped.push(SkippedEntry {
            parent: parent.clone(),
            reason: reason.to_owned(),
        });
    }
}

pub(super) fn entry_from(path: RelPath, info: &ObjectInfo) -> Entry {
    let kind = if info.is_folder() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let size = match kind {
        EntryKind::File => info.size,
        EntryKind::Dir => 0,
    };
    Entry {
        path,
        kind,
        size,
        modified: info.modified.and_then(ModifiedTime::from_mtp),
    }
}

#[cfg(test)]
mod classify_tests {
    #![allow(clippy::unwrap_used)]

    use super::{DUPLICATE_NAME, classify};
    use crate::path::RelPath;
    use mtp_rs::{ObjectHandle, ObjectInfo};

    fn object(handle: u64, filename: &str) -> ObjectInfo {
        let mut info = ObjectInfo::default();
        info.handle = ObjectHandle(handle);
        info.filename = filename.to_owned();
        info
    }

    #[test]
    fn a_repeated_name_keeps_the_first_object_and_reports_the_rest() {
        let parent = RelPath::new(["DCIM"]).unwrap();
        let objects = [object(1, "a.jpg"), object(2, "b.jpg"), object(3, "a.jpg")];
        let listing = classify(&parent, &objects);
        let named: Vec<_> = listing
            .children
            .iter()
            .map(|(path, info)| (path.to_string(), info.handle))
            .collect();
        assert_eq!(
            named,
            vec![
                ("DCIM/a.jpg".to_owned(), ObjectHandle(1)),
                ("DCIM/b.jpg".to_owned(), ObjectHandle(2)),
            ]
        );
        assert_eq!(listing.skipped.len(), 1);
        assert_eq!(listing.skipped[0].parent, parent);
        assert_eq!(listing.skipped[0].reason, DUPLICATE_NAME);
    }

    #[test]
    fn an_unusable_name_is_reported_under_its_parent_with_the_path_error() {
        let parent = RelPath::root();
        let objects = [object(1, "a/b"), object(2, ".."), object(3, "ok")];
        let listing = classify(&parent, &objects);
        assert_eq!(listing.children.len(), 1);
        assert_eq!(listing.children[0].0.to_string(), "ok");
        let reasons: Vec<_> = listing.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(
            reasons,
            vec![
                "invalid path segment: \"a/b\"",
                "'.' and '..' segments are not allowed"
            ]
        );
        assert!(listing.skipped.iter().all(|s| s.parent == parent));
    }
}

#[cfg(all(test, feature = "virtual-device"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use crate::{
        entry::EntryKind,
        error::Error,
        internal::{
            endpoint::Endpoint,
            mtp::{
                MtpEndpoint,
                test_support::{open_device, remote, seed_tree},
            },
        },
    };
    use mtp_rs::CancelToken;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    #[tokio::test]
    async fn scan_lists_the_whole_tree_sorted_with_kinds_and_sizes() {
        let (storage, dir, serial) = open_device("scan-tree").await;
        seed_tree(dir.path());
        let endpoint = MtpEndpoint::open(storage, remote("/"), &serial)
            .await
            .unwrap();
        let found = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&found);
        let on_found = Arc::new(move |count: u64| seen.store(count, Ordering::SeqCst));
        let result = endpoint.scan(&CancelToken::new(), on_found).await.unwrap();
        let listed: Vec<_> = result
            .snapshot
            .entries()
            .iter()
            .map(|e| (e.path.to_string(), e.kind, e.size))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("DCIM".to_owned(), EntryKind::Dir, 0),
                ("DCIM/Camera".to_owned(), EntryKind::Dir, 0),
                ("DCIM/Camera/a.jpg".to_owned(), EntryKind::File, 3),
                ("DCIM/Camera/sub".to_owned(), EntryKind::Dir, 0),
                ("DCIM/Camera/sub/b.jpg".to_owned(), EntryKind::File, 2),
                ("DCIM/photo.jpg".to_owned(), EntryKind::File, 5),
                ("Empty".to_owned(), EntryKind::Dir, 0),
                ("Music".to_owned(), EntryKind::Dir, 0),
                ("Music/c.mp3".to_owned(), EntryKind::File, 1),
            ]
        );
        assert!(
            result
                .snapshot
                .entries()
                .iter()
                .all(|e| e.modified.is_none()),
            "the virtual device sends an empty DateModified"
        );
        assert_eq!(found.load(Ordering::SeqCst), 9);
        assert!(result.snapshot.skipped().is_empty());
        assert!(result.partials.is_empty());
        assert_eq!(result.snapshot.root(), endpoint.label());
    }

    #[tokio::test]
    async fn scan_from_a_non_root_root_yields_paths_relative_to_it() {
        let (storage, dir, serial) = open_device("scan-subroot").await;
        seed_tree(dir.path());
        let endpoint = MtpEndpoint::open(storage, remote("/DCIM"), &serial)
            .await
            .unwrap();
        let result = endpoint
            .scan(&CancelToken::new(), Arc::new(|_| {}))
            .await
            .unwrap();
        let listed: Vec<_> = result
            .snapshot
            .entries()
            .iter()
            .map(|e| e.path.to_string())
            .collect();
        assert_eq!(
            listed,
            vec![
                "Camera",
                "Camera/a.jpg",
                "Camera/sub",
                "Camera/sub/b.jpg",
                "photo.jpg"
            ]
        );
    }

    #[tokio::test]
    async fn scan_with_a_set_token_is_cancelled() {
        let (storage, dir, serial) = open_device("scan-cancelled").await;
        seed_tree(dir.path());
        let endpoint = MtpEndpoint::open(storage, remote("/"), &serial)
            .await
            .unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = endpoint.scan(&cancel, Arc::new(|_| {})).await.unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
    }

    #[tokio::test]
    async fn scan_stops_with_cancelled_when_the_token_is_set_mid_walk() {
        let (storage, dir, serial) = open_device("scan-cancel-mid-walk").await;
        seed_tree(dir.path());
        let endpoint = MtpEndpoint::open(storage, remote("/"), &serial)
            .await
            .unwrap();
        let cancel = CancelToken::new();
        let calls = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&calls);
        let trigger = cancel.clone();
        let on_found = Arc::new(move |_: u64| {
            seen.fetch_add(1, Ordering::SeqCst);
            trigger.cancel();
        });
        let err = endpoint.scan(&cancel, on_found).await.unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "the root listing already in hand is still reported; the next folder is refused"
        );
    }
}
