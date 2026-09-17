//! Plain-text tables for stdout: no colors, so the output pipes cleanly.

use crate::ui::format;
use jiff::{Timestamp, tz::TimeZone};
use mtpx_core::{
    Action, DeviceSummary, EntryKind, ModifiedTime, Plan, SkipReason, Snapshot, UsbSpeed,
};

const COLUMN_GAP: &str = "  ";
const ABSENT: &str = "-";
const DIR_SUFFIX: char = '/';
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";

/// Attached devices, one per row, with the index `--device` accepts.
pub fn devices(devices: &[DeviceSummary]) -> String {
    let header = row(["INDEX", "DEVICE", "ID", "SERIAL", "SPEED"]);
    let rows = devices.iter().enumerate().map(|(index, device)| {
        row([
            &index.to_string(),
            &device.label,
            &format!("{:04x}:{:04x}", device.vendor_id, device.product_id),
            device.serial.as_deref().unwrap_or(ABSENT),
            speed(device.speed),
        ])
    });
    render(
        &std::iter::once(header).chain(rows).collect::<Vec<_>>(),
        &[],
    )
}

/// A listing: names only, or kind, size, modification time and name with `long`.
pub fn ls(snapshot: &Snapshot, long: bool) -> String {
    if !long {
        return snapshot.entries().iter().map(name).collect();
    }
    let tz = TimeZone::system();
    let rows = snapshot
        .entries()
        .iter()
        .map(|entry| {
            row([
                kind(entry.kind),
                &entry_size(entry.kind, entry.size),
                &modified(entry.modified, &tz),
                &entry.path.to_string(),
            ])
        })
        .collect::<Vec<_>>();
    render(&rows, &[1])
}

/// A plan, one line per action that would change something; the totals stay on stderr with
/// the progress output. Files skipped as identical are left out, since a synced tree would
/// otherwise drown the few real actions; a conflict skip stays, being a policy decision.
pub fn plan(plan: &Plan) -> String {
    actions(plan.actions())
}

fn actions(actions: &[Action]) -> String {
    actions.iter().filter_map(action).collect()
}

fn action(action: &Action) -> Option<String> {
    match action {
        Action::Mkdir { path } => Some(format!("mkdir {path}\n")),
        Action::Copy {
            path,
            size,
            resume_from,
            ..
        } => Some(format!(
            "copy  {path} ({})\n",
            copy_detail(*size, *resume_from)
        )),
        Action::Skip {
            reason: SkipReason::Identical,
            ..
        } => None,
        Action::Skip { path, reason } => {
            Some(format!("skip  {path} ({})\n", format::skip_reason(*reason)))
        }
        _ => None,
    }
}

fn copy_detail(size: u64, resume_from: u64) -> String {
    if resume_from == 0 {
        return format::size(size);
    }
    format!(
        "{}, resume from {}",
        format::size(size),
        format::size(resume_from)
    )
}

fn name(entry: &mtpx_core::Entry) -> String {
    match entry.kind {
        EntryKind::Dir => format!("{}{DIR_SUFFIX}\n", entry.path),
        EntryKind::File => format!("{}\n", entry.path),
    }
}

const fn kind(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dir => "dir",
        EntryKind::File => "file",
    }
}

fn entry_size(kind: EntryKind, size: u64) -> String {
    match kind {
        EntryKind::Dir => ABSENT.to_owned(),
        EntryKind::File => format::size(size),
    }
}

fn modified(time: Option<ModifiedTime>, tz: &TimeZone) -> String {
    time.and_then(|t| Timestamp::from_second(t.unix_seconds()).ok())
        .map_or_else(
            || ABSENT.to_owned(),
            |t| t.to_zoned(tz.clone()).strftime(TIME_FORMAT).to_string(),
        )
}

const fn speed(speed: Option<UsbSpeed>) -> &'static str {
    match speed {
        Some(UsbSpeed::Low) => "USB 1.0 Low Speed",
        Some(UsbSpeed::Full) => "USB 1.1 Full Speed",
        Some(UsbSpeed::High) => "USB 2.0 High Speed",
        Some(UsbSpeed::Super) => "USB 3.0 SuperSpeed",
        Some(UsbSpeed::SuperPlus) => "USB 3.1 SuperSpeed+",
        None => ABSENT,
    }
}

fn row<const N: usize>(cells: [&str; N]) -> Vec<String> {
    cells.iter().map(|cell| (*cell).to_owned()).collect()
}

/// Pads every column to its widest cell; `right` lists the columns aligned to the right.
fn render(rows: &[Vec<String>], right: &[usize]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let widths = widths(rows);
    let lines: Vec<String> = rows.iter().map(|row| line(row, &widths, right)).collect();
    format!("{}\n", lines.join("\n"))
}

fn widths(rows: &[Vec<String>]) -> Vec<usize> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect()
}

fn line(row: &[String], widths: &[usize], right: &[usize]) -> String {
    let cells = row.iter().enumerate().map(|(column, cell)| {
        let width = widths.get(column).copied().unwrap_or(0);
        if right.contains(&column) {
            format!("{cell:>width$}")
        } else {
            format!("{cell:<width$}")
        }
    });
    cells
        .collect::<Vec<_>>()
        .join(COLUMN_GAP)
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::*;
    use mtpx_core::{CopyReason, Entry, RelPath};

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    fn entry(path: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            path: rel(path),
            kind,
            size,
            modified: None,
        }
    }

    fn device(label: &str, serial: Option<&str>, speed: Option<UsbSpeed>) -> DeviceSummary {
        DeviceSummary {
            serial: serial.map(str::to_owned),
            label: label.to_owned(),
            vendor_id: 0x18d1,
            product_id: 0x4ee1,
            location_id: 1,
            speed,
        }
    }

    #[test]
    fn devices_table_has_a_header_and_aligned_columns() {
        let listed = [
            device("Google Pixel 9", Some("ZY22"), Some(UsbSpeed::Super)),
            device("Moto g52", None, None),
        ];
        assert_eq!(
            devices(&listed),
            "INDEX  DEVICE          ID         SERIAL  SPEED\n\
             0      Google Pixel 9  18d1:4ee1  ZY22    USB 3.0 SuperSpeed\n\
             1      Moto g52        18d1:4ee1  -       -\n"
        );
    }

    #[test]
    fn short_listing_marks_directories_with_a_slash() {
        let snapshot = Snapshot::new(
            "/",
            vec![
                entry("DCIM", EntryKind::Dir, 0),
                entry("notes.txt", EntryKind::File, 12),
            ],
            vec![],
        );
        assert_eq!(ls(&snapshot, false), "DCIM/\nnotes.txt\n");
    }

    #[test]
    fn long_listing_right_aligns_sizes_and_dashes_missing_times() {
        let snapshot = Snapshot::new(
            "/",
            vec![
                entry("DCIM", EntryKind::Dir, 0),
                entry("DCIM/big.mp4", EntryKind::File, 1_500_000),
            ],
            vec![],
        );
        assert_eq!(
            ls(&snapshot, true),
            "dir        -  -  DCIM\nfile  1.5 MB  -  DCIM/big.mp4\n"
        );
    }

    #[test]
    fn modified_times_render_in_the_given_zone() {
        let time = ModifiedTime::from_system(std::time::UNIX_EPOCH);
        assert_eq!(modified(Some(time), &TimeZone::UTC), "1970-01-01 00:00");
        assert_eq!(modified(None, &TimeZone::UTC), ABSENT);
    }

    #[test]
    fn listings_of_an_empty_directory_print_nothing() {
        let snapshot = Snapshot::new("/Empty", vec![], vec![]);
        assert_eq!(ls(&snapshot, false), "");
        assert_eq!(ls(&snapshot, true), "");
    }

    #[test]
    fn plan_lists_what_would_change_and_leaves_identical_files_out() {
        let listed = [
            Action::Skip {
                path: rel("DCIM/same.jpg"),
                reason: SkipReason::Identical,
            },
            Action::Mkdir { path: rel("DCIM") },
            Action::Copy {
                path: rel("DCIM/a.jpg"),
                size: 2_000,
                modified: None,
                resume_from: 0,
                reason: CopyReason::New,
            },
            Action::Copy {
                path: rel("DCIM/b.jpg"),
                size: 5_000,
                modified: None,
                resume_from: 1_000,
                reason: CopyReason::SizeDiffers,
            },
            Action::Skip {
                path: rel("DCIM/c.jpg"),
                reason: SkipReason::Conflict,
            },
        ];
        assert_eq!(
            actions(&listed),
            "mkdir DCIM\ncopy  DCIM/a.jpg (2 kB)\ncopy  DCIM/b.jpg (5 kB, resume from 1 kB)\nskip  DCIM/c.jpg (conflict)\n"
        );
    }
}
