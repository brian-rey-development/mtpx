//! The `mtpx` binary against an in-process virtual phone: exit codes, stdout and stderr.
//!
//! Every assertion is on captured output and the exit code; nothing sleeps or polls.

#![cfg(feature = "virtual-device")]
#![allow(clippy::unwrap_used)]

mod errors;
mod ls;
mod support;
mod transfer;
