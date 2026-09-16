# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
