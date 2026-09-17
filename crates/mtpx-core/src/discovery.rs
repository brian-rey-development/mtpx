//! Plain descriptions of devices and storages, as listed before a session is opened.

use crate::{
    UsbSpeed,
    error::{Error, Result},
};
use mtp_rs::{MtpDevice, mtp::MtpDeviceInfo};

/// One MTP device visible on the USB bus.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceSummary {
    /// USB serial number, when the device exposes one.
    pub serial: Option<String>,
    /// Human-readable name built from manufacturer and product strings.
    pub label: String,
    /// `idVendor` from the USB device descriptor.
    pub vendor_id: u16,
    /// `idProduct` from the USB device descriptor.
    pub product_id: u16,
    /// Bus location, stable while the device stays plugged into the same port.
    pub location_id: u64,
    /// Negotiated link speed, when the OS reports it.
    pub speed: Option<UsbSpeed>,
}

/// One storage on a device, as reported by `GetStorageInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
#[non_exhaustive]
pub struct ExclusiveHolder {
    /// OS process id of the holder.
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

impl From<&str> for DeviceSelector {
    /// Digits select by index; anything else is a USB serial.
    fn from(value: &str) -> Self {
        value
            .parse()
            .map_or_else(|_| Self::Serial(value.to_owned()), Self::Index)
    }
}

/// Lists attached MTP devices without opening any of them.
///
/// The order is the one the USB stack reports, so [`DeviceSelector::Index`] is stable within
/// a process.
///
/// # Errors
/// `Error::Mtp` when the USB bus cannot be enumerated.
pub fn list_devices() -> Result<Vec<DeviceSummary>> {
    let devices = MtpDevice::list_devices()?;
    Ok(devices.into_iter().map(DeviceSummary::from_mtp).collect())
}

impl DeviceSummary {
    /// Describes one attached device; `label` is what a listing shows for it.
    #[must_use]
    pub const fn new(
        serial: Option<String>,
        label: String,
        vendor_id: u16,
        product_id: u16,
        location_id: u64,
        speed: Option<UsbSpeed>,
    ) -> Self {
        Self {
            serial,
            label,
            vendor_id,
            product_id,
            location_id,
            speed,
        }
    }

    pub(crate) fn from_mtp(info: MtpDeviceInfo) -> Self {
        let label = device_label(
            info.manufacturer.as_deref(),
            info.product.as_deref(),
            info.vendor_id,
            info.product_id,
        );
        Self::new(
            info.serial_number,
            label,
            info.vendor_id,
            info.product_id,
            info.location_id,
            info.speed,
        )
    }
}

impl StorageSummary {
    /// Describes one storage; `index` is its position in the device's enumeration order.
    #[must_use]
    pub const fn new(index: usize, name: String, free: u64, total: u64) -> Self {
        Self {
            index,
            name,
            free,
            total,
        }
    }
}

impl ExclusiveHolder {
    /// Names the process that holds the device.
    #[must_use]
    pub fn new(pid: u32, name: impl Into<String>) -> Self {
        Self {
            pid,
            name: name.into(),
        }
    }
}

/// Picks the device `selector` names out of a listing.
///
/// # Errors
/// `NoDevice` when nothing is attached, `DeviceNotFound` when `selector` matches none of the
/// attached devices, `AmbiguousDevice` when `Only` finds several.
pub(crate) fn select_device(
    mut devices: Vec<DeviceSummary>,
    selector: &DeviceSelector,
) -> Result<DeviceSummary> {
    if devices.is_empty() {
        return Err(Error::NoDevice);
    }
    let (position, wanted) = match selector {
        DeviceSelector::Only => return select_only(devices),
        DeviceSelector::Serial(serial) => (position_of_serial(&devices, serial), serial.clone()),
        DeviceSelector::Index(index) => (
            (*index < devices.len()).then_some(*index),
            index.to_string(),
        ),
    };
    match position {
        Some(position) => Ok(devices.swap_remove(position)),
        None => Err(Error::DeviceNotFound {
            selector: wanted,
            available: devices,
        }),
    }
}

fn position_of_serial(devices: &[DeviceSummary], serial: &str) -> Option<usize> {
    devices
        .iter()
        .position(|device| device.serial.as_deref() == Some(serial))
}

fn select_only(mut devices: Vec<DeviceSummary>) -> Result<DeviceSummary> {
    if devices.len() == 1 {
        return Ok(devices.remove(0));
    }
    Err(Error::AmbiguousDevice(devices))
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
        DeviceSummary::new(
            serial.map(str::to_owned),
            "Google Pixel".into(),
            VENDOR,
            PRODUCT,
            7,
            None,
        )
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
    fn serial_and_index_pick_one_device() {
        let devices = vec![summary(None), summary(Some("B"))];
        let by_serial =
            select_device(devices.clone(), &DeviceSelector::Serial("B".into())).unwrap();
        assert_eq!(by_serial.serial.as_deref(), Some("B"));
        let by_index = select_device(devices, &DeviceSelector::Index(0)).unwrap();
        assert_eq!(by_index.serial, None);
    }

    #[test]
    fn a_selector_matching_no_attached_device_lists_what_is_attached() {
        let devices = vec![summary(None), summary(Some("B"))];
        let cases = [
            (DeviceSelector::Serial("C".into()), "C"),
            (DeviceSelector::Index(2), "2"),
        ];
        for (selector, shown) in cases {
            let err = select_device(devices.clone(), &selector).unwrap_err();
            let Error::DeviceNotFound {
                selector: named,
                available,
            } = err
            else {
                panic!("{selector:?}: {err:?}");
            };
            assert_eq!(named, shown);
            assert_eq!(available, devices);
        }
    }

    #[test]
    fn an_empty_bus_is_no_device_whatever_the_selector() {
        let selectors = [
            DeviceSelector::Only,
            DeviceSelector::Serial("A".into()),
            DeviceSelector::Index(0),
        ];
        for selector in selectors {
            let err = select_device(vec![], &selector).unwrap_err();
            assert!(matches!(err, Error::NoDevice), "{selector:?}: {err:?}");
        }
    }
}
