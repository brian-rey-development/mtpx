//! `mtpx pull` and `mtpx sync`: the plan, the copies, the conflict policies and the summary.

use crate::support::{
    A_JPG, A_JPG_SIZE, B_JPG, B_JPG_SIZE, BOTH_SIZE, CONFLICTING_LOCAL, CONFLICTS, Phone,
    TRANSFER_FAILED, only_line_starting_with,
};
use predicates::prelude::*;
use std::fs;

#[test]
fn pull_dry_run_prints_the_actions_and_touches_nothing() {
    let phone = Phone::with_two_photos();
    let local = phone.local().join("out");
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera"])
        .arg(&local)
        .arg("--dry-run")
        .assert()
        .code(0)
        .stdout(format!(
            "copy  a.jpg ({A_JPG_SIZE} B)\ncopy  b.jpg ({B_JPG_SIZE} B)\n"
        ))
        .stderr(predicate::str::contains(format!(
            "Plan: copy 2 files ({BOTH_SIZE} B), skip 0"
        )));
    assert!(!local.exists());
}

#[test]
fn sync_copies_new_files_and_prints_a_summary() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0)
        .stdout("")
        .stderr(
            predicate::str::contains(format!("Done in 0s: 2 copied ({BOTH_SIZE} B, "))
                .and(predicate::str::contains(" avg), 0 skipped, 0 failed\n")),
        );
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), A_JPG);
    assert_eq!(fs::read(phone.local().join("b.jpg")).unwrap(), B_JPG);
}

#[test]
fn sync_of_already_synced_files_reports_all_skips() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0);
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0)
        .stdout("")
        .stderr(
            predicate::str::contains("Plan: copy 0 files (0 B), skip 2").and(
                predicate::str::contains("Done in 0s: 0 copied, 2 skipped, 0 failed\n"),
            ),
        );
    assert_eq!(phone.local_names(), ["a.jpg", "b.jpg"]);
}

#[test]
fn pull_of_a_single_file_lands_it_under_local() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera/a.jpg", phone.local_str()])
        .assert()
        .code(0);
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), A_JPG);
    assert_eq!(phone.local_names(), ["a.jpg"]);
}

#[test]
fn pull_refuses_conflicts_with_exit_7_and_lists_them() {
    let phone = Phone::with_two_photos();
    fs::write(phone.local().join("a.jpg"), CONFLICTING_LOCAL).unwrap();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(CONFLICTS)
        .stdout("")
        .stderr(
            predicate::str::contains("conflicting files on both sides (1)")
                .and(predicate::str::contains("1 file differs:"))
                .and(predicate::str::contains("a.jpg"))
                .and(predicate::str::contains(
                    "use --overwrite to replace them or --skip-existing to leave them",
                )),
        );
    assert_eq!(
        fs::read(phone.local().join("a.jpg")).unwrap(),
        CONFLICTING_LOCAL
    );
}

#[test]
fn pull_overwrite_replaces_the_conflicting_file() {
    let phone = Phone::with_two_photos();
    fs::write(phone.local().join("a.jpg"), CONFLICTING_LOCAL).unwrap();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera", phone.local_str(), "--overwrite"])
        .assert()
        .code(0);
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), A_JPG);
}

#[test]
fn pull_skip_existing_leaves_the_conflicting_file() {
    let phone = Phone::with_two_photos();
    fs::write(phone.local().join("a.jpg"), CONFLICTING_LOCAL).unwrap();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera", phone.local_str(), "--skip-existing"])
        .assert()
        .code(0)
        .stderr(predicate::str::contains("skipped a.jpg (conflict)"));
    assert_eq!(
        fs::read(phone.local().join("a.jpg")).unwrap(),
        CONFLICTING_LOCAL
    );
}

#[test]
fn pull_exits_6_and_names_the_file_when_its_part_cannot_be_written() {
    let phone = Phone::with_two_photos();
    fs::create_dir(phone.local().join("a.jpg.mtpx-part")).unwrap();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(TRANSFER_FAILED)
        .stdout("")
        .stderr(
            predicate::str::contains("failed a.jpg: ").and(predicate::str::contains(" 1 copied (")),
        )
        .stderr(predicate::str::contains(", 0 skipped, 1 failed\n"));
    assert_eq!(fs::read(phone.local().join("b.jpg")).unwrap(), B_JPG);
    assert!(!phone.local().join("a.jpg").exists());
}

#[test]
fn quiet_prints_only_the_summary() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str(), "-q"])
        .assert()
        .code(0)
        .stdout("")
        .stderr(only_line_starting_with("Done in "));
}
