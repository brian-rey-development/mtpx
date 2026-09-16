//! `mtpx devices`: the listing, without opening anything.

use crate::{commands::Outcome, ui::table};
use mtpx_core::{Error, Result, list_devices};

/// Prints every attached device; an empty bus is the `NoDevice` error so the exit code says so.
pub fn run() -> Result<Outcome> {
    let devices = list_devices()?;
    if devices.is_empty() {
        return Err(Error::NoDevice);
    }
    print!("{}", table::devices(&devices));
    Ok(Outcome::Done)
}
