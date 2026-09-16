//! The public pull contract, exercised from outside the crate against a virtual phone.
//!
//! Resume and cancellation are covered deterministically in-crate (`device/pull.rs`) and are
//! deliberately not repeated here.

#![cfg(feature = "virtual-device")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Pulled, VirtualPhone, collect, count, events, pseudo_random, rel, run_collecting};
use mtpx_core::{Hint, ProgressEvent, PullJob, RelPath, Report, Side, TransferOptions};
use std::{collections::BTreeMap, fs, path::Path};
use tokio::{sync::mpsc::Sender, task::JoinHandle};

const CAMERA: &str = "/DCIM/Camera";
const CAMERA_FILE: &str = "DCIM/Camera/a.jpg";
const REKEYED_FILE_LEN: usize = 64 * 1024;
const TRUNCATED_FILE_LEN: usize = 64 * 1024;
const TRUNCATED_TO: usize = 1024;

/// Three levels deep, one empty file, one Unicode name; paths relative to `/DCIM`. Deliberately
/// not in path order, so a sorted copy order can only come from the snapshot.
const TREE: [(&str, &[u8]); 5] = [
    ("notes.txt", b"notes"),
    ("Fotos/a\u{f1}o 2026/IMG_1.jpg", b"unicode photo"),
    ("Camera/IMG_0001.jpg", b"first photo"),
    ("Camera/empty.dat", b""),
    ("Camera/2026/01/IMG_0002.jpg", b"second photo"),
];

/// The seeded paths in the order a snapshot lists them, which is not the order they were seeded.
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

/// The part holds exactly `prefix`, its sidecar sits beside it, and the final file never landed.
fn assert_only_partial(phone: &VirtualPhone, prefix: &[u8]) {
    let tree = phone.local_tree();
    assert_eq!(tree["a.jpg.mtpx-part"], prefix);
    assert!(tree.contains_key("a.jpg.mtpx-part.json"), "{tree:?}");
    assert!(!tree.contains_key("a.jpg"), "{tree:?}");
}

fn only_failure(report: &Report) -> &(RelPath, String) {
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

/// Both sides scanned once, one plan, `files` copies started, and `Finished` carrying `report`.
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
    assert_eq!(phone.local_tree(), expected);
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
    assert_eq!(plan.summary().to_skip, 5);
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
    let (failed_path, message) = only_failure(&report);
    assert_eq!(*failed_path, rel("b.jpg"));
    assert!(message.contains("vanished"), "{message}");
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), b"aaa");
    assert!(!phone.local().join("b.jpg").exists());
    assert!(ends_with_finished(&seen), "{:?}", seen.last());
}

#[tokio::test]
async fn a_file_truncated_on_the_phone_after_planning_fails_with_length_mismatch_and_keeps_the_partial()
 {
    let phone = VirtualPhone::open("truncated").await;
    let content = pseudo_random(TRUNCATED_FILE_LEN);
    phone.seed(&[(CAMERA_FILE, &content)]);
    let (job, tx, collector) = plan_camera(&phone).await;
    fs::write(phone.backing().join(CAMERA_FILE), &content[..TRUNCATED_TO]).unwrap();
    let (report, seen) = run_collecting(job, tx, collector).await;
    assert_eq!(report.copied, 0);
    assert!(!report.interrupted);
    let (failed_path, message) = only_failure(&report);
    assert_eq!(*failed_path, rel("a.jpg"));
    assert_eq!(
        *message,
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
    let expected_hints = [Hint::DeviceSkippedObjects { count: 1 }];
    assert_eq!(plan_ready_hints(&pulled.events), expected_hints);
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
