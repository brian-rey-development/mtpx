//! Exit codes: a script can tell "no device" from "conflicts".

use crate::commands::Outcome;
use mtpx_core::{Error, Report};
use std::process::ExitCode;

const SUCCESS: u8 = 0;
const GENERIC: u8 = 1;
const USAGE: u8 = 2;
const NO_DEVICE: u8 = 3;
const ACCESS_DENIED: u8 = 4;
const NOT_FOUND: u8 = 5;
const TRANSFER_FAILED: u8 = 6;
const CONFLICTS: u8 = 7;
/// What a shell reports for a process killed by SIGINT; scripts already expect it.
pub const INTERRUPTED: u8 = 130;

/// Appended to `mtpx --help` so scripts do not need the README to branch on the exit status.
pub const HELP: &str = "\
Exit codes:
  0    success, including a sync with nothing to do
  1    unexpected error
  2    usage error
  3    no device, one that does not answer or disconnected, or several and no --device
  4    device held by another process or access denied by the OS
  5    remote path or storage not found
  6    some files failed; the summary lists them
  7    pull found differing files and no --overwrite/--skip-existing
  130  interrupted";

pub fn code_for_error(error: &Error) -> ExitCode {
    let code = match error {
        Error::NoDevice
        | Error::DeviceNotFound { .. }
        | Error::AmbiguousDevice(_)
        | Error::DeviceUnresponsive
        | Error::Disconnected
        | Error::NoStorage => NO_DEVICE,
        Error::ExclusiveAccess { .. } | Error::PermissionDenied => ACCESS_DENIED,
        Error::RemotePathNotFound(_)
        | Error::RemotePathUndescribed { .. }
        | Error::NotADirectory(_)
        | Error::LocalNotADirectory(_)
        | Error::StorageRequired(_)
        | Error::StorageNotFound { .. } => NOT_FOUND,
        Error::Conflicts(_) => CONFLICTS,
        Error::Cancelled => INTERRUPTED,
        Error::InvalidPath(_) => USAGE,
        _ => GENERIC,
    };
    ExitCode::from(code)
}

pub fn code_for_outcome(outcome: &Outcome) -> ExitCode {
    match outcome {
        Outcome::Transferred(report) => code_for_report(report),
        Outcome::Done | Outcome::DryRun(_) => ExitCode::from(SUCCESS),
    }
}

/// An interrupt wins over failed files: the run did not finish, and the re-run retries them.
pub fn code_for_report(report: &Report) -> ExitCode {
    if report.interrupted {
        return ExitCode::from(INTERRUPTED);
    }
    if report.failed.is_empty() {
        return ExitCode::from(SUCCESS);
    }
    ExitCode::from(TRANSFER_FAILED)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::*;
    use mtpx_core::{DevicePath, FailedFile, PathError, RelPath, StorageSummary};

    fn path() -> DevicePath {
        "/a".parse().unwrap()
    }

    fn storage(index: usize) -> StorageSummary {
        StorageSummary::new(index, format!("Storage {index}"), 1, 2)
    }

    // `ExclusiveAccess` cannot be built across crates (`non_exhaustive`); it shares the `PermissionDenied` arm.
    #[test]
    fn errors_map_to_the_documented_codes() {
        let cases = [
            (Error::NoDevice, NO_DEVICE),
            (
                Error::DeviceNotFound {
                    selector: "x".into(),
                    available: vec![],
                },
                NO_DEVICE,
            ),
            (Error::AmbiguousDevice(vec![]), NO_DEVICE),
            (Error::DeviceUnresponsive, NO_DEVICE),
            (Error::NoStorage, NO_DEVICE),
            (Error::PermissionDenied, ACCESS_DENIED),
            (Error::RemotePathNotFound(path()), NOT_FOUND),
            (
                Error::RemotePathUndescribed {
                    path: path(),
                    skipped: 1,
                },
                NOT_FOUND,
            ),
            (Error::NotADirectory(path()), NOT_FOUND),
            (
                Error::LocalNotADirectory("/tmp/notes.txt".into()),
                NOT_FOUND,
            ),
            (
                Error::StorageRequired(vec![storage(0), storage(1)]),
                NOT_FOUND,
            ),
            (
                Error::StorageNotFound {
                    wanted: "x".into(),
                    available: vec![],
                },
                NOT_FOUND,
            ),
            (Error::Conflicts(vec![RelPath::root()]), CONFLICTS),
            (Error::Cancelled, INTERRUPTED),
            (Error::InvalidPath(PathError::Empty), USAGE),
            (Error::Disconnected, NO_DEVICE),
            (Error::Unsupported("x"), GENERIC),
            (Error::Io(std::io::Error::other("x")), GENERIC),
        ];
        for (error, expected) in cases {
            assert_eq!(code_for_error(&error), ExitCode::from(expected), "{error}");
        }
    }

    #[test]
    fn reports_are_success_unless_interrupted_or_failed() {
        let clean = Report::default();
        assert_eq!(code_for_report(&clean), ExitCode::from(SUCCESS));
        let mut report = Report::default();
        report.failed.push(FailedFile::new(RelPath::root(), "boom"));
        assert_eq!(code_for_report(&report), ExitCode::from(TRANSFER_FAILED));
        report.interrupted = true;
        assert_eq!(code_for_report(&report), ExitCode::from(INTERRUPTED));
    }

    #[test]
    fn help_lists_every_code_next_to_its_meaning() {
        for code in [SUCCESS, GENERIC, USAGE, NO_DEVICE, ACCESS_DENIED] {
            assert!(HELP.contains(&format!("\n  {code}    ")), "{code}");
        }
        for code in [NOT_FOUND, TRANSFER_FAILED, CONFLICTS] {
            assert!(HELP.contains(&format!("\n  {code}    ")), "{code}");
        }
        assert!(HELP.ends_with(&format!("{INTERRUPTED}  interrupted")));
    }

    #[test]
    fn non_transfer_outcomes_are_success() {
        assert_eq!(code_for_outcome(&Outcome::Done), ExitCode::from(SUCCESS));
    }
}
