//! Fixtures the UI unit tests share.

use mtpx_core::{DeviceSummary, UsbSpeed};

const VENDOR_ID: u16 = 0x18d1;
const PRODUCT_ID: u16 = 0x4ee1;
const LOCATION_ID: u64 = 1;

pub fn device(label: &str, serial: Option<&str>, speed: Option<UsbSpeed>) -> DeviceSummary {
    DeviceSummary::new(
        serial.map(str::to_owned),
        label.to_owned(),
        VENDOR_ID,
        PRODUCT_ID,
        LOCATION_ID,
        speed,
    )
}
