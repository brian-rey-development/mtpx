//! Maps relative paths to device object handles, re-listing on demand when a handle goes stale.

use crate::{
    error::{Error, Result},
    internal::mtp::scan::list_folder,
    path::RelPath,
};
use mtp_rs::{ObjectHandle, Storage};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// Every handle learned so far, keyed by path relative to the transfer root.
#[derive(Debug, Default)]
struct HandleCache(HashMap<RelPath, ObjectHandle>);

impl HandleCache {
    fn get(&self, path: &RelPath) -> Option<ObjectHandle> {
        self.0.get(path).copied()
    }

    fn prime(&mut self, entries: impl IntoIterator<Item = (RelPath, ObjectHandle)>) {
        self.0.extend(entries);
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

/// Resolves paths under one root to object handles, listing directories only when the cache misses.
pub struct Resolver {
    storage: Arc<Storage>,
    root: Option<ObjectHandle>,
    cache: Mutex<HandleCache>,
}

impl Resolver {
    /// Starts with an empty cache; `root` is `None` for the storage root.
    pub fn new(storage: Arc<Storage>, root: Option<ObjectHandle>) -> Self {
        Self {
            storage,
            root,
            cache: Mutex::new(HandleCache::default()),
        }
    }

    /// The transfer root as a listing parent; `None` is the storage root.
    #[must_use]
    pub const fn root(&self) -> Option<ObjectHandle> {
        self.root
    }

    /// Records handles a scan already learned so later lookups need no listing.
    pub fn prime(&self, entries: impl IntoIterator<Item = (RelPath, ObjectHandle)>) {
        self.lock().prime(entries);
    }

    /// The handle for `path`, listing its ancestors as needed.
    ///
    /// # Errors
    /// `SourceVanished` when a segment is missing on the device, or a listing error.
    pub async fn resolve(&self, path: &RelPath) -> Result<ObjectHandle> {
        let Some(parent) = path.parent() else {
            return self
                .root
                .ok_or(Error::Unsupported("object operations on the storage root"));
        };
        let parent_handle = self.listing_parent(&parent).await?;
        self.child(parent_handle, &parent, path).await
    }

    /// The handle to list `dir` under, walking down from the root; `None` is the storage root.
    async fn listing_parent(&self, dir: &RelPath) -> Result<Option<ObjectHandle>> {
        let mut handle = self.root;
        let mut current = RelPath::root();
        for segment in dir.segments() {
            let next = current.join(segment)?;
            handle = Some(self.child(handle, &current, &next).await?);
            current = next;
        }
        Ok(handle)
    }

    /// The cached handle of `path`, or the one a fresh listing of its parent reports.
    async fn child(
        &self,
        parent_handle: Option<ObjectHandle>,
        parent: &RelPath,
        path: &RelPath,
    ) -> Result<ObjectHandle> {
        if let Some(handle) = self.cached(path) {
            return Ok(handle);
        }
        let listing = list_folder(&self.storage, parent_handle, parent, None).await?;
        let children = listing
            .children
            .iter()
            .map(|(p, info)| (p.clone(), info.handle));
        self.lock().prime(children);
        self.cached(path)
            .ok_or_else(|| Error::SourceVanished(path.clone()))
    }

    /// Runs `op` on the resolved handle; a stale handle anywhere in the sequence rebuilds the
    /// cache from scratch and retries the whole sequence once.
    ///
    /// # Errors
    /// Whatever `op` or the resolution returns, lifted through `Error::from_mtp`.
    pub async fn with_handle<T, F, Fut>(&self, path: &RelPath, op: F) -> Result<T>
    where
        T: Send,
        F: Fn(ObjectHandle) -> Fut + Send + Sync,
        Fut: Future<Output = std::result::Result<T, mtp_rs::Error>> + Send,
    {
        match self.attempt(path, &op).await {
            Err(e) if e.is_stale_handle() => {
                // A media rescan re-keys every object on the device, so nothing cached survives it.
                self.lock().clear();
                self.attempt(path, &op).await
            }
            outcome => outcome,
        }
    }

    async fn attempt<T, F, Fut>(&self, path: &RelPath, op: &F) -> Result<T>
    where
        T: Send,
        F: Fn(ObjectHandle) -> Fut + Send + Sync,
        Fut: Future<Output = std::result::Result<T, mtp_rs::Error>> + Send,
    {
        let handle = self.resolve(path).await?;
        op(handle).await.map_err(Error::from_mtp)
    }

    /// The handle the cache holds for `path`, without touching the device.
    #[must_use]
    pub fn cached(&self, path: &RelPath) -> Option<ObjectHandle> {
        self.lock().get(path)
    }

    // A poisoned lock can at worst lose entries, and a missing entry only costs a re-listing.
    fn lock(&self) -> MutexGuard<'_, HandleCache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod cache_tests {
    #![allow(clippy::unwrap_used)]

    use super::HandleCache;
    use crate::path::RelPath;
    use mtp_rs::ObjectHandle;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    fn primed() -> HandleCache {
        let mut cache = HandleCache::default();
        cache.prime([
            (rel("DCIM"), ObjectHandle(1)),
            (rel("DCIM/Camera"), ObjectHandle(2)),
            (rel("DCIM/Camera/a.jpg"), ObjectHandle(3)),
            (rel("DCIMx"), ObjectHandle(4)),
            (rel("Music/c.mp3"), ObjectHandle(5)),
        ]);
        cache
    }

    #[test]
    fn clear_forgets_every_handle() {
        let mut cache = primed();
        cache.clear();
        assert!(cache.0.is_empty());
        assert_eq!(cache.get(&rel("DCIM")), None);
    }

    #[test]
    fn prime_overwrites_an_existing_handle() {
        let mut cache = primed();
        cache.prime([(rel("DCIM"), ObjectHandle(9))]);
        assert_eq!(cache.get(&rel("DCIM")), Some(ObjectHandle(9)));
    }
}

#[cfg(all(test, feature = "virtual-device"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::Resolver;
    use crate::{
        error::Error,
        internal::mtp::test_support::{open_device, rel, seed_tree},
        path::RelPath,
    };
    use mtp_rs::ObjectHandle;
    use std::{
        fs,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[tokio::test]
    async fn resolve_lists_ancestors_on_a_cold_cache_and_caches_siblings() {
        let (storage, dir, _serial) = open_device("resolver-cold").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(storage, None);
        let handle = resolver.resolve(&rel("DCIM/Camera/a.jpg")).await.unwrap();
        assert_ne!(handle, ObjectHandle::ROOT);
        assert!(resolver.cached(&rel("DCIM")).is_some());
        assert!(resolver.cached(&rel("DCIM/Camera")).is_some());
        assert!(resolver.cached(&rel("DCIM/Camera/sub")).is_some());
        assert_eq!(resolver.cached(&rel("DCIM/Camera/a.jpg")), Some(handle));
        assert!(resolver.cached(&rel("Music")).is_some());
    }

    #[tokio::test]
    async fn resolve_of_a_missing_path_is_source_vanished() {
        let (storage, dir, _serial) = open_device("resolver-missing").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(storage, None);
        let err = resolver.resolve(&rel("DCIM/nope.jpg")).await.unwrap_err();
        assert!(
            matches!(&err, Error::SourceVanished(path) if *path == rel("DCIM/nope.jpg")),
            "{err:?}"
        );
        let err = resolver.resolve(&rel("Nope/deeper.jpg")).await.unwrap_err();
        assert!(
            matches!(&err, Error::SourceVanished(path) if *path == rel("Nope")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn with_handle_retries_once_after_a_rekey_and_caches_the_new_handle() {
        let (storage, dir, serial) = open_device("resolver-rekey").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(Arc::clone(&storage), None);
        let path = rel("DCIM/Camera/a.jpg");
        let old = resolver.resolve(&path).await.unwrap();
        let (_, rekeyed) =
            mtp_rs::rekey_virtual_object(&serial, Path::new("DCIM/Camera/a.jpg")).unwrap();
        let calls = AtomicUsize::new(0);
        let info = resolver
            .with_handle(&path, |handle| {
                calls.fetch_add(1, Ordering::SeqCst);
                storage.get_object_info(handle)
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(info.filename, "a.jpg");
        assert_ne!(info.handle, old);
        assert_eq!(info.handle.0, u64::from(rekeyed.0));
        assert_eq!(resolver.cached(&path), Some(info.handle));
    }

    #[tokio::test]
    async fn with_handle_recovers_when_an_ancestor_was_rekeyed_as_well() {
        let (storage, dir, serial) = open_device("resolver-rekey-ancestor").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(Arc::clone(&storage), None);
        let path = rel("DCIM/Camera/a.jpg");
        resolver.resolve(&path).await.unwrap();
        mtp_rs::rekey_virtual_object(&serial, Path::new("DCIM")).unwrap();
        let (_, rekeyed) =
            mtp_rs::rekey_virtual_object(&serial, Path::new("DCIM/Camera/a.jpg")).unwrap();
        let calls = AtomicUsize::new(0);
        let info = resolver
            .with_handle(&path, |handle| {
                calls.fetch_add(1, Ordering::SeqCst);
                storage.get_object_info(handle)
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(info.handle.0, u64::from(rekeyed.0));
        assert_eq!(resolver.cached(&path), Some(info.handle));
    }

    #[tokio::test]
    async fn with_handle_on_a_file_deleted_behind_the_cache_is_source_vanished() {
        let (storage, dir, _serial) = open_device("resolver-deleted").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(Arc::clone(&storage), None);
        let path = rel("DCIM/Camera/a.jpg");
        resolver.resolve(&path).await.unwrap();
        fs::remove_file(dir.path().join("DCIM/Camera/a.jpg")).unwrap();
        let calls = AtomicUsize::new(0);
        let err = resolver
            .with_handle(&path, |handle| {
                calls.fetch_add(1, Ordering::SeqCst);
                storage.get_object_info(handle)
            })
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            matches!(&err, Error::SourceVanished(gone) if *gone == path),
            "{err:?}"
        );
        assert_eq!(resolver.cached(&path), None);
    }

    #[tokio::test]
    async fn with_handle_passes_other_errors_through_without_a_retry() {
        let (storage, dir, _serial) = open_device("resolver-passthrough").await;
        seed_tree(dir.path());
        let resolver = Resolver::new(storage, None);
        let calls = AtomicUsize::new(0);
        let err = resolver
            .with_handle(&rel("Music/c.mp3"), |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err::<(), _>(mtp_rs::Error::Busy))
            })
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(err, Error::Mtp(mtp_rs::Error::Busy)), "{err:?}");
    }

    #[tokio::test]
    async fn resolve_under_a_non_root_transfer_root_is_relative_to_it() {
        let (storage, dir, _serial) = open_device("resolver-subroot").await;
        seed_tree(dir.path());
        let cold = Resolver::new(Arc::clone(&storage), None);
        let dcim = cold.resolve(&rel("DCIM")).await.unwrap();
        let resolver = Resolver::new(storage, Some(dcim));
        let handle = resolver.resolve(&rel("Camera/a.jpg")).await.unwrap();
        assert_eq!(
            handle,
            cold.resolve(&rel("DCIM/Camera/a.jpg")).await.unwrap()
        );
        assert!(resolver.cached(&RelPath::root()).is_none());
    }
}
