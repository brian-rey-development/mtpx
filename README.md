# mtpx

rsync for your phone. Fast, incremental file transfers over MTP, in pure Rust.

## Status

Milestone 1 is feature complete and being validated on hardware: `devices`, `ls`, `pull`,
`sync`, `--dry-run`, and resumable transfers that survive Ctrl-C and a pulled cable.
Nothing is released yet and the CLI surface may still change before 0.1.0.

## Why

Android File Transfer is discontinued, MTP mounts are flaky, `adb` needs developer mode,
and none of them knows what you already copied. `mtpx` scans both sides, plans the
difference, and moves only what changed, with resume when something interrupts it.

## Install

From source, with a Rust toolchain (1.85 or newer):

```sh
cargo install --path crates/mtpx
```

Linux needs permission to open the USB device; `mtpx devices` prints the vendor and
product ids for a udev rule. macOS claims MTP devices for Image Capture through
`ptpcamerad`; if `mtpx` reports the device is held by another process, `pkill ptpcamerad`
releases it for the session.

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

## What sync does

| Remote file | Local file | Result |
|---|---|---|
| present | absent | copied |
| present | same size | skipped |
| present | different size | `sync`: replaced; `pull`: refused unless `--overwrite` or `--skip-existing` |
| absent | present | left alone (nothing is ever deleted in this milestone) |

Files are compared by size. MTP devices report modification times inconsistently, so a
same-size file counts as unchanged; that is the same trade-off `rsync --size-only` makes.

## Resume and safety

A file in flight is written to `name.mtpx-part` next to a small JSON sidecar that records
which device and storage it came from, the size and modification time the phone reported,
and how many bytes landed. The next run resumes only when all of that still matches;
otherwise it starts the file over. A finished file is renamed into place, so a name without
the `.mtpx-part` suffix is always complete.

Ctrl-C finishes the current 4 MiB window, records the partial, prints how many files
remain, and exits 130. A second Ctrl-C exits at once.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success, including a sync with nothing to do |
| 1 | unexpected error |
| 2 | usage error |
| 3 | no device, one that does not answer (locked or charging-only), or more than one and no `--device` |
| 4 | the device is held by another process or the OS denied access |
| 5 | remote path or storage not found |
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
channel. That is what makes cancellation and resume safe by construction.

## Development

```sh
cargo test --workspace --features virtual-device   # no phone needed
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The `virtual-device` feature runs the whole pull and resume path against an in-process
MTP device backed by a temporary directory.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

## Credits

Built on [`mtp-rs`](https://github.com/vdavid/mtp-rs) by David Veszelovszki.
