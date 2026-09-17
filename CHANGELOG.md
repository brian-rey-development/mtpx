# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `mtpx devices`, `ls` (`-l`, `-R`), `pull` (`--overwrite`, `--skip-existing`, `--dry-run`)
  and `sync` (`--dry-run`).
- Single-file pulls: `mtpx pull /DCIM/Camera/IMG_0001.jpg ~/Desktop`.
- Global flags: `--device` (index or USB serial, `serial:` prefix for an all-digit serial),
  `--storage`, `-q`, `-v` up to `-vvv` (overrides `RUST_LOG`), `--no-color`,
  `--no-interactive`.
- Two progress bars on a terminal, one line per file elsewhere, plain tables on stdout, a
  help line per error, and exit codes a script can branch on (`mtpx --help` lists them).
- Files compare by kind and size, like `rsync --size-only`; timestamps are ignored.
- A file on one side and a directory on the other is skipped as a kind conflict, together
  with everything beneath it. Nothing is ever deleted to make room.
- On a case-folding destination (macOS, Windows) names match case-insensitively, and phone
  files that collide are reported instead of merged.
- Resume: a file in flight is `name.mtpx-part` plus a JSON sidecar keyed by device serial,
  storage, size and modification time, checkpointed every 64 MiB and finalised by rename,
  so a name without the suffix is always complete. The next run resumes or starts over,
  never splices.
- Ctrl-C finishes the current 4 MiB window and exits 130 with the count of files remaining.
  A lost device or a full disk ends the run with an `Aborted after ...` line and keeps the
  partial.
- A locked or charging-only phone is reported as unresponsive with the way out; `--device`
  and `--storage` selectors that match nothing list what is attached; objects the phone
  refuses to describe are counted as `left out` and listed by folder; `mtpx ls | head` ends
  quietly.
- `mtpx-core`: a `Device` facade (`open`, `ls`, `plan_pull`, `close`), `PullJob`, a pure
  planner with `ConflictPolicy`, fingerprinted resume, windowed downloads with cooperative
  cancellation, recovery from re-keyed object handles, read retries with backoff, and an
  event stream that ends with `Finished` or `Aborted`.
- `mtpx-core` result types are `#[non_exhaustive]` with constructors. `MtpError`,
  `MtpDateTime`, `CancelToken` and `UsbSpeed` are re-exported from `mtp-rs` 0.32, which is
  therefore a public dependency.
- `virtual-device` feature: the whole pull, resume and CLI path runs in tests without a
  phone, and the hidden `--virtual <dir>` flag drives the real binary against it.

### Security

- Device-controlled text (labels, serials, storage names, file names) is sanitized before
  display, including bidi controls and line separators, so an escape sequence in a file
  name cannot reach tables, prompts, progress lines or diagnostics.
- On Windows, names Win32 would reinterpret (`:`, trailing dots, `CON`, `COM1`) are refused
  as invalid paths.

[Unreleased]: https://github.com/brian-rey-development/mtpx/commits/main
