//! The public pull contract, exercised from outside the crate against a virtual phone.
//!
//! Resume and cancellation are covered deterministically in-crate (`device/pull.rs`) and are
//! deliberately not repeated here.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use common::{Pulled, VirtualPhone, collect, count, events, pseudo_random, rel, run_collecting};
use mtpx_core::{
    Action, Error, FailedFile, Hint, ProgressEvent, PullJob, RelPath, Report, Side, TransferOptions,
};
use std::{collections::BTreeMap, fs, path::Path};
use tokio::{sync::mpsc::Sender, task::JoinHandle};

const CAMERA: &str = "/DCIM/Camera";
const CAMERA_FILE: &str = "DCIM/Camera/a.jpg";
const REKEYED_FILE_LEN: usize = 64 * 1024;
const TRUNCATED_FILE_LEN: usize = 64 * 1024;
const TRUNCATED_TO: usize = 1024;
/// A directory with nothing in it, which only a `Mkdir` action can reproduce.
const EMPTY_DIR: &str = "Camera/Empty";

/// Three levels deep, one empty file, one Unicode name; paths relative to `/DCIM`. Deliberately
/// not in path order, so a sorted copy order can only come from the snapshot.
const TREE: [(&str, &[u8]); 5] = [
    ("notes.txt", b"notes"),
    ("Fotos/a\u{f1}o 2026/IMG_1.jpg", b"unicode photo"),
    ("Camera/IMG_0001.jpg", b"first photo"),
    ("Camera/empty.dat", b""),
    ("Camera/2026/01/IMG_0002.jpg", b"second photo"),
];

/// The seeded paths in snapshot order.
fn sorted_tree_paths() -> Vec<RelPath> {
    let seeded: Vec<RelPath> = TREE.iter().map(|(path, _)| rel(path)).collect();
    assert!(
        !seeded.is_sorted(),
        "the seed order must not already be sorted"
    );
    let mut sorted = seeded;
    sorted.sort();
    sorted
}

fn seed_tree(phone: &VirtualPhone) -> BTreeMap<String, Vec<u8>> {
    for (path, content) in TREE {
        phone.seed(&[(&format!("DCIM/{path}"), content)]);
    }
    fs::create_dir_all(phone.backing().join(format!("DCIM/{EMPTY_DIR}"))).unwrap();
    TREE.iter()
        .map(|(path, content)| ((*path).to_owned(), content.to_vec()))
        .collect()
}

fn scan_events(events: &[ProgressEvent], side: Side) -> (usize, usize) {
    let started = count(
        events,
        |e| matches!(e, ProgressEvent::ScanStarted { side: s } if *s == side),
    );
    let finished = count(
        events,
        |e| matches!(e, ProgressEvent::ScanFinished { side: s, .. } if *s == side),
    );
    (started, finished)
}

const fn ends_with_finished(seen: &[ProgressEvent]) -> bool {
    matches!(seen.last(), Some(ProgressEvent::Finished { .. }))
}

fn started_paths(seen: &[ProgressEvent]) -> Vec<RelPath> {
    seen.iter()
        .filter_map(|e| match e {
            ProgressEvent::FileStarted { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// Plans a pull of the camera directory with a collector already draining, so the test can
/// change the phone before handing everything to `run_collecting`.
async fn plan_camera(
    phone: &VirtualPhone,
) -> (
    PullJob<'_>,
    Sender<ProgressEvent>,
    JoinHandle<Vec<ProgressEvent>>,
) {
    let (tx, rx) = events();
    let collector = collect(rx);
    let job = phone
        .plan(CAMERA, &TransferOptions::pull(), &tx)
        .await
        .unwrap();
    (job, tx, collector)
}

fn assert_only_partial(phone: &VirtualPhone, prefix: &[u8]) {
    let tree = phone.local_tree();
    assert_eq!(tree["a.jpg.mtpx-part"], prefix);
    assert!(tree.contains_key("a.jpg.mtpx-part.json"), "{tree:?}");
    assert!(!tree.contains_key("a.jpg"), "{tree:?}");
}

fn only_failure(report: &Report) -> &FailedFile {
    let [failure] = report.failed.as_slice() else {
        panic!("expected exactly one failure: {:?}", report.failed);
    };
    failure
}

fn plan_ready_hints(seen: &[ProgressEvent]) -> &[Hint] {
    let hints = seen.iter().find_map(|e| match e {
        ProgressEvent::PlanReady { hints, .. } => Some(hints.as_slice()),
        _ => None,
    });
    hints.unwrap_or_else(|| panic!("no PlanReady: {seen:?}"))
}

fn assert_full_event_sequence(seen: &[ProgressEvent], files: usize, report: &Report) {
    for side in [Side::Source, Side::Dest] {
        assert_eq!(scan_events(seen, side), (1, 1), "{side:?}");
    }
    assert_eq!(
        count(seen, |e| matches!(e, ProgressEvent::PlanReady { .. })),
        1
    );
    assert_eq!(started_paths(seen).len(), files);
    assert!(
        matches!(seen.last(), Some(ProgressEvent::Finished { report: r }) if r == report),
        "{:?}",
        seen.last()
    );
}

#[tokio::test]
async fn the_documented_three_step_flow_copies_a_tree() {
    let phone = VirtualPhone::open("three-step").await;
    let expected = seed_tree(&phone);
    let Pulled {
        plan,
        report,
        events,
    } = phone.pull("/DCIM", &TransferOptions::pull()).await;
    assert_eq!(plan.summary().files_to_copy, 5);
    assert!(plan.actions().contains(&Action::Mkdir {
        path: rel(EMPTY_DIR)
    }));
    assert_eq!(phone.local_tree(), expected);
    assert!(phone.local().join(EMPTY_DIR).is_dir());
    assert_eq!(report.copied, 5);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!report.interrupted);
    assert_full_event_sequence(&events, 5, &report);
    assert_eq!(started_paths(&events), sorted_tree_paths());
}

#[tokio::test]
async fn a_second_pull_is_all_skips() {
    let phone = VirtualPhone::open("second-pull").await;
    let expected = seed_tree(&phone);
    let opts = TransferOptions::pull();
    phone.pull("/DCIM", &opts).await;
    let Pulled {
        plan,
        report,
        events,
    } = phone.pull("/DCIM", &opts).await;
    assert_eq!(plan.summary().files_to_copy, 0);
    assert_eq!(plan.summary().files_to_skip, 5);
    assert!(
        !plan
            .actions()
            .iter()
            .any(|action| matches!(action, Action::Mkdir { .. })),
        "an existing local directory needs no action"
    );
    assert_eq!(report.skipped, 5);
    assert_eq!(report.copied, 0);
    assert!(started_paths(&events).is_empty());
    assert_eq!(phone.local_tree(), expected);
}

#[tokio::test]
async fn a_file_deleted_on_the_phone_between_plan_and_run_fails_that_file_only() {
    let phone = VirtualPhone::open("vanished").await;
    phone.seed(&[("DCIM/Camera/a.jpg", b"aaa"), ("DCIM/Camera/b.jpg", b"bbb")]);
    let (job, tx, collector) = plan_camera(&phone).await;
    fs::remove_file(phone.backing().join("DCIM/Camera/b.jpg")).unwrap();
    let (report, seen) = run_collecting(job, tx, collector).await;
    assert_eq!(report.copied, 1);
    assert!(!report.interrupted);
    let failure = only_failure(&report);
    assert_eq!(failure.path, rel("b.jpg"));
    assert!(failure.error.contains("vanished"), "{}", failure.error);
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), b"aaa");
    assert!(!phone.local().join("b.jpg").exists());
    assert!(ends_with_finished(&seen), "{:?}", seen.last());
}

#[tokio::test]
async fn a_file_truncated_after_planning_is_a_length_mismatch_that_keeps_the_partial() {
    let phone = VirtualPhone::open("truncated").await;
    let content = pseudo_random(TRUNCATED_FILE_LEN);
    phone.seed(&[(CAMERA_FILE, &content)]);
    let (job, tx, collector) = plan_camera(&phone).await;
    fs::write(phone.backing().join(CAMERA_FILE), &content[..TRUNCATED_TO]).unwrap();
    let (report, seen) = run_collecting(job, tx, collector).await;
    assert_eq!(report.copied, 0);
    assert!(!report.interrupted);
    let failure = only_failure(&report);
    assert_eq!(failure.path, rel("a.jpg"));
    assert_eq!(
        failure.error,
        format!(
            "length mismatch for a.jpg: expected {TRUNCATED_FILE_LEN} bytes, got {TRUNCATED_TO}"
        )
    );
    assert_only_partial(&phone, &content[..TRUNCATED_TO]);
    assert!(ends_with_finished(&seen), "{:?}", seen.last());
}

#[tokio::test]
async fn a_rekeyed_handle_is_recovered_transparently() {
    let phone = VirtualPhone::open("rekey").await;
    let content = pseudo_random(REKEYED_FILE_LEN);
    phone.seed(&[(CAMERA_FILE, &content)]);
    let (job, tx, collector) = plan_camera(&phone).await;
    // The only `mtp_rs` call in these suites: invalidating a cached handle is a device-side
    // fault the public API has no reason to expose.
    let rekeyed = mtp_rs::rekey_virtual_object(&phone.serial, Path::new(CAMERA_FILE));
    assert!(rekeyed.is_some(), "the planned file was not tracked");
    let (report, seen) = run_collecting(job, tx, collector).await;
    assert_eq!(report.copied, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), content);
    assert_eq!(
        count(&seen, |e| matches!(e, ProgressEvent::FileFailed { .. })),
        0,
        "{seen:?}"
    );
}

#[tokio::test]
async fn device_skipped_objects_surface_as_a_hint() {
    let phone = VirtualPhone::open_with("device-skips", |config| {
        config.undescribable_objects = vec!["DCIM/Camera/broken.jpg".into()];
    })
    .await;
    phone.seed(&[
        ("DCIM/Camera/a.jpg", b"aaa"),
        ("DCIM/Camera/broken.jpg", b"broken"),
    ]);
    let pulled = phone.pull(CAMERA, &TransferOptions::pull()).await;
    let [Hint::DeviceSkippedObjects { skipped }] = plan_ready_hints(&pulled.events) else {
        panic!("{:?}", plan_ready_hints(&pulled.events));
    };
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].parent, RelPath::root());
    assert!(!skipped[0].reason.is_empty());
    assert_eq!(pulled.plan.summary().files_to_copy, 1);
    assert_eq!(started_paths(&pulled.events), [rel("a.jpg")]);
    assert_eq!(pulled.report.copied, 1);
    assert!(
        pulled.report.failed.is_empty(),
        "{:?}",
        pulled.report.failed
    );
    assert_eq!(phone.local_tree().keys().collect::<Vec<_>>(), ["a.jpg"]);
}

#[tokio::test]
async fn opening_a_path_the_device_refuses_to_describe_says_so_instead_of_not_found() {
    let phone = VirtualPhone::open_with("device-undescribed", |config| {
        config.undescribable_objects = vec!["DCIM/Camera/broken.jpg".into()];
    })
    .await;
    phone.seed(&[
        ("DCIM/Camera/a.jpg", b"aaa"),
        ("DCIM/Camera/broken.jpg", b"broken"),
    ]);
    let (tx, _rx) = events();
    let opts = TransferOptions::pull();
    let err = phone
        .plan("/DCIM/Camera/broken.jpg", &opts, &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::RemotePathUndescribed { skipped: 1, path } if path.to_string() == "/DCIM/Camera/broken.jpg"),
        "{err:?}"
    );
    let err = phone
        .plan("/DCIM/Camera/missing.jpg", &opts, &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::RemotePathUndescribed { skipped: 1, .. }),
        "the missing name may hide among the undescribed objects: {err:?}"
    );
    let err = phone.plan("/DCIM/missing", &opts, &tx).await.unwrap_err();
    assert!(matches!(err, Error::RemotePathNotFound(_)), "{err:?}");
}

/// The second `seed` rewrites the backing file, so the device reports a fresh modification
/// time while the local copy keeps its own; a size-only comparison must not care.
#[tokio::test]
async fn a_rewritten_phone_file_of_the_same_length_is_skipped_as_identical() {
    let phone = VirtualPhone::open("size-only").await;
    phone.seed(&[("DCIM/Camera/a.jpg", b"aaa")]);
    let opts = TransferOptions::pull();
    phone.pull("/DCIM", &opts).await;
    phone.seed(&[("DCIM/Camera/a.jpg", b"bbb")]);
    let Pulled { plan, report, .. } = phone.pull("/DCIM", &opts).await;
    assert_eq!(plan.summary().files_to_copy, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(
        fs::read(phone.local().join("Camera/a.jpg")).unwrap(),
        b"aaa"
    );
}
