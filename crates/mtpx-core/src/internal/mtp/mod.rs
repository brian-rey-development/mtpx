//! One MTP storage as a side of a transfer: listing and windowed reads, rooted at a device folder.

mod resolver;
mod scan;

use crate::{
    device_path::{DevicePath, StorageSelector},
    entry::Snapshot,
    error::{Error, Result},
    internal::endpoint::{ByteStream, Endpoint, Identity, ScanResult, WriteOutcome, WriteRequest},
    path::{RelPath, RemotePath},
    planner::Partials,
};
use bytes::Bytes;
use mtp_rs::{ByteRange, CancelToken, ObjectHandle, Storage, WindowedDownload};
use resolver::Resolver;
use std::{fmt, io, sync::Arc};
use tokio::{
    sync::mpsc::{self, Sender},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;

/// Bytes requested per `GetPartialObject`; no session is held between windows, so a cancel
/// waits at most this long.
pub const DOWNLOAD_WINDOW: u32 = 4 * 1024 * 1024;
/// Windows the read pump may fetch ahead of the consumer.
pub const PUMP_DEPTH: usize = 4;

/// One storage on one device, rooted at a folder, as the source or destination of a transfer.
pub struct MtpEndpoint {
    storage: Arc<Storage>,
    root: RemotePath,
    resolver: Resolver,
    identity: Identity,
}

impl MtpEndpoint {
    /// Locates `root` on `storage`, walking one folder per segment.
    ///
    /// # Errors
    /// `RemotePathNotFound` when a segment is missing, `NotADirectory` when one is a file.
    pub async fn open(
        storage: Arc<Storage>,
        root: RemotePath,
        device_serial: &str,
    ) -> Result<Self> {
        let root_handle = locate_root(&storage, &root).await?;
        let identity = Identity {
            device_serial: device_serial.to_owned(),
            storage: storage.info().description.clone(),
        };
        Ok(Self {
            resolver: Resolver::new(Arc::clone(&storage), root_handle),
            storage,
            root,
            identity,
        })
    }

    /// The device and storage this endpoint talks to.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Lists the root's immediate children without descending into folders.
    ///
    /// # Errors
    /// `Cancelled` once the token is set, or the listing error.
    pub async fn list(&self, cancel: &CancelToken) -> Result<Snapshot> {
        let root = RelPath::root();
        let listing =
            scan::list_folder(&self.storage, self.resolver.root(), &root, Some(cancel)).await?;
        let entries = listing
            .children
            .into_iter()
            .map(|(path, info)| scan::entry_from(path, &info))
            .collect();
        Ok(Snapshot::new(self.label(), entries, listing.skipped))
    }
}

impl Endpoint for MtpEndpoint {
    fn label(&self) -> String {
        format!(
            "{}:{}:{}",
            self.identity.device_serial, self.identity.storage, self.root
        )
    }

    async fn scan(
        &self,
        cancel: &CancelToken,
        on_found: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<ScanResult> {
        let walked = scan::walk(&self.storage, self.resolver.root(), cancel, &*on_found).await?;
        self.resolver.prime(walked.handles);
        Ok(ScanResult {
            snapshot: Snapshot::new(self.label(), walked.entries, walked.skipped),
            partials: Partials::default(),
        })
    }

    /// Streams the file at `path` from `offset`. `path` must name a file: the plan never emits a
    /// copy for a folder, and a folder here would read as an empty file rather than fail.
    async fn read(&self, path: &RelPath, offset: u64, cancel: &CancelToken) -> Result<ByteStream> {
        let download = self
            .resolver
            .with_handle(path, |handle| {
                self.storage
                    .download_windowed(handle, ByteRange::From(offset), DOWNLOAD_WINDOW)
            })
            .await?;
        let (tx, rx) = mpsc::channel(PUMP_DEPTH);
        let pump = tokio::spawn(pump(download, cancel.clone(), tx.clone()));
        tokio::spawn(report_pump_failure(pump, tx));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "upload lands with push in M3"
    )]
    async fn write(&self, _request: WriteRequest, _input: ByteStream) -> Result<WriteOutcome> {
        Err(Error::Unsupported("upload"))
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "mkdir lands with push in M3"
    )]
    async fn mkdir(&self, _path: &RelPath) -> Result<()> {
        Err(Error::Unsupported("mkdir"))
    }
}

impl fmt::Debug for MtpEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MtpEndpoint")
            .field("identity", &self.identity)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Walks `root` one folder per segment from the storage root; `None` means the storage root itself.
async fn locate_root(storage: &Storage, root: &RemotePath) -> Result<Option<ObjectHandle>> {
    let mut parent = None;
    for segment in root.segments() {
        let listing = storage
            .collect_objects(parent)
            .await
            .map_err(Error::from_mtp)?;
        let Some(found) = listing.objects.iter().find(|o| o.filename == *segment) else {
            return Err(Error::RemotePathNotFound(device_path(storage, root)));
        };
        if !found.is_folder() {
            return Err(Error::NotADirectory(device_path(storage, root)));
        }
        parent = Some(found.handle);
    }
    Ok(parent)
}

fn device_path(storage: &Storage, root: &RemotePath) -> DevicePath {
    DevicePath {
        storage: StorageSelector::Named(storage.info().description.clone()),
        path: root.clone(),
    }
}

/// Forwards windows until EOF, an error, a dropped receiver, or a cancel seen between windows.
async fn pump(mut download: WindowedDownload, cancel: CancelToken, tx: Sender<Result<Bytes>>) {
    loop {
        let item = if cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            match download.next_window().await {
                None => return,
                Some(window) => window.map(Bytes::from).map_err(Error::from_mtp),
            }
        };
        let last = item.is_err();
        if tx.send(item).await.is_err() || last {
            return;
        }
    }
}

/// A pump that panics would otherwise close the channel like a clean EOF; the consumer sees an error instead.
async fn report_pump_failure(pump: JoinHandle<()>, tx: Sender<Result<Bytes>>) {
    if let Err(e) = pump.await {
        let _ = tx.send(Err(Error::Io(io::Error::other(e)))).await;
    }
}

#[cfg(test)]
mod pump_tests {
    #![allow(clippy::unwrap_used)]

    use super::{PUMP_DEPTH, report_pump_failure};
    use crate::error::Error;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn a_panicking_pump_surfaces_as_one_error_item_then_eof() {
        let (tx, mut rx) = mpsc::channel(PUMP_DEPTH);
        let pump = tokio::spawn(async { panic!("boom") });
        report_pump_failure(pump, tx).await;
        let first = rx.recv().await.unwrap();
        assert!(matches!(first, Err(Error::Io(_))), "{first:?}");
        assert!(rx.recv().await.is_none());
    }
}

#[cfg(all(test, feature = "virtual-device"))]
pub mod test_support {
    #![allow(clippy::unwrap_used)]

    use crate::path::{RelPath, RemotePath};
    use mtp_rs::{MtpDevice, Storage, VirtualDeviceConfig, VirtualStorageConfig};
    use std::{fs, path::Path, sync::Arc, time::Duration};
    use tempfile::TempDir;

    pub const STORAGE_DESCRIPTION: &str = "Internal Storage";
    const CAPACITY: u64 = 1024 * 1024 * 1024;

    pub async fn open_device(test_name: &str) -> (Arc<Storage>, TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let serial = format!("mtpx-{test_name}");
        let device = MtpDevice::builder()
            .open_virtual(config(&serial, dir.path()))
            .await
            .unwrap();
        let storage = device.storages().await.unwrap().remove(0);
        (Arc::new(storage), dir, serial)
    }

    fn config(serial: &str, backing_dir: &Path) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "mtpx".into(),
            model: "Virtual Phone".into(),
            serial: serial.to_owned(),
            storages: vec![VirtualStorageConfig {
                description: STORAGE_DESCRIPTION.into(),
                capacity: CAPACITY,
                backing_dir: backing_dir.to_path_buf(),
                read_only: false,
            }],
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
            ..Default::default()
        }
    }

    pub fn seed_tree(root: &Path) {
        for dir in ["DCIM/Camera/sub", "Music", "Empty"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let files: [(&str, &[u8]); 4] = [
            ("DCIM/Camera/a.jpg", b"aaa"),
            ("DCIM/Camera/sub/b.jpg", b"bb"),
            ("DCIM/photo.jpg", b"photo"),
            ("Music/c.mp3", b"c"),
        ];
        for (path, content) in files {
            fs::write(root.join(path), content).unwrap();
        }
    }

    pub fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    pub fn remote(path: &str) -> RemotePath {
        RemotePath::parse(path).unwrap()
    }

    pub fn pseudo_random(len: usize) -> Vec<u8> {
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
}

#[cfg(all(test, feature = "virtual-device"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::{DOWNLOAD_WINDOW, MtpEndpoint, PUMP_DEPTH, test_support::*};
    use crate::{
        device_path::StorageSelector,
        error::Error,
        internal::endpoint::{Endpoint, WriteRequest},
        path::RemotePath,
    };
    use bytes::Bytes;
    use futures::StreamExt;
    use mtp_rs::CancelToken;
    use std::{fs, path::Path, sync::Arc};

    const LARGE_FILE: usize = 9 * 1024 * 1024;
    /// More windows than the pump can hold buffered plus in flight once the token is set,
    /// so the stream cannot reach EOF before the cancel is observed.
    const CANCEL_FILE: usize = (PUMP_DEPTH + 4) * DOWNLOAD_WINDOW as usize;

    async fn open_at(test_name: &str, root: &str) -> (MtpEndpoint, tempfile::TempDir, String) {
        let (storage, dir, serial) = open_device(test_name).await;
        seed_tree(dir.path());
        let endpoint = MtpEndpoint::open(storage, remote(root), &serial)
            .await
            .unwrap();
        (endpoint, dir, serial)
    }

    async fn read_all(endpoint: &MtpEndpoint, path: &str, offset: u64) -> Vec<Bytes> {
        endpoint
            .read(&rel(path), offset, &CancelToken::new())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await
    }

    #[tokio::test]
    async fn open_at_the_storage_root_reports_identity_and_label() {
        let (endpoint, _dir, serial) = open_at("open-root", "/").await;
        assert_eq!(endpoint.identity().device_serial, serial);
        assert_eq!(endpoint.identity().storage, STORAGE_DESCRIPTION);
        assert_eq!(
            endpoint.label(),
            format!("{serial}:{STORAGE_DESCRIPTION}:/")
        );
    }

    #[tokio::test]
    async fn open_at_a_nested_folder_labels_with_its_path() {
        let (nested, _dir, serial) = open_at("open-nested", "/DCIM/Camera").await;
        assert_eq!(
            nested.label(),
            format!("{serial}:{STORAGE_DESCRIPTION}:/DCIM/Camera")
        );
    }

    #[tokio::test]
    async fn open_of_a_missing_folder_is_remote_path_not_found() {
        let (storage, dir, serial) = open_device("open-missing").await;
        seed_tree(dir.path());
        let root = remote("/DCIM/Missing");
        let err = MtpEndpoint::open(storage, root.clone(), &serial)
            .await
            .unwrap_err();
        let Error::RemotePathNotFound(device_path) = err else {
            panic!("{err:?}");
        };
        assert_eq!(
            device_path.storage,
            StorageSelector::Named(STORAGE_DESCRIPTION.into())
        );
        assert_eq!(device_path.path, root);
    }

    #[tokio::test]
    async fn open_of_a_file_is_not_a_directory() {
        let (storage, dir, serial) = open_device("open-file").await;
        seed_tree(dir.path());
        let root = remote("/DCIM/photo.jpg");
        let err = MtpEndpoint::open(storage, root.clone(), &serial)
            .await
            .unwrap_err();
        let Error::NotADirectory(device_path) = err else {
            panic!("{err:?}");
        };
        assert_eq!(device_path.path, root);
        assert_eq!(device_path.to_string(), "Internal Storage:/DCIM/photo.jpg");
    }

    #[tokio::test]
    async fn read_yields_the_whole_file_or_the_suffix_from_an_offset() {
        let (endpoint, _dir, _serial) = open_at("read-small", "/").await;
        let whole = read_all(&endpoint, "DCIM/photo.jpg", 0).await;
        assert_eq!(whole.concat(), b"photo");
        let tail = read_all(&endpoint, "DCIM/photo.jpg", 3).await;
        assert_eq!(tail.concat(), b"to");
        let at_end = read_all(&endpoint, "DCIM/photo.jpg", 5).await;
        assert!(at_end.is_empty());
    }

    #[tokio::test]
    async fn read_from_an_offset_past_the_size_is_an_mtp_error() {
        let (endpoint, _dir, _serial) = open_at("read-past-end", "/").await;
        let Err(err) = endpoint
            .read(&rel("DCIM/photo.jpg"), 6, &CancelToken::new())
            .await
        else {
            panic!("read past the end of the file succeeded");
        };
        assert!(matches!(err, Error::Mtp(_)), "{err:?}");
    }

    #[tokio::test]
    async fn read_of_an_empty_file_ends_immediately() {
        let (storage, dir, serial) = open_device("read-empty").await;
        fs::write(dir.path().join("empty.bin"), b"").unwrap();
        let endpoint = MtpEndpoint::open(storage, RemotePath::root(), &serial)
            .await
            .unwrap();
        let chunks = read_all(&endpoint, "empty.bin", 0).await;
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn read_of_a_large_file_arrives_in_bounded_windows() {
        let (storage, dir, serial) = open_device("read-large").await;
        let content = pseudo_random(LARGE_FILE);
        fs::write(dir.path().join("big.bin"), &content).unwrap();
        let endpoint = MtpEndpoint::open(storage, RemotePath::root(), &serial)
            .await
            .unwrap();
        let chunks = read_all(&endpoint, "big.bin", 0).await;
        assert!(chunks.len() > 1, "{}", chunks.len());
        assert!(chunks.iter().all(|c| c.len() <= DOWNLOAD_WINDOW as usize));
        assert_eq!(chunks.concat(), content);
    }

    #[tokio::test]
    async fn read_ends_with_cancelled_once_the_token_is_set_between_windows() {
        let (storage, dir, serial) = open_device("read-cancel").await;
        fs::write(dir.path().join("big.bin"), pseudo_random(CANCEL_FILE)).unwrap();
        let endpoint = MtpEndpoint::open(storage, RemotePath::root(), &serial)
            .await
            .unwrap();
        let cancel = CancelToken::new();
        let mut stream = endpoint.read(&rel("big.bin"), 0, &cancel).await.unwrap();
        let first = stream.next().await.unwrap();
        assert!(first.is_ok(), "{first:?}");
        cancel.cancel();
        let rest: Vec<_> = stream.collect().await;
        let (last, before) = rest.split_last().unwrap();
        assert!(matches!(last, Err(Error::Cancelled)), "{last:?}");
        assert!(before.iter().all(Result::is_ok));
        assert!(before.len() <= PUMP_DEPTH + 1, "{}", before.len());
    }

    #[tokio::test]
    async fn read_recovers_from_a_handle_rekeyed_after_the_scan() {
        let (endpoint, _dir, serial) = open_at("read-rekey", "/").await;
        endpoint
            .scan(&CancelToken::new(), Arc::new(|_| {}))
            .await
            .unwrap();
        let rekeyed = mtp_rs::rekey_virtual_object(&serial, Path::new("DCIM/Camera/a.jpg"));
        assert!(rekeyed.is_some());
        let chunks = read_all(&endpoint, "DCIM/Camera/a.jpg", 0).await;
        assert_eq!(chunks.concat(), b"aaa");
    }

    #[tokio::test]
    async fn read_of_a_file_deleted_after_the_scan_is_source_vanished() {
        let (endpoint, dir, _serial) = open_at("read-deleted", "/").await;
        endpoint
            .scan(&CancelToken::new(), Arc::new(|_| {}))
            .await
            .unwrap();
        fs::remove_file(dir.path().join("DCIM/Camera/a.jpg")).unwrap();
        let Err(err) = endpoint
            .read(&rel("DCIM/Camera/a.jpg"), 0, &CancelToken::new())
            .await
        else {
            panic!("read of a deleted file succeeded");
        };
        assert!(
            matches!(&err, Error::SourceVanished(path) if *path == rel("DCIM/Camera/a.jpg")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn write_and_mkdir_are_unsupported_until_push_lands() {
        let (endpoint, _dir, _serial) = open_at("unsupported", "/").await;
        let request = WriteRequest {
            path: rel("x.bin"),
            expected_size: 0,
            resume_from: 0,
            modified: None,
        };
        let input = Box::pin(futures::stream::empty());
        let err = endpoint.write(request, input).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("upload")), "{err:?}");
        let err = endpoint.mkdir(&rel("x")).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported("mkdir")), "{err:?}");
    }
}
