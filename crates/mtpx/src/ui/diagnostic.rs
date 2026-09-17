//! Turns a core error into a miette report with the help text the situation calls for.

use crate::ui::format;
use mtpx_core::{
    DevicePath, DeviceSummary, Error, ExclusiveHolder, RelPath, RemotePath, StorageSummary,
    sanitize_for_display,
};

const NO_DEVICE_HELP: &str =
    "Unlock the phone and choose File transfer / MTP in its USB notification.";
const DEVICE_UNRESPONSIVE_HELP: &str = "Unlock the phone, open the USB notification and choose \
     File transfer (MTP). Charging-only mode answers nothing.";
const EXCLUSIVE_ACCESS_MACOS_HELP: &str = "macOS claims MTP devices for Image Capture through \
     ptpcamerad. Run `pkill ptpcamerad` and retry. If Android File Transfer is installed, quit it.";
const EXCLUSIVE_ACCESS_OTHER_HELP: &str = "Quit the application using the phone (Android File Transfer, a file manager, gphoto2) and \
     retry.";
const PERMISSION_DENIED_HELP: &str = "On Linux, allow your user to open the device with a udev \
     rule, then replug the phone:\n  SUBSYSTEM==\"usb\", ATTR{idVendor}==\"xxxx\", MODE=\"0666\"\n\
     `mtpx devices` shows the vendor id in its ID column.";
const PICK_DEVICE_HELP: &str = "Pick one with --device <SERIAL|INDEX>:";
const PICK_STORAGE_HELP: &str = "Pick one with --storage <NAME|INDEX>:";
const NO_STORAGE_HELP: &str = "Unlock the phone, then retry.";
const CONFLICTS_HELP: &str = "use --overwrite to replace them or --skip-existing to leave them";
const LENGTH_MISMATCH_HELP: &str = "The partial was kept; the next run resumes it.";
const NOT_A_DIRECTORY_HELP: &str = "The path names a file or passes through one; only \
     directories can be listed or walked. To see a single file, list its parent directory.";
const LOCAL_NOT_A_DIRECTORY_HELP: &str =
    "The local path must be a directory; pass one, or move the file out of the way.";
const REMOTE_PATH_UNDESCRIBED_HELP: &str = "Reconnect the device and retry; if it persists, list the parent to see how many objects it refuses to describe:";

/// The report `main` prints for `error`, with help attached when there is any. The message
/// may embed a device-supplied name, so it is sanitized here, at the terminal.
pub fn report(error: &Error) -> miette::Report {
    let message = sanitize_for_display(&error.to_string());
    help(error).map_or_else(
        || miette::miette!("{message}"),
        |help| miette::miette!(help = help, "{message}"),
    )
}

fn help(error: &Error) -> Option<String> {
    match error {
        Error::ExclusiveAccess { holder } => Some(exclusive_access(holder.as_ref())),
        Error::AmbiguousDevice(devices)
        | Error::DeviceNotFound {
            available: devices, ..
        } => Some(pick_device(devices)),
        Error::StorageRequired(storages)
        | Error::StorageNotFound {
            available: storages,
            ..
        } => Some(pick_storage(storages)),
        Error::Conflicts(paths) => Some(conflicts(paths)),
        Error::RemotePathNotFound(path) => Some(remote_path_not_found(path)),
        Error::RemotePathUndescribed { path, .. } => Some(remote_path_undescribed(path)),
        _ => fixed_help(error).map(str::to_owned),
    }
}

/// Advice that needs nothing from the error itself.
const fn fixed_help(error: &Error) -> Option<&'static str> {
    match error {
        Error::NoDevice => Some(NO_DEVICE_HELP),
        Error::PermissionDenied => Some(PERMISSION_DENIED_HELP),
        Error::DeviceUnresponsive => Some(DEVICE_UNRESPONSIVE_HELP),
        Error::NoStorage => Some(NO_STORAGE_HELP),
        Error::LengthMismatch { .. } => Some(LENGTH_MISMATCH_HELP),
        Error::NotADirectory(_) => Some(NOT_A_DIRECTORY_HELP),
        Error::LocalNotADirectory(_) => Some(LOCAL_NOT_A_DIRECTORY_HELP),
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

fn pick_device(devices: &[DeviceSummary]) -> String {
    let lines = devices
        .iter()
        .enumerate()
        .map(|(index, device)| format!("\n  {index}: {}", format::device(device)));
    format!("{PICK_DEVICE_HELP}{}", lines.collect::<String>())
}

/// A device that exposes no storage at all gets the unlock advice instead of an empty list.
fn pick_storage(storages: &[StorageSummary]) -> String {
    if storages.is_empty() {
        return NO_STORAGE_HELP.to_owned();
    }
    let lines = storages
        .iter()
        .map(|storage| format!("\n  {}: {}", storage.index, format::storage(storage)));
    format!("{PICK_STORAGE_HELP}{}", lines.collect::<String>())
}

/// The library message only counts the conflicts, so this is the one place the flags are named.
fn conflicts(paths: &[RelPath]) -> String {
    let mut lines = vec![conflicts_heading(paths.len())];
    lines.extend(format::capped_list(paths.iter().map(format::path)));
    lines.push(CONFLICTS_HELP.to_owned());
    lines.join("\n")
}

fn conflicts_heading(count: usize) -> String {
    let verb = if count == 1 { "differs" } else { "differ" };
    format!("{} {verb}:", format::files(count as u64))
}

fn remote_path_not_found(path: &DevicePath) -> String {
    format!("Check the name with `{}`.", ls_parent_command(path))
}

fn remote_path_undescribed(path: &DevicePath) -> String {
    format!(
        "{REMOTE_PATH_UNDESCRIBED_HELP}\n  {}",
        ls_parent_command(path)
    )
}

fn ls_parent_command(path: &DevicePath) -> String {
    let parent = DevicePath {
        storage: path.storage.clone(),
        path: path.path.parent().unwrap_or_else(RemotePath::root),
    };
    format!("mtpx ls {}", shell_argument(&parent.to_string()))
}

/// Quotes an argument so a POSIX shell passes it through verbatim; `'` is the only character
/// single quotes cannot contain, so it is spliced in as `'\''`.
fn shell_argument(argument: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "_/.:,+=@%-".contains(c);
    if !argument.is_empty() && argument.chars().all(is_safe) {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ui::test_util::device;

    fn storage(index: usize, name: &str) -> StorageSummary {
        StorageSummary::new(index, name.to_owned(), 1_000, 2_000)
    }

    #[test]
    fn conflicts_help_counts_the_files_then_lists_them_then_names_both_flags_once() {
        let paths = vec![
            RelPath::new(["a.jpg"]).unwrap(),
            RelPath::new(["b.jpg"]).unwrap(),
        ];
        let error = Error::Conflicts(paths);
        assert!(!error.to_string().contains("--overwrite"));
        let text = help(&error).unwrap();
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
        let devices = vec![
            device("Pixel", Some("ZY22"), None),
            device("Moto", None, None),
        ];
        let text = help(&Error::AmbiguousDevice(devices)).unwrap();
        assert!(text.contains("--device"), "{text}");
        assert!(text.contains("\n  0: Pixel (ZY22)\n  1: Moto"), "{text}");
    }

    #[test]
    fn device_not_found_help_lists_what_is_attached() {
        let err = Error::DeviceNotFound {
            selector: "ZY99".into(),
            available: vec![device("Pixel", Some("ZY22"), None)],
        };
        let text = help(&err).unwrap();
        assert!(text.contains("--device"), "{text}");
        assert!(text.contains("\n  0: Pixel (ZY22)"), "{text}");
    }

    #[test]
    fn storage_required_help_lists_the_storages_and_the_flag() {
        let text = help(&Error::StorageRequired(vec![storage(0, "Internal")])).unwrap();
        assert!(text.contains("--storage"), "{text}");
        assert!(text.contains("0: Internal (1 kB free of 2 kB)"), "{text}");
    }

    #[test]
    fn storage_not_found_help_lists_the_storages_or_explains_none() {
        let listed = Error::StorageNotFound {
            wanted: "nope".into(),
            available: vec![storage(0, "Internal")],
        };
        let text = help(&listed).unwrap();
        assert!(text.contains("--storage"), "{text}");
        assert!(text.contains("0: Internal"), "{text}");
        let none = Error::StorageNotFound {
            wanted: "nope".into(),
            available: vec![],
        };
        assert_eq!(help(&none).unwrap(), NO_STORAGE_HELP);
    }

    #[test]
    fn remote_path_undescribed_help_points_at_the_parent_listing() {
        let path: DevicePath = "/DCIM/Camera/x.jpg".parse().unwrap();
        let text = help(&Error::RemotePathUndescribed { path, skipped: 2 }).unwrap();
        assert!(text.starts_with("Reconnect the device"), "{text}");
        assert!(text.ends_with("\n  mtpx ls /DCIM/Camera"), "{text}");
    }

    #[test]
    fn no_storage_help_says_to_unlock_without_repeating_the_message() {
        let none = help(&Error::NoStorage).unwrap();
        assert_eq!(none, NO_STORAGE_HELP);
        assert!(!none.contains("no storage"), "{none}");
    }

    #[test]
    fn report_sanitizes_a_device_supplied_name_in_the_message() {
        let path = RelPath::new(["\x1b[2Jx.jpg"]).unwrap();
        let rendered = format!("{:?}", report(&Error::SourceVanished(path)));
        assert!(rendered.contains("\u{FFFD}[2Jx.jpg"), "{rendered}");
        assert!(!rendered.contains('\x1b'), "{rendered}");
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
    fn remote_path_not_found_quotes_a_parent_with_spaces() {
        let path: DevicePath = "sd card:/My Photos/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(path)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls 'sd card:/My Photos'`.");
    }

    #[test]
    fn remote_path_not_found_quotes_shell_metacharacters_in_the_parent() {
        let path: DevicePath = "/DCIM/Photos&Vids/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(path)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls '/DCIM/Photos&Vids'`.");
        let path: DevicePath = "/$HOME/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(path)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls '/$HOME'`.");
    }

    #[test]
    fn remote_path_not_found_splices_a_single_quote_inside_the_parent() {
        let path: DevicePath = "/It's/Nope".parse().unwrap();
        let text = help(&Error::RemotePathNotFound(path)).unwrap();
        assert_eq!(text, "Check the name with `mtpx ls '/It'\\''s'`.");
    }

    #[test]
    fn not_a_directory_help_is_command_neutral() {
        let path: DevicePath = "/DCIM/a.jpg".parse().unwrap();
        let text = help(&Error::NotADirectory(path)).unwrap();
        assert_eq!(text, NOT_A_DIRECTORY_HELP);
        assert!(!text.starts_with("ls "), "{text}");
        assert!(text.contains("passes through one"), "{text}");
    }

    #[test]
    fn an_unresponsive_device_is_told_how_to_enter_file_transfer_mode() {
        let text = help(&Error::DeviceUnresponsive).unwrap();
        assert_eq!(
            text,
            "Unlock the phone, open the USB notification and choose File transfer (MTP). \
             Charging-only mode answers nothing."
        );
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
