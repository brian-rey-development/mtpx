//! Planning a pull: scan both sides, diff them, and bind the plan to the endpoints it came from.

use super::Device;
use crate::{
    device_path::DevicePath,
    entry::Snapshot,
    error::Result,
    event::{Hint, ProgressEvent, Report, Side},
    internal::{
        endpoint::{Endpoint, ScanResult},
        executor::Executor,
        local::LocalEndpoint,
        mtp::MtpEndpoint,
    },
    options::TransferOptions,
    plan::Plan,
    planner,
};
use mtp_rs::{CancelToken, UsbSpeed};
use std::{fmt, marker::PhantomData, path::Path, sync::Arc};
use tokio::sync::mpsc;

/// Plans that move more than this over a USB 2.0 or slower link get a `SlowLink` hint.
const SLOW_LINK_BYTES: u64 = 1 << 30;

impl Device {
    /// Scans `remote` and `local`, then decides what a pull would do without touching either side.
    ///
    /// `remote` may be a directory or a single file. A directory's contents land under `local`;
    /// a file lands directly under `local` with its own name.
    ///
    /// Emits `ScanStarted`, `ScanProgress` and `ScanFinished` for each side, then `PlanReady`.
    ///
    /// # Errors
    /// `Conflicts` under `ConflictPolicy::Fail`, the path and storage errors of [`Device::ls`],
    /// `Cancelled` once the token is set.
    pub async fn plan_pull<'d>(
        &'d self,
        remote: &DevicePath,
        local: &Path,
        opts: &TransferOptions,
        cancel: &CancelToken,
        events: &mpsc::Sender<ProgressEvent>,
    ) -> Result<PullJob<'d>> {
        let source = self.endpoint(remote).await?;
        let dest = LocalEndpoint::new(local, source.identity().clone());
        let (scanned_source, scanned_dest) = tokio::try_join!(
            scan_side(&source, Side::Source, cancel, events),
            scan_side(&dest, Side::Dest, cancel, events),
        )?;
        let plan = planner::plan(
            &scanned_source.snapshot,
            &scanned_dest.snapshot,
            &scanned_dest.partials,
            opts,
        )?;
        let hints = hints(self.summary().speed, &plan, &scanned_source.snapshot);
        let summary = plan.summary();
        emit(events, ProgressEvent::PlanReady { summary, hints }).await;
        Ok(PullJob::bind(plan, source, dest))
    }
}

/// A planned pull, bound to the endpoints that produced the plan. It borrows the device so the
/// session cannot be closed while a job exists.
pub struct PullJob<'d> {
    plan: Plan,
    source: MtpEndpoint,
    dest: LocalEndpoint,
    device: PhantomData<&'d Device>,
}

impl PullJob<'_> {
    const fn bind(plan: Plan, source: MtpEndpoint, dest: LocalEndpoint) -> Self {
        Self {
            plan,
            source,
            dest,
            device: PhantomData,
        }
    }

    /// What [`PullJob::run`] will do, in order.
    #[must_use]
    pub const fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Copies every planned file, reporting per-file events and a final `Finished` through `events`.
    ///
    /// # Errors
    /// Only the errors that end the whole batch: `Disconnected`, `NoDevice`, `PermissionDenied`,
    /// `ExclusiveAccess`, or a device reset. A cancelled run is `Ok` with `report.interrupted`
    /// set, and a file that fails after retries lands in `report.failed`.
    pub async fn run(
        self,
        cancel: &CancelToken,
        events: &mpsc::Sender<ProgressEvent>,
    ) -> Result<Report> {
        Executor::new(cancel.clone(), events.clone())
            .run(&self.plan, &self.source, &self.dest)
            .await
    }
}

impl fmt::Debug for PullJob<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullJob")
            .field("summary", &self.plan.summary())
            .field("source", &self.source.label())
            .field("dest", &self.dest.label())
            .finish_non_exhaustive()
    }
}

/// Scans one side, bracketing it with `ScanStarted` and `ScanFinished` and reporting the running
/// count in between. Progress uses `try_send` so a slow observer never stalls the walk.
async fn scan_side<E: Endpoint>(
    endpoint: &E,
    side: Side,
    cancel: &CancelToken,
    events: &mpsc::Sender<ProgressEvent>,
) -> Result<ScanResult> {
    emit(events, ProgressEvent::ScanStarted { side }).await;
    let progress = events.clone();
    let on_found = Arc::new(move |found| {
        let _ = progress.try_send(ProgressEvent::ScanProgress { side, found });
    });
    let scanned = endpoint.scan(cancel, on_found).await?;
    let entries = scanned.snapshot.len() as u64;
    let skipped = scanned.snapshot.skipped().len() as u64;
    let finished = ProgressEvent::ScanFinished {
        side,
        entries,
        skipped,
    };
    emit(events, finished).await;
    Ok(scanned)
}

fn hints(speed: Option<UsbSpeed>, plan: &Plan, source: &Snapshot) -> Vec<Hint> {
    let mut hints = Vec::new();
    let bytes = plan.summary().bytes_to_copy;
    if is_slow(speed) && bytes > SLOW_LINK_BYTES {
        hints.push(Hint::SlowLink { bytes });
    }
    let count = source.skipped().len();
    if count > 0 {
        hints.push(Hint::DeviceSkippedObjects { count });
    }
    hints
}

const fn is_slow(speed: Option<UsbSpeed>) -> bool {
    matches!(speed, Some(UsbSpeed::Low | UsbSpeed::Full | UsbSpeed::High))
}

/// A plan without an observer still completes, so a closed receiver is not an error.
async fn emit(events: &mpsc::Sender<ProgressEvent>, event: ProgressEvent) {
    let _ = events.send(event).await;
}

#[cfg(test)]
mod hint_tests {
    #![allow(clippy::unwrap_used)]

    use super::{SLOW_LINK_BYTES, hints};
    use crate::{
        entry::{SkippedEntry, Snapshot},
        event::Hint,
        path::RelPath,
        plan::{Action, CopyReason, Plan},
    };
    use mtp_rs::UsbSpeed;

    fn plan_moving(bytes: u64) -> Plan {
        Plan::new(vec![Action::Copy {
            path: RelPath::new(["big.bin"]).unwrap(),
            size: bytes,
            modified: None,
            resume_from: 0,
            reason: CopyReason::New,
        }])
    }

    #[test]
    fn slow_link_needs_a_usb2_or_slower_link_and_more_than_the_threshold() {
        let empty = Snapshot::new("/", vec![], vec![]);
        let big = plan_moving(SLOW_LINK_BYTES + 1);
        for speed in [UsbSpeed::Low, UsbSpeed::Full, UsbSpeed::High] {
            let expected = vec![Hint::SlowLink {
                bytes: SLOW_LINK_BYTES + 1,
            }];
            assert_eq!(hints(Some(speed), &big, &empty), expected, "{speed:?}");
        }
        for speed in [Some(UsbSpeed::Super), Some(UsbSpeed::SuperPlus), None] {
            assert!(hints(speed, &big, &empty).is_empty(), "{speed:?}");
        }
        let at_threshold = plan_moving(SLOW_LINK_BYTES);
        assert!(hints(Some(UsbSpeed::High), &at_threshold, &empty).is_empty());
    }

    #[test]
    fn skipped_objects_on_the_source_are_reported_with_their_count() {
        let refused = SkippedEntry {
            parent: RelPath::root(),
            reason: "refused".into(),
        };
        let source = Snapshot::new("/", vec![], vec![refused.clone(), refused]);
        assert_eq!(
            hints(None, &plan_moving(1), &source),
            vec![Hint::DeviceSkippedObjects { count: 2 }]
        );
    }
}

#[cfg(all(test, feature = "virtual-device"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::PullJob;
    use crate::{
        device::test_support::*,
        error::{Error, Result},
        event::{ProgressEvent, Side},
        internal::mtp::{
            DOWNLOAD_WINDOW, PUMP_DEPTH,
            test_support::{pseudo_random, rel, seed_tree},
        },
        options::TransferOptions,
        plan::{Action, CopyReason, SkipReason},
    };
    use mtp_rs::CancelToken;
    use std::{fs, path::Path};
    use tokio::sync::mpsc::{Receiver, Sender};

    const CAMERA: &str = "/DCIM/Camera";
    const CAMERA_FILE: &str = "/DCIM/Camera/a.jpg";
    /// More windows than the pump can hold buffered plus in flight once the token is set,
    /// so the run cannot finish before the cancel is observed.
    const RESUME_FILE: usize = (PUMP_DEPTH + 4) * DOWNLOAD_WINDOW as usize;

    async fn plan_remote<'d>(
        fixture: &'d Fixture,
        remote: &str,
        local: &Path,
        opts: &TransferOptions,
        events: &Sender<ProgressEvent>,
    ) -> Result<PullJob<'d>> {
        let cancel = CancelToken::new();
        fixture
            .device
            .plan_pull(&device_path(remote), local, opts, &cancel, events)
            .await
    }

    async fn plan_camera<'d>(
        fixture: &'d Fixture,
        local: &Path,
        opts: &TransferOptions,
        events: &Sender<ProgressEvent>,
    ) -> Result<PullJob<'d>> {
        plan_remote(fixture, CAMERA, local, opts, events).await
    }

    fn local_names(local: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(local)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn count(events: &[ProgressEvent], matches: impl Fn(&ProgressEvent) -> bool) -> usize {
        events.iter().filter(|event| matches(event)).count()
    }

    fn scan_started(events: &[ProgressEvent], side: Side) -> usize {
        count(
            events,
            |e| matches!(e, ProgressEvent::ScanStarted { side: s } if *s == side),
        )
    }

    fn copy(path: &str, size: u64, resume_from: u64, reason: CopyReason) -> Action {
        Action::Copy {
            path: rel(path),
            size,
            modified: None,
            resume_from,
            reason,
        }
    }

    async fn forward_and_cancel_on_progress(
        mut rx: Receiver<ProgressEvent>,
        tx: Sender<ProgressEvent>,
        cancel: CancelToken,
    ) {
        while let Some(event) = rx.recv().await {
            if matches!(event, ProgressEvent::FileProgress { .. }) {
                cancel.cancel();
            }
            let _ = tx.send(event).await;
        }
    }

    #[tokio::test]
    async fn plan_pull_scans_both_sides_and_run_copies_every_byte() {
        let fixture = open_device("pull").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        let (tx, mut rx) = events();
        let job = plan_camera(&fixture, local.path(), &TransferOptions::pull(), &tx)
            .await
            .unwrap();
        let planned = drain(&mut rx);
        for side in [Side::Source, Side::Dest] {
            assert_eq!(scan_started(&planned, side), 1, "{side:?}");
        }
        assert!(planned.contains(&ProgressEvent::ScanProgress {
            side: Side::Source,
            found: 3
        }));
        assert!(planned.contains(&ProgressEvent::ScanFinished {
            side: Side::Source,
            entries: 3,
            skipped: 0
        }));
        assert!(planned.contains(&ProgressEvent::ScanFinished {
            side: Side::Dest,
            entries: 0,
            skipped: 0
        }));
        let Some(ProgressEvent::PlanReady { summary, hints }) = planned.last() else {
            panic!("{planned:?}");
        };
        assert_eq!(summary.files_to_copy, 2);
        assert_eq!(summary.bytes_to_copy, 5);
        assert!(hints.is_empty(), "{hints:?}");
        assert_eq!(*summary, job.plan().summary());
        assert_eq!(
            job.plan().actions(),
            [
                Action::Mkdir { path: rel("sub") },
                copy("a.jpg", 3, 0, CopyReason::New),
                copy("sub/b.jpg", 2, 0, CopyReason::New),
            ]
        );
        let report = job.run(&CancelToken::new(), &tx).await.unwrap();
        assert_eq!(report.copied, 2);
        assert_eq!(report.bytes, 5);
        assert!(report.failed.is_empty());
        assert!(!report.interrupted);
        assert_eq!(fs::read(local.path().join("a.jpg")).unwrap(), b"aaa");
        assert_eq!(fs::read(local.path().join("sub/b.jpg")).unwrap(), b"bb");
        let ran = drain(&mut rx);
        assert!(
            matches!(ran.last(), Some(ProgressEvent::Finished { report: r }) if *r == report),
            "{ran:?}"
        );
    }

    #[tokio::test]
    async fn a_second_plan_after_a_completed_pull_skips_every_file_as_identical() {
        let fixture = open_device("pull-again").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        let (tx, _rx) = events();
        let opts = TransferOptions::pull();
        let first = plan_camera(&fixture, local.path(), &opts, &tx)
            .await
            .unwrap();
        first.run(&CancelToken::new(), &tx).await.unwrap();
        let second = plan_camera(&fixture, local.path(), &opts, &tx)
            .await
            .unwrap();
        assert_eq!(second.plan().summary().files_to_copy, 0);
        assert_eq!(second.plan().summary().to_skip, 2);
        assert!(second.plan().actions().iter().all(|action| matches!(
            action,
            Action::Skip {
                reason: SkipReason::Identical,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn pull_of_a_single_file_lands_it_under_local_with_its_own_name() {
        let fixture = open_device("pull-file").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        let (tx, _rx) = events();
        let opts = TransferOptions::pull();
        let job = plan_remote(&fixture, CAMERA_FILE, local.path(), &opts, &tx)
            .await
            .unwrap();
        assert_eq!(job.plan().actions(), [copy("a.jpg", 3, 0, CopyReason::New)]);
        let report = job.run(&CancelToken::new(), &tx).await.unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(report.bytes, 3);
        assert!(report.failed.is_empty());
        assert_eq!(fs::read(local.path().join("a.jpg")).unwrap(), b"aaa");
        assert_eq!(local_names(local.path()), ["a.jpg"]);
        let second = plan_remote(&fixture, CAMERA_FILE, local.path(), &opts, &tx)
            .await
            .unwrap();
        assert_eq!(
            second.plan().actions(),
            [Action::Skip {
                path: rel("a.jpg"),
                reason: SkipReason::Identical
            }]
        );
    }

    #[tokio::test]
    async fn pull_of_a_single_file_refuses_a_different_local_file() {
        let fixture = open_device("pull-file-conflict").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        fs::write(local.path().join("a.jpg"), b"aaaa").unwrap();
        let (tx, _rx) = events();
        let err = plan_remote(
            &fixture,
            CAMERA_FILE,
            local.path(),
            &TransferOptions::pull(),
            &tx,
        )
        .await
        .unwrap_err();
        let Error::Conflicts(paths) = err else {
            panic!("{err:?}");
        };
        assert_eq!(paths, vec![rel("a.jpg")]);
        assert_eq!(fs::read(local.path().join("a.jpg")).unwrap(), b"aaaa");
    }

    #[tokio::test]
    async fn pull_refuses_a_local_file_of_another_size_and_touches_nothing() {
        let fixture = open_device("pull-conflict").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        fs::write(local.path().join("a.jpg"), b"aaaa").unwrap();
        let (tx, mut rx) = events();
        let err = plan_camera(&fixture, local.path(), &TransferOptions::pull(), &tx)
            .await
            .unwrap_err();
        let Error::Conflicts(paths) = err else {
            panic!("{err:?}");
        };
        assert_eq!(paths, vec![rel("a.jpg")]);
        assert_eq!(fs::read(local.path().join("a.jpg")).unwrap(), b"aaaa");
        assert!(!local.path().join("sub").exists());
        let seen = drain(&mut rx);
        assert_eq!(scan_started(&seen, Side::Source), 1);
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, ProgressEvent::PlanReady { .. }))
        );
    }

    #[tokio::test]
    async fn sync_overwrites_a_local_file_of_another_size() {
        let fixture = open_device("sync-overwrite").await;
        seed_tree(fixture.root());
        let local = tempfile::tempdir().unwrap();
        fs::write(local.path().join("a.jpg"), b"aaaa").unwrap();
        let (tx, _rx) = events();
        let job = plan_camera(&fixture, local.path(), &TransferOptions::sync(), &tx)
            .await
            .unwrap();
        assert!(
            job.plan()
                .actions()
                .contains(&copy("a.jpg", 3, 0, CopyReason::SizeDiffers))
        );
        let report = job.run(&CancelToken::new(), &tx).await.unwrap();
        assert_eq!(report.copied, 2);
        assert_eq!(fs::read(local.path().join("a.jpg")).unwrap(), b"aaa");
    }

    #[tokio::test]
    async fn an_interrupted_pull_leaves_a_partial_that_the_next_pull_resumes() {
        let fixture = open_device("resume").await;
        let content = pseudo_random(RESUME_FILE);
        fs::create_dir_all(fixture.root().join("DCIM/Camera")).unwrap();
        fs::write(fixture.root().join("DCIM/Camera/big.bin"), &content).unwrap();
        let local = tempfile::tempdir().unwrap();
        let part = local.path().join("big.bin.mtpx-part");
        let sidecar = local.path().join("big.bin.mtpx-part.json");
        let cancel = CancelToken::new();
        let (tx, rx) = events();
        let (forwarded_tx, mut forwarded_rx) = events();
        // Drains events concurrently because the executor blocks on a full channel, and cancels
        // on the first progress event so the interruption always lands mid-file.
        let forwarder = tokio::spawn(forward_and_cancel_on_progress(
            rx,
            forwarded_tx,
            cancel.clone(),
        ));
        let job = plan_camera(&fixture, local.path(), &TransferOptions::pull(), &tx)
            .await
            .unwrap();
        let report = job.run(&cancel, &tx).await.unwrap();
        drop(tx);
        forwarder.await.unwrap();
        assert!(report.interrupted);
        assert_eq!(report.copied, 0);
        assert!(part.exists() && sidecar.exists());
        let part_len = fs::metadata(&part).unwrap().len();
        assert!(part_len > 0 && part_len < RESUME_FILE as u64, "{part_len}");
        let seen = drain(&mut forwarded_rx);
        assert!(seen.contains(&ProgressEvent::Interrupted { remaining_files: 1 }));

        let (tx, _rx) = events();
        let job = plan_camera(&fixture, local.path(), &TransferOptions::pull(), &tx)
            .await
            .unwrap();
        assert_eq!(job.plan().summary().resumable_bytes, part_len);
        assert_eq!(
            job.plan().actions(),
            [copy(
                "big.bin",
                RESUME_FILE as u64,
                part_len,
                CopyReason::New
            )]
        );
        let report = job.run(&CancelToken::new(), &tx).await.unwrap();
        assert!(!report.interrupted);
        assert_eq!(report.copied, 1);
        assert_eq!(report.bytes, RESUME_FILE as u64 - part_len);
        assert_eq!(fs::read(local.path().join("big.bin")).unwrap(), content);
        assert!(!part.exists() && !sidecar.exists());
    }
}

#[cfg(test)]
mod send_tests {
    use super::PullJob;
    use crate::{
        device::Device, device_path::DevicePath, event::ProgressEvent, options::TransferOptions,
    };
    use mtp_rs::CancelToken;
    use std::path::Path;
    use tokio::sync::mpsc;

    fn assert_send<T: Send>(_: T) {}

    fn assert_send_sync<T: Send + Sync>() {}

    #[expect(
        dead_code,
        reason = "compile-time check: a CLI spawns these onto a multi-threaded runtime"
    )]
    fn the_public_futures_are_send(
        device: &Device,
        job: PullJob<'_>,
        remote: &DevicePath,
        local: &Path,
        opts: &TransferOptions,
        cancel: &CancelToken,
        events: &mpsc::Sender<ProgressEvent>,
    ) {
        assert_send(device.plan_pull(remote, local, opts, cancel, events));
        assert_send(device.ls(remote, true, cancel));
        assert_send(job.run(cancel, events));
        assert_send_sync::<Device>();
    }
}
