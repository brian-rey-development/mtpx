//! Usage errors, missing paths and storages, and the help text.

use crate::support::{NOT_FOUND, Phone, USAGE_ERROR};
use predicates::prelude::*;

#[test]
fn missing_remote_path_exits_5_with_a_hint() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["pull", "/Nope", phone.local_str()])
        .assert()
        .code(NOT_FOUND)
        .stderr(
            predicate::str::contains("remote path not found")
                .and(predicate::str::contains("mtpx ls")),
        );
}

#[test]
fn ls_of_a_file_exits_5() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["ls", "/DCIM/Camera/a.jpg"])
        .assert()
        .code(NOT_FOUND)
        .stderr(predicate::str::contains("not a directory"));
}

#[test]
fn unknown_storage_exits_5() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["--storage", "nope", "ls", "/"])
        .assert()
        .code(NOT_FOUND)
        .stderr(predicate::str::contains("storage not found: nope"));
}

#[test]
fn empty_device_value_is_a_usage_error() {
    let phone = Phone::empty();
    phone
        .mtpx()
        .args(["--device", "", "devices"])
        .assert()
        .code(USAGE_ERROR);
}

#[test]
fn overwrite_and_skip_existing_conflict_is_a_usage_error() {
    let phone = Phone::empty();
    phone
        .mtpx()
        .args([
            "pull",
            "/DCIM/Camera",
            phone.local_str(),
            "--overwrite",
            "--skip-existing",
        ])
        .assert()
        .code(USAGE_ERROR);
}

#[test]
fn help_mentions_the_four_commands() {
    let phone = Phone::empty();
    phone.mtpx().arg("--help").assert().code(0).stdout(
        predicate::str::contains("devices")
            .and(predicate::str::contains("ls"))
            .and(predicate::str::contains("pull"))
            .and(predicate::str::contains("sync")),
    );
}
