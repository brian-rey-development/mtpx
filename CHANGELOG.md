# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### M1 hardware validation (2026-09-16, Motorola moto g52, macOS, USB 2.0 High Speed)

- Camera folder of 2,214 files, 12.5 GB: one run interrupted with Ctrl-C after 36 s
  (457 files copied, exit 130, one partial with its sidecar), one resume run that
  finished the remaining 1,757 files in 6m 15s at 30.1 MB/s average, 0 failed,
  no part files left, sizes and modification times equal to the phone's.
- A second interrupt cut a 9 MB photo at two windows; the next run resumed it from
  8.4 MB and the result is byte-exact.
- Rerun on a synced folder: 0 copied, 2,214 skipped.
- Listing is one metadata request per object: 2,214 objects took 9 s right after
  unlocking and 39 s later, so scan time depends on the phone's state.
- Eight fixes came out of the run; see `docs/manual-checklist.md`.

### Fixed

- A cancel that lands after a file's last window finishes the file instead of failing it.
- A complete partial is finalised in place instead of being copied again.
- A phone that does not answer (locked, charging-only) is reported as unresponsive
  with the way out, exit 3.
- Errors and hints show the path as typed, without an unrequested storage prefix.
- `mtpx ls | head` ends quietly instead of panicking on the closed pipe.
- The summary times the whole command; a dry run lists only what would change;
  an interrupt during the scan prints one plain line.

### Added

- `mtpx devices`, `ls` (`-l`, `-R`), `pull` (`--overwrite`, `--skip-existing`, `--dry-run`)
  and `sync` (`--dry-run`), with two progress bars on a terminal and one line per file
  elsewhere, plain tables on stdout, a help line per error, and the documented exit codes.
- `mtpx-core`: `Device` facade (`open`, `ls`, `plan_pull`, `close`), `PullJob`, a pure
  planner with `ConflictPolicy`, fingerprinted resume through `.mtpx-part` files and JSON
  sidecars, windowed MTP downloads with cooperative cancellation, transparent recovery
  from re-keyed object handles, and read retries with backoff.
- Single-file pulls: `mtpx pull /DCIM/Camera/IMG_0001.jpg ~/Desktop`.
- `virtual-device` feature: the whole pull, resume and CLI path runs in tests without a phone.
- Workspace scaffolding: `mtpx-core` library crate and `mtpx` CLI crate.
- CI on macOS, Ubuntu and Windows: fmt, clippy, test, doc, cargo-deny, MSRV build.
