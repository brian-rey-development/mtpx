//! A virtual phone the binary is pointed at, plus the constants every suite shares.

use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, path::Path, process};
use tempfile::TempDir;

pub const A_JPG: &[u8] = b"a thirty byte long photo file!";
pub const B_JPG: &[u8] = b"b fifty-seven bytes of photo, a little larger than a.jpg!";
pub const A_JPG_SIZE: usize = A_JPG.len();
pub const B_JPG_SIZE: usize = B_JPG.len();
pub const BOTH_SIZE: usize = A_JPG_SIZE + B_JPG_SIZE;
pub const CONFLICTING_LOCAL: &[u8] = b"local";
pub const USAGE_ERROR: i32 = 2;
pub const NOT_FOUND: i32 = 5;
pub const TRANSFER_FAILED: i32 = 6;
pub const CONFLICTS: i32 = 7;
#[cfg(unix)]
pub const INTERRUPTED: i32 = 130;

/// A virtual phone backed by a temp dir, plus an empty local directory to pull into.
pub struct Phone {
    backing: TempDir,
    local: TempDir,
}

impl Phone {
    /// `a.jpg` and `b.jpg` under `DCIM/Camera`, nothing else.
    pub fn with_two_photos() -> Self {
        let phone = Self::empty();
        phone.seed("DCIM/Camera/a.jpg", A_JPG);
        phone.seed("DCIM/Camera/b.jpg", B_JPG);
        phone
    }

    pub fn empty() -> Self {
        Self {
            backing: tempfile::tempdir().unwrap(),
            local: tempfile::tempdir().unwrap(),
        }
    }

    pub fn seed(&self, path: &str, content: &[u8]) {
        let full = self.backing.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    pub fn local(&self) -> &Path {
        self.local.path()
    }

    /// The directory the virtual device serves; what `seed` writes into.
    #[cfg(unix)]
    pub fn backing(&self) -> &Path {
        self.backing.path()
    }

    pub fn local_str(&self) -> &str {
        self.local().to_str().unwrap()
    }

    /// `mtpx --virtual <backing> --no-color`, ready for a subcommand.
    pub fn mtpx(&self) -> Command {
        Command::from_std(self.mtpx_process())
    }

    /// The same invocation as a plain process, for tests that drive the pipes themselves.
    pub fn mtpx_process(&self) -> process::Command {
        let mut cmd = process::Command::new(env!("CARGO_BIN_EXE_mtpx"));
        cmd.arg("--virtual")
            .arg(self.backing.path())
            .arg("--no-color");
        cmd
    }

    pub fn local_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.local())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// Exactly one line, and it starts with `prefix`.
pub fn only_line_starting_with(prefix: &'static str) -> impl Predicate<str> {
    predicate::function(move |text: &str| text.lines().count() == 1 && text.starts_with(prefix))
}
