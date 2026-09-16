//! Turns a core error into a miette report with the help text the situation calls for.

use crate::ui::{
    format,
    prompt::{device_item, storage_item},
};
use mtpx_core::{DevicePath, DeviceSummary, Error, ExclusiveHolder, RelPath, StorageSummary};

const NO_DEVICE_HELP: &str =
    "Unlock the phone and choose File transfer / MTP in its USB notification.";
const EXCLUSIVE_ACCESS_MACOS_HELP: &str = "macOS claims MTP devices for Image Capture through \
     ptpcamerad. Run `pkill ptpcamerad` and retry. If Android File Transfer is installed, quit it.";
const EXCLUSIVE_ACCESS_OTHER_HELP: &str = "Quit the application using the phone (Android File Transfer, a file manager, gphoto2) and \
     retry.";
const PERMISSION_DENIED_HELP: &str = "On Linux, allow your user to open the device with a udev \
     rule, then replug the phone:\n  SUBSYSTEM==\"usb\", ATTR{idVendor}==\"xxxx\", MODE=\"0666\"\n\
     `mtpx devices` shows the vendor id in its ID column.";
const AMBIGUOUS_DEVICE_HELP: &str = "Pick one with --device <SERIAL|INDEX>:";
const STORAGE_REQUIRED_HELP: &str = "Pick one with --storage <NAME|INDEX>:";
const NO_STORAGE_HELP: &str = "The device exposes no storage; unlock the phone, then retry.";
const CONFLICTS_HELP: &str = "use --overwrite to replace them or --skip-existing to leave them";
const LENGTH_MISMATCH_HELP: &str = "The partial was kept; the next run resumes it.";
const NOT_A_DIRECTORY_HELP: &str =
    "ls lists directories; to see one file, list its parent directory.";
const MAX_LISTED_CONFLICTS: usize = 20;

/// The report `main` prints for `error`, with help attached where the spec defines some.
pub fn report(error: &Error) -> miette::Report {
    help(error).map_or_else(
        || miette::miette!("{error}"),
        |help| miette::miette!(help = help, "{error}"),
    )
}

fn help(error: &Error) -> Option<String> {
    match error {
        Error::NoDevice => Some(NO_DEVICE_HELP.to_owned()),
        Error::ExclusiveAccess { holder } => Some(exclusive_access(holder.as_ref())),
        Error::PermissionDenied => Some(PERMISSION_DENIED_HELP.to_owned()),
        Error::AmbiguousDevice(devices) => Some(ambiguous_device(devices)),
        Error::StorageRequired(storages) => Some(storage_required(storages)),
        Error::Conflicts(paths) => Some(conflicts(paths)),
        Error::LengthMismatch { .. } => Some(LENGTH_MISMATCH_HELP.to_owned()),
        Error::RemotePathNotFound(path) => Some(remote_path_not_found(path)),
        Error::NotADirectory(_) => Some(NOT_A_DIRECTORY_HELP.to_owned()),
        _ => None,
    }
}

fn exclusive_access(holder: Option<&ExclusiveHolder>) -> String {
    let platform = if cfg!(target_os = "macos") {
        EXCLUSIVE_ACCESS_MACOS_HELP
    } else {
        EXCLUSIVE_ACCESS_OTHER_HELP
    };
    holder.map_or_else(
        || platform.to_owned(),
        |holder| {
            format!(
                "{} (pid {}) holds the device.\n{platform}",
                holder.name, holder.pid
            )
        },
    )
}

fn ambiguous_device(devices: &[DeviceSummary]) -> String {
    let lines = devices
        .iter()
        .enumerate()
        .map(|(index, device)| format!("\n  {index}: {}", device_item(device)));
    format!("{AMBIGUOUS_DEVICE_HELP}{}", lines.collect::<String>())
}

fn storage_required(storages: &[StorageSummary]) -> String {
    if storages.is_empty() {
        return NO_STORAGE_HELP.to_owned();
    }
    let lines = storages
        .iter()
        .map(|storage| format!("\n  {}: {}", storage.index, storage_item(storage)));
    format!("{STORAGE_REQUIRED_HELP}{}", lines.collect::<String>())
}

fn conflicts(paths: &[RelPath]) -> String {
    let mut lines = vec![conflicts_heading(paths.len())];
    lines.extend(
        paths
            .iter()
            .take(MAX_LISTED_CONFLICTS)
            .map(|path| format!("  {path}")),
    );
    let more = paths.len().saturating_sub(MAX_LISTED_CONFLICTS);
    if more > 0 {
        lines.push(format!("  ... and {more} more"));
    }
    lines.push(CONFLICTS_HELP.to_owned());
    lines.join("\n")
}

fn conflicts_heading(count: usize) -> String {
    let verb = if count == 1 { "differs" } else { "differ" };
    format!("{} {verb}:", format::files(count as u64))
}

fn remote_path_not_found(path: &DevicePath) -> String {
    let parent = DevicePath {
        storage: path.storage.clone(),
        path: path
            .path
            .parent()
            .unwrap_or_else(mtpx_core::RemotePath::root),
    };
    format!("Check the name with `mtpx ls {parent}`.")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn device(label: &str, serial: Option<&str>) -> DeviceSummary {
        DeviceSummary {
            serial: serial.map(str::to_owned),
            label: label.to_owned(),
            vendor_id: 1,
            product_id: 2,
            location_id: 3,
            speed: None,
        }
    }

    fn storage(index: usize, name: &str) -> StorageSummary {
        StorageSummary {
            index,
            name: name.to_owned(),
            free: 1_000,
            total: 2_000,
        }
    }

    #[test]
    fn conflicts_help_counts_the_files_then_lists_them_then_names_both_flags() {
        let paths = vec![
            RelPath::new(["a.jpg"]).unwrap(),
            RelPath::new(["b.jpg"]).unwrap(),
        ];
        let text = help(&Error::Conflicts(paths)).unwrap();
        assert_eq!(
            text,
            "2 files differ:\n  a.jpg\n  b.jpg\n\
             use --overwrite to replace them or --skip-existing to leave them"
        );
    }

    #[test]
    fn conflicts_help_reads_singular_for_one_file() {
        let text = help(&Error::Conflicts(vec![RelPath::new(["a.jpg"]).unwrap()])).unwrap();
        assert!(text.starts_with("1 file differs:\n  a.jpg\n"), "{text}");
    }

    #[test]
    fn conflicts_help_caps_the_list() {
        let paths = (0..25)
            .map(|i| RelPath::new([format!("{i}.jpg")]).unwrap())
            .collect();
        let text = help(&Error::Conflicts(paths)).unwrap();
        assert!(text.starts_with("25 files differ:\n  0.jpg\n"), "{text}");
        assert!(
            text.contains("  19.jpg\n  ... and 5 more\nuse --overwrite"),
            "{text}"
        );
        assert!(!text.contains("  20.jpg"), "{text}");
    }

    #[test]
    fn ambiguous_device_help_lists_indexes_and_the_flag() {
        let devices = vec![device("Pixel", Some("ZY22")), device("Moto", None)];
        let text = help(&Error::AmbiguousDevice(devices)).unwrap();
        assert!(text.contains("--device"), "{text}");
        assert!(text.contains("\n  0: Pixel (ZY22)\n  1: Moto"), "{text}");
    }

    #[test]
    fn storage_required_help_lists_storages_or_explains_none() {
        let text = help(&Error::StorageRequired(vec![storage(0, "Internal")])).unwrap();
        assert!(text.contains("--storage"), "{text}");
        assert!(text.contains("0: Internal (1 kB free of 2 kB)"), "{text}");
        let none = help(&Error::StorageRequired(vec![])).unwrap();
        assert_eq!(none, NO_STORAGE_HELP);
    }

    #[test]
    fn remote_path_not_found_points_at_the_parent_listing() {
        let path: DevicePath = "sd:/DCIM/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(path)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls sd:/DCIM`.");
        let root: DevicePath = "/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(root)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls /`.");
    }

    #[test]
    fn not_a_directory_points_at_the_parent() {
        let path: DevicePath = "/DCIM/a.jpg".parse().unwrap();
        let text = help(&Error::NotADirectory(path)).unwrap();
        assert_eq!(text, NOT_A_DIRECTORY_HELP);
    }

    #[test]
    fn errors_without_advice_get_no_help() {
        assert_eq!(help(&Error::Cancelled), None);
        assert_eq!(help(&Error::Disconnected), None);
    }

    #[test]
    fn report_carries_the_message_and_the_help() {
        let rendered = format!("{:?}", report(&Error::NoDevice));
        assert!(rendered.contains("no MTP device found"), "{rendered}");
        assert!(rendered.contains("Unlock the phone"), "{rendered}");
    }
}
