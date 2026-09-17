//! `mtpx pull` and `mtpx sync`: the plan, the copies, the conflict policies and the summary.

use crate::support::{
    A_JPG, A_JPG_SIZE, B_JPG, B_JPG_SIZE, BOTH_SIZE, CONFLICTING_LOCAL, CONFLICTS, INTERRUPTED,
    NOT_FOUND, Phone, TRANSFER_FAILED, only_line_starting_with,
};
use predicates::prelude::*;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    process::{self, Child, Stdio},
};

/// A name that would retitle or clear a terminal if printed raw.
const ESCAPED_NAME: &str = "a\x1b[31mRED\x1b[0m.jpg";
const ESCAPED_NAME_SHOWN: &str = "a\u{FFFD}[31mRED\u{FFFD}[0m.jpg";
/// Enough progress lines to overflow a 64 KiB pipe buffer, so the writer sees the reader leave.
const MANY_FILES: usize = 600;
const LONG_DIR_NAME_LEN: usize = 200;

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
            predicate::str::is_match(format!(r"Done in \d+s: 2 copied \({BOTH_SIZE} B, "))
                .unwrap()
                .and(predicate::str::contains(" avg), 0 skipped, 0 failed\n")),
        );
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), A_JPG);
    assert_eq!(fs::read(phone.local().join("b.jpg")).unwrap(), B_JPG);
}

/// Mirrors the camera directory once, so the next run has nothing to copy.
fn synced_phone() -> Phone {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0);
    phone
}

#[test]
fn sync_of_already_synced_files_counts_the_skips_without_listing_them() {
    let phone = synced_phone();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0)
        .stdout("")
        .stderr(
            predicate::str::contains("Plan: copy 0 files (0 B), skip 2")
                .and(
                    predicate::str::is_match(r"Done in \d+s: 0 copied, 2 skipped, 0 failed\n")
                        .unwrap(),
                )
                .and(predicate::str::contains("skipped").count(1)),
        );
    assert_eq!(phone.local_names(), ["a.jpg", "b.jpg"]);
}

#[test]
fn sync_dry_run_of_already_synced_files_prints_no_skip_lines() {
    let phone = synced_phone();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera", phone.local_str(), "--dry-run"])
        .assert()
        .code(0)
        .stdout("")
        .stderr(predicate::str::contains("Plan: copy 0 files (0 B), skip 2"));
}

#[test]
fn pull_dry_run_still_lists_a_conflict_skip() {
    let phone = Phone::with_two_photos();
    fs::write(phone.local().join("a.jpg"), CONFLICTING_LOCAL).unwrap();
    fs::write(phone.local().join("b.jpg"), B_JPG).unwrap();
    phone
        .mtpx()
        .args([
            "pull",
            "/DCIM/Camera",
            phone.local_str(),
            "--skip-existing",
            "--dry-run",
        ])
        .assert()
        .code(0)
        .stdout("skip  a.jpg (conflict)\n")
        .stderr(predicate::str::contains("Plan: copy 0 files (0 B), skip 2"));
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
fn sync_of_a_single_file_lands_it_under_local() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["sync", "/DCIM/Camera/a.jpg", phone.local_str()])
        .assert()
        .code(0)
        .stderr(predicate::str::contains(" 1 copied ("));
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
            predicate::str::contains("failed a.jpg: open ")
                .and(predicate::str::contains("a.jpg.mtpx-part: "))
                .and(predicate::str::contains(" 1 copied (")),
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

#[test]
fn pull_into_an_existing_file_is_refused_before_anything_is_copied() {
    let phone = Phone::with_two_photos();
    let local = phone.local().join("not-a-dir");
    fs::write(&local, CONFLICTING_LOCAL).unwrap();
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera"])
        .arg(&local)
        .assert()
        .code(NOT_FOUND)
        .stdout("")
        .stderr(
            predicate::str::contains("not a directory")
                .and(predicate::str::contains("copying").not()),
        );
    assert_eq!(fs::read(&local).unwrap(), CONFLICTING_LOCAL);
    assert_eq!(phone.local_names(), ["not-a-dir"]);
}

#[test]
fn quiet_still_reports_objects_the_device_refused_to_describe() {
    let phone = Phone::with_two_photos();
    phone.seed("DCIM/Camera/broken.jpg", b"broken");
    let hint = "1 object the device refused to describe were left out of the plan\n";
    let summary = r"Done in \d+s: 2 copied .*, 0 skipped, 0 failed, 1 left out\n";
    phone
        .mtpx()
        .args(["--virtual-refuse", "DCIM/Camera/broken.jpg"])
        .args(["sync", "/DCIM/Camera", phone.local_str(), "-q"])
        .assert()
        .code(0)
        .stdout("")
        .stderr(predicate::str::contains(hint).and(predicate::str::is_match(summary).unwrap()));
    assert_eq!(phone.local_names(), ["a.jpg", "b.jpg"]);
}

#[test]
fn progress_and_the_plan_neutralize_control_characters_in_device_names() {
    let phone = Phone::empty();
    phone.seed(&format!("DCIM/{ESCAPED_NAME}"), b"x");
    phone
        .mtpx()
        .args(["pull", "/DCIM", phone.local_str(), "--dry-run"])
        .assert()
        .code(0)
        .stdout(format!("copy  {ESCAPED_NAME_SHOWN} (1 B)\n"));
    phone
        .mtpx()
        .args(["pull", "/DCIM", phone.local_str()])
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains(format!("copying {ESCAPED_NAME_SHOWN} (1 B)\n"))
                .and(predicate::str::contains(format!(
                    "copied {ESCAPED_NAME_SHOWN} ("
                )))
                .and(predicate::str::contains("\x1b[").not()),
        );
}

/// Returns the directory name the files were seeded under.
fn seed_many_files(phone: &Phone) -> String {
    let dir = "d".repeat(LONG_DIR_NAME_LEN);
    for index in 0..MANY_FILES {
        phone.seed(&format!("DCIM/{dir}/photo_{index:04}.jpg"), A_JPG);
    }
    dir
}

/// Reads one line of the child's stderr and closes the pipe, as `2>&1 | head -1` does.
fn read_one_stderr_line_then_leave(child: &mut Child) -> String {
    let stderr = child.stderr.take().unwrap();
    let mut first = String::new();
    BufReader::new(stderr).read_line(&mut first).unwrap();
    first
}

#[test]
fn pull_into_a_stderr_reader_that_leaves_early_exits_0_without_a_panic() {
    let phone = Phone::empty();
    let dir = seed_many_files(&phone);
    let mut child = phone
        .mtpx_process()
        .args(["pull", "/DCIM", phone.local_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        read_one_stderr_line_then_leave(&mut child).starts_with("Scanned "),
        "the first line is the source scan"
    );
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let landed = fs::read_dir(phone.local().join(&dir)).unwrap().count();
    assert_eq!(landed, MANY_FILES);
}

#[test]
fn quiet_pull_into_a_closed_stderr_exits_0() {
    let phone = Phone::with_two_photos();
    let mut child = phone
        .mtpx_process()
        .args(["pull", "/DCIM/Camera", phone.local_str(), "-q"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(phone.local_names(), ["a.jpg", "b.jpg"]);
}

/// Leaves `prefix` of `name` as a part with the sidecar the core would have written for it on
/// the virtual phone.
fn seed_partial(phone: &Phone, name: &str, size: usize, prefix: &[u8]) {
    let sidecar = format!(
        r#"{{"version":1,"identity":{{"device_serial":"VIRTUAL","storage":"Internal"}},"path":"{name}","fingerprint":{{"size":{size},"modified":null}},"bytes":{}}}"#,
        prefix.len()
    );
    fs::write(phone.local().join(format!("{name}.mtpx-part")), prefix).unwrap();
    fs::write(
        phone.local().join(format!("{name}.mtpx-part.json")),
        sidecar,
    )
    .unwrap();
}

#[test]
fn pull_resumes_a_valid_partial_and_removes_the_part_files() {
    const RESUMED: usize = 10;
    let phone = Phone::with_two_photos();
    seed_partial(&phone, "a.jpg", A_JPG_SIZE, &A_JPG[..RESUMED]);
    let plan = format!(
        "Plan: copy 1 file ({} B, {RESUMED} B resumable), skip 0",
        A_JPG_SIZE - RESUMED
    );
    let resuming = format!("resuming a.jpg from {RESUMED} B of {A_JPG_SIZE} B");
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera/a.jpg", phone.local_str()])
        .assert()
        .code(0)
        .stderr(predicate::str::contains(plan).and(predicate::str::contains(resuming)));
    assert_eq!(fs::read(phone.local().join("a.jpg")).unwrap(), A_JPG);
    assert_eq!(phone.local_names(), ["a.jpg"]);
}

/// Big enough that the signal lands while bytes are still moving: the virtual device streams
/// hundreds of MiB per second, and nothing here waits on a clock.
const BIG_FILE: u64 = 256 * 1024 * 1024;

/// Reads stderr lines until one starts with `prefix`, or the pipe ends.
fn read_until_line_starting_with(stderr: &mut impl BufRead, prefix: &str) -> String {
    let mut line = String::new();
    while stderr.read_line(&mut line).unwrap() > 0 && !line.starts_with(prefix) {
        line.clear();
    }
    line
}

/// Starts pulling `big.bin`, sends SIGINT once the copy is under way, and returns the rest of
/// stderr with the exit code.
#[cfg(unix)]
fn interrupt_a_big_pull(phone: &Phone) -> (Option<i32>, String) {
    let mut child = phone
        .mtpx_process()
        .args(["pull", "/DCIM/Camera", phone.local_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let copying = read_until_line_starting_with(&mut stderr, "copying ");
    assert!(copying.starts_with("copying big.bin ("), "{copying}");
    let sent = process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    (child.wait().unwrap().code(), rest)
}

/// A phone with one sparse `big.bin` under the camera directory.
fn phone_with_a_big_file() -> Phone {
    let phone = Phone::empty();
    let big = phone.backing().join("DCIM/Camera/big.bin");
    fs::create_dir_all(big.parent().unwrap()).unwrap();
    fs::File::create(&big).unwrap().set_len(BIG_FILE).unwrap();
    phone
}

#[cfg(unix)]
#[test]
fn ctrl_c_mid_transfer_exits_130_and_keeps_the_partial() {
    let phone = phone_with_a_big_file();
    let (code, rest) = interrupt_a_big_pull(&phone);
    assert_eq!(code, Some(INTERRUPTED), "{rest}");
    assert!(
        rest.contains("interrupting, finishing the current window...\n"),
        "{rest}"
    );
    assert!(rest.contains("Interrupted after "), "{rest}");
    assert!(
        rest.contains(" 0 copied, 1 remaining, re-run to resume\n"),
        "{rest}"
    );
    assert_eq!(
        phone.local_names(),
        ["big.bin.mtpx-part", "big.bin.mtpx-part.json"]
    );
}

#[cfg(unix)]
#[test]
fn the_run_after_a_ctrl_c_resumes_the_partial_and_completes_the_file() {
    let phone = phone_with_a_big_file();
    let (code, rest) = interrupt_a_big_pull(&phone);
    assert_eq!(code, Some(INTERRUPTED), "{rest}");
    phone
        .mtpx()
        .args(["pull", "/DCIM/Camera", phone.local_str()])
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains(" resumable), skip 0\n")
                .and(predicate::str::contains("resuming big.bin from ")),
        );
    assert_eq!(
        fs::metadata(phone.local().join("big.bin")).unwrap().len(),
        BIG_FILE
    );
    assert_eq!(phone.local_names(), ["big.bin"]);
}
