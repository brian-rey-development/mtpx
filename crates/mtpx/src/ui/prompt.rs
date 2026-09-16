//! Pickers shown on stderr when a choice is needed and a terminal is there to make it.

use crate::ui::format;
use console::Term;
use dialoguer::Select;
use mtpx_core::{DeviceSummary, StorageSummary};

const DEVICE_PROMPT: &str = "Several devices are attached, pick one";
const STORAGE_PROMPT: &str = "The device has several storages, pick one";

/// Lets the user pick a device; `None` when they escape or the terminal refuses.
pub fn pick_device(devices: &[DeviceSummary]) -> Option<usize> {
    let items: Vec<String> = devices.iter().map(device_item).collect();
    select(DEVICE_PROMPT, &items)
}

/// Lets the user pick a storage; `None` when they escape or the terminal refuses.
pub fn pick_storage(storages: &[StorageSummary]) -> Option<usize> {
    let items: Vec<String> = storages.iter().map(storage_item).collect();
    select(STORAGE_PROMPT, &items)
}

/// One line per device, as the `AmbiguousDevice` help lists them too.
pub fn device_item(device: &DeviceSummary) -> String {
    match device.serial.as_deref() {
        Some(serial) if !serial.is_empty() => format!("{} ({serial})", device.label),
        _ => device.label.clone(),
    }
}

/// One line per storage, as the `StorageRequired` help lists them too.
pub fn storage_item(storage: &StorageSummary) -> String {
    format!(
        "{} ({} free of {})",
        storage.name,
        format::size(storage.free),
        format::size(storage.total)
    )
}

fn select(prompt: &str, items: &[String]) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    Select::new()
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact_on_opt(&Term::stderr())
        .ok()
        .flatten()
}
