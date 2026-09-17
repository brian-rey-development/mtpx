//! Detects whether the volume a directory lives on tells `a.jpg` and `A.jpg` apart.

use crate::planner::NameFolding;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// How the volume under `root` compares names, learned without writing anything: the case of
/// an existing name is flipped and the two spellings are checked for being the same inode.
///
/// A root that does not exist yet is created later on the same volume as its nearest
/// existing ancestor, so that ancestor is probed instead. `Exact` is the answer whenever
/// nothing on the volume has a letter to flip, which errs towards planning every file.
pub(super) fn detect(root: &Path) -> NameFolding {
    let Some(base) = nearest_existing(root) else {
        return NameFolding::Exact;
    };
    std::iter::once(base.clone())
        .chain(children(&base))
        .find_map(|candidate| probe(&candidate))
        .unwrap_or(NameFolding::Exact)
}

fn nearest_existing(root: &Path) -> Option<PathBuf> {
    let absolute = fs::canonicalize(root)
        .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(root)))
        .ok()?;
    absolute
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .map(Path::to_path_buf)
}

fn children(dir: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
}

/// `None` when `path` has no letter whose case can be flipped, so it cannot tell.
fn probe(path: &Path) -> Option<NameFolding> {
    let name = path.file_name()?.to_str()?;
    let flipped = flip_case(name);
    if flipped == name {
        return None;
    }
    let same = same_file::is_same_file(path, path.with_file_name(flipped)).unwrap_or(false);
    Some(if same {
        NameFolding::CaseInsensitive
    } else {
        NameFolding::Exact
    })
}

fn flip_case(name: &str) -> String {
    let mut flipped = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_lowercase() {
            flipped.extend(c.to_uppercase());
        } else {
            flipped.extend(c.to_lowercase());
        }
    }
    flipped
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn flip_case_swaps_every_cased_letter_and_leaves_the_rest() {
        assert_eq!(flip_case("Photos 2024"), "pHOTOS 2024");
        assert_eq!(flip_case("año"), "AÑO");
        assert_eq!(flip_case("123"), "123");
    }

    #[test]
    fn a_name_without_letters_cannot_be_probed() {
        let dir = tempfile::tempdir().unwrap();
        let numeric = dir.path().join("2024");
        fs::create_dir(&numeric).unwrap();
        assert_eq!(probe(&numeric), None);
    }

    #[test]
    fn a_missing_root_is_judged_by_its_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("Not").join("Yet");
        assert_eq!(detect(&missing), detect(dir.path()));
    }

    #[test]
    fn a_root_without_letters_is_judged_by_a_child_that_has_some() {
        let dir = tempfile::tempdir().unwrap();
        let numeric = dir.path().join("2024");
        fs::create_dir(&numeric).unwrap();
        fs::write(numeric.join("Photo.jpg"), b"x").unwrap();
        assert_eq!(detect(&numeric), detect(dir.path()));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_default_volume_folds_case() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect(dir.path()), NameFolding::CaseInsensitive);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_default_volume_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect(dir.path()), NameFolding::Exact);
    }
}
