//! Locates the object a remote path names, walking one folder per segment from the storage root.

use crate::{
    device_path::DevicePath,
    error::{Error, Result},
};
use mtp_rs::{CancelToken, ListingItem, ObjectHandle, ObjectInfo, Storage};

/// Where a root lives: the folder to list under (`None` is the storage root), plus the file
/// itself, as found at open time, when the root is a file.
pub(super) struct Target {
    pub folder: Option<ObjectHandle>,
    pub file: Option<ObjectInfo>,
}

impl Target {
    /// What `found` under `parent` means: a folder is the root itself, anything else a file inside it.
    fn found(parent: Option<ObjectHandle>, found: ObjectInfo) -> Self {
        if found.is_folder() {
            return Self {
                folder: Some(found.handle),
                file: None,
            };
        }
        Self {
            folder: parent,
            file: Some(found),
        }
    }
}

/// Walks `root.path` one folder per segment; errors carry `root` as the caller wrote it.
///
/// # Errors
/// `RemotePathNotFound` when a segment is missing, `RemotePathUndescribed` when it is missing
/// among objects the device refused to describe, `NotADirectory` when a segment before the
/// last is a file, `Cancelled` once the token is set, or the first listing error.
pub(super) async fn locate_root(
    storage: &Storage,
    root: &DevicePath,
    cancel: &CancelToken,
) -> Result<Target> {
    let mut folder = None;
    let Some((last, folders)) = root.path.segments().split_last() else {
        return Ok(Target { folder, file: None });
    };
    for segment in folders {
        let found = find_child(storage, folder, segment, root, cancel).await?;
        if !found.is_folder() {
            return Err(Error::NotADirectory(root.clone()));
        }
        folder = Some(found.handle);
    }
    let found = find_child(storage, folder, last, root, cancel).await?;
    Ok(Target::found(folder, found))
}

/// Streams `parent` and stops at the first object named `name`, so a hit early in a large
/// folder costs only the metadata reads before it; the first match wins, as `classify` decides.
async fn find_child(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    name: &str,
    root: &DevicePath,
    cancel: &CancelToken,
) -> Result<ObjectInfo> {
    let mut listing = storage
        .list_objects_stream_with_cancel(parent, Some(cancel))
        .await?;
    let mut skipped = 0;
    while let Some(item) = listing.next().await {
        match item? {
            ListingItem::Object(info) if info.filename == name => return Ok(info),
            ListingItem::Object(_) => {}
            ListingItem::Skipped(object) => {
                tracing::warn!(handle = object.handle.0, error = %object.error, "device could not describe an object while locating the root");
                skipped += 1;
            }
        }
    }
    Err(not_found(root, skipped))
}

fn not_found(root: &DevicePath, skipped: usize) -> Error {
    if skipped == 0 {
        return Error::RemotePathNotFound(root.clone());
    }
    Error::RemotePathUndescribed {
        path: root.clone(),
        skipped,
    }
}
