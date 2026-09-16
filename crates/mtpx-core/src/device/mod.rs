//! The open-device facade: the one entry point for listing a phone and pulling files off it.

mod pull;

pub use pull::PullJob;

use crate::{
    device_path::{DevicePath, StorageSelector},
    discovery::{self, DeviceSelector, DeviceSummary, StorageSummary},
    entry::Snapshot,
    error::{Error, Result},
    internal::{endpoint::Endpoint, mtp::MtpEndpoint},
};
use mtp_rs::{CancelToken, MtpDevice, Storage, StorageInfo};
use std::{fmt, sync::Arc};

/// An open MTP device: the entry point for listing and transfers.
pub struct Device {
    inner: MtpDevice,
    storages: Vec<Arc<Storage>>,
    summary: DeviceSummary,
    // Declared last so an implicit drop releases the session before the device leaves discovery.
    registration: Option<VirtualRegistration>,
}

/// Keeps a virtual device visible to [`discovery::list_devices`] for as long as the `Device` lives.
#[cfg(feature = "virtual-device")]
struct VirtualRegistration(u64);

/// Without the feature no registration can exist, so the `Option` is always `None`.
#[cfg(not(feature = "virtual-device"))]
enum VirtualRegistration {}

#[cfg(feature = "virtual-device")]
impl Drop for VirtualRegistration {
    fn drop(&mut self) {
        mtp_rs::unregister_virtual_device(self.0);
    }
}

impl Device {
    /// Opens the device `selector` picks out of [`discovery::list_devices`] and reads its storages.
    ///
    /// # Errors
    /// `NoDevice` when nothing matches, `AmbiguousDevice` when `Only` finds several,
    /// `ExclusiveAccess` or `PermissionDenied` when the OS refuses the USB interface,
    /// `Disconnected` when the device was unplugged between listing and opening.
    pub async fn open(selector: &DeviceSelector) -> Result<Self> {
        let summary = discovery::select_device(discovery::list_devices()?, selector)?;
        let opened = match summary.serial.as_deref() {
            Some(serial) if !serial.is_empty() => MtpDevice::open_by_serial(serial).await,
            _ => MtpDevice::open_by_location(summary.location_id).await,
        };
        Self::load(opened.map_err(Error::from_mtp)?, summary).await
    }

    /// Opens an in-process virtual device backed by local directories and lists it as attached
    /// until it is closed or dropped.
    ///
    /// # Errors
    /// `Error::Mtp` when the configuration declares no storage or a session cannot be opened.
    #[cfg(feature = "virtual-device")]
    pub async fn open_virtual(config: mtp_rs::VirtualDeviceConfig) -> Result<Self> {
        // Registered first so the guard exists before anything can fail and cleanup is automatic.
        let info = mtp_rs::register_virtual_device(&config);
        let registration = VirtualRegistration(info.location_id);
        let opened = MtpDevice::builder().open_virtual(config).await;
        let summary = DeviceSummary::from_mtp(info);
        let mut device = Self::load(opened.map_err(Error::from_mtp)?, summary).await?;
        device.registration = Some(registration);
        Ok(device)
    }

    async fn load(inner: MtpDevice, summary: DeviceSummary) -> Result<Self> {
        let storages = inner.storages().await.map_err(Error::from_mtp)?;
        Ok(Self {
            inner,
            storages: storages.into_iter().map(Arc::new).collect(),
            summary,
            registration: None,
        })
    }

    /// The listing entry this device was opened from.
    #[must_use]
    pub const fn summary(&self) -> &DeviceSummary {
        &self.summary
    }

    /// Human-readable name, such as "Google Pixel 9".
    #[must_use]
    pub fn label(&self) -> String {
        self.summary.label.clone()
    }

    /// Serial number the device reports in its MTP `DeviceInfo`.
    ///
    /// May differ from [`DeviceSummary::serial`], which is the USB descriptor serial and can be
    /// absent.
    #[must_use]
    pub fn serial(&self) -> &str {
        &self.inner.device_info().serial_number
    }

    /// Every storage on the device, in enumeration order.
    #[must_use]
    pub fn storages(&self) -> Vec<StorageSummary> {
        self.storages
            .iter()
            .enumerate()
            .map(|(index, storage)| storage_summary(index, storage.info()))
            .collect()
    }

    /// Lists `path`: its immediate children, or the whole subtree sorted by path when `recursive`.
    ///
    /// # Errors
    /// `RemotePathNotFound` or `NotADirectory` for the path, `StorageRequired` or
    /// `StorageNotFound` for the storage, `Cancelled` once the token is set.
    pub async fn ls(
        &self,
        path: &DevicePath,
        recursive: bool,
        cancel: &CancelToken,
    ) -> Result<Snapshot> {
        let endpoint = self.endpoint(path).await?;
        if !recursive {
            return endpoint.list(cancel).await;
        }
        let scanned = endpoint.scan(cancel, Arc::new(|_| {})).await?;
        Ok(scanned.snapshot)
    }

    /// Closes the MTP session best-effort. Dropping a `Device` without calling this skips the
    /// `CloseSession` command, which some devices need before they leave MTP mode.
    ///
    /// # Errors
    /// Currently never fails; the `Result` exists so a future backend can report a failed
    /// `CloseSession`.
    pub async fn close(self) -> Result<()> {
        self.inner.close().await.map_err(Error::from_mtp)
    }

    async fn endpoint(&self, path: &DevicePath) -> Result<MtpEndpoint> {
        let storage = self.select_storage(&path.storage)?;
        MtpEndpoint::open(storage, path.path.clone(), self.serial()).await
    }

    fn select_storage(&self, selector: &StorageSelector) -> Result<Arc<Storage>> {
        let found = match selector {
            StorageSelector::Default => self.only_storage()?,
            StorageSelector::Index(index) => self
                .storages
                .get(*index)
                .ok_or_else(|| Error::StorageNotFound(index.to_string()))?,
            StorageSelector::Named(name) => self
                .storages
                .iter()
                .find(|storage| is_named(storage.info(), name))
                .ok_or_else(|| Error::StorageNotFound(name.clone()))?,
        };
        Ok(Arc::clone(found))
    }

    fn only_storage(&self) -> Result<&Arc<Storage>> {
        match self.storages.as_slice() {
            [only] => Ok(only),
            _ => Err(Error::StorageRequired(self.storages())),
        }
    }
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("summary", &self.summary)
            .field("storages", &self.storages())
            .field("virtual", &self.registration.is_some())
            .finish_non_exhaustive()
    }
}

fn storage_summary(index: usize, info: &StorageInfo) -> StorageSummary {
    StorageSummary {
        index,
        name: info.description.clone(),
        free: info.free_space,
        total: info.total_capacity,
    }
}

// Devices expose a friendly description or a stable volume id; users need not know which.
fn is_named(info: &StorageInfo, name: &str) -> bool {
    let wanted = name.to_lowercase();
    info.description.to_lowercase() == wanted || info.volume_identifier.to_lowercase() == wanted
}

#[cfg(all(test, feature = "virtual-device"))]
pub mod test_support {
    #![allow(clippy::unwrap_used)]

    use super::Device;
    use crate::{device_path::DevicePath, event::ProgressEvent};
    use mtp_rs::{VirtualDeviceConfig, VirtualStorageConfig};
    use std::{path::Path, time::Duration};
    use tempfile::TempDir;
    use tokio::sync::mpsc::{self, Receiver, Sender};

    pub const FIRST_STORAGE: &str = "Internal Storage";
    pub const SECOND_STORAGE: &str = "Second";
    pub const LABEL: &str = "mtpx Virtual Phone";
    const CAPACITY: u64 = 1024 * 1024 * 1024;
    const EVENTS_CAPACITY: usize = 1024;

    /// An open virtual device plus the directories backing its storages, in storage order.
    pub struct Fixture {
        pub device: Device,
        pub dirs: Vec<TempDir>,
        pub serial: String,
    }

    impl Fixture {
        pub fn root(&self) -> &Path {
            self.dirs[0].path()
        }
    }

    pub async fn open_device(test_name: &str) -> Fixture {
        open_with(test_name, &[FIRST_STORAGE]).await
    }

    pub async fn open_two_storage_device(test_name: &str) -> Fixture {
        open_with(test_name, &[FIRST_STORAGE, SECOND_STORAGE]).await
    }

    async fn open_with(test_name: &str, descriptions: &[&str]) -> Fixture {
        let serial = format!("mtpx-device-{test_name}");
        let dirs: Vec<TempDir> = descriptions
            .iter()
            .map(|_| tempfile::tempdir().unwrap())
            .collect();
        let storages = descriptions
            .iter()
            .zip(&dirs)
            .map(|(description, dir)| storage(description, dir.path()))
            .collect();
        let device = Device::open_virtual(config(&serial, storages))
            .await
            .unwrap();
        Fixture {
            device,
            dirs,
            serial,
        }
    }

    fn storage(description: &str, backing_dir: &Path) -> VirtualStorageConfig {
        VirtualStorageConfig {
            description: description.to_owned(),
            capacity: CAPACITY,
            backing_dir: backing_dir.to_path_buf(),
            read_only: false,
        }
    }

    fn config(serial: &str, storages: Vec<VirtualStorageConfig>) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "mtpx".into(),
            model: "Virtual Phone".into(),
            serial: serial.to_owned(),
            storages,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
            ..Default::default()
        }
    }

    pub fn device_path(input: &str) -> DevicePath {
        input.parse().unwrap()
    }

    pub fn events() -> (Sender<ProgressEvent>, Receiver<ProgressEvent>) {
        mpsc::channel(EVENTS_CAPACITY)
    }

    pub fn drain(rx: &mut Receiver<ProgressEvent>) -> Vec<ProgressEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = rx.try_recv() {
            drained.push(event);
        }
        drained
    }
}

#[cfg(all(test, feature = "virtual-device"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::{Device, test_support::*};
    use crate::{
        discovery::{DeviceSelector, DeviceSummary, list_devices},
        entry::{EntryKind, Snapshot},
        error::Error,
        internal::mtp::test_support::{rel, seed_tree},
        options::TransferOptions,
    };
    use mtp_rs::CancelToken;
    use std::fs;

    fn listed(serial: &str) -> Option<DeviceSummary> {
        list_devices()
            .unwrap()
            .into_iter()
            .find(|device| device.serial.as_deref() == Some(serial))
    }

    fn paths(snapshot: &Snapshot) -> Vec<String> {
        snapshot
            .entries()
            .iter()
            .map(|entry| entry.path.to_string())
            .collect()
    }

    #[tokio::test]
    async fn list_devices_includes_the_open_virtual_device_with_its_serial_and_label() {
        let fixture = open_device("list").await;
        let summary = listed(&fixture.serial).unwrap();
        assert_eq!(summary.label, LABEL);
        assert_eq!(summary.speed, None);
        assert_eq!(&summary, fixture.device.summary());
        assert_eq!(fixture.device.label(), LABEL);
        assert_eq!(fixture.device.serial(), fixture.serial);
    }

    #[tokio::test]
    async fn open_by_serial_opens_a_second_session_on_the_registered_virtual_device() {
        let fixture = open_device("open-by-serial").await;
        let selector = DeviceSelector::Serial(fixture.serial.clone());
        let second = Device::open(&selector).await.unwrap();
        assert_eq!(second.serial(), fixture.serial);
        assert_eq!(second.summary(), fixture.device.summary());
        assert_eq!(second.storages(), fixture.device.storages());
        second.close().await.unwrap();
        assert!(
            listed(&fixture.serial).is_some(),
            "only the session that registered the device unlists it"
        );
    }

    #[tokio::test]
    async fn open_by_an_unknown_serial_is_no_device() {
        let selector = DeviceSelector::Serial("mtpx-device-unknown".into());
        let err = Device::open(&selector).await.unwrap_err();
        assert!(matches!(err, Error::NoDevice), "{err:?}");
    }

    #[tokio::test]
    async fn storages_reports_index_name_and_capacity() {
        let fixture = open_device("storages").await;
        let storages = fixture.device.storages();
        assert_eq!(storages.len(), 1);
        assert_eq!(storages[0].index, 0);
        assert_eq!(storages[0].name, FIRST_STORAGE);
        assert!(storages[0].total > 0);
        assert!(storages[0].free <= storages[0].total);
    }

    #[tokio::test]
    async fn a_device_with_two_storages_needs_one_named_or_indexed() {
        let fixture = open_two_storage_device("two-storages").await;
        fs::write(fixture.dirs[1].path().join("only-here.txt"), b"x").unwrap();
        let local = tempfile::tempdir().unwrap();
        let (tx, _rx) = events();
        let cancel = CancelToken::new();
        let opts = TransferOptions::pull();
        let err = fixture
            .device
            .plan_pull(&device_path("/"), local.path(), &opts, &cancel, &tx)
            .await
            .unwrap_err();
        let Error::StorageRequired(storages) = err else {
            panic!("{err:?}");
        };
        assert_eq!(storages, fixture.device.storages());
        assert_eq!(storages.len(), 2);
        let job = fixture
            .device
            .plan_pull(&device_path("second:/"), local.path(), &opts, &cancel, &tx)
            .await
            .unwrap();
        assert_eq!(job.plan().summary().files_to_copy, 1);
        let by_index = fixture
            .device
            .ls(&device_path("1:/"), false, &cancel)
            .await
            .unwrap();
        assert_eq!(paths(&by_index), vec!["only-here.txt"]);
        for (input, expected) in [("5:/", "5"), ("nope:/", "nope")] {
            let err = fixture
                .device
                .ls(&device_path(input), false, &cancel)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::StorageNotFound(name) if name == expected),
                "{input}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn ls_lists_the_top_level_or_the_whole_tree_sorted() {
        let fixture = open_device("ls").await;
        seed_tree(fixture.root());
        let cancel = CancelToken::new();
        let top = fixture
            .device
            .ls(&device_path("/"), false, &cancel)
            .await
            .unwrap();
        assert_eq!(paths(&top), vec!["DCIM", "Empty", "Music"]);
        assert!(top.entries().iter().all(|e| e.kind == EntryKind::Dir));
        let camera = fixture
            .device
            .ls(&device_path("/DCIM/Camera"), false, &cancel)
            .await
            .unwrap();
        assert_eq!(paths(&camera), vec!["a.jpg", "sub"]);
        let all = fixture
            .device
            .ls(&device_path("/"), true, &cancel)
            .await
            .unwrap();
        assert_eq!(
            paths(&all),
            vec![
                "DCIM",
                "DCIM/Camera",
                "DCIM/Camera/a.jpg",
                "DCIM/Camera/sub",
                "DCIM/Camera/sub/b.jpg",
                "DCIM/photo.jpg",
                "Empty",
                "Music",
                "Music/c.mp3",
            ]
        );
        assert_eq!(all.get(&rel("DCIM/photo.jpg")).unwrap().size, 5);
    }

    #[tokio::test]
    async fn ls_of_a_missing_path_or_a_file_fails_with_the_offending_path() {
        let fixture = open_device("ls-errors").await;
        seed_tree(fixture.root());
        let cancel = CancelToken::new();
        let err = fixture
            .device
            .ls(&device_path("/DCIM/Nope"), false, &cancel)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::RemotePathNotFound(path) if path.path.to_string() == "/DCIM/Nope"),
            "{err:?}"
        );
        let err = fixture
            .device
            .ls(&device_path("/DCIM/photo.jpg"), true, &cancel)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::NotADirectory(path) if path.path.to_string() == "/DCIM/photo.jpg"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn close_succeeds_and_unlists_the_virtual_device() {
        let fixture = open_device("close").await;
        assert!(listed(&fixture.serial).is_some());
        fixture.device.close().await.unwrap();
        assert!(listed(&fixture.serial).is_none());
    }
}
