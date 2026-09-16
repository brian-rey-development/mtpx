//! What each conflict policy does with a local file that differs from the phone's copy.

#![cfg(feature = "virtual-device")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Pulled, VirtualPhone, collect, count, events, rel};
use mtpx_core::{
    Action, ConflictPolicy, CopyReason, Error, ProgressEvent, SkipReason, TransferOptions,
};
use std::fs;

const CAMERA: &str = "/DCIM/Camera";
const PHONE_COPY: &[u8] = b"the phone's thirty byte photo!";
const LOCAL_COPY: &[u8] = b"local";
const LOCAL_ONLY: &[u8] = b"kept";

/// The phone and the local directory disagree on `a.jpg`; the local directory also holds a
/// file the phone knows nothing about.
async fn open_with_conflict(test_name: &str) -> VirtualPhone {
    let phone = VirtualPhone::open(test_name).await;
    phone.seed(&[("DCIM/Camera/a.jpg", PHONE_COPY)]);
    fs::write(phone.local().join("a.jpg"), LOCAL_COPY).unwrap();
    fs::write(phone.local().join("local-only.txt"), LOCAL_ONLY).unwrap();
    phone
}

const fn skip_existing() -> TransferOptions {
    let mut opts = TransferOptions::pull();
    opts.conflict = ConflictPolicy::Skip;
    opts
}

#[tokio::test]
async fn sync_overwrites_a_differing_local_file_and_keeps_extras() {
    let phone = open_with_conflict("sync-overwrite").await;
    let Pulled { plan, report, .. } = phone.pull(CAMERA, &TransferOptions::sync()).await;
    let copies: Vec<&Action> = plan
        .actions()
        .iter()
        .filter(|action| matches!(action, Action::Copy { .. }))
        .collect();
    assert!(
        matches!(
            copies.as_slice(),
            [Action::Copy { path, reason: CopyReason::SizeDiffers, .. }] if *path == rel("a.jpg")
        ),
        "{copies:?}"
    );
    assert_eq!(report.copied, 1);
    let tree = phone.local_tree();
    assert_eq!(tree["a.jpg"], PHONE_COPY);
    assert_eq!(tree["local-only.txt"], LOCAL_ONLY);
    assert_eq!(tree.len(), 2, "{tree:?}");
}

#[tokio::test]
async fn pull_refuses_a_differing_local_file_and_touches_nothing() {
    let phone = open_with_conflict("pull-conflict").await;
    let before = phone.local_tree();
    let (tx, rx) = events();
    let collector = collect(rx);
    let err = phone
        .plan(CAMERA, &TransferOptions::pull(), &tx)
        .await
        .unwrap_err();
    drop(tx);
    let seen = collector.await.unwrap();
    let Error::Conflicts(paths) = err else {
        panic!("{err:?}");
    };
    assert_eq!(paths, vec![rel("a.jpg")]);
    assert_eq!(phone.local_tree(), before);
    assert_eq!(
        count(&seen, |e| matches!(e, ProgressEvent::PlanReady { .. })),
        0,
        "{seen:?}"
    );
}

#[tokio::test]
async fn pull_skip_existing_leaves_the_conflicting_file() {
    let phone = open_with_conflict("pull-skip-existing").await;
    let pulled = phone.pull(CAMERA, &skip_existing()).await;
    let skip = Action::Skip {
        path: rel("a.jpg"),
        reason: SkipReason::Conflict,
    };
    assert_eq!(pulled.plan.actions(), [skip]);
    assert_eq!(pulled.report.skipped, 1);
    assert_eq!(pulled.report.copied, 0);
    assert_eq!(phone.local_tree()["a.jpg"], LOCAL_COPY);
    let skipped = ProgressEvent::Skipped {
        path: rel("a.jpg"),
        reason: SkipReason::Conflict,
    };
    assert!(pulled.events.contains(&skipped), "{:?}", pulled.events);
}
