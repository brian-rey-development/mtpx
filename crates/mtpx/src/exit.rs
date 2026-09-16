//! Exit codes, as documented in the design spec: a script can tell "no device" from "conflicts".

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

/// The exit code for a command that failed with `error`.
pub fn code_for_error(error: &Error) -> ExitCode {
    let code = match error {
        Error::NoDevice | Error::AmbiguousDevice(_) => NO_DEVICE,
        Error::ExclusiveAccess { .. } | Error::PermissionDenied => ACCESS_DENIED,
        Error::RemotePathNotFound(_)
        | Error::NotADirectory(_)
        | Error::StorageRequired(_)
        | Error::StorageNotFound(_) => NOT_FOUND,
        Error::Conflicts(_) => CONFLICTS,
        Error::Cancelled => INTERRUPTED,
        Error::InvalidPath(_) => USAGE,
        _ => GENERIC,
    };
    ExitCode::from(code)
}

/// The exit code for a command that ran to the end.
pub fn code_for_outcome(outcome: &Outcome) -> ExitCode {
    match outcome {
        Outcome::Transferred(report) => code_for_report(report),
        Outcome::Done | Outcome::DryRun(_) => ExitCode::from(SUCCESS),
    }
}

/// The exit code for a transfer that produced `report`, interrupted or not.
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
    #![allow(clippy::unwrap_used)]

    use super::*;
    use mtpx_core::{DevicePath, PathError, RelPath};

    fn path() -> DevicePath {
        "/a".parse().unwrap()
    }

    // `Error` is `non_exhaustive`, so `ExclusiveAccess { holder }` cannot be built from another
    // crate; its row shares an arm with `PermissionDenied`, which is covered.
    #[test]
    fn errors_map_to_the_documented_codes() {
        let cases = [
            (Error::NoDevice, NO_DEVICE),
            (Error::AmbiguousDevice(vec![]), NO_DEVICE),
            (Error::PermissionDenied, ACCESS_DENIED),
            (Error::RemotePathNotFound(path()), NOT_FOUND),
            (Error::NotADirectory(path()), NOT_FOUND),
            (Error::StorageRequired(vec![]), NOT_FOUND),
            (Error::StorageNotFound("x".into()), NOT_FOUND),
            (Error::Conflicts(vec![RelPath::root()]), CONFLICTS),
            (Error::Cancelled, INTERRUPTED),
            (Error::InvalidPath(PathError::Empty), USAGE),
            (Error::Disconnected, GENERIC),
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
        report.failed.push((RelPath::root(), "boom".into()));
        assert_eq!(code_for_report(&report), ExitCode::from(TRANSFER_FAILED));
        report.interrupted = true;
        assert_eq!(code_for_report(&report), ExitCode::from(INTERRUPTED));
    }

    #[test]
    fn non_transfer_outcomes_are_success() {
        assert_eq!(code_for_outcome(&Outcome::Done), ExitCode::from(SUCCESS));
    }
}
