//! Pure text formatting shared by the progress renderer and the tables.

use humansize::{DECIMAL, FormatSizeOptions, format_size};
use mtpx_core::{PlanSummary, Report, SkipReason};
use std::time::Duration;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const MILLIS_PER_SECOND: u128 = 1000;
const THOUSANDS_GROUP: usize = 3;
/// One decimal reads like a file manager: `4.3 GB`, not `4.30 GB`.
const SIZE_DECIMALS: usize = 1;

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
    let head = digits.len() % THOUSANDS_GROUP;
    let mut grouped = String::with_capacity(digits.len() + digits.len() / THOUSANDS_GROUP);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (index + THOUSANDS_GROUP - head) % THOUSANDS_GROUP == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
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
        _ => "skipped",
    }
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
        count(summary.to_skip)
    )
}

/// The one line that says how a transfer ended; `remaining` is what the plan still had left.
/// The size and rate only appear when something was copied.
pub fn summary_line(report: &Report, remaining: u64) -> String {
    if report.interrupted {
        return format!(
            "Interrupted after {}: {} copied, {} remaining, re-run to resume",
            duration(report.elapsed),
            count(report.copied),
            count(remaining)
        );
    }
    format!(
        "Done in {}: {} copied{}, {} skipped, {} failed",
        duration(report.elapsed),
        count(report.copied),
        copied_detail(report),
        count(report.skipped),
        count(report.failed.len() as u64)
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
    use mtpx_core::RelPath;

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
    fn skip_reasons_have_one_word_each() {
        assert_eq!(skip_reason(SkipReason::Identical), "identical");
        assert_eq!(skip_reason(SkipReason::Conflict), "conflict");
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
        let summary = PlanSummary {
            files_to_copy: 182,
            bytes_to_copy: 4_300_000_000,
            resumable_bytes: 81_000_000,
            to_skip: 1_102,
        };
        assert_eq!(
            plan_line(&summary),
            "Plan: copy 182 files (4.3 GB, 81 MB resumable), skip 1,102"
        );
        let fresh = PlanSummary {
            files_to_copy: 1,
            bytes_to_copy: 10,
            resumable_bytes: 0,
            to_skip: 0,
        };
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
            summary_line(&report, 0),
            "Done in 2m 12s: 182 copied (4.3 GB, 32.6 MB/s avg), 1,102 skipped, 0 failed"
        );
        report.interrupted = true;
        report.copied = 5;
        report.elapsed = Duration::from_secs(63);
        assert_eq!(
            summary_line(&report, 177),
            "Interrupted after 1m 03s: 5 copied, 177 remaining, re-run to resume"
        );
    }

    #[test]
    fn summary_line_drops_the_size_and_rate_when_nothing_was_copied() {
        let mut report = Report::default();
        report.skipped = 2;
        assert_eq!(
            summary_line(&report, 0),
            "Done in 0s: 0 copied, 2 skipped, 0 failed"
        );
        report
            .failed
            .push((RelPath::new(["a.jpg"]).unwrap(), "boom".into()));
        assert_eq!(
            summary_line(&report, 0),
            "Done in 0s: 0 copied, 2 skipped, 1 failed"
        );
    }
}
