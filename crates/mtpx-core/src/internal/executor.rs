//! Runs a plan: streams every `Copy` from source to destination, retries transient reads,
//! and reports progress, failures and cancellation as events.

use crate::{
    entry::ModifiedTime,
    error::{Error, Result},
    event::{self, FailedFile, ProgressEvent, Report},
    internal::endpoint::{ByteStream, Endpoint, WriteRequest},
    path::RelPath,
    plan::{Action, Plan, SkipReason},
};
use futures::StreamExt;
use mtp_rs::CancelToken;
use std::{io, time::Duration};
use tokio::{sync::mpsc, time::Instant};

/// How many times a read that failed with a transient MTP error is attempted again.
pub(crate) const MAX_READ_RETRIES: u32 = 3;
/// Pause before the first retry; it doubles on every further attempt.
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Longest stretch a backoff sleeps before looking at the cancel token again.
pub(crate) const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Runs a plan against two endpoints, reporting through `events`.
///
/// Every event except `FileProgress` is awaited, so the receiver must be drained while the
/// run is in flight; `FileProgress` is best-effort and dropped when the channel is full.
pub(crate) struct Executor {
    cancel: CancelToken,
    events: mpsc::Sender<ProgressEvent>,
}

/// Why a run stops before the last action.
enum Stop {
    Interrupted,
    Aborted(Error),
}

type StepResult = std::result::Result<(), Stop>;

impl Executor {
    pub(crate) const fn new(cancel: CancelToken, events: mpsc::Sender<ProgressEvent>) -> Self {
        Self { cancel, events }
    }

    /// Executes every action in order and returns the totals.
    ///
    /// # Errors
    /// Only the errors that end the whole batch: the device disconnected, vanished, was reset,
    /// or refused access, or the destination is full or read-only. Such a run ends with
    /// `Aborted` instead of `Finished`, carrying the totals so far with the file in flight
    /// among the failed. A cancelled run is `Ok` with `report.interrupted` set; a failed file
    /// lands in `report.failed` and the run continues.
    pub(crate) async fn run<S: Endpoint, D: Endpoint>(
        &self,
        plan: &Plan,
        source: &S,
        dest: &D,
    ) -> Result<Report> {
        let since = Instant::now();
        let mut report = Report::default();
        let actions = plan.actions();
        for (index, action) in actions.iter().enumerate() {
            let Err(stop) = self.step(action, source, dest, &mut report).await else {
                continue;
            };
            let remaining = remaining_copies(&actions[index..]);
            return match stop {
                Stop::Interrupted => Ok(self.interrupt(report, since, remaining).await),
                Stop::Aborted(error) => {
                    self.abort(report, since, remaining).await;
                    Err(error)
                }
            };
        }
        Ok(self.finish(report, since).await)
    }

    async fn step<S: Endpoint, D: Endpoint>(
        &self,
        action: &Action,
        source: &S,
        dest: &D,
        report: &mut Report,
    ) -> StepResult {
        if self.cancel.is_cancelled() {
            return Err(Stop::Interrupted);
        }
        match action {
            Action::Mkdir { path } => self.mkdir(dest, path, report).await,
            Action::Skip { path, reason } => self.skip(path, *reason, report).await,
            Action::Copy {
                path,
                size,
                modified,
                resume_from,
                ..
            } => {
                let request = request(path, *size, *modified, *resume_from);
                self.copy(source, dest, request, report).await
            }
        }
    }

    async fn mkdir<D: Endpoint>(
        &self,
        dest: &D,
        path: &RelPath,
        report: &mut Report,
    ) -> StepResult {
        match dest.mkdir(path).await {
            Ok(()) => Ok(()),
            Err(error) => self.fail(report, path, error).await,
        }
    }

    async fn skip(&self, path: &RelPath, reason: SkipReason, report: &mut Report) -> StepResult {
        report.skipped += 1;
        self.emit(ProgressEvent::Skipped {
            path: path.clone(),
            reason,
        })
        .await;
        Ok(())
    }

    async fn copy<S: Endpoint, D: Endpoint>(
        &self,
        source: &S,
        dest: &D,
        request: WriteRequest,
        report: &mut Report,
    ) -> StepResult {
        let path = request.path.clone();
        let request = match self.settle(dest, request).await {
            Ok(request) => request,
            Err(error) => return self.fail(report, &path, error).await,
        };
        match self.copy_one(source, dest, request).await {
            Ok(bytes) => {
                report.copied += 1;
                report.bytes += bytes;
                Ok(())
            }
            Err(error) => self.fail(report, &path, error).await,
        }
    }

    /// Announces `FileStarted` with the offset the copy will actually stream from.
    async fn settle<D: Endpoint>(&self, dest: &D, request: WriteRequest) -> Result<WriteRequest> {
        let resume_from = dest
            .resume_offset(&request.path, request.resume_from)
            .await?;
        self.emit(ProgressEvent::FileStarted {
            path: request.path.clone(),
            size: request.expected_size,
            resume_from,
        })
        .await;
        Ok(WriteRequest {
            resume_from,
            ..request
        })
    }

    /// Returns the bytes that flowed this run: the destination's `Ok` commits exactly
    /// `expected_size`, so the difference from the resume offset is what moved.
    async fn copy_one<S: Endpoint, D: Endpoint>(
        &self,
        source: &S,
        dest: &D,
        request: WriteRequest,
    ) -> Result<u64> {
        let resume_from = request.resume_from;
        let raw = self
            .read_with_retry(source, &request.path, resume_from)
            .await?;
        let since = Instant::now();
        let path = request.path.clone();
        let bytes = request.expected_size - resume_from;
        let stream = self.progress_stream(raw, path.clone(), resume_from);
        dest.write(request, stream).await?;
        let elapsed = since.elapsed();
        self.emit(ProgressEvent::FileFinished {
            path,
            bytes,
            elapsed,
        })
        .await;
        Ok(bytes)
    }

    /// Opens the source stream, retrying transient MTP errors with a doubling backoff. Errors
    /// after the stream has started are not retried: the file is reported failed and the
    /// destination's partial lets the next run resume it.
    async fn read_with_retry<S: Endpoint>(
        &self,
        source: &S,
        path: &RelPath,
        offset: u64,
    ) -> Result<ByteStream> {
        let mut backoff = RETRY_BACKOFF;
        for _ in 0..MAX_READ_RETRIES {
            match source.read(path, offset, &self.cancel).await {
                Err(Error::Mtp(e)) if e.is_retryable() => {
                    self.report_and_back_off(path, &e, backoff).await?;
                }
                outcome => return outcome,
            }
            backoff *= 2;
        }
        source.read(path, offset, &self.cancel).await
    }

    /// A cancel that landed with the error is honoured before a retry is announced, so the
    /// observer never sees a retry that cannot happen.
    async fn report_and_back_off(
        &self,
        path: &RelPath,
        error: &mtp_rs::Error,
        backoff: Duration,
    ) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.emit(ProgressEvent::FileFailed {
            path: path.clone(),
            error: error.to_string(),
            will_retry: true,
        })
        .await;
        self.back_off(backoff).await
    }

    /// Sleeps `backoff` in short slices so a cancel is noticed within `CANCEL_POLL`.
    async fn back_off(&self, backoff: Duration) -> Result<()> {
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            tokio::time::sleep(CANCEL_POLL.min(deadline - Instant::now())).await;
        }
        Ok(())
    }

    /// Reports every chunk that passes. `try_send` keeps a slow observer from stalling the
    /// pump: a full channel drops the progress event, and the next one catches up.
    fn progress_stream(&self, stream: ByteStream, path: RelPath, resume_from: u64) -> ByteStream {
        let events = self.events.clone();
        let mut so_far = resume_from;
        let reported = stream.inspect(move |item| {
            let Ok(chunk) = item else { return };
            so_far += chunk.len() as u64;
            let _ = events.try_send(ProgressEvent::FileProgress {
                path: path.clone(),
                bytes: so_far,
            });
        });
        Box::pin(reported)
    }

    async fn fail(&self, report: &mut Report, path: &RelPath, error: Error) -> StepResult {
        if matches!(error, Error::Cancelled) {
            return Err(Stop::Interrupted);
        }
        let message = error.to_string();
        report
            .failed
            .push(FailedFile::new(path.clone(), message.clone()));
        if aborts_batch(&error) {
            return Err(Stop::Aborted(error));
        }
        self.emit(ProgressEvent::FileFailed {
            path: path.clone(),
            error: message,
            will_retry: false,
        })
        .await;
        Ok(())
    }

    async fn interrupt(&self, mut report: Report, since: Instant, remaining_files: u64) -> Report {
        self.emit(ProgressEvent::Interrupted { remaining_files })
            .await;
        report.interrupted = true;
        self.finish(report, since).await
    }

    async fn abort(&self, mut report: Report, since: Instant, remaining_files: u64) {
        report.elapsed = since.elapsed();
        self.emit(ProgressEvent::Aborted {
            report,
            remaining_files,
        })
        .await;
    }

    async fn finish(&self, mut report: Report, since: Instant) -> Report {
        report.elapsed = since.elapsed();
        self.emit(ProgressEvent::Finished {
            report: report.clone(),
        })
        .await;
        report
    }

    async fn emit(&self, event: ProgressEvent) {
        event::emit(&self.events, event).await;
    }
}

fn request(
    path: &RelPath,
    size: u64,
    modified: Option<ModifiedTime>,
    resume_from: u64,
) -> WriteRequest {
    WriteRequest {
        path: path.clone(),
        expected_size: size,
        resume_from,
        modified,
    }
}

/// The errors after which no further endpoint call can succeed: the device is gone or refuses
/// us, or the destination cannot take another byte.
fn aborts_batch(error: &Error) -> bool {
    match error {
        Error::Disconnected
        | Error::NoDevice
        | Error::PermissionDenied
        | Error::ExclusiveAccess { .. }
        | Error::Mtp(mtp_rs::Error::DeviceReset) => true,
        Error::Io(e) | Error::LocalIo { source: e, .. } => destination_is_unwritable(e),
        _ => false,
    }
}

fn destination_is_unwritable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn remaining_copies(actions: &[Action]) -> u64 {
    actions
        .iter()
        .filter(|action| matches!(action, Action::Copy { .. }))
        .count() as u64
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::too_many_lines,
        clippy::unused_async_trait_impl
    )]

    use super::*;
    use crate::{internal::endpoint::ScanResult, plan::CopyReason, test_support::rel};
    use bytes::Bytes;
    use futures::StreamExt;
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    const TEST_CHUNK: usize = 4;
    const EVENT_CAPACITY: usize = 1024;
    const TEN_BYTES: &[u8] = b"0123456789";

    struct FakeEndpoint {
        files: Mutex<HashMap<RelPath, Vec<u8>>>,
        dirs: Mutex<Vec<RelPath>>,
        read_offsets: Mutex<Vec<u64>>,
        read_failures: AtomicUsize,
        failure: fn() -> Error,
        /// Set the token from inside a failing read, as a Ctrl-C landing with the error would.
        cancel_on_failure: Option<CancelToken>,
        fail_after_chunks: Option<(usize, fn() -> Error)>,
        cancel_after_write: Option<CancelToken>,
        write_failure: Option<fn() -> Error>,
        mkdir_fails: bool,
        resume_probe: Option<u64>,
    }

    impl Default for FakeEndpoint {
        fn default() -> Self {
            Self {
                files: Mutex::default(),
                dirs: Mutex::default(),
                read_offsets: Mutex::default(),
                read_failures: AtomicUsize::new(0),
                failure: || Error::Disconnected,
                cancel_on_failure: None,
                fail_after_chunks: None,
                cancel_after_write: None,
                write_failure: None,
                mkdir_fails: false,
                resume_probe: None,
            }
        }
    }

    impl FakeEndpoint {
        fn with_files(files: &[(&str, &[u8])]) -> Self {
            let endpoint = Self::default();
            for (path, data) in files {
                endpoint
                    .files
                    .lock()
                    .unwrap()
                    .insert(rel(path), data.to_vec());
            }
            endpoint
        }

        fn failing_reads(self, count: usize, failure: fn() -> Error) -> Self {
            Self {
                read_failures: AtomicUsize::new(count),
                failure,
                ..self
            }
        }

        fn file(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(&rel(path)).cloned()
        }

        fn take_read_failure(&self) -> bool {
            self.read_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        }

        fn chunks_from(&self, data: &[u8], offset: u64) -> Vec<Result<Bytes>> {
            let tail = &data[usize::try_from(offset).unwrap()..];
            let mut items: Vec<Result<Bytes>> = tail
                .chunks(TEST_CHUNK)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect();
            if let Some((after, failure)) = self.fail_after_chunks {
                items.truncate(after);
                items.push(Err(failure()));
            }
            items
        }
    }

    async fn drain_into(data: &mut Vec<u8>, input: &mut ByteStream) -> Result<()> {
        while let Some(item) = input.next().await {
            data.extend_from_slice(&item?);
        }
        Ok(())
    }

    impl Endpoint for FakeEndpoint {
        fn label(&self) -> String {
            "fake".to_owned()
        }

        async fn scan(
            &self,
            _cancel: &CancelToken,
            _on_found: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> Result<ScanResult> {
            Ok(ScanResult {
                snapshot: crate::entry::Snapshot::new("fake", Vec::new(), Vec::new()),
                partials: HashMap::new(),
                folding: crate::planner::NameFolding::Exact,
            })
        }

        async fn resume_offset(&self, path: &RelPath, requested: u64) -> Result<u64> {
            let _ = path;
            Ok(self.resume_probe.unwrap_or(requested))
        }

        async fn read(
            &self,
            path: &RelPath,
            offset: u64,
            _cancel: &CancelToken,
        ) -> Result<ByteStream> {
            self.read_offsets.lock().unwrap().push(offset);
            if self.take_read_failure() {
                if let Some(cancel) = &self.cancel_on_failure {
                    cancel.cancel();
                }
                return Err((self.failure)());
            }
            let data = self
                .file(&path.to_string())
                .ok_or_else(|| Error::SourceVanished(path.clone()))?;
            Ok(Box::pin(futures::stream::iter(
                self.chunks_from(&data, offset),
            )))
        }

        async fn write(&self, request: WriteRequest, mut input: ByteStream) -> Result<()> {
            if let Some(failure) = self.write_failure {
                return Err(failure());
            }
            let mut data = self.file(&request.path.to_string()).unwrap_or_default();
            data.truncate(usize::try_from(request.resume_from).unwrap());
            let drained = drain_into(&mut data, &mut input).await;
            let actual = u64::try_from(data.len()).unwrap();
            self.files
                .lock()
                .unwrap()
                .insert(request.path.clone(), data);
            drained?;
            if actual != request.expected_size {
                return Err(Error::LengthMismatch {
                    path: request.path,
                    expected: request.expected_size,
                    actual,
                });
            }
            if let Some(cancel) = &self.cancel_after_write {
                cancel.cancel();
            }
            Ok(())
        }

        async fn mkdir(&self, path: &RelPath) -> Result<()> {
            if self.mkdir_fails {
                return Err(Error::Io(io::Error::other("read-only destination")));
            }
            self.dirs.lock().unwrap().push(path.clone());
            Ok(())
        }
    }

    fn mkdir(path: &str) -> Action {
        Action::Mkdir { path: rel(path) }
    }

    fn copy(path: &str, size: u64, resume_from: u64) -> Action {
        Action::Copy {
            path: rel(path),
            size,
            modified: None,
            resume_from,
            reason: CopyReason::New,
        }
    }

    fn skip(path: &str) -> Action {
        Action::Skip {
            path: rel(path),
            reason: SkipReason::Identical,
        }
    }

    fn plan_of(actions: &[Action]) -> Plan {
        Plan::new(actions.to_vec())
    }

    async fn collect(mut rx: mpsc::Receiver<ProgressEvent>) -> Vec<ProgressEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    async fn run_plan(
        plan: &Plan,
        source: &FakeEndpoint,
        dest: &FakeEndpoint,
        cancel: &CancelToken,
    ) -> (Result<Report>, Vec<ProgressEvent>) {
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        let outcome = Executor::new(cancel.clone(), tx)
            .run(plan, source, dest)
            .await;
        (outcome, collect(rx).await)
    }

    /// One line per event, without the timings that no test can predict.
    fn describe(event: &ProgressEvent) -> String {
        match event {
            ProgressEvent::FileStarted {
                path,
                size,
                resume_from,
            } => format!("started {path} {size} from {resume_from}"),
            ProgressEvent::FileProgress { path, bytes } => format!("progress {path} {bytes}"),
            ProgressEvent::FileFinished { path, bytes, .. } => format!("finished {path} {bytes}"),
            ProgressEvent::FileFailed {
                path, will_retry, ..
            } => format!("failed {path} retry={will_retry}"),
            ProgressEvent::Skipped { path, reason } => format!("skipped {path} {reason:?}"),
            ProgressEvent::Interrupted { remaining_files } => {
                format!("interrupted {remaining_files}")
            }
            ProgressEvent::Finished { .. } => "done".to_owned(),
            ProgressEvent::Aborted {
                remaining_files, ..
            } => format!("aborted {remaining_files}"),
            other => format!("{other:?}"),
        }
    }

    fn without_progress(events: &[ProgressEvent]) -> Vec<String> {
        events
            .iter()
            .filter(|e| !matches!(e, ProgressEvent::FileProgress { .. }))
            .map(describe)
            .collect()
    }

    fn progress_of(events: &[ProgressEvent], file: &str) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::FileProgress { path, bytes } if path.to_string() == file => {
                    Some(*bytes)
                }
                _ => None,
            })
            .collect()
    }

    fn final_report(events: &[ProgressEvent]) -> &Report {
        match events.last() {
            Some(ProgressEvent::Finished { report } | ProgressEvent::Aborted { report, .. }) => {
                report
            }
            other => panic!("last event is not terminal: {other:?}"),
        }
    }

    fn finished_elapsed(events: &[ProgressEvent], file: &str) -> Duration {
        events
            .iter()
            .find_map(|e| match e {
                ProgressEvent::FileFinished { path, elapsed, .. } if path.to_string() == file => {
                    Some(*elapsed)
                }
                _ => None,
            })
            .unwrap()
    }

    fn failed_paths(report: &Report) -> Vec<String> {
        report.failed.iter().map(|f| f.path.to_string()).collect()
    }

    #[tokio::test]
    async fn a_mixed_plan_copies_bytes_creates_dirs_and_emits_events_in_order() {
        let source = FakeEndpoint::with_files(&[("a/x", TEN_BYTES), ("c", b"")]);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[mkdir("a"), copy("a/x", 10, 0), skip("b"), copy("c", 0, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        let report = outcome.unwrap();
        assert_eq!(dest.file("a/x").unwrap(), TEN_BYTES);
        assert_eq!(dest.file("c").unwrap(), b"");
        assert_eq!(*dest.dirs.lock().unwrap(), vec![rel("a")]);
        assert_eq!(
            without_progress(&events),
            [
                "started a/x 10 from 0",
                "finished a/x 10",
                "skipped b Identical",
                "started c 0 from 0",
                "finished c 0",
                "done",
            ]
        );
        let progress = progress_of(&events, "a/x");
        assert!(!progress.is_empty());
        assert!(progress.is_sorted_by(|a, b| a < b), "{progress:?}");
        assert_eq!(progress.last(), Some(&10));
        assert_eq!(report.copied, 2);
        assert_eq!(report.bytes, 10);
        assert_eq!(report.skipped, 1);
        assert!(report.failed.is_empty());
        assert!(!report.interrupted);
        assert_eq!(final_report(&events), &report);
    }

    #[tokio::test]
    async fn an_empty_plan_yields_only_finished_and_a_zero_report() {
        let plan = plan_of(&[]);
        let (outcome, events) = run_plan(
            &plan,
            &FakeEndpoint::default(),
            &FakeEndpoint::default(),
            &CancelToken::new(),
        )
        .await;

        assert_eq!(without_progress(&events), ["done"]);
        let report = outcome.unwrap();
        assert_eq!(
            Report {
                elapsed: Duration::ZERO,
                ..report
            },
            Report::default()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_that_fails_twice_with_a_transient_error_is_retried_with_backoff() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)])
            .failing_reads(2, || Error::Mtp(mtp_rs::Error::Timeout));
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let before = tokio::time::Instant::now();
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(
            before.elapsed() >= RETRY_BACKOFF * 3,
            "{:?}",
            before.elapsed()
        );
        assert_eq!(
            without_progress(&events),
            [
                "started a 10 from 0",
                "failed a retry=true",
                "failed a retry=true",
                "finished a 10",
                "done",
            ]
        );
        assert!(
            finished_elapsed(&events, "a") < RETRY_BACKOFF,
            "the backoff is not billed to the file"
        );
        let report = outcome.unwrap();
        assert_eq!(report.copied, 1);
        assert!(report.failed.is_empty());
        assert_eq!(dest.file("a").unwrap(), TEN_BYTES);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_that_keeps_timing_out_gives_up_after_the_last_retry() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES), ("b", b"bb")])
            .failing_reads(4, || Error::Mtp(mtp_rs::Error::Timeout));
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(
            without_progress(&events),
            [
                "started a 10 from 0",
                "failed a retry=true",
                "failed a retry=true",
                "failed a retry=true",
                "failed a retry=false",
                "started b 2 from 0",
                "finished b 2",
                "done",
            ]
        );
        let report = outcome.unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, rel("a"));
        assert!(dest.file("a").is_none());
        assert_eq!(dest.file("b").unwrap(), b"bb");
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_retryable_read_error_fails_the_file_at_once() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)])
            .failing_reads(1, || Error::Mtp(mtp_rs::Error::StaleHandle));
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let before = tokio::time::Instant::now();
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(before.elapsed(), Duration::ZERO);
        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "failed a retry=false", "done"]
        );
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        let report = outcome.unwrap();
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.copied, 0);
    }

    #[tokio::test]
    async fn a_length_mismatch_fails_that_file_and_the_next_one_still_copies() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES), ("b", b"bb")]);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 12, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(
            without_progress(&events),
            [
                "started a 12 from 0",
                "failed a retry=false",
                "started b 2 from 0",
                "finished b 2",
                "done",
            ]
        );
        let report = outcome.unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(report.bytes, 2);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].error.contains("length mismatch"));
        assert_eq!(dest.file("b").unwrap(), b"bb");
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_during_a_backoff_interrupts_within_the_poll_interval() {
        let cancel = CancelToken::new();
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)])
            .failing_reads(1, || Error::Mtp(mtp_rs::Error::Timeout));
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let before = tokio::time::Instant::now();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_POLL).await;
            canceller.cancel();
        });
        let (outcome, events) = run_plan(&plan, &source, &dest, &cancel).await;

        assert!(
            before.elapsed() <= CANCEL_POLL * 2,
            "{:?}",
            before.elapsed()
        );
        assert_eq!(
            without_progress(&events),
            [
                "started a 10 from 0",
                "failed a retry=true",
                "interrupted 1",
                "done",
            ]
        );
        let report = outcome.unwrap();
        assert!(report.interrupted);
        assert_eq!(report.copied, 0);
        assert!(report.failed.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_retryable_error_that_lands_with_a_cancel_is_an_interruption_not_a_retry() {
        let cancel = CancelToken::new();
        let source = FakeEndpoint {
            cancel_on_failure: Some(cancel.clone()),
            ..FakeEndpoint::with_files(&[("a", TEN_BYTES)])
                .failing_reads(1, || Error::Mtp(mtp_rs::Error::Timeout))
        };
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let before = tokio::time::Instant::now();
        let (outcome, events) = run_plan(&plan, &source, &dest, &cancel).await;

        assert_eq!(before.elapsed(), Duration::ZERO);
        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "interrupted 1", "done"]
        );
        let report = outcome.unwrap();
        assert!(report.interrupted);
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn a_token_cancelled_before_the_first_action_interrupts_at_once() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb")]);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[mkdir("d"), copy("a", 2, 0), skip("s"), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &cancel).await;

        assert_eq!(without_progress(&events), ["interrupted 2", "done"]);
        let report = outcome.unwrap();
        assert!(report.interrupted);
        assert_eq!(report.copied, 0);
        assert!(dest.dirs.lock().unwrap().is_empty());
        assert!(dest.file("a").is_none());
    }

    #[tokio::test]
    async fn cancelling_between_files_interrupts_with_the_copies_left() {
        let cancel = CancelToken::new();
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb"), ("c", b"cc")]);
        let dest = FakeEndpoint {
            cancel_after_write: Some(cancel.clone()),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0), copy("c", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &cancel).await;

        assert_eq!(
            without_progress(&events),
            [
                "started a 2 from 0",
                "finished a 2",
                "interrupted 2",
                "done"
            ]
        );
        let report = outcome.unwrap();
        assert!(report.interrupted);
        assert_eq!(report.copied, 1);
        assert!(final_report(&events).interrupted);
        assert!(dest.file("b").is_none());
    }

    #[tokio::test]
    async fn a_stream_cancelled_mid_file_keeps_the_partial_and_counts_that_file_as_remaining() {
        let source = FakeEndpoint {
            fail_after_chunks: Some((1, || Error::Cancelled)),
            ..FakeEndpoint::with_files(&[("a", TEN_BYTES)])
        };
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "interrupted 1", "done"]
        );
        assert_eq!(dest.file("a").unwrap(), &TEN_BYTES[..TEST_CHUNK]);
        let report = outcome.unwrap();
        assert!(report.interrupted);
        assert_eq!(report.copied, 0);
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn a_stream_error_mid_file_fails_that_file_without_retry_and_keeps_the_partial() {
        let source = FakeEndpoint {
            fail_after_chunks: Some((1, || Error::Mtp(mtp_rs::Error::Timeout))),
            ..FakeEndpoint::with_files(&[("a", TEN_BYTES)])
        };
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "failed a retry=false", "done"]
        );
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        assert_eq!(dest.file("a").unwrap(), &TEN_BYTES[..TEST_CHUNK]);
        let report = outcome.unwrap();
        assert_eq!(report.copied, 0);
        assert_eq!(failed_paths(&report), ["a"]);
        assert!(!report.interrupted);
    }

    #[tokio::test]
    async fn a_disconnect_mid_stream_aborts_the_batch_and_keeps_the_partial() {
        let source = FakeEndpoint {
            fail_after_chunks: Some((1, || Error::Disconnected)),
            ..FakeEndpoint::with_files(&[("a", TEN_BYTES), ("b", b"bb")])
        };
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(matches!(outcome, Err(Error::Disconnected)), "{outcome:?}");
        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "aborted 2"]
        );
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        assert_eq!(dest.file("a").unwrap(), &TEN_BYTES[..TEST_CHUNK]);
        assert!(dest.file("b").is_none());
        let report = final_report(&events);
        assert_eq!(report.copied, 0);
        assert_eq!(failed_paths(report), ["a"]);
        assert!(!report.interrupted);
    }

    #[tokio::test]
    async fn a_disconnect_on_open_aborts_the_batch_with_the_totals_so_far() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb"), ("c", b"cc")])
            .failing_reads(1, || Error::Disconnected);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0), copy("c", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(matches!(outcome, Err(Error::Disconnected)), "{outcome:?}");
        assert_eq!(
            without_progress(&events),
            ["started a 2 from 0", "aborted 3"]
        );
        let report = final_report(&events);
        assert_eq!(report.copied, 0);
        assert_eq!(failed_paths(report), ["a"]);
        assert!(report.failed[0].error.contains("disconnected"));
        assert!(!report.interrupted);
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn a_disconnect_while_writing_aborts_without_touching_the_next_file() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb")]);
        let dest = FakeEndpoint {
            write_failure: Some(|| Error::Disconnected),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(matches!(outcome, Err(Error::Disconnected)), "{outcome:?}");
        assert_eq!(
            without_progress(&events),
            ["started a 2 from 0", "aborted 2"]
        );
        let report = final_report(&events);
        assert_eq!(report.copied, 0);
        assert_eq!(failed_paths(report), ["a"]);
        assert!(!report.interrupted);
        assert!(dest.file("b").is_none());
    }

    #[tokio::test]
    async fn a_full_destination_aborts_the_batch_without_touching_the_next_file() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb")]);
        let dest = FakeEndpoint {
            write_failure: Some(|| Error::Io(io::Error::from(io::ErrorKind::StorageFull))),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(
            matches!(&outcome, Err(Error::Io(e)) if e.kind() == io::ErrorKind::StorageFull),
            "{outcome:?}"
        );
        assert_eq!(
            without_progress(&events),
            ["started a 2 from 0", "aborted 2"]
        );
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        assert!(dest.file("b").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_error_while_writing_is_recorded_without_retry() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb")]);
        let dest = FakeEndpoint {
            write_failure: Some(|| Error::Mtp(mtp_rs::Error::Timeout)),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0)]);
        let before = tokio::time::Instant::now();
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(before.elapsed(), Duration::ZERO);
        assert_eq!(
            without_progress(&events),
            [
                "started a 2 from 0",
                "failed a retry=false",
                "started b 2 from 0",
                "failed b retry=false",
                "done",
            ]
        );
        let report = outcome.unwrap();
        assert_eq!(report.copied, 0);
        assert_eq!(failed_paths(&report), ["a", "b"]);
        assert!(!report.interrupted);
    }

    #[test]
    fn only_device_level_and_unwritable_destination_errors_abort_the_batch() {
        let local_io = |kind: io::ErrorKind| Error::LocalIo {
            op: "write",
            path: "/x".into(),
            source: io::Error::from(kind),
        };
        let aborting = [
            Error::Disconnected,
            Error::NoDevice,
            Error::PermissionDenied,
            Error::ExclusiveAccess { holder: None },
            Error::Mtp(mtp_rs::Error::DeviceReset),
            Error::Io(io::Error::from(io::ErrorKind::StorageFull)),
            Error::Io(io::Error::from(io::ErrorKind::ReadOnlyFilesystem)),
            local_io(io::ErrorKind::StorageFull),
            local_io(io::ErrorKind::ReadOnlyFilesystem),
        ];
        for error in &aborting {
            assert!(aborts_batch(error), "{error:?}");
        }
        let recorded = [
            Error::Cancelled,
            Error::Mtp(mtp_rs::Error::Timeout),
            Error::Mtp(mtp_rs::Error::StaleHandle),
            Error::SourceVanished(rel("a")),
            Error::Io(io::Error::other("disk")),
            Error::Io(io::Error::from(io::ErrorKind::NotFound)),
            local_io(io::ErrorKind::IsADirectory),
        ];
        for error in &recorded {
            assert!(!aborts_batch(error), "{error:?}");
        }
    }

    #[tokio::test]
    async fn a_failed_mkdir_is_recorded_and_the_batch_goes_on() {
        let source = FakeEndpoint::with_files(&[("a", b"aa")]);
        let dest = FakeEndpoint {
            mkdir_fails: true,
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[mkdir("d"), copy("a", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(
            without_progress(&events),
            [
                "failed d retry=false",
                "started a 2 from 0",
                "finished a 2",
                "done",
            ]
        );
        let report = outcome.unwrap();
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, rel("d"));
        assert_eq!(report.copied, 1);
    }

    /// Runs on the single-threaded test runtime, where the drain task only gets the CPU when
    /// the executor awaits: the fake pump never does, so the one-slot channel stays full for it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_full_events_channel_drops_progress_without_stalling_the_copy() {
        let (tx, rx) = mpsc::channel(1);
        let drain = tokio::spawn(collect(rx));
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)]);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let outcome = Executor::new(CancelToken::new(), tx)
            .run(&plan, &source, &dest)
            .await;
        let events = drain.await.unwrap();

        let report = outcome.unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(report.bytes, 10);
        assert_eq!(dest.file("a").unwrap(), TEN_BYTES);
        let chunks = TEN_BYTES.len().div_ceil(TEST_CHUNK);
        assert!(progress_of(&events, "a").len() < chunks);
        assert_eq!(
            without_progress(&events),
            ["started a 10 from 0", "finished a 10", "done"]
        );
    }

    #[tokio::test]
    async fn a_dropped_receiver_does_not_fail_the_run() {
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        drop(rx);
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)]);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0), skip("b")]);
        let report = Executor::new(CancelToken::new(), tx)
            .run(&plan, &source, &dest)
            .await
            .unwrap();

        assert_eq!(report.copied, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(dest.file("a").unwrap(), TEN_BYTES);
    }

    #[tokio::test]
    async fn resume_from_is_forwarded_to_both_sides_and_only_new_bytes_are_counted() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)]);
        let dest = FakeEndpoint::with_files(&[("a", &TEN_BYTES[..4])]);
        let plan = plan_of(&[copy("a", 10, 4)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert_eq!(*source.read_offsets.lock().unwrap(), vec![4]);
        assert_eq!(dest.file("a").unwrap(), TEN_BYTES);
        assert_eq!(
            without_progress(&events),
            ["started a 10 from 4", "finished a 6", "done"]
        );
        let progress = progress_of(&events, "a");
        assert!(progress.iter().all(|bytes| *bytes > 4), "{progress:?}");
        assert_eq!(progress.last(), Some(&10));
        let report = outcome.unwrap();
        assert_eq!(report.bytes, 6);
        assert_eq!(report.copied, 1);
    }

    #[tokio::test]
    async fn a_stale_planned_offset_is_probed_and_streams_from_zero() {
        let source = FakeEndpoint::with_files(&[("f.bin", b"abcdef")]);
        let dest = FakeEndpoint {
            resume_probe: Some(0),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("f.bin", 6, 4)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        assert_eq!(dest.file("f.bin").unwrap(), b"abcdef");
        assert_eq!(
            without_progress(&events),
            ["started f.bin 6 from 0", "finished f.bin 6", "done"]
        );
    }

    #[tokio::test]
    async fn a_stale_plan_against_a_real_destination_lands_the_whole_file() {
        use crate::{internal::local::LocalEndpoint, test_support::identity};
        let source = FakeEndpoint::with_files(&[("f.bin", b"abcdef")]);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.bin.mtpx-part"), b"xx").unwrap();
        let dest = LocalEndpoint::new(dir.path(), identity("ZY22", "Internal"));
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        let report = Executor::new(CancelToken::new(), tx)
            .run(&plan_of(&[copy("f.bin", 6, 4)]), &source, &dest)
            .await
            .unwrap();
        drop(rx);
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
        assert_eq!(report.copied, 1);
        assert_eq!(report.bytes, 6);
        assert_eq!(std::fs::read(dir.path().join("f.bin")).unwrap(), b"abcdef");
        assert!(!dir.path().join("f.bin.mtpx-part").exists());
    }
}
