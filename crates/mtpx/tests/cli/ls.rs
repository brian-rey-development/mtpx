//! `mtpx ls`: what lands on stdout, and that stderr stays empty.

use crate::support::{A_JPG, A_JPG_SIZE, B_JPG_SIZE, Phone};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Stdio},
};

/// Seeded so that neither creation order nor its reverse is already sorted.
const UNSORTED_SEED: [&str; 3] = ["c.jpg", "a.jpg", "b.jpg"];
/// Enough listing to overflow a 64 KiB pipe buffer, so the writer sees the reader leave.
const MANY_FILES: usize = 600;
const LONG_DIR_NAME_LEN: usize = 200;

#[test]
fn ls_lists_the_top_level_on_stdout_only() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["ls", "/"])
        .assert()
        .code(0)
        .stdout("DCIM/\n")
        .stderr("");
}

#[test]
fn ls_recursive_lists_the_tree_sorted() {
    let phone = Phone::empty();
    for name in UNSORTED_SEED {
        phone.seed(&format!("DCIM/Camera/{name}"), A_JPG);
    }
    phone
        .mtpx()
        .args(["ls", "/DCIM", "-R"])
        .assert()
        .code(0)
        .stdout("Camera/\nCamera/a.jpg\nCamera/b.jpg\nCamera/c.jpg\n");
}

#[test]
fn ls_long_prints_kind_size_and_name() {
    let phone = Phone::with_two_photos();
    phone
        .mtpx()
        .args(["ls", "/DCIM/Camera", "-l"])
        .assert()
        .code(0)
        .stdout(format!(
            "file  {A_JPG_SIZE} B  -  a.jpg\nfile  {B_JPG_SIZE} B  -  b.jpg\n"
        ));
}

#[test]
fn ls_neutralizes_hostile_characters_in_device_names() {
    let phone = Phone::empty();
    phone.seed("DCIM/photo\u{202E}gpj.exe", b"x");
    phone
        .mtpx()
        .args(["ls", "/DCIM"])
        .assert()
        .code(0)
        .stdout("photo\u{FFFD}gpj.exe\n");
}

/// Returns the directory name the files were seeded under.
fn seed_many_files(phone: &Phone) -> String {
    let dir = "d".repeat(LONG_DIR_NAME_LEN);
    for index in 0..MANY_FILES {
        phone.seed(&format!("DCIM/{dir}/photo_{index:04}.jpg"), A_JPG);
    }
    dir
}

/// Reads one line of the child's stdout and closes the pipe, as `head -1` does.
fn read_one_line_then_leave(child: &mut Child) -> String {
    let stdout = child.stdout.take().unwrap();
    let mut first = String::new();
    BufReader::new(stdout).read_line(&mut first).unwrap();
    first
}

#[test]
fn ls_recursive_into_a_reader_that_leaves_early_exits_0_without_a_panic() {
    let phone = Phone::empty();
    let dir = seed_many_files(&phone);
    let mut child = phone
        .mtpx_process()
        .args(["ls", "/DCIM", "-R"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(read_one_line_then_leave(&mut child), format!("{dir}/\n"));
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
}
