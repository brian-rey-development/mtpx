//! Locates the object a remote path names, walking one folder per segment from the storage root.

use crate::{
    device_path::{DevicePath, StorageSelector},
    error::{Error, Result},
    path::RemotePath,
};
use mtp_rs::{ObjectHandle, ObjectInfo, Storage};

/// Where a root lives: the folder to list under (`None` is the storage root) and, when the root
/// names a file inside that folder, the file's name.
pub(super) struct Target {
    pub folder: Option<ObjectHandle>,
    pub file: Option<String>,
}

impl Target {
    /// The object `found` under `parent`: a folder becomes the root itself, anything else is a
    /// file inside `parent`.
    fn found(parent: Option<ObjectHandle>, found: ObjectInfo) -> Self {
        if found.is_folder() {
            return Self {
                folder: Some(found.handle),
                file: None,
            };
        }
        Self {
            folder: parent,
            file: Some(found.filename),
        }
    }
}

/// Walks `root` one folder per segment from the storage root; the last segment may be a file.
///
/// # Errors
/// `RemotePathNotFound` when a segment is missing, `NotADirectory` when a segment before the
/// last is a file, or the first listing error.
pub(super) async fn locate_root(storage: &Storage, root: &RemotePath) -> Result<Target> {
    let mut folder = None;
    let Some((last, folders)) = root.segments().split_last() else {
        return Ok(Target { folder, file: None });
    };
    for segment in folders {
        let found = find_child(storage, folder, segment, root).await?;
        if !found.is_folder() {
            return Err(Error::NotADirectory(device_path(storage, root)));
        }
        folder = Some(found.handle);
    }
    let found = find_child(storage, folder, last, root).await?;
    Ok(Target::found(folder, found))
}

async fn find_child(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    name: &str,
    root: &RemotePath,
) -> Result<ObjectInfo> {
    let listing = storage
        .collect_objects(parent)
        .await
        .map_err(Error::from_mtp)?;
    listing
        .objects
        .into_iter()
        .find(|o| o.filename == name)
        .ok_or_else(|| Error::RemotePathNotFound(device_path(storage, root)))
}

fn device_path(storage: &Storage, root: &RemotePath) -> DevicePath {
    DevicePath {
        storage: StorageSelector::Named(storage.info().description.clone()),
        path: root.clone(),
    }
}
