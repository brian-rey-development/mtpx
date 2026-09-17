//! Plain-text tables for stdout: no colors, so the output pipes cleanly.

use crate::ui::format;
use console::{Alignment, measure_text_width, pad_str};
use jiff::{Timestamp, tz::TimeZone};
use mtpx_core::{
    Action, DeviceSummary, Entry, EntryKind, ModifiedTime, Plan, RelPath, SkipReason, Snapshot,
    UsbSpeed, sanitize_for_display,
};

const COLUMN_GAP: &str = "  ";
const ABSENT: &str = "-";
const DIR_SUFFIX: char = '/';
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";
/// The one right-aligned column of a long listing.
const SIZE_COLUMN: usize = 1;

/// Attached devices, one per row, with the index `--device` accepts.
pub fn devices(devices: &[DeviceSummary]) -> String {
    let header = row(["INDEX", "DEVICE", "ID", "SERIAL", "SPEED"]);
    let rows = devices.iter().enumerate().map(|(index, device)| {
        let label = sanitize_for_display(&device.label);
        let serial = device
            .serial
            .as_deref()
            .filter(|serial| !serial.is_empty())
            .map_or_else(|| ABSENT.to_owned(), sanitize_for_display);
        row([
            &index.to_string(),
            &label,
            &format!("{:04x}:{:04x}", device.vendor_id, device.product_id),
            &serial,
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
                &format::path(&entry.path),
            ])
        })
        .collect::<Vec<_>>();
    render(&rows, &[SIZE_COLUMN])
}

/// A plan, one line per changing action. Identical skips stay out; conflict skips stay in.
/// Every path comes from the device, so each goes through `format::path`.
pub fn plan(plan: &Plan) -> String {
    actions(plan.actions())
}

fn actions(actions: &[Action]) -> String {
    actions.iter().filter_map(action).collect()
}

fn action(action: &Action) -> Option<String> {
    match action {
        Action::Mkdir { path } => Some(format!("mkdir {}\n", format::path(path))),
        Action::Copy {
            path,
            size,
            resume_from,
            ..
        } => Some(copy_line(path, *size, *resume_from)),
        Action::Skip {
            reason: SkipReason::Identical,
            ..
        } => None,
        Action::Skip { path, reason } => Some(format!(
            "skip  {} ({})\n",
            format::path(path),
            format::skip_reason(*reason)
        )),
        _ => None,
    }
}

fn copy_line(path: &RelPath, size: u64, resume_from: u64) -> String {
    format!(
        "copy  {} ({})\n",
        format::path(path),
        copy_detail(size, resume_from)
    )
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

fn name(entry: &Entry) -> String {
    let shown = format::path(&entry.path);
    match entry.kind {
        EntryKind::Dir => format!("{shown}{DIR_SUFFIX}\n"),
        EntryKind::File => format!("{shown}\n"),
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
                .map(|cell| measure_text_width(cell))
                .max()
                .unwrap_or(0)
        })
        .collect()
}

/// Pads by display width, so a wide label (CJK, emoji) does not shift the columns after it.
fn line(row: &[String], widths: &[usize], right: &[usize]) -> String {
    let cells = row.iter().enumerate().map(|(column, cell)| {
        let width = widths.get(column).copied().unwrap_or(0);
        let alignment = if right.contains(&column) {
            Alignment::Right
        } else {
            Alignment::Left
        };
        pad_str(cell, width, alignment, None).into_owned()
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
    use crate::ui::test_util::device;
    use mtpx_core::CopyReason;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    fn entry(path: &str, kind: EntryKind, size: u64) -> Entry {
        Entry::new(rel(path), kind, size, None)
    }

    #[test]
    fn devices_table_has_a_header_and_aligned_columns() {
        let listed = [
            device("Google Pixel 9", Some("ZY22"), Some(UsbSpeed::Super)),
            device("Moto g52", None, None),
            device("Nokia 2", Some(""), None),
        ];
        assert_eq!(
            devices(&listed),
            "INDEX  DEVICE          ID         SERIAL  SPEED\n\
             0      Google Pixel 9  18d1:4ee1  ZY22    USB 3.0 SuperSpeed\n\
             1      Moto g52        18d1:4ee1  -       -\n\
             2      Nokia 2         18d1:4ee1  -       -\n"
        );
    }

    #[test]
    fn devices_table_aligns_wide_characters_by_display_width() {
        let listed = [
            device("小米 Redmi", None, None),
            device("Moto g52", None, None),
        ];
        assert_eq!(
            devices(&listed),
            "INDEX  DEVICE      ID         SERIAL  SPEED\n\
             0      小米 Redmi  18d1:4ee1  -       -\n\
             1      Moto g52    18d1:4ee1  -       -\n"
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

    #[test]
    fn plan_lines_neutralize_hostile_characters_in_device_names() {
        let listed = [
            Action::Mkdir {
                path: rel("\u{202E}DCIM"),
            },
            Action::Copy {
                path: rel("DCIM/\u{2028}a.jpg"),
                size: 1,
                modified: None,
                resume_from: 0,
                reason: CopyReason::New,
            },
            Action::Skip {
                path: rel("DCIM/\u{200F}c.jpg"),
                reason: SkipReason::Conflict,
            },
        ];
        let text = actions(&listed);
        assert_eq!(
            text,
            "mkdir \u{FFFD}DCIM\ncopy  DCIM/\u{FFFD}a.jpg (1 B)\nskip  DCIM/\u{FFFD}c.jpg (conflict)\n"
        );
    }
}
