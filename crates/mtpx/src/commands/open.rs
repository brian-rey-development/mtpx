//! Opening the device the flags name, with a picker when several are attached.

use crate::{
    cli::Global,
    commands::UiOptions,
    ui::prompt::{self, Choice},
};
use mtpx_core::{Device, DeviceSelector, Error, Result};

/// Opens the device `--device` names, or the picked one when ambiguous; `--virtual` bypasses USB.
pub async fn open_device(global: &Global, ui: UiOptions) -> Result<Device> {
    #[cfg(feature = "virtual-device")]
    if let Some(dir) = &global.r#virtual {
        let config = virtual_config(dir.clone(), global.virtual_refuse.clone());
        return Device::open_virtual(config).await;
    }
    match Device::open(&global.device_selector()).await {
        Err(Error::AmbiguousDevice(devices)) if ui.prompts => match prompt::pick_device(&devices) {
            Choice::Picked(index) => Device::open(&DeviceSelector::Index(index)).await,
            Choice::Dismissed => Err(Error::AmbiguousDevice(devices)),
            Choice::Interrupted => Err(Error::Cancelled),
        },
        opened => opened,
    }
}

#[cfg(feature = "virtual-device")]
fn virtual_config(
    backing_dir: std::path::PathBuf,
    undescribable_objects: Vec<String>,
) -> mtpx_core::VirtualDeviceConfig {
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
        undescribable_objects,
        ..Default::default()
    }
}
