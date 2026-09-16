//! Opening the device the flags name, with a picker when several are attached.

use crate::{cli::Global, commands::UiOptions, ui::prompt};
use mtpx_core::{Device, DeviceSelector, Error, Result};

/// Opens the device `--device` names, or the one the user picks when the choice is ambiguous
/// and a prompt is possible; `--virtual` bypasses USB entirely.
pub async fn open_device(global: &Global, ui: UiOptions) -> Result<Device> {
    #[cfg(feature = "virtual-device")]
    if let Some(dir) = &global.r#virtual {
        return Device::open_virtual(virtual_config(dir.clone())).await;
    }
    match Device::open(&global.device_selector()).await {
        Err(Error::AmbiguousDevice(devices)) if ui.prompts => {
            let Some(index) = prompt::pick_device(&devices) else {
                return Err(Error::AmbiguousDevice(devices));
            };
            Device::open(&DeviceSelector::Index(index)).await
        }
        opened => opened,
    }
}

#[cfg(feature = "virtual-device")]
fn virtual_config(backing_dir: std::path::PathBuf) -> mtpx_core::VirtualDeviceConfig {
    const CAPACITY: u64 = 64 * 1024 * 1024 * 1024;
    let storage = mtpx_core::VirtualStorageConfig {
        description: "Internal".into(),
        capacity: CAPACITY,
        backing_dir,
        read_only: false,
    };
    mtpx_core::VirtualDeviceConfig {
        manufacturer: "mtpx".into(),
        model: "Virtual Phone".into(),
        serial: "VIRTUAL".into(),
        storages: vec![storage],
        event_poll_interval: std::time::Duration::ZERO,
        watch_backing_dirs: false,
        ..Default::default()
    }
}
