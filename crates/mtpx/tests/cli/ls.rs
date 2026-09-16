//! `mtpx ls`: what lands on stdout, and that stderr stays empty.

use crate::support::{A_JPG, A_JPG_SIZE, B_JPG_SIZE, Phone};

/// Seeded so that neither creation order nor its reverse is already sorted.
const UNSORTED_SEED: [&str; 3] = ["c.jpg", "a.jpg", "b.jpg"];

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
