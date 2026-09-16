//! Plain descriptions of devices and storages, as listed before a session is opened.

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
