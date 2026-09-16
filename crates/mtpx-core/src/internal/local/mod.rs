//! The local filesystem as one side of a transfer: scan, stream, and resumable writes.

mod scan;

use crate::{
    entry::ModifiedTime,
    error::{Error, Result},
    internal::{
        endpoint::{
            ByteStream, CHUNK_SIZE, Endpoint, Identity, ScanResult, WriteOutcome, WriteRequest,
        },
        partial::{Fingerprint, Sidecar, part_path, remove_partial, write_sidecar},
    },
    path::RelPath,
};
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use mtp_rs::CancelToken;
use scan::Walker;
use std::{
    io::{self, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

/// The local filesystem side of a transfer, rooted at one directory.
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    root: PathBuf,
    peer: Identity,
}

impl LocalEndpoint {
    /// Wraps `root`; `peer` is the device this side exchanges files with, used to vet partials.
    pub fn new(root: impl Into<PathBuf>, peer: Identity) -> Self {
        Self {
            root: root.into(),
            peer,
        }
    }

    /// The directory every relative path is resolved under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Endpoint for LocalEndpoint {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    async fn scan(
        &self,
        cancel: &CancelToken,
        on_found: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<ScanResult> {
        let walker = Walker::new(
            self.root.clone(),
            self.peer.clone(),
            cancel.clone(),
            on_found,
        );
        tokio::task::spawn_blocking(move || walker.run())
            .await
            .map_err(|e| Error::Io(io::Error::other(e)))?
    }

    async fn read(&self, path: &RelPath, offset: u64, cancel: &CancelToken) -> Result<ByteStream> {
        let mut file = File::open(path.to_local_path(&self.root)).await?;
        file.seek(SeekFrom::Start(offset)).await?;
        Ok(Box::pin(chunks(file, cancel.clone())))
    }

    async fn write(&self, request: WriteRequest, input: ByteStream) -> Result<WriteOutcome> {
        let final_path = request.path.to_local_path(&self.root);
        let mut sidecar = sidecar_for(&self.peer, &request);
        let mut file = prepare_part(&final_path, &mut sidecar).await?;
        if let Err(e) = pump_into(&mut file, &mut sidecar.bytes, input).await {
            return keep_partial(&final_path, &sidecar, e).await;
        }
        if sidecar.bytes != request.expected_size {
            let mismatch = length_mismatch(&request, sidecar.bytes);
            return keep_partial(&final_path, &sidecar, mismatch).await;
        }
        finish(file, &final_path, request.modified).await?;
        Ok(WriteOutcome {
            bytes: sidecar.bytes,
        })
    }

    async fn mkdir(&self, path: &RelPath) -> Result<()> {
        tokio::fs::create_dir_all(path.to_local_path(&self.root)).await?;
        Ok(())
    }
}

fn chunks(file: File, cancel: CancelToken) -> impl Stream<Item = Result<Bytes>> + Send {
    futures::stream::unfold(Some((file, cancel)), |state| async move {
        let (mut file, cancel) = state?;
        if cancel.is_cancelled() {
            return Some((Err(Error::Cancelled), None));
        }
        let mut buffer = BytesMut::with_capacity(CHUNK_SIZE);
        match file.read_buf(&mut buffer).await {
            Ok(0) => None,
            Ok(_) => Some((Ok(buffer.freeze()), Some((file, cancel)))),
            Err(e) => Some((Err(e.into()), None)),
        }
    })
}

fn sidecar_for(peer: &Identity, request: &WriteRequest) -> Sidecar {
    let fingerprint = Fingerprint {
        size: request.expected_size,
        modified: request.modified,
    };
    Sidecar::new(
        peer.clone(),
        &request.path,
        fingerprint,
        request.resume_from,
    )
}

/// Creates the parent, settles the resume offset against the disk, and opens the part file.
async fn prepare_part(final_path: &Path, sidecar: &mut Sidecar) -> Result<File> {
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_path(final_path);
    sidecar.bytes = effective_resume(&part, sidecar.bytes).await?;
    // The sidecar lands before the first byte so that a crash at any later point leaves a
    // record that describes the part; without one, the part is orphaned and never resumed.
    write_sidecar(final_path, sidecar).await?;
    open_part(&part, sidecar.bytes).await
}

/// Any disagreement between the plan's offset and the part on disk invalidates the resume:
/// the part changed since planning, so neither number can be trusted and the write restarts.
async fn effective_resume(part: &Path, requested: u64) -> Result<u64> {
    if requested == 0 {
        return Ok(0);
    }
    let on_disk = match tokio::fs::metadata(part).await {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    if on_disk == requested {
        return Ok(requested);
    }
    tracing::warn!(
        part = %part.display(),
        requested,
        on_disk,
        "partial file changed since planning; restarting from zero"
    );
    Ok(0)
}

async fn open_part(part: &Path, resume_from: u64) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true);
    if resume_from > 0 {
        options.append(true);
    } else {
        options.write(true).truncate(true);
    }
    Ok(options.open(part).await?)
}

/// Flushes even after a failed chunk: tokio completes file writes in the background, and the
/// sidecar must not be written before every byte it counts has reached the part.
async fn pump_into(file: &mut File, bytes: &mut u64, input: ByteStream) -> Result<()> {
    let copied = copy_chunks(file, bytes, input).await;
    let flushed = file.flush().await.map_err(Error::from);
    copied.and(flushed)
}

async fn copy_chunks(file: &mut File, bytes: &mut u64, mut input: ByteStream) -> Result<()> {
    while let Some(item) = input.next().await {
        let chunk = item?;
        file.write_all(&chunk).await?;
        *bytes += chunk.len() as u64;
    }
    Ok(())
}

/// Records how far the part got so the next run can resume, then hands back the original error.
async fn keep_partial(final_path: &Path, sidecar: &Sidecar, error: Error) -> Result<WriteOutcome> {
    if let Err(e) = write_sidecar(final_path, sidecar).await {
        tracing::warn!(path = %final_path.display(), error = %e, "could not update sidecar");
    }
    Err(error)
}

fn length_mismatch(request: &WriteRequest, actual: u64) -> Error {
    Error::LengthMismatch {
        path: request.path.clone(),
        expected: request.expected_size,
        actual,
    }
}

async fn finish(file: File, final_path: &Path, modified: Option<ModifiedTime>) -> Result<()> {
    file.sync_all().await?;
    // Closed before the rename so no handle outlives the part.
    drop(file);
    let part = part_path(final_path);
    if let Some(modified) = modified {
        filetime::set_file_mtime(&part, modified.as_system().into())?;
    }
    tokio::fs::rename(&part, final_path).await?;
    // A crash between the rename and this removal leaves an orphan sidecar with no part;
    // scans ignore it, and a later cleanup command may remove it.
    if let Err(e) = remove_partial(final_path).await {
        tracing::warn!(path = %final_path.display(), error = %e, "could not remove sidecar");
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod test_support {
    #![allow(clippy::unwrap_used)]

    use super::LocalEndpoint;
    use crate::{
        entry::ModifiedTime,
        internal::{
            endpoint::{Endpoint, Identity, ScanResult},
            partial::{Fingerprint, Sidecar, part_path, sidecar_path},
        },
        path::RelPath,
    };
    use mtp_rs::CancelToken;
    use std::{
        fs,
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };
    use tempfile::TempDir;

    pub fn identity(serial: &str) -> Identity {
        Identity {
            device_serial: serial.into(),
            storage: "Internal".into(),
        }
    }

    pub fn endpoint(dir: &TempDir) -> LocalEndpoint {
        LocalEndpoint::new(dir.path(), identity("ZY22"))
    }

    pub fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    pub fn at(seconds: u64) -> ModifiedTime {
        ModifiedTime::from_system(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    pub fn write_valid_partial(
        dir: &TempDir,
        name: &str,
        serial: &str,
        content: &[u8],
        bytes: u64,
    ) {
        let final_path = dir.path().join(name);
        fs::write(part_path(&final_path), content).unwrap();
        let fingerprint = Fingerprint {
            size: 100,
            modified: Some(at(1)),
        };
        let sidecar = Sidecar::new(identity(serial), &rel(name), fingerprint, bytes);
        let json = serde_json::to_vec(&sidecar).unwrap();
        fs::write(sidecar_path(&final_path), json).unwrap();
    }

    pub async fn scan(local: &LocalEndpoint) -> ScanResult {
        local
            .scan(&CancelToken::new(), Arc::new(|_| {}))
            .await
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::{test_support::*, *};
    use crate::internal::partial::{read_sidecar_blocking, sidecar_path};
    use std::fs;

    fn stream(items: Vec<Result<Bytes>>) -> ByteStream {
        Box::pin(futures::stream::iter(items))
    }

    fn ok_chunks(chunks: &[&[u8]]) -> Vec<Result<Bytes>> {
        chunks
            .iter()
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect()
    }

    fn request(path: &str, expected_size: u64, resume_from: u64) -> WriteRequest {
        WriteRequest {
            path: rel(path),
            expected_size,
            resume_from,
            modified: Some(at(1_700_000_000)),
        }
    }

    #[tokio::test]
    async fn full_write_lands_the_file_with_its_mtime_and_no_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let input = stream(ok_chunks(&[b"hello ", b"world"]));
        let outcome = local.write(request("a/b.txt", 11, 0), input).await.unwrap();
        assert_eq!(outcome, WriteOutcome { bytes: 11 });
        let final_path = dir.path().join("a/b.txt");
        assert_eq!(fs::read(&final_path).unwrap(), b"hello world");
        let mtime =
            ModifiedTime::from_system(fs::metadata(&final_path).unwrap().modified().unwrap());
        assert_eq!(mtime, at(1_700_000_000));
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn empty_write_lands_an_empty_file_and_no_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = endpoint(&dir)
            .write(request("empty.bin", 0, 0), stream(Vec::new()))
            .await
            .unwrap();
        assert_eq!(outcome, WriteOutcome { bytes: 0 });
        let final_path = dir.path().join("empty.bin");
        assert_eq!(fs::read(&final_path).unwrap(), b"");
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn write_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let input = stream(ok_chunks(&[b"x"]));
        endpoint(&dir)
            .write(request("x/y/z.bin", 1, 0), input)
            .await
            .unwrap();
        assert!(dir.path().join("x/y").is_dir());
        assert_eq!(fs::read(dir.path().join("x/y/z.bin")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn interrupted_stream_keeps_the_part_and_a_matching_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let mut items = ok_chunks(&[b"hello", b"world"]);
        items.push(Err(Error::Cancelled));
        let err = local
            .write(request("f.bin", 12, 0), stream(items))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        let final_path = dir.path().join("f.bin");
        assert!(!final_path.exists());
        assert_eq!(fs::read(part_path(&final_path)).unwrap(), b"helloworld");
        let sidecar = read_sidecar_blocking(&final_path).unwrap().unwrap();
        assert_eq!(sidecar.bytes, 10);
        assert_eq!(sidecar.path, "f.bin");
        assert_eq!(sidecar.identity, identity("ZY22"));
    }

    #[tokio::test]
    async fn resuming_from_the_part_length_completes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let mut items = ok_chunks(&[b"helloworld"]);
        items.push(Err(Error::Cancelled));
        let _ = local.write(request("f.bin", 12, 0), stream(items)).await;
        let input = stream(ok_chunks(&[b"!!"]));
        let outcome = local.write(request("f.bin", 12, 10), input).await.unwrap();
        assert_eq!(outcome.bytes, 12);
        let final_path = dir.path().join("f.bin");
        assert_eq!(fs::read(&final_path).unwrap(), b"helloworld!!");
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn short_stream_is_a_length_mismatch_that_keeps_the_partial() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let input = stream(ok_chunks(&[b"abc"]));
        let err = local
            .write(request("f.bin", 100, 0), input)
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                Error::LengthMismatch {
                    expected: 100,
                    actual: 3,
                    ..
                }
            ),
            "{err:?}"
        );
        let final_path = dir.path().join("f.bin");
        assert_eq!(fs::read(part_path(&final_path)).unwrap(), b"abc");
        assert_eq!(
            read_sidecar_blocking(&final_path).unwrap().unwrap().bytes,
            3
        );
    }

    #[tokio::test]
    async fn resume_offset_that_disagrees_with_the_part_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        write_valid_partial(&dir, "f.bin", "ZY22", b"stale", 5);
        let input = stream(ok_chunks(&[b"fresh"]));
        let outcome = local.write(request("f.bin", 5, 3), input).await.unwrap();
        assert_eq!(outcome.bytes, 5);
        assert_eq!(fs::read(dir.path().join("f.bin")).unwrap(), b"fresh");
    }

    #[tokio::test]
    async fn read_from_an_offset_yields_exactly_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello world").unwrap();
        let local = endpoint(&dir);
        let cancel = CancelToken::new();
        let chunks: Vec<_> = local
            .read(&rel("a.txt"), 3, &cancel)
            .await
            .unwrap()
            .collect()
            .await;
        let chunks: Vec<_> = chunks.into_iter().map(Result::unwrap).collect();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].len() <= CHUNK_SIZE);
        assert_eq!(chunks[0], Bytes::from_static(b"lo world"));
        let tail: Vec<_> = local
            .read(&rel("a.txt"), 11, &cancel)
            .await
            .unwrap()
            .collect()
            .await;
        assert!(tail.is_empty());
    }

    #[tokio::test]
    async fn read_ends_with_cancelled_once_the_token_is_set() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let cancel = CancelToken::new();
        let mut chunks = endpoint(&dir)
            .read(&rel("a.txt"), 0, &cancel)
            .await
            .unwrap();
        cancel.cancel();
        let first = chunks.next().await.unwrap();
        assert!(matches!(first, Err(Error::Cancelled)), "{first:?}");
        assert!(chunks.next().await.is_none());
    }

    #[tokio::test]
    async fn mkdir_creates_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        endpoint(&dir).mkdir(&rel("x/y/z")).await.unwrap();
        assert!(dir.path().join("x/y/z").is_dir());
    }
}
