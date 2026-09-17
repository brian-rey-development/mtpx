//! Pure text formatting shared by the progress renderer, the tables, the prompts and the
//! error reports.

use humansize::{DECIMAL, FormatSizeOptions, format_size};
use mtpx_core::{
    DeviceSummary, PlanSummary, RelPath, Report, SkipReason, SkippedEntry, StorageSummary,
    sanitize_for_display,
};
use std::time::Duration;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const MILLIS_PER_SECOND: u128 = 1000;
const THOUSANDS_GROUP: usize = 3;
const SIZE_DECIMALS: usize = 1;
/// Longest list of paths or objects shown in full; the rest is summed up as `... and N more`.
const MAX_LISTED: usize = 20;
/// How the root of a listing reads in a `parent: reason` line.
const ROOT_LABEL: &str = ".";

/// `4.3 GB` style sizes, decimal units like the phone's own file manager.
pub fn size(bytes: u64) -> String {
    format_size(
        bytes,
        FormatSizeOptions::from(DECIMAL).decimal_places(SIZE_DECIMALS),
    )
}

/// `1,102` style counts.
pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / THOUSANDS_GROUP);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % THOUSANDS_GROUP == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A device path as the terminal may show it: control characters neutralized, length capped.
pub fn path(path: &RelPath) -> String {
    sanitize_for_display(&path.to_string())
}

/// `Pixel (ZY22)`; the serial is dropped when the device reports none or an empty one.
pub fn device(device: &DeviceSummary) -> String {
    match device.serial.as_deref() {
        Some(serial) if !serial.is_empty() => format!(
            "{} ({})",
            sanitize_for_display(&device.label),
            sanitize_for_display(serial)
        ),
        _ => sanitize_for_display(&device.label),
    }
}

/// `SD card (1.2 GB free of 32 GB)`.
pub fn storage(storage: &StorageSummary) -> String {
    format!(
        "{} ({} free of {})",
        sanitize_for_display(&storage.name),
        size(storage.free),
        size(storage.total)
    )
}

/// `1 file` or `182 files`.
pub fn files(value: u64) -> String {
    if value == 1 {
        return "1 file".to_owned();
    }
    format!("{} files", count(value))
}

/// `1 object` or `1,284 objects`.
pub fn objects(value: u64) -> String {
    if value == 1 {
        return "1 object".to_owned();
    }
    format!("{} objects", count(value))
}

/// The word a plan line or a skip event uses for `reason`.
pub const fn skip_reason(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::Identical => "identical",
        SkipReason::Conflict => "conflict",
        SkipReason::KindConflict => "kind conflict",
        SkipReason::NameCollision => "name collision",
        _ => "skipped",
    }
}

/// Whether a skip is worth a line of its own even when bars are drawing: anything the user
/// may want to act on, as opposed to an identical file.
pub const fn skip_is_notable(reason: SkipReason) -> bool {
    !matches!(reason, SkipReason::Identical)
}

/// `items` indented by two spaces, at most `MAX_LISTED` of them, then `  ... and N more`
/// when some were cut.
pub fn capped_list(items: impl ExactSizeIterator<Item = String>) -> Vec<String> {
    let total = items.len();
    let mut lines: Vec<String> = items
        .take(MAX_LISTED)
        .map(|item| format!("  {item}"))
        .collect();
    let more = total.saturating_sub(MAX_LISTED);
    if more > 0 {
        lines.push(format!("  ... and {} more", count(more as u64)));
    }
    lines
}

/// One `parent: reason` line per refused object, capped; both halves come from the device.
pub fn skipped_lines(skipped: &[SkippedEntry]) -> Vec<String> {
    capped_list(skipped.iter().map(|entry| {
        let parent = if entry.parent.is_root() {
            ROOT_LABEL.to_owned()
        } else {
            sanitize_for_display(&entry.parent.to_string())
        };
        format!("{parent}: {}", sanitize_for_display(&entry.reason))
    }))
}

/// `45s`, `2m 12s` or `1h 02m`.
pub fn duration(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let hours = total / SECONDS_PER_HOUR;
    let minutes = (total % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let seconds = total % SECONDS_PER_MINUTE;
    if hours > 0 {
        return format!("{hours}h {minutes:02}m");
    }
    if minutes > 0 {
        return format!("{minutes}m {seconds:02}s");
    }
    format!("{seconds}s")
}

/// `33.1 MB/s`; zero when nothing moved or no time passed.
pub fn rate(bytes: u64, elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis == 0 {
        return format!("{}/s", size(0));
    }
    let per_second = u128::from(bytes) * MILLIS_PER_SECOND / millis;
    format!("{}/s", size(u64::try_from(per_second).unwrap_or(u64::MAX)))
}

/// `Plan: copy 182 files (4.3 GB, 81 MB resumable), skip 1,102`.
pub fn plan_line(summary: &PlanSummary) -> String {
    let resumable = if summary.resumable_bytes > 0 {
        format!(", {} resumable", size(summary.resumable_bytes))
    } else {
        String::new()
    };
    format!(
        "Plan: copy {} ({}{resumable}), skip {}",
        files(summary.files_to_copy),
        size(summary.bytes_to_copy),
        count(summary.files_to_skip)
    )
}

/// Files the summary must mention that `Report` does not count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Omitted {
    /// Copies not attempted because the run was interrupted.
    pub remaining: u64,
    /// Objects the device refused to describe, so they were never planned.
    pub left_out: u64,
}

/// How a transfer ended. `total` covers the whole command, scans included; the rate only
/// counts moving bytes. Failures and objects left out stay visible even when interrupted.
pub fn summary_line(report: &Report, omitted: Omitted, total: Duration) -> String {
    if report.interrupted {
        return format!(
            "Interrupted after {}: {} copied{}, {} remaining{}, re-run to resume",
            duration(total),
            count(report.copied),
            failed_detail(report),
            count(omitted.remaining),
            left_out_detail(omitted.left_out)
        );
    }
    format!(
        "Done in {}: {} copied{}, {} skipped, {} failed{}",
        duration(total),
        count(report.copied),
        copied_detail(report),
        count(report.skipped),
        count(report.failed.len() as u64),
        left_out_detail(omitted.left_out)
    )
}

fn failed_detail(report: &Report) -> String {
    if report.failed.is_empty() {
        return String::new();
    }
    format!(", {} failed", count(report.failed.len() as u64))
}

fn left_out_detail(left_out: u64) -> String {
    if left_out == 0 {
        return String::new();
    }
    format!(", {} left out", count(left_out))
}

/// How an aborted transfer ended; the error itself is printed by the diagnostic that follows.
pub fn aborted_line(report: &Report, remaining: u64, total: Duration) -> String {
    format!(
        "Aborted after {}: {} copied, {} remaining, re-run to resume",
        duration(total),
        count(report.copied),
        count(remaining)
    )
}

fn copied_detail(report: &Report) -> String {
    if report.copied == 0 {
        return String::new();
    }
    format!(
        " ({}, {} avg)",
        size(report.bytes),
        rate(report.bytes, report.elapsed)
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ui::test_util::device as summary;
    use mtpx_core::FailedFile;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    #[test]
    fn device_lines_show_label_and_serial_but_no_control_characters() {
        assert_eq!(
            device(&summary("Pixel", Some("ZY22"), None)),
            "Pixel (ZY22)"
        );
        assert_eq!(
            device(&summary("a\x1b[2Jb", Some("x\ny"), None)),
            "a\u{FFFD}[2Jb (x\u{FFFD}y)"
        );
        assert_eq!(device(&summary("Moto", None, None)), "Moto");
        assert_eq!(device(&summary("Moto", Some(""), None)), "Moto");
    }

    #[test]
    fn storage_lines_show_the_name_but_no_control_characters() {
        let listed = StorageSummary::new(0, "SD\x07card".into(), 1, 2);
        assert_eq!(storage(&listed), "SD\u{FFFD}card (1 B free of 2 B)");
    }

    #[test]
    fn paths_are_shown_with_control_characters_neutralized() {
        assert_eq!(path(&rel("DCIM/a.jpg")), "DCIM/a.jpg");
        assert_eq!(path(&rel("\x1b[2Jx.jpg")), "\u{FFFD}[2Jx.jpg");
    }

    #[test]
    fn counts_get_thousands_separators() {
        let cases = [(0, "0"), (999, "999"), (1_000, "1,000"), (1_102, "1,102")];
        for (value, expected) in cases {
            assert_eq!(count(value), expected);
        }
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn files_pluralizes() {
        assert_eq!(files(1), "1 file");
        assert_eq!(files(0), "0 files");
        assert_eq!(files(1_200), "1,200 files");
    }

    #[test]
    fn objects_pluralizes() {
        assert_eq!(objects(1), "1 object");
        assert_eq!(objects(0), "0 objects");
        assert_eq!(objects(1_284), "1,284 objects");
    }

    #[test]
    fn skip_reasons_have_a_label_each() {
        assert_eq!(skip_reason(SkipReason::Identical), "identical");
        assert_eq!(skip_reason(SkipReason::Conflict), "conflict");
        assert_eq!(skip_reason(SkipReason::KindConflict), "kind conflict");
        assert_eq!(skip_reason(SkipReason::NameCollision), "name collision");
        assert!(!skip_is_notable(SkipReason::Identical));
        assert!(skip_is_notable(SkipReason::KindConflict));
    }

    #[test]
    fn capped_list_indents_and_sums_up_the_overflow() {
        let short = capped_list(["a".to_owned(), "b".to_owned()].into_iter());
        assert_eq!(short, ["  a", "  b"]);
        let long = capped_list((0..25).map(|i| i.to_string()));
        assert_eq!(long.len(), MAX_LISTED + 1);
        assert_eq!(long[0], "  0");
        assert_eq!(long[MAX_LISTED - 1], "  19");
        assert_eq!(long[MAX_LISTED], "  ... and 5 more");
    }

    #[test]
    fn skipped_lines_name_the_parent_and_the_reason_sanitized() {
        let skipped = [
            SkippedEntry::new(RelPath::root(), "refused"),
            SkippedEntry::new(rel("DCIM/Camera"), "bad\x1bname"),
        ];
        assert_eq!(
            skipped_lines(&skipped),
            ["  .: refused", "  DCIM/Camera: bad\u{FFFD}name"]
        );
    }

    #[test]
    fn durations_use_the_largest_two_units() {
        let cases = [
            (0, "0s"),
            (45, "45s"),
            (132, "2m 12s"),
            (3_720, "1h 02m"),
            (36_000, "10h 00m"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(duration(Duration::from_secs(seconds)), expected);
        }
    }

    #[test]
    fn durations_switch_units_exactly_at_the_minute_and_the_hour() {
        let cases = [
            (59, "59s"),
            (60, "1m 00s"),
            (3_599, "59m 59s"),
            (3_600, "1h 00m"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(duration(Duration::from_secs(seconds)), expected);
        }
    }

    #[test]
    fn rate_divides_bytes_by_elapsed_and_survives_zero() {
        assert_eq!(rate(33_100_000, Duration::from_secs(1)), "33.1 MB/s");
        assert_eq!(rate(1_000, Duration::from_millis(500)), "2 kB/s");
        assert_eq!(rate(1_000, Duration::ZERO), "0 B/s");
    }

    #[test]
    fn plan_line_mentions_resumable_bytes_only_when_present() {
        let mut summary = PlanSummary::default();
        summary.files_to_copy = 182;
        summary.bytes_to_copy = 4_300_000_000;
        summary.resumable_bytes = 81_000_000;
        summary.files_to_skip = 1_102;
        assert_eq!(
            plan_line(&summary),
            "Plan: copy 182 files (4.3 GB, 81 MB resumable), skip 1,102"
        );
        let mut fresh = PlanSummary::default();
        fresh.files_to_copy = 1;
        fresh.bytes_to_copy = 10;
        assert_eq!(plan_line(&fresh), "Plan: copy 1 file (10 B), skip 0");
    }

    #[test]
    fn summary_line_reads_done_or_interrupted() {
        let mut report = Report::default();
        report.copied = 182;
        report.bytes = 4_300_000_000;
        report.skipped = 1_102;
        report.elapsed = Duration::from_secs(132);
        assert_eq!(
            summary_line(&report, Omitted::default(), Duration::from_secs(132)),
            "Done in 2m 12s: 182 copied (4.3 GB, 32.6 MB/s avg), 1,102 skipped, 0 failed"
        );
        report.interrupted = true;
        report.copied = 5;
        report.elapsed = Duration::from_secs(63);
        let omitted = Omitted {
            remaining: 177,
            left_out: 0,
        };
        assert_eq!(
            summary_line(&report, omitted, Duration::from_secs(63)),
            "Interrupted after 1m 03s: 5 copied, 177 remaining, re-run to resume"
        );
    }

    #[test]
    fn summary_line_keeps_failures_visible_when_interrupted() {
        let mut report = Report::default();
        report.interrupted = true;
        report.copied = 5;
        report.failed.push(FailedFile::new(rel("a.jpg"), "boom"));
        let omitted = Omitted {
            remaining: 177,
            left_out: 0,
        };
        assert_eq!(
            summary_line(&report, omitted, Duration::from_secs(63)),
            "Interrupted after 1m 03s: 5 copied, 1 failed, 177 remaining, re-run to resume"
        );
    }

    #[test]
    fn summary_line_counts_objects_the_device_left_out() {
        let mut report = Report::default();
        report.skipped = 2;
        let omitted = Omitted {
            remaining: 0,
            left_out: 3,
        };
        assert_eq!(
            summary_line(&report, omitted, Duration::from_secs(9)),
            "Done in 9s: 0 copied, 2 skipped, 0 failed, 3 left out"
        );
        report.interrupted = true;
        assert_eq!(
            summary_line(&report, omitted, Duration::from_secs(9)),
            "Interrupted after 9s: 0 copied, 0 remaining, 3 left out, re-run to resume"
        );
    }

    #[test]
    fn aborted_line_counts_what_landed_and_what_is_left() {
        let mut report = Report::default();
        report.copied = 12;
        assert_eq!(
            aborted_line(&report, 170, Duration::from_secs(75)),
            "Aborted after 1m 15s: 12 copied, 170 remaining, re-run to resume"
        );
    }

    #[test]
    fn summary_line_times_the_whole_command_but_rates_only_the_transfer() {
        let mut report = Report::default();
        report.copied = 182;
        report.bytes = 4_300_000_000;
        report.elapsed = Duration::from_secs(132);
        let with_scans = Duration::from_secs(141);
        assert_eq!(
            summary_line(&report, Omitted::default(), with_scans),
            "Done in 2m 21s: 182 copied (4.3 GB, 32.6 MB/s avg), 0 skipped, 0 failed"
        );
        report.interrupted = true;
        let omitted = Omitted {
            remaining: 3,
            left_out: 0,
        };
        assert_eq!(
            summary_line(&report, omitted, with_scans),
            "Interrupted after 2m 21s: 182 copied, 3 remaining, re-run to resume"
        );
    }

    #[test]
    fn summary_line_drops_the_size_and_rate_when_nothing_was_copied() {
        let mut report = Report::default();
        report.skipped = 2;
        assert_eq!(
            summary_line(&report, Omitted::default(), Duration::from_secs(9)),
            "Done in 9s: 0 copied, 2 skipped, 0 failed"
        );
        report.failed.push(FailedFile::new(rel("a.jpg"), "boom"));
        assert_eq!(
            summary_line(&report, Omitted::default(), Duration::from_secs(9)),
            "Done in 9s: 0 copied, 2 skipped, 1 failed"
        );
    }
}
