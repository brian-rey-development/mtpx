//! Plain descriptions of devices and storages, as listed before a session is opened.

use crate::error::{Error, Result};
use mtp_rs::{MtpDevice, mtp::MtpDeviceInfo};

/// One MTP device visible on the USB bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    /// USB serial number, when the device exposes one.
    pub serial: Option<String>,
    /// Human-readable name built from manufacturer and product strings.
    pub label: String,
    /// USB vendor id.
    pub vendor_id: u16,
    /// USB product id.
    pub product_id: u16,
    /// Bus location, stable while the device stays plugged into the same port.
    pub location_id: u64,
    /// Negotiated link speed, when the OS reports it.
    pub speed: Option<mtp_rs::UsbSpeed>,
}

/// One storage on a device, as reported by `GetStorageInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSummary {
    /// Position in the device's enumeration order; what `StorageSelector::Index` refers to.
    pub index: usize,
    /// Description the device gives, such as "Internal shared storage".
    pub name: String,
    /// Free bytes.
    pub free: u64,
    /// Capacity in bytes.
    pub total: u64,
}

/// The process holding the device open exclusively, when it can be identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusiveHolder {
    /// Process id.
    pub pid: u32,
    /// Process name, such as "Android File Transfer".
    pub name: String,
}

/// Which device to open when more than one may be attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSelector {
    /// The only attached device; an error when there are none or several.
    Only,
    /// The device with this USB serial number.
    Serial(String),
    /// The device at this position in [`list_devices`].
    Index(usize),
}

/// Lists attached MTP devices without opening any of them.
///
/// The order is the one the USB stack reports, so [`DeviceSelector::Index`] is stable within
/// a process.
///
/// # Errors
/// `Error::Mtp` when the USB bus cannot be enumerated.
pub fn list_devices() -> Result<Vec<DeviceSummary>> {
    let devices = MtpDevice::list_devices().map_err(Error::from_mtp)?;
    Ok(devices.into_iter().map(DeviceSummary::from_mtp).collect())
}

impl DeviceSummary {
    pub(crate) fn from_mtp(info: MtpDeviceInfo) -> Self {
        let label = device_label(
            info.manufacturer.as_deref(),
            info.product.as_deref(),
            info.vendor_id,
            info.product_id,
        );
        Self {
            serial: info.serial_number,
            label,
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            location_id: info.location_id,
            speed: info.speed,
        }
    }
}

/// Picks the device `selector` names out of a listing.
///
/// # Errors
/// `NoDevice` when nothing matches, `AmbiguousDevice` when `Only` finds several.
pub fn select_device(
    devices: Vec<DeviceSummary>,
    selector: &DeviceSelector,
) -> Result<DeviceSummary> {
    match selector {
        DeviceSelector::Only => select_only(devices),
        DeviceSelector::Serial(serial) => devices
            .into_iter()
            .find(|device| device.serial.as_deref() == Some(serial))
            .ok_or(Error::NoDevice),
        DeviceSelector::Index(index) => devices.into_iter().nth(*index).ok_or(Error::NoDevice),
    }
}

fn select_only(mut devices: Vec<DeviceSummary>) -> Result<DeviceSummary> {
    match devices.len() {
        0 => Err(Error::NoDevice),
        1 => Ok(devices.remove(0)),
        _ => Err(Error::AmbiguousDevice(devices)),
    }
}

/// The USB manufacturer and product strings joined, or `vvvv:pppp` when the device reports neither.
fn device_label(
    manufacturer: Option<&str>,
    product: Option<&str>,
    vendor_id: u16,
    product_id: u16,
) -> String {
    let joined = [manufacturer, product]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let label = joined.trim();
    if label.is_empty() {
        return format!("{vendor_id:04x}:{product_id:04x}");
    }
    label.to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const VENDOR: u16 = 0x18d1;
    const PRODUCT: u16 = 0x4ee1;

    fn summary(serial: Option<&str>) -> DeviceSummary {
        DeviceSummary {
            serial: serial.map(str::to_owned),
            label: "Google Pixel".into(),
            vendor_id: VENDOR,
            product_id: PRODUCT,
            location_id: 7,
            speed: None,
        }
    }

    #[test]
    fn label_joins_manufacturer_and_product_and_falls_back_to_ids() {
        let cases = [
            (Some("Google"), Some("Pixel 9"), "Google Pixel 9"),
            (None, Some("Pixel 9"), "Pixel 9"),
            (Some("Google"), None, "Google"),
            (Some("  "), Some(" Pixel 9 "), "Pixel 9"),
            (None, None, "18d1:4ee1"),
            (Some(""), Some(""), "18d1:4ee1"),
        ];
        for (manufacturer, product, expected) in cases {
            let label = device_label(manufacturer, product, VENDOR, PRODUCT);
            assert_eq!(label, expected, "{manufacturer:?} {product:?}");
        }
    }

    #[test]
    fn only_needs_exactly_one_device() {
        let err = select_device(vec![], &DeviceSelector::Only).unwrap_err();
        assert!(matches!(err, Error::NoDevice), "{err:?}");
        let one = select_device(vec![summary(Some("A"))], &DeviceSelector::Only).unwrap();
        assert_eq!(one.serial.as_deref(), Some("A"));
        let two = vec![summary(Some("A")), summary(Some("B"))];
        let err = select_device(two.clone(), &DeviceSelector::Only).unwrap_err();
        let Error::AmbiguousDevice(listed) = err else {
            panic!("{err:?}");
        };
        assert_eq!(listed, two);
    }

    #[test]
    fn serial_and_index_pick_one_device_or_report_none() {
        let devices = vec![summary(None), summary(Some("B"))];
        let by_serial =
            select_device(devices.clone(), &DeviceSelector::Serial("B".into())).unwrap();
        assert_eq!(by_serial.serial.as_deref(), Some("B"));
        let by_index = select_device(devices.clone(), &DeviceSelector::Index(0)).unwrap();
        assert_eq!(by_index.serial, None);
        for selector in [DeviceSelector::Serial("C".into()), DeviceSelector::Index(2)] {
            let err = select_device(devices.clone(), &selector).unwrap_err();
            assert!(matches!(err, Error::NoDevice), "{selector:?}: {err:?}");
        }
    }
}
