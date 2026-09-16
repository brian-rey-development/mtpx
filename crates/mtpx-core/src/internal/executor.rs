//! Runs a plan: streams every `Copy` from source to destination, retries transient reads,
//! and reports progress, failures and cancellation as events.

use crate::{
    entry::ModifiedTime,
    error::{Error, Result},
    event::{ProgressEvent, Report},
    internal::endpoint::{ByteStream, Endpoint, WriteRequest},
    path::RelPath,
    plan::{Action, Plan, SkipReason},
};
use futures::StreamExt;
use mtp_rs::CancelToken;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};

/// How many times a read that failed with a transient MTP error is attempted again.
pub const MAX_READ_RETRIES: u32 = 3;
/// Pause before the first retry; it doubles on every further attempt.
pub const RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Longest stretch a backoff sleeps before looking at the cancel token again.
pub const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Runs a plan against two endpoints, reporting through `events`.
///
/// Every event except `FileProgress` is awaited, so the receiver must be drained while the
/// run is in flight; `FileProgress` is best-effort and dropped when the channel is full.
pub struct Executor {
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
    /// Binds a cancellation token and an event channel for the runs that follow.
    pub const fn new(cancel: CancelToken, events: mpsc::Sender<ProgressEvent>) -> Self {
        Self { cancel, events }
    }

    /// Executes every action in order and returns the totals.
    ///
    /// # Errors
    /// Only the errors that end the whole batch: the device disconnected, vanished, was reset,
    /// or refused access. A cancelled run is `Ok` with `report.interrupted` set; a failed file
    /// lands in `report.failed` and the run continues.
    pub async fn run<S: Endpoint, D: Endpoint>(
        &self,
        plan: &Plan,
        source: &S,
        dest: &D,
    ) -> Result<Report> {
        let since = Instant::now();
        let mut report = Report::default();
        let actions = plan.actions();
        for (index, action) in actions.iter().enumerate() {
            match self.step(action, source, dest, &mut report).await {
                Ok(()) => {}
                Err(Stop::Interrupted) => {
                    let remaining = remaining_copies(&actions[index..]);
                    return Ok(self.interrupt(report, since, remaining).await);
                }
                Err(Stop::Aborted(error)) => {
                    self.finish(report, since).await;
                    return Err(error);
                }
            }
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
        self.emit(ProgressEvent::FileStarted {
            path: path.clone(),
            size: request.expected_size,
            resume_from: request.resume_from,
        })
        .await;
        match self.copy_one(source, dest, request).await {
            Ok(bytes) => {
                report.copied += 1;
                report.bytes += bytes;
                Ok(())
            }
            Err(error) => self.fail(report, &path, error).await,
        }
    }

    /// Streams one file and returns the bytes that flowed this run, excluding any resumed prefix.
    async fn copy_one<S: Endpoint, D: Endpoint>(
        &self,
        source: &S,
        dest: &D,
        request: WriteRequest,
    ) -> Result<u64> {
        let since = Instant::now();
        let raw = self
            .read_with_retry(source, &request.path, request.resume_from)
            .await?;
        let path = request.path.clone();
        let (stream, streamed) = self.progress_stream(raw, path.clone(), request.resume_from);
        dest.write(request, stream).await?;
        let bytes = streamed.load(Ordering::Relaxed);
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
    /// after the stream has started are not retried in M1: the file is reported failed and the
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

    async fn report_and_back_off(
        &self,
        path: &RelPath,
        error: &mtp_rs::Error,
        backoff: Duration,
    ) -> Result<()> {
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

    /// Counts every chunk that passes and reports it. `try_send` keeps a slow observer from
    /// stalling the pump: a full channel drops the progress event, and the next one catches up.
    fn progress_stream(
        &self,
        stream: ByteStream,
        path: RelPath,
        resume_from: u64,
    ) -> (ByteStream, Arc<AtomicU64>) {
        let streamed = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&streamed);
        let events = self.events.clone();
        let reported = stream.inspect(move |item| {
            let Ok(chunk) = item else { return };
            let len = chunk.len() as u64;
            let so_far = counter.fetch_add(len, Ordering::Relaxed) + len;
            let _ = events.try_send(ProgressEvent::FileProgress {
                path: path.clone(),
                bytes: resume_from + so_far,
            });
        });
        (Box::pin(reported), streamed)
    }

    async fn fail(&self, report: &mut Report, path: &RelPath, error: Error) -> StepResult {
        if matches!(error, Error::Cancelled) {
            return Err(Stop::Interrupted);
        }
        if aborts_batch(&error) {
            return Err(Stop::Aborted(error));
        }
        let message = error.to_string();
        self.emit(ProgressEvent::FileFailed {
            path: path.clone(),
            error: message.clone(),
            will_retry: false,
        })
        .await;
        report.failed.push((path.clone(), message));
        Ok(())
    }

    async fn interrupt(&self, mut report: Report, since: Instant, remaining_files: u64) -> Report {
        self.emit(ProgressEvent::Interrupted { remaining_files })
            .await;
        report.interrupted = true;
        self.finish(report, since).await
    }

    async fn finish(&self, mut report: Report, since: Instant) -> Report {
        report.elapsed = since.elapsed();
        self.emit(ProgressEvent::Finished {
            report: report.clone(),
        })
        .await;
        report
    }

    /// A transfer without an observer still completes, so a closed receiver is not an error.
    async fn emit(&self, event: ProgressEvent) {
        let _ = self.events.send(event).await;
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

/// The errors after which no further endpoint call can succeed.
const fn aborts_batch(error: &Error) -> bool {
    matches!(
        error,
        Error::Disconnected
            | Error::NoDevice
            | Error::PermissionDenied
            | Error::ExclusiveAccess { .. }
            | Error::Mtp(mtp_rs::Error::DeviceReset)
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
    use crate::{
        internal::endpoint::{ScanResult, WriteOutcome},
        plan::CopyReason,
    };
    use bytes::Bytes;
    use futures::StreamExt;
    use std::{
        collections::HashMap,
        io,
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
        cancel_after_chunks: Option<usize>,
        cancel_after_write: Option<CancelToken>,
        write_failure: Option<fn() -> Error>,
        mkdir_fails: bool,
    }

    impl Default for FakeEndpoint {
        fn default() -> Self {
            Self {
                files: Mutex::default(),
                dirs: Mutex::default(),
                read_offsets: Mutex::default(),
                read_failures: AtomicUsize::new(0),
                failure: || Error::Disconnected,
                cancel_after_chunks: None,
                cancel_after_write: None,
                write_failure: None,
                mkdir_fails: false,
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
            self.read_failures.store(count, Ordering::SeqCst);
            Self { failure, ..self }
        }

        fn file(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(&rel(path)).cloned()
        }

        fn take_read_failure(&self) -> bool {
            let remaining = self.read_failures.load(Ordering::SeqCst);
            if remaining == 0 {
                return false;
            }
            self.read_failures.store(remaining - 1, Ordering::SeqCst);
            true
        }

        fn chunks_from(&self, data: &[u8], offset: u64) -> Vec<Result<Bytes>> {
            let tail = &data[usize::try_from(offset).unwrap()..];
            let mut items: Vec<Result<Bytes>> = tail
                .chunks(TEST_CHUNK)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect();
            if let Some(after) = self.cancel_after_chunks {
                items.truncate(after);
                items.push(Err(Error::Cancelled));
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
            })
        }

        async fn read(
            &self,
            path: &RelPath,
            offset: u64,
            _cancel: &CancelToken,
        ) -> Result<ByteStream> {
            self.read_offsets.lock().unwrap().push(offset);
            if self.take_read_failure() {
                return Err((self.failure)());
            }
            let data = self
                .file(&path.to_string())
                .ok_or_else(|| Error::SourceVanished(path.clone()))?;
            Ok(Box::pin(futures::stream::iter(
                self.chunks_from(&data, offset),
            )))
        }

        async fn write(
            &self,
            request: WriteRequest,
            mut input: ByteStream,
        ) -> Result<WriteOutcome> {
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
            Ok(WriteOutcome { bytes: actual })
        }

        async fn mkdir(&self, path: &RelPath) -> Result<()> {
            if self.mkdir_fails {
                return Err(Error::Io(io::Error::other("read-only destination")));
            }
            self.dirs.lock().unwrap().push(path.clone());
            Ok(())
        }
    }

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
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
            Some(ProgressEvent::Finished { report }) => report,
            other => panic!("last event is not Finished: {other:?}"),
        }
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
        assert_eq!(report.failed[0].0, rel("a"));
        assert!(dest.file("a").is_none());
        assert_eq!(dest.file("b").unwrap(), b"bb");
    }

    #[tokio::test]
    async fn a_non_retryable_read_error_fails_the_file_at_once() {
        let source = FakeEndpoint::with_files(&[("a", TEN_BYTES)])
            .failing_reads(1, || Error::Mtp(mtp_rs::Error::StaleHandle));
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 10, 0)]);
        let before = tokio::time::Instant::now();
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(before.elapsed() < RETRY_BACKOFF);
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
        assert!(report.failed[0].1.contains("length mismatch"));
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

        assert!(before.elapsed() < RETRY_BACKOFF, "{:?}", before.elapsed());
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
            cancel_after_chunks: Some(1),
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
    async fn a_disconnect_ends_the_batch_with_a_partial_report() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb"), ("c", b"cc")])
            .failing_reads(1, || Error::Disconnected);
        let dest = FakeEndpoint::default();
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0), copy("c", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(matches!(outcome, Err(Error::Disconnected)), "{outcome:?}");
        assert_eq!(without_progress(&events), ["started a 2 from 0", "done"]);
        let report = final_report(&events);
        assert_eq!(report.copied, 0);
        assert!(!report.interrupted);
        assert_eq!(*source.read_offsets.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn a_disconnect_while_writing_ends_the_batch_after_finished() {
        let source = FakeEndpoint::with_files(&[("a", b"aa"), ("b", b"bb")]);
        let dest = FakeEndpoint {
            write_failure: Some(|| Error::Disconnected),
            ..FakeEndpoint::default()
        };
        let plan = plan_of(&[copy("a", 2, 0), copy("b", 2, 0)]);
        let (outcome, events) = run_plan(&plan, &source, &dest, &CancelToken::new()).await;

        assert!(matches!(outcome, Err(Error::Disconnected)), "{outcome:?}");
        assert_eq!(without_progress(&events), ["started a 2 from 0", "done"]);
        let report = final_report(&events);
        assert_eq!(report.copied, 0);
        assert!(report.failed.is_empty());
        assert!(!report.interrupted);
        assert!(dest.file("b").is_none());
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
        assert_eq!(report.failed[0].0, rel("d"));
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
}
