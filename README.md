# mtpx

[![CI](https://github.com/brian-rey-development/mtpx/actions/workflows/ci.yml/badge.svg)](https://github.com/brian-rey-development/mtpx/actions/workflows/ci.yml)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-blue)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE-MIT)

rsync for your phone: fast, incremental file transfers over MTP, in pure Rust

## Status

Pre-release. What works today: `devices`, `ls`, `pull`, `sync`, `--dry-run`, and resumable
transfers that survive Ctrl-C and a pulled cable. Tested against a Motorola moto g52 (2,214
files, 12.5 GB, interrupted and resumed; the log is in
[docs/hardware-runs](docs/hardware-runs/2026-09-16-moto-g52.md)). Push, delete and move
are not implemented, and the CLI surface may still change before 0.1.0.

## Why

Android File Transfer is discontinued, MTP mounts are flaky, `adb` needs developer mode,
and none of them knows what you already copied. `mtpx` scans both sides, plans the
difference, and moves only what changed, with resume when something interrupts it.

## Install

With a Rust toolchain (1.85 or newer), straight from the repository:

```sh
cargo install --git https://github.com/brian-rey-development/mtpx mtpx
```

or, from a checkout, `cargo install --path crates/mtpx`.

Linux needs permission to open the USB device. `mtpx devices` prints the vendor id; put it
in a udev rule, reload, and replug the phone:

```sh
# /etc/udev/rules.d/99-mtpx.rules
SUBSYSTEM=="usb", ATTR{idVendor}=="xxxx", MODE="0666"
```

```sh
sudo udevadm control --reload-rules
```

macOS claims MTP devices for Image Capture through `ptpcamerad`; if `mtpx` reports the
device is held by another process, `pkill ptpcamerad` releases it for the session.

On Windows, `mtp-rs` goes through Windows Portable Devices, so no driver is needed. The
test suite passes there in CI, but nobody has run it against a real phone on Windows yet.

## Quick start

```sh
mtpx devices                                  # what is plugged in
mtpx ls /DCIM/Camera -l                       # size, date, name
mtpx pull /DCIM/Camera ~/Pictures/phone       # copy, refuse to overwrite differing files
mtpx pull /DCIM/Camera/IMG_0001.jpg ~/Desktop # a single file works too
mtpx sync /DCIM/Camera ~/Pictures/phone       # copy new and changed files, skip the rest
mtpx sync /DCIM/Camera ~/Pictures/phone --dry-run
```

Paths on the phone are absolute. A phone with several storages takes a prefix
(`SD card:/DCIM`, `1:/DCIM`) or `--storage`.

## Options

Every command accepts these, before or after the subcommand:

| Flag | Effect |
|---|---|
| `--device <SERIAL\|INDEX>` | which phone: an index from `mtpx devices` or a USB serial; an all-digit serial needs the `serial:` prefix |
| `--storage <NAME\|INDEX>` | which storage; overrides the `storage:` prefix of every path argument |
| `-q`, `--quiet` | print only failures and the final summary |
| `-v`, `--verbose` | log more; repeat up to `-vvv` for debug and trace output |
| `--no-color` | disable colors and styling; `NO_COLOR` in the environment does the same |
| `--no-interactive` | never prompt; when a device or storage must be chosen, exit 3 or 5 with the list |

`pull` takes `--overwrite`, `--skip-existing` and `--dry-run`; `sync` takes `--dry-run`;
`ls` takes `-l`/`--long` and `-R`/`--recursive`. `mtpx <command> --help` has the details.

Bars, prompts and color follow the terminal: `CLICOLOR_FORCE=1` turns styling on for a pipe,
and `TERM=dumb` gets plain lines and no prompts. `-v` sets the log level outright;
`RUST_LOG` only applies when no `-v` is given.

## What sync does

| Remote file | Local file | Result |
|---|---|---|
| present | absent | copied |
| present | same size | skipped |
| present | different size | `sync`: replaced; `pull`: refused unless `--overwrite` or `--skip-existing` |
| file | directory, or vice versa | skipped as a kind conflict, together with everything beneath a directory; never replaced by deleting |
| absent | present | left alone; nothing is ever deleted |

Files are compared by size. MTP devices report modification times inconsistently, so a
same-size file counts as unchanged; that is the same trade-off `rsync --size-only` makes.

Names are compared byte for byte, with one exception: when the local volume folds letter
case (the macOS and Windows defaults), `A.jpg` and `a.jpg` count as one file, and two phone
files that differ only by case are reported as a name collision rather than silently
merged. A volume that rewrites names another way, such as an NFD-normalizing HFS+ or SMB
share, can make an unchanged file look new on every run.

## Resume and safety

A file in flight is written to `name.mtpx-part` next to a small JSON sidecar that records
which device and storage it came from, the size and modification time the phone reported,
and how many bytes landed, refreshed every 64 MiB. The next run resumes only when all of
that still matches, dropping anything past the last record; otherwise it starts the file
over. A finished file is renamed into place, so a name without the `.mtpx-part` suffix is
always complete.

Ctrl-C finishes the current 4 MiB window, records the partial, prints how many files
remain, and exits 130. A second Ctrl-C exits at once. When the phone disconnects or the
local disk fills mid-transfer, the run stops with an `Aborted after ...` line, keeps the
partial, and exits with that error's code; re-running resumes it.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success, including a sync with nothing to do |
| 1 | unexpected error |
| 2 | usage error |
| 3 | no device, one that does not answer or lists no storage (locked or charging-only), one that disconnected mid-command, or more than one and no `--device` |
| 4 | the device is held by another process or the OS denied access |
| 5 | remote path not found or not a directory, the local path is a file, the storage named by `--storage` does not exist, or the phone has several storages and none was chosen |
| 6 | some files failed; the summary lists them |
| 7 | `pull` found differing files and no policy flag |
| 130 | interrupted |

## Library

The CLI is a thin layer over `mtpx-core`, which exposes the same flow as a facade:

```rust
use mtpx_core::{CancelToken, Device, DeviceSelector, DevicePath, TransferOptions};
use std::path::Path;

async fn backup() -> mtpx_core::Result<()> {
    let device = Device::open(&DeviceSelector::Only).await?;
    let cancel = CancelToken::new();
    let (events, mut rx) = tokio::sync::mpsc::channel(256);
    tokio::spawn(async move { while let Some(event) = rx.recv().await { println!("{event:?}"); } });

    let remote: DevicePath = "/DCIM/Camera".parse()?;
    let job = device.plan_pull(&remote, Path::new("backup"), &TransferOptions::sync(), &cancel, &events).await?;
    let report = job.run(&cancel, &events).await?;
    println!("copied {} files", report.copied);
    device.close().await
}
```

Object handles never leave the library: plans and snapshots are paths plus metadata, so a
phone that re-keys its objects mid-transfer (Android does, after a media rescan) is handled
inside `mtpx-core`.

## How it works

```text
scan phone ──┐
             ├──> plan (pure, testable) ──> execute ──> events ──> progress bars
scan local ──┘
```

Downloads are windowed: each 4 MiB window is one MTP transaction, nothing is held
between windows, and the USB producer runs ahead of the disk writer through a bounded
channel. Cancelling between two windows therefore never leaves a half-written window, which
is what makes resume a matter of bookkeeping rather than luck.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --features virtual-device   # no phone needed
cargo doc --no-deps --workspace --features virtual-device
```

The `virtual-device` feature runs the whole pull and resume path against an in-process
MTP device backed by a temporary directory, and `--virtual <dir>` drives the real binary
against it. MSRV is 1.85; CI checks fmt, clippy, tests, docs, `cargo-deny`, and an MSRV
build on macOS, Ubuntu, and Windows.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full checklist and code standards,
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for conduct expectations,
[SECURITY.md](SECURITY.md) for how to report a vulnerability, and
[docs/design.md](docs/design.md) for the target design across milestones.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.

## Credits

Built on [`mtp-rs`](https://github.com/vdavid/mtp-rs) by David Veszelovszki.
