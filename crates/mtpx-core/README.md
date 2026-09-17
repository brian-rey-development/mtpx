# mtpx-core

Incremental MTP transfer engine: plan, transfer, sync and resume files on Android devices.
This is the library behind the [`mtpx`](https://github.com/brian-rey-development/mtpx) CLI;
the CLI is a thin layer over the facade below.

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
channel. That is what makes cancellation and resume safe by construction. A file in flight
is `name.mtpx-part` next to a JSON sidecar that records which device and storage it came
from, the size and modification time the phone reported, and how many bytes landed; the
next plan resumes only when all of that still matches.

## Features

- `virtual-device`: `Device::open_virtual` plus the re-exported `VirtualDeviceConfig` and
  `VirtualStorageConfig`, an in-process device backed by local directories so the full
  pull, sync and resume path runs in tests without a phone.
- `bench-internals`: exposes private planner inputs to the Criterion benches. Not part of
  the API.

`CancelToken`, `UsbSpeed`, `MtpError`, `MtpDateTime` and the virtual-device configs are
re-exported from `mtp-rs` 0.32 and are part of this crate's API, so a minor bump of
`mtp-rs` is a breaking change for `mtpx-core`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
