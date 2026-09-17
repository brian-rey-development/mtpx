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
            predicate::str::contains("remote path not found: /Nope")
                .and(predicate::str::contains("`mtpx ls /`")),
        );
}

#[test]
fn a_typed_storage_prefix_stays_in_the_message_and_the_hint() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["ls", "internal:/DCIM/Nope"])
        .assert()
        .code(NOT_FOUND)
        .stderr(
            predicate::str::contains("remote path not found: internal:/DCIM/Nope")
                .and(predicate::str::contains("`mtpx ls internal:/DCIM`")),
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
        .stderr(
            predicate::str::contains("storage not found: nope")
                .and(predicate::str::contains("--storage")),
        );
}

#[test]
fn storage_flag_wins_over_the_path_prefix() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["--storage", "nope", "ls", "internal:/"])
        .assert()
        .code(NOT_FOUND)
        .stderr(predicate::str::contains("storage not found: nope"));
    phone
        .mtpx()
        .args(["--storage", "0", "ls", "internal-typo:/"])
        .assert()
        .code(0)
        .stdout("DCIM/\n");
}

#[test]
fn storage_flag_selects_by_name_regardless_of_case() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["--storage", "internal", "ls", "/"])
        .assert()
        .code(0)
        .stdout("DCIM/\n");
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
fn help_mentions_the_four_commands_and_the_exit_codes() {
    let phone = Phone::empty();
    phone.mtpx().arg("--help").assert().code(0).stdout(
        predicate::str::contains("devices")
            .and(predicate::str::contains("ls"))
            .and(predicate::str::contains("pull"))
            .and(predicate::str::contains("sync"))
            .and(predicate::str::contains("Exit codes:"))
            .and(predicate::str::contains("Global options:")),
    );
}
