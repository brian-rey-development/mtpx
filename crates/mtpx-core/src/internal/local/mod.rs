//! The local filesystem as one side of a transfer: scan, stream, and resumable writes.

mod folding;
mod scan;

use crate::{
    entry::ModifiedTime,
    error::{Error, Result},
    internal::{
        endpoint::{ByteStream, CHUNK_SIZE, Endpoint, Identity, ScanResult, WriteRequest},
        partial::{Fingerprint, Sidecar, part_path, remove_partial, remove_sidecar, write_sidecar},
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

/// Bytes a part may grow between two sidecar records, so a process killed mid-file loses at
/// most this much of what already landed.
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;

/// The local filesystem side of a transfer, rooted at one directory.
#[derive(Debug, Clone)]
pub(crate) struct LocalEndpoint {
    root: PathBuf,
    peer: Identity,
    /// When set, the scan looks at this one file and its resume files rather than the whole tree.
    only: Option<String>,
}

impl LocalEndpoint {
    /// Wraps `root`; `peer` is the device this side exchanges files with, used to vet partials.
    pub(crate) fn new(root: impl Into<PathBuf>, peer: Identity) -> Self {
        Self {
            root: root.into(),
            peer,
            only: None,
        }
    }

    /// Wraps `root` for a transfer of the single file `name` directly beneath it, so a scan
    /// never walks the siblings.
    pub(crate) fn for_file(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        peer: Identity,
    ) -> Self {
        Self {
            root: root.into(),
            peer,
            only: Some(name.into()),
        }
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
            self.only.clone(),
            self.peer.clone(),
            cancel.clone(),
            on_found,
        );
        tokio::task::spawn_blocking(move || walker.run())
            .await
            .map_err(|e| Error::Io(io::Error::other(e)))?
    }

    async fn resume_offset(&self, path: &RelPath, requested: u64) -> Result<u64> {
        let part = part_path(&path.to_local_path(&self.root));
        effective_resume(&part, requested).await
    }

    async fn read(&self, path: &RelPath, offset: u64, cancel: &CancelToken) -> Result<ByteStream> {
        let full = path.to_local_path(&self.root);
        let mut file = File::open(&full)
            .await
            .map_err(Error::local_io("open", &full))?;
        file.set_max_buf_size(CHUNK_SIZE);
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(Error::local_io("seek", &full))?;
        Ok(Box::pin(chunks(file, full, cancel.clone())))
    }

    async fn write(&self, request: WriteRequest, input: ByteStream) -> Result<()> {
        let final_path = request.path.to_local_path(&self.root);
        refuse_directory(&final_path).await?;
        let sidecar = sidecar_for(&self.peer, &request);
        let mut writer = PartWriter::open(final_path, sidecar).await?;
        let pumped = writer.pump(input).await;
        match pumped.and_then(|()| writer.expect_length(&request)) {
            Ok(()) => writer.finish(request.modified).await,
            Err(e) => writer.stop(e).await,
        }
    }

    async fn mkdir(&self, path: &RelPath) -> Result<()> {
        let full = path.to_local_path(&self.root);
        tokio::fs::create_dir_all(&full)
            .await
            .map_err(Error::local_io("create directory", &full))
    }
}

fn chunks(
    file: File,
    path: PathBuf,
    cancel: CancelToken,
) -> impl Stream<Item = Result<Bytes>> + Send {
    futures::stream::unfold(Some((file, path, cancel)), |state| async move {
        let (mut file, path, cancel) = state?;
        if cancel.is_cancelled() {
            return Some((Err(Error::Cancelled), None));
        }
        let mut buffer = BytesMut::with_capacity(CHUNK_SIZE);
        match file.read_buf(&mut buffer).await {
            Ok(0) => None,
            Ok(_) => Some((Ok(buffer.freeze()), Some((file, path, cancel)))),
            Err(e) => Some((Err(Error::local_io("read", &path)(e)), None)),
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

/// A directory at the final path can never be replaced by a file; refusing it up front keeps
/// the part and sidecar from ever being created next to it.
async fn refuse_directory(final_path: &Path) -> Result<()> {
    match tokio::fs::metadata(final_path).await {
        Ok(meta) if meta.is_dir() => Err(Error::LocalIo {
            op: "write",
            path: final_path.to_path_buf(),
            source: io::ErrorKind::IsADirectory.into(),
        }),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::local_io("stat", final_path)(e)),
    }
}

/// A part file open for appending, with the record that describes it if the write stops early.
struct PartWriter {
    file: File,
    part: PathBuf,
    final_path: PathBuf,
    sidecar: Sidecar,
    /// False when the offset was reset after the source stream was opened at the planned one:
    /// the bytes that follow are a stale tail, and nothing may advertise them as a prefix.
    resumable: bool,
    checkpointed: u64,
}

impl PartWriter {
    async fn open(final_path: PathBuf, mut sidecar: Sidecar) -> Result<Self> {
        ensure_parent(&final_path).await?;
        let part = part_path(&final_path);
        let planned = sidecar.bytes;
        sidecar.bytes = effective_resume(&part, planned).await?;
        let resumable = sidecar.bytes == planned;
        // The old record must go before the part regrows past its count, or it would vouch
        // for a stale tail.
        if !resumable {
            remove_sidecar(&final_path).await?;
        }
        let file = open_part(&part, sidecar.bytes).await?;
        Ok(Self {
            file,
            part,
            final_path,
            resumable,
            checkpointed: sidecar.bytes,
            sidecar,
        })
    }

    /// Flushes even after a failed chunk, then trusts the disk over the counter: tokio
    /// completes file writes in the background, so a late failure leaves the count ahead of
    /// the part, and the part on disk is always a prefix of what was counted.
    async fn pump(&mut self, input: ByteStream) -> Result<()> {
        let copied = self.copy_chunks(input).await;
        let flushed = self.flush().await;
        self.sidecar.bytes = self.landed().await;
        copied.and(flushed)
    }

    async fn copy_chunks(&mut self, mut input: ByteStream) -> Result<()> {
        while let Some(item) = input.next().await {
            let chunk = item?;
            self.file
                .write_all(&chunk)
                .await
                .map_err(Error::local_io("write", &self.part))?;
            self.sidecar.bytes += chunk.len() as u64;
            if self.resumable && self.sidecar.bytes - self.checkpointed >= CHECKPOINT_BYTES {
                self.checkpoint().await?;
            }
        }
        Ok(())
    }

    async fn checkpoint(&mut self) -> Result<()> {
        self.flush().await?;
        write_sidecar(&self.final_path, &self.sidecar).await?;
        self.checkpointed = self.sidecar.bytes;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        self.file
            .flush()
            .await
            .map_err(Error::local_io("write", &self.part))
    }

    async fn landed(&self) -> u64 {
        let counted = self.sidecar.bytes;
        self.file
            .metadata()
            .await
            .map_or(counted, |meta| counted.min(meta.len()))
    }

    fn expect_length(&self, request: &WriteRequest) -> Result<()> {
        if self.sidecar.bytes == request.expected_size {
            return Ok(());
        }
        Err(Error::LengthMismatch {
            path: request.path.clone(),
            expected: request.expected_size,
            actual: self.sidecar.bytes,
        })
    }

    /// Keeps whatever lets the next run resume, or nothing when the bytes were a stale tail.
    async fn stop(self, error: Error) -> Result<()> {
        if self.resumable {
            return keep_partial(&self.final_path, &self.sidecar, error).await;
        }
        discard_partial(&self.final_path, error).await
    }

    /// Every byte is on disk, so a failure while committing keeps the full part and the next
    /// run only finalises it instead of streaming the file again.
    async fn finish(self, modified: Option<ModifiedTime>) -> Result<()> {
        let Self {
            file,
            part,
            final_path,
            sidecar,
            ..
        } = self;
        match commit(file, &part, &final_path, modified).await {
            Ok(()) => Ok(()),
            Err(e) => keep_partial(&final_path, &sidecar, e).await,
        }
    }
}

async fn ensure_parent(final_path: &Path) -> Result<()> {
    let Some(parent) = final_path.parent() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(Error::local_io("create directory", parent))
}

/// A part shorter than the plan's offset changed since planning, so the write restarts from
/// zero; a longer one keeps its planned prefix, and `open_part` drops the tail beyond it.
async fn effective_resume(part: &Path, requested: u64) -> Result<u64> {
    if requested == 0 {
        return Ok(0);
    }
    let on_disk = match tokio::fs::metadata(part).await {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(Error::local_io("stat", part)(e)),
    };
    if on_disk >= requested {
        return Ok(requested);
    }
    tracing::warn!(
        part = %part.display(),
        requested,
        on_disk,
        "partial file shrank since planning; restarting from zero"
    );
    Ok(0)
}

async fn open_part(part: &Path, resume_from: u64) -> Result<File> {
    let mut file = if resume_from == 0 {
        File::create(part)
            .await
            .map_err(Error::local_io("open", part))?
    } else {
        open_resumed(part, resume_from).await?
    };
    file.set_max_buf_size(CHUNK_SIZE);
    Ok(file)
}

/// Opens an existing part positioned at `resume_from`. The length is re-checked through the
/// handle, not the path, so a part replaced between the probe and the open fails here instead
/// of landing a hole in the final file; an unverified tail past the checkpoint is dropped.
async fn open_resumed(part: &Path, resume_from: u64) -> Result<File> {
    // Plain write access, not append: Windows grants an append handle no right to truncate.
    let mut file = OpenOptions::new()
        .write(true)
        .open(part)
        .await
        .map_err(Error::local_io("open", part))?;
    ensure_still_holds(&file, part, resume_from).await?;
    file.set_len(resume_from)
        .await
        .map_err(Error::local_io("truncate", part))?;
    file.seek(SeekFrom::Start(resume_from))
        .await
        .map_err(Error::local_io("seek", part))?;
    Ok(file)
}

async fn ensure_still_holds(file: &File, part: &Path, resume_from: u64) -> Result<()> {
    let meta = file
        .metadata()
        .await
        .map_err(Error::local_io("stat", part))?;
    if meta.len() >= resume_from {
        return Ok(());
    }
    let changed = io::Error::new(
        io::ErrorKind::InvalidData,
        "partial file changed while opening",
    );
    Err(Error::local_io("open", part)(changed))
}

async fn discard_partial(final_path: &Path, error: Error) -> Result<()> {
    if let Err(e) = remove_partial(final_path).await {
        tracing::warn!(path = %final_path.display(), error = %e, "could not drop stale partial");
    }
    Err(error)
}

async fn keep_partial(final_path: &Path, sidecar: &Sidecar, error: Error) -> Result<()> {
    if let Err(e) = write_sidecar(final_path, sidecar).await {
        tracing::warn!(path = %final_path.display(), error = %e, "could not update sidecar");
    }
    Err(error)
}

async fn commit(
    file: File,
    part: &Path,
    final_path: &Path,
    modified: Option<ModifiedTime>,
) -> Result<()> {
    sync(file, part, modified).await?;
    tokio::fs::rename(part, final_path)
        .await
        .map_err(Error::local_io("rename", part))?;
    // A crash before this removal leaves an orphan sidecar with no part, which scans ignore.
    if let Err(e) = remove_sidecar(final_path).await {
        tracing::warn!(path = %final_path.display(), error = %e, "could not remove sidecar");
    }
    Ok(())
}

/// Stamps the mtime and syncs through the open handle on the blocking pool. The stamp is
/// best-effort: a mount that refuses it must not fail a file whose every byte landed. The
/// handle is dropped inside the closure so nothing outlives the part at rename time.
async fn sync(file: File, part: &Path, modified: Option<ModifiedTime>) -> Result<()> {
    let file = file.into_std().await;
    let stamp = modified.map(ModifiedTime::as_system);
    let shown = part.display().to_string();
    let synced = tokio::task::spawn_blocking(move || {
        let stamped = stamp.map_or(Ok(()), |stamp| file.set_modified(stamp));
        if let Err(e) = stamped {
            tracing::warn!(path = %shown, error = %e, "could not set modification time");
        }
        file.sync_all()
    })
    .await
    .map_err(io::Error::other)?;
    synced.map_err(Error::local_io("sync", part))
}

#[cfg(test)]
pub(super) mod test_support {
    #![allow(clippy::unwrap_used)]

    use super::LocalEndpoint;
    use crate::internal::{
        endpoint::{Endpoint, Identity, ScanResult},
        partial::{Fingerprint, Sidecar, part_path, sidecar_path},
    };
    pub(crate) use crate::test_support::{at, rel};
    use mtp_rs::CancelToken;
    use std::{fs, sync::Arc};
    use tempfile::TempDir;

    pub(crate) fn identity(serial: &str) -> Identity {
        crate::test_support::identity(serial, "Internal")
    }

    pub(crate) fn endpoint(dir: &TempDir) -> LocalEndpoint {
        LocalEndpoint::new(dir.path(), identity("ZY22"))
    }

    pub(crate) fn write_valid_partial(
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

    pub(crate) async fn scan(local: &LocalEndpoint) -> ScanResult {
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

    fn sidecar_bytes(final_path: &Path) -> u64 {
        read_sidecar_blocking(final_path).unwrap().unwrap().bytes
    }

    #[tokio::test]
    async fn full_write_lands_the_file_with_its_mtime_and_no_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let input = stream(ok_chunks(&[b"hello ", b"world"]));
        local.write(request("a/b.txt", 11, 0), input).await.unwrap();
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
        endpoint(&dir)
            .write(request("empty.bin", 0, 0), stream(Vec::new()))
            .await
            .unwrap();
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
    async fn no_sidecar_exists_until_the_write_stops_early() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let final_path = dir.path().join("f.bin");
        let probe = final_path.clone();
        let items = futures::stream::unfold(0, move |sent| {
            let probe = probe.clone();
            async move {
                match sent {
                    0 => Some((Ok(Bytes::from_static(b"abc")), 1)),
                    1 => {
                        assert!(
                            !sidecar_path(&probe).exists(),
                            "sidecar written before a stop"
                        );
                        Some((Err(Error::Cancelled), 2))
                    }
                    _ => None,
                }
            }
        });
        local
            .write(request("f.bin", 100, 0), Box::pin(items))
            .await
            .unwrap_err();
        assert_eq!(sidecar_bytes(&final_path), 3);
    }

    #[tokio::test]
    async fn a_checkpoint_records_the_bytes_landed_so_far_while_the_stream_is_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let final_path = dir.path().join("big.bin");
        let probe = final_path.clone();
        let pieces = usize::try_from(CHECKPOINT_BYTES).unwrap() / CHUNK_SIZE;
        let items = futures::stream::unfold(0, move |sent| {
            let probe = probe.clone();
            async move {
                if sent < pieces {
                    return Some((Ok(Bytes::from(vec![7; CHUNK_SIZE])), sent + 1));
                }
                if sent == pieces {
                    assert_eq!(sidecar_bytes(&probe), CHECKPOINT_BYTES);
                    return Some((Err(Error::Cancelled), sent + 1));
                }
                None
            }
        });
        local
            .write(request("big.bin", CHECKPOINT_BYTES * 2, 0), Box::pin(items))
            .await
            .unwrap_err();
        assert_eq!(sidecar_bytes(&final_path), CHECKPOINT_BYTES);
        let part = fs::metadata(part_path(&final_path)).unwrap().len();
        assert_eq!(part, CHECKPOINT_BYTES);
    }

    #[tokio::test]
    async fn resuming_from_the_part_length_completes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let mut items = ok_chunks(&[b"helloworld"]);
        items.push(Err(Error::Cancelled));
        let _ = local.write(request("f.bin", 12, 0), stream(items)).await;
        let input = stream(ok_chunks(&[b"!!"]));
        local.write(request("f.bin", 12, 10), input).await.unwrap();
        let final_path = dir.path().join("f.bin");
        assert_eq!(fs::read(&final_path).unwrap(), b"helloworld!!");
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn a_resume_equal_to_the_expected_size_with_an_empty_stream_finalises_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        write_valid_partial(&dir, "f.bin", "ZY22", b"whole", 5);
        local
            .write(request("f.bin", 5, 5), stream(Vec::new()))
            .await
            .unwrap();
        let final_path = dir.path().join("f.bin");
        assert_eq!(fs::read(&final_path).unwrap(), b"whole");
        let mtime =
            ModifiedTime::from_system(fs::metadata(&final_path).unwrap().modified().unwrap());
        assert_eq!(mtime, at(1_700_000_000));
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn a_resume_drops_the_unverified_tail_beyond_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        write_valid_partial(&dir, "f.bin", "ZY22", b"helloworldJUNK", 10);
        let input = stream(ok_chunks(&[b"!!"]));
        local.write(request("f.bin", 12, 10), input).await.unwrap();
        assert_eq!(fs::read(dir.path().join("f.bin")).unwrap(), b"helloworld!!");
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
        assert_eq!(sidecar_bytes(&final_path), 3);
    }

    #[tokio::test]
    async fn resume_offset_that_disagrees_with_the_part_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        write_valid_partial(&dir, "f.bin", "ZY22", b"st", 5);
        let input = stream(ok_chunks(&[b"fresh"]));
        local.write(request("f.bin", 5, 3), input).await.unwrap();
        assert_eq!(fs::read(dir.path().join("f.bin")).unwrap(), b"fresh");
    }

    #[tokio::test]
    async fn invalidated_tail_is_dropped_instead_of_kept_as_a_partial() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        write_valid_partial(&dir, "f.bin", "ZY22", b"st", 5);
        let input = stream(ok_chunks(&[b"DE"]));
        let err = local
            .write(request("f.bin", 5, 3), input)
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::LengthMismatch { .. }), "{err:?}");
        let final_path = dir.path().join("f.bin");
        assert!(!final_path.exists());
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn a_failed_finish_records_the_full_part_for_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let local = endpoint(&dir);
        let final_path = dir.path().join("f.bin");
        let mut writer = PartWriter::open(
            final_path.clone(),
            sidecar_for(&local.peer, &request("f.bin", 5, 0)),
        )
        .await
        .unwrap();
        writer.pump(stream(ok_chunks(&[b"hello"]))).await.unwrap();
        fs::create_dir(&final_path).unwrap();
        fs::write(final_path.join("keep"), b"k").unwrap();
        let err = writer.finish(Some(at(1))).await.unwrap_err();
        assert!(
            matches!(&err, Error::LocalIo { op: "rename", .. }),
            "{err:?}"
        );
        assert_eq!(fs::read(part_path(&final_path)).unwrap(), b"hello");
        assert_eq!(sidecar_bytes(&final_path), 5);
        assert_eq!(fs::read(final_path.join("keep")).unwrap(), b"k");
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
    async fn read_fills_whole_chunks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("big.bin"), vec![7; CHUNK_SIZE + 1]).unwrap();
        let chunks: Vec<_> = endpoint(&dir)
            .read(&rel("big.bin"), 0, &CancelToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        let lengths: Vec<_> = chunks.iter().map(|c| c.as_ref().unwrap().len()).collect();
        assert_eq!(lengths, [CHUNK_SIZE, 1]);
    }

    #[tokio::test]
    async fn read_of_a_missing_file_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let Err(err) = endpoint(&dir)
            .read(&rel("nope.txt"), 0, &CancelToken::new())
            .await
        else {
            panic!("a missing file opened");
        };
        assert!(matches!(&err, Error::LocalIo { op: "open", .. }), "{err:?}");
        assert!(err.to_string().contains("nope.txt"), "{err}");
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

    #[tokio::test]
    async fn write_over_a_directory_is_refused_before_anything_is_created() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/keep.txt"), b"keep").unwrap();
        let err = endpoint(&dir)
            .write(request("sub", 1, 0), stream(ok_chunks(&[b"x"])))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::LocalIo { op: "write", .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("sub"), "{err}");
        assert_eq!(fs::read(dir.path().join("sub/keep.txt")).unwrap(), b"keep");
        let final_path = dir.path().join("sub");
        assert!(final_path.is_dir());
        assert!(!part_path(&final_path).exists());
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn a_directory_in_the_way_of_the_part_names_the_part() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("a.jpg");
        fs::create_dir(part_path(&final_path)).unwrap();
        let err = endpoint(&dir)
            .write(request("a.jpg", 1, 0), stream(ok_chunks(&[b"x"])))
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::LocalIo { op: "open", .. }), "{err:?}");
        assert!(err.to_string().contains("a.jpg.mtpx-part"), "{err}");
        assert!(!sidecar_path(&final_path).exists());
    }

    #[tokio::test]
    async fn mkdir_over_a_file_fails_and_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("taken"), b"data").unwrap();
        let err = endpoint(&dir).mkdir(&rel("taken")).await.unwrap_err();
        assert!(
            matches!(
                &err,
                Error::LocalIo {
                    op: "create directory",
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(fs::read(dir.path().join("taken")).unwrap(), b"data");
    }
}
