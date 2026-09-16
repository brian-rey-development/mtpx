# mtpx design

Status: revision 2, for review
Date: 2026-09-16
Author: Brian Rey (with Claude)

Revision 2 incorporates an external design review. The main changes: MTP handles no longer leak into snapshots or plans, the endpoint abstraction is transfer-shaped instead of filesystem-shaped, resume is fingerprinted, retries are classified per operation, deletes are post-order, sync policy is split into compare / conflict / delete, the public API is narrowed to a facade, JSON output is a versioned contract separate from the internal event enum, cancellation is a state machine built on windowed downloads, `doctor --fix` is reversible, storage selection is a separate concept from paths, and v0.1 is a vertical slice validated on real hardware before polish.

## 1. Summary

`mtpx` is a fast, incremental file transfer tool for MTP devices (Android phones, e-readers, cameras) written in pure Rust. It is "rsync for your phone": list, pull, push, move and sync folders between a device and a computer over USB, without libmtp, libusb, Android File Transfer or a mounted filesystem.

Two crates in one workspace:

- `mtpx-core`: a library with a small facade API. Discovery, path model, planning, transfer engine, progress events. No terminal code.
- `mtpx`: the CLI. Rendering, flags, config, exit codes. Thin.

A TUI is planned on the same core. Nothing in the core assumes a terminal, and nothing about core stability depends on the TUI.

The MTP protocol is handled by `mtp-rs` 0.32 (pure Rust on `nusb`). `mtpx` does not reimplement MTP. It adds the layer that turns a protocol into a tool people use every day, designed around the protocol's actual failure modes: ephemeral handles, single serial session, non-resumable uploads, devices that wedge when a transfer is dropped mid-flight.

## 2. Goals

- Fast: one scan per side, in-memory planning, streaming transfers with bounded memory, disk I/O overlapped with USB I/O.
- Correct: local writes are atomic (temp file plus rename), every transfer is length-verified, resume only continues the same logical source object, a `--move` never deletes a source before its copy is verified.
- Incremental: `sync` transfers only what is missing or different and can be re-run at any time.
- Predictable: `--dry-run` shows the exact plan, `--json` is a versioned machine-readable stream, exit codes are stable and documented.
- Polished: progress with speed and ETA, colored summaries, actionable error messages, interactive device picker when needed.
- Cross-platform: macOS, Linux, Windows, inherited from `mtp-rs`. CI runs on all three.
- Reusable: `mtpx-core` is publishable and usable without the CLI.

## 3. Non-goals

- Bidirectional sync with conflict resolution. Sync is one-way, device to computer. Push is a plain copy.
- Mounting the device as a filesystem. That is `mtp-mount`'s job.
- Content hashing for skip decisions. MTP has no hash operation; hashing means downloading everything.
- Vendor extensions, MTPZ, playlists, camera capture. Pre-Android 5 devices.
- A GUI.

## 4. Positioning

| Tool | Incremental sync | Dry run | Resume | Move | Filters | Profiles | Progress | Pure Rust | Notes |
|---|---|---|---|---|---|---|---|---|---|
| Android File Transfer | no | no | no | no | no | no | basic | no | discontinued, unreliable |
| OpenMTP | no | no | no | no | no | no | yes | no | Electron, GUI only |
| `adb pull` | no | no | no | no | no | no | basic | no | needs USB debugging |
| `mtp-rs-cli` | no | no | no | remote only | no | no | basic | yes | reference CLI for the library |
| `mtpx` | yes | yes | yes | yes | yes | yes | rich | yes | this project |

Tagline: "rsync for your phone. Fast, incremental file transfers over MTP, in pure Rust."

## 5. Architecture

### 5.1 Workspace layout

```text
mtpx/
├── Cargo.toml                  workspace, shared lints, shared deps
├── rust-toolchain.toml
├── rustfmt.toml
├── clippy.toml
├── deny.toml
├── README.md
├── CHANGELOG.md
├── CONTRIBUTING.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── .github/workflows/          ci.yml, release.yml
├── docs/
│   ├── cli.md                  full command reference, JSON schema, exit codes
│   ├── sync-semantics.md       what sync does and does not do
│   ├── troubleshooting.md      macOS ptpcamerad, Linux udev, Windows notes
│   └── superpowers/specs/      this document
└── crates/
    ├── mtpx-core/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs            facade re-exports only
    │   │   ├── device.rs         Device (facade): open, ls, plan, transfer, remove
    │   │   ├── error.rs
    │   │   ├── path.rs           RemotePath, RelPath, DevicePath, StorageSelector
    │   │   ├── entry.rs          Entry, EntryKind, ModifiedTime, Snapshot
    │   │   ├── filter.rs         Filter
    │   │   ├── options.rs        TransferOptions, ComparePolicy, ConflictPolicy, DeletePolicy
    │   │   ├── plan/
    │   │   │   ├── mod.rs        Plan, Action, PlanSummary
    │   │   │   └── planner.rs    pure diff
    │   │   ├── event.rs          ProgressEvent, Report
    │   │   ├── discovery.rs      list devices, select, exclusive-holder lookup
    │   │   ├── doctor.rs         platform diagnostics
    │   │   └── internal/         pub(crate) only
    │   │       ├── endpoint.rs   Endpoint trait, ByteStream, WriteRequest
    │   │       ├── local.rs      LocalEndpoint
    │   │       ├── mtp/
    │   │       │   ├── mod.rs    MtpEndpoint
    │   │       │   ├── resolver.rs   path to handle, cache, stale-handle recovery
    │   │       │   └── transfer.rs   windowed download, upload with partial cleanup
    │   │       ├── partial.rs    .mtpx-part sidecar, fingerprint
    │   │       ├── retry.rs      RetryClass, per-operation policies
    │   │       ├── executor.rs   runs a Plan, emits events, cancellation state machine
    │   │       └── pump.rs       bounded channel copy, USB/disk overlap
    │   ├── benches/planner.rs
    │   └── tests/
    │       ├── common/mod.rs     virtual device fixtures
    │       ├── planner.rs
    │       ├── pull.rs
    │       ├── sync.rs
    │       └── resume.rs
    └── mtpx/
        ├── Cargo.toml
        ├── src/
        │   ├── main.rs
        │   ├── cli.rs            clap derive tree
        │   ├── exit.rs           ExitCode mapping
        │   ├── config.rs         profiles
        │   ├── commands/         one file per subcommand
        │   └── ui/
        │       ├── progress.rs   indicatif renderer
        │       ├── json.rs       JsonEventV1, From<&ProgressEvent>
        │       ├── table.rs
        │       ├── prompt.rs     device picker, confirmations
        │       └── theme.rs
        └── tests/cli.rs          assert_cmd against the virtual device
```

### 5.2 Layering

```text
                    ┌──────────────┐
                    │   mtpx CLI   │   clap, indicatif, miette, config
                    └──────┬───────┘
                           │ Device facade, Plan, ProgressEvent, Report
                    ┌──────▼───────┐
                    │  mtpx-core   │
                    │  Planner     │   pure, no I/O, no handles
                    │  Executor    │   drives Endpoints, emits events
                    │  Endpoint    │   pub(crate): Local, Mtp
                    │  Resolver    │   path to handle, just in time
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │   mtp-rs     │   MTP/PTP over nusb
                    └──────────────┘
```

Rules:

- The CLI never calls `mtp-rs`. Everything goes through the `mtpx-core` facade.
- The planner never sees an MTP handle. Handles are session-scoped transport identifiers, not file identities. They live only inside `internal::mtp::resolver` and are looked up immediately before each operation.

### 5.3 The central idea: logical plan, just-in-time transport

```text
scan(source) ─┐
              ├─> Planner(options) ─> Plan (paths + metadata) ─> Executor ─> ProgressEvent*
scan(dest)   ─┘                                                     │
                                                                    ▼
                                                        Endpoint.resolve(path) -> handle
                                                        operate, recover on StaleHandle
```

- `pull` is `Plan` with `source = Mtp, dest = Local`.
- `push` is the same with the endpoints swapped.
- `sync` is `pull` with `conflict = SourceWins`, identical files skipped, optional `delete = Extraneous`.
- `--move` is any of the above with `delete_source_after_verify = true`.
- `--dry-run` runs the planner and prints the plan without an executor.

## 6. Core API (`mtpx-core`)

### 6.1 Public surface

Only these are `pub`. Everything under `internal/` is `pub(crate)`. Public structs and enums that may grow are `#[non_exhaustive]`. `#![deny(missing_docs)]`, `#![forbid(unsafe_code)]`.

```rust
pub use device::{Device, DeviceSelector, OpenOptions};
pub use discovery::{list_devices, DeviceSummary, ExclusiveHolder};
pub use doctor::{diagnose, Diagnosis, Check};
pub use path::{DevicePath, RemotePath, RelPath, StorageSelector, PathError};
pub use entry::{Entry, EntryKind, ModifiedTime, Snapshot, SkippedEntry};
pub use filter::Filter;
pub use options::{TransferOptions, ComparePolicy, ConflictPolicy, DeletePolicy, Direction};
pub use plan::{Plan, Action, CopyReason, SkipReason, PlanSummary, Side};
pub use event::{ProgressEvent, Report, Hint};
pub use error::{Error, Result};
pub use mtp_rs::CancelToken;
```

The `Endpoint` trait stays private until `pull`, `push`, `sync`, resume and move have all shipped and the abstraction has proven itself. Exposing it is a semver commitment that is cheap to make later and expensive to undo.

### 6.2 Facade

```rust
pub struct Device { /* session, storages, resolver cache */ }

impl Device {
    pub async fn open(selector: &DeviceSelector, opts: &OpenOptions) -> Result<Self>;
    pub fn info(&self) -> &DeviceInfo;
    pub fn storages(&self) -> &[StorageSummary];

    pub async fn ls(&self, path: &DevicePath, recursive: bool, cancel: &CancelToken) -> Result<Snapshot>;

    pub async fn plan(&self, dir: Direction, remote: &DevicePath, local: &Path, opts: &TransferOptions, cancel: &CancelToken, events: &Sender<ProgressEvent>) -> Result<Plan>;

    pub async fn transfer(&self, plan: &Plan, cancel: &CancelToken, events: &Sender<ProgressEvent>) -> Result<Report>;

    pub async fn remove(&self, path: &DevicePath, recursive: bool, cancel: &CancelToken, events: &Sender<ProgressEvent>) -> Result<Report>;
    pub async fn mkdir(&self, path: &DevicePath) -> Result<()>;

    pub async fn close(self) -> Result<()>;
}
```

`plan` emits `ScanStarted`, `ScanProgress`, `ScanFinished` and `PlanReady`. `transfer` emits the per-file events and `Finished`. A `--dry-run` is `plan` without `transfer`.

### 6.3 Paths and storage selection

```rust
/// Which storage on the device. Separate from the path: "Internal" is not a directory.
pub enum StorageSelector { Default, Index(usize), Named(String) }

/// Absolute, normalized POSIX-style path inside one storage. No `.`, `..`, or empty segments.
pub struct RemotePath(Vec<String>);

/// A storage plus a path. What every CLI remote argument parses into.
pub struct DevicePath { pub storage: StorageSelector, pub path: RemotePath }

/// Relative path inside a snapshot, shared by both sides. Always forward slashes.
pub struct RelPath(Vec<String>);
```

Parsing: `/DCIM/Camera` gives `StorageSelector::Default`; `Internal:/DCIM/Camera` gives `Named("Internal")`; `1:/Music` gives `Index(1)`. Named matches case-insensitively against `StorageInfo.description` and `volume_identifier`.

`Default` resolves to the only storage when the device has exactly one. When it has several and no storage is named, `Device::open` returns `Error::StorageRequired(Vec<StorageSummary>)`. The CLI turns that into a picker on a TTY, or exit code 5 with the list otherwise. A default storage can be stored per device in config. Destructive commands never operate on "whichever storage enumerated first".

### 6.4 Entries and snapshots

```rust
pub enum EntryKind { File, Dir }

/// Second-resolution timestamp. MTP DateTime has no sub-second precision, and FAT rounds to 2 s.
pub struct ModifiedTime(SystemTime);

pub struct Entry {
    pub path: RelPath,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<ModifiedTime>,
}

/// Recursive listing of one root, sorted by path, directories before their children.
pub struct Snapshot { pub root: String, pub entries: Vec<Entry>, pub skipped: Vec<SkippedEntry> }
```

No handle. `skipped` records objects the device refused to describe (from `mtp-rs`'s `ObjectCollection.skipped`) and is surfaced in the report. If a folder returns handles but every metadata lookup fails, `mtp-rs` already reports an error instead of an empty folder; `mtpx` propagates it, because treating a read failure as "empty" would turn into `--delete` data loss.

### 6.5 Filters

```rust
pub struct Filter { pub include: Vec<Glob>, pub exclude: Vec<Glob>, pub since: Option<ModifiedTime>, pub until: Option<ModifiedTime> }
```

Globs via `globset`. Filters apply to files. Directories survive if any descendant survives. Applied during scan, so the planner never sees filtered entries.

### 6.6 Transfer options

```rust
pub enum Direction { Pull, Push }

/// How two same-path files are judged equal.
pub enum ComparePolicy { Size, SizeAndMtime }

/// What to do when the destination already has a same-path file that is not equal.
pub enum ConflictPolicy { Fail, SourceWins, Skip }

/// What to do with destination files that do not exist on the source.
pub enum DeletePolicy { Keep, Extraneous }

pub struct TransferOptions {
    pub compare: ComparePolicy,
    pub conflict: ConflictPolicy,
    pub delete: DeletePolicy,
    pub delete_source_after_verify: bool,
    pub filter: Filter,
}
```

Defaults per command:

| Command | compare | conflict | delete |
|---|---|---|---|
| `pull` / `push` | Size | Fail | Keep |
| `sync` | Size | SourceWins | Keep (`--delete` gives Extraneous) |

`ConflictPolicy::Fail` fails at plan time, before anything is touched, listing every conflicting path and suggesting `--overwrite` or `--skip-existing`. Equal files (per `compare`) are never conflicts; they are skipped. `SizeAndMtime` uses a 2 second tolerance.

Documented precisely: by default, same-path files with the same size are considered unchanged. Two different files of identical size at the same path will not be re-transferred. Pass `--compare size-mtime` for a stricter check.

### 6.7 Plan

```rust
pub enum Action {
    Mkdir  { path: RelPath },
    Copy   { path: RelPath, size: u64, modified: Option<ModifiedTime>, resume_from: u64, reason: CopyReason },
    Skip   { path: RelPath, reason: SkipReason },
    Delete { path: RelPath, kind: EntryKind, side: Side },
}

pub enum CopyReason { New, SizeDiffers, MtimeDiffers }
pub enum SkipReason { Identical, Conflict }
pub enum Side { Source, Dest }

pub struct Plan { pub actions: Vec<Action>, pub summary: PlanSummary }

pub(crate) fn plan(source: &Snapshot, dest: &Snapshot, partials: &Partials, opts: &TransferOptions) -> Result<Plan>;
```

Invariants, each with a planner test:

- `Mkdir` precedes every `Copy` beneath it.
- `Copy` actions are in source order.
- `Delete { side: Dest }` actions come after all copies and are in post-order: children before parents, deepest first. MTP devices are inconsistent about deleting non-empty folders; post-order works everywhere.
- `Delete { side: Source }` is never emitted by the planner. The executor performs it immediately after each verified copy, so an interruption leaves every file either fully copied and deleted, or untouched on the source.
- `resume_from > 0` only when the partial's fingerprint matches the source entry (section 6.9).
- With `ConflictPolicy::Fail`, any conflict makes `plan` return `Error::Conflicts(Vec<RelPath>)`; no partial plan.

### 6.8 Executor and events

```rust
pub enum ProgressEvent {
    ScanStarted { side: Side }, ScanProgress { side: Side, found: u64 }, ScanFinished { side: Side, entries: u64, skipped: u64 },
    PlanReady { summary: PlanSummary, hints: Vec<Hint> },
    FileStarted { path: RelPath, size: u64, resume_from: u64 },
    FileProgress { path: RelPath, bytes: u64 },
    FileFinished { path: RelPath, bytes: u64, elapsed: Duration },
    FileFailed { path: RelPath, error: String, will_retry: bool },
    Deleted { path: RelPath, side: Side },
    Skipped { path: RelPath, reason: SkipReason },
    Interrupted { remaining_files: u64 },
    Finished { report: Report },
}

pub enum Hint { SlowLink { speed: UsbSpeed, bytes: u64 }, DeviceSkippedObjects { count: usize } }

pub struct Report { pub copied: u64, pub bytes: u64, pub skipped: u64, pub deleted: u64, pub failed: Vec<(RelPath, String)>, pub elapsed: Duration, pub interrupted: bool }
```

This enum is the Rust API. It is not the JSON contract (section 7.2).

Copy loop for one file:

1. `dest.partial(path)` returns the sidecar if present; the planner already decided `resume_from`.
2. `source.read(path, resume_from)` returns a `ByteStream`. For MTP this resolves the handle now, not at plan time.
3. `dest.write(WriteRequest { path, expected_size, resume_from, modified }, stream)` pumps chunks through a bounded channel (depth 8, 1 MiB chunks). The USB side fills it, the disk side drains it in `spawn_blocking`. `FileProgress` at most every 100 ms.
4. `write` returns `WriteOutcome { bytes }`. The executor checks `bytes == expected_size`. A mismatch is `Error::LengthMismatch`; the partial is kept, the file is reported failed.
5. With `delete_source_after_verify`, `source.remove(path)` now, then `Deleted { side: Source }`.

Guarantees, stated precisely: local destinations use atomic temp-file replacement. Remote destinations use the strongest commit the device supports: an upload creates the object then streams data, and a failed data phase deletes the partial object `mtp-rs` reports. Every transfer is length-verified. Length verification is not integrity verification; `push --verify` (read back and compare) is the integrity option.

### 6.9 Partial files and resume

A local partial is two files:

```text
IMG_123.jpg.mtpx-part
IMG_123.jpg.mtpx-part.json
```

```json
{
  "version": 1,
  "identity": { "device_serial": "ZY22H5V3TK", "storage": "Internal shared storage" },
  "path": "IMG_123.jpg",
  "fingerprint": { "size": 124124515, "modified": 1757874153 },
  "bytes": 81000000
}
```

`path` is relative to the transfer root and `modified` is Unix seconds in the device's local time. Resume happens only when `identity` matches the device the transfer talks to, `path` matches the entry, the fingerprint (size, modified) equals the current source entry, and the `.mtpx-part` length equals `bytes`. Otherwise the partial is discarded and the copy restarts from zero, with `CopyReason::New`. This prevents concatenating two different files that happened to share a name.

MTP uploads are not resumable in place. `MtpEndpoint::partial` always returns `None`.

### 6.10 Endpoint (private)

```rust
pub(crate) trait Endpoint: Send + Sync {
    async fn scan(&self, filter: &Filter, cancel: &CancelToken, progress: &dyn Fn(u64)) -> Result<Snapshot>;
    async fn stat(&self, path: &RelPath) -> Result<Option<Entry>>;
    async fn read(&self, path: &RelPath, offset: u64) -> Result<ByteStream>;
    async fn write(&self, req: WriteRequest, input: ByteStream) -> Result<WriteOutcome>;
    async fn partial(&self, path: &RelPath) -> Result<Option<PartialInfo>>;
    async fn mkdir(&self, path: &RelPath) -> Result<()>;
    async fn remove(&self, path: &RelPath, kind: EntryKind) -> Result<()>;
    fn caps(&self) -> EndpointCaps;
}

pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;
```

`LocalEndpoint`: `std::fs` inside `spawn_blocking`, `BufWriter` 1 MiB, writes to `.mtpx-part`, sidecar written before the first byte and updated on `abort`, `finish` verifies length, sets mtime with `filetime`, `fsync`, renames.

`MtpEndpoint`: wraps one `Storage` and a root `RemotePath`. `read` uses `download_windowed(handle, ByteRange::From(offset), 8 MiB)` and yields each window as chunks. `write` uses `upload_with_progress`; on `UploadError { partial: Some(h) }` it deletes `h` before returning the error. `scan` uses `collect_objects_recursive` reporting objects found so far.

### 6.11 Resolver and stale handles

```rust
pub(crate) struct Resolver { storage: Storage, root: ObjectHandle, cache: HashMap<RelPath, ObjectHandle> }

impl Resolver {
    async fn resolve(&self, path: &RelPath) -> Result<ObjectHandle>;   // cache, else list parent and fill
    fn invalidate(&self, path: &RelPath);                               // drops path and all descendants
    async fn with_handle<T>(&self, path: &RelPath, op: impl Fn(ObjectHandle) -> Fut<T>) -> Result<T>;
}
```

`with_handle` resolves, runs the operation, and on `mtp_rs::Error::StaleHandle` invalidates the parent, re-lists it, re-resolves, and retries exactly once. If the path is gone after re-listing, the error is `Error::SourceVanished(path)` and the file is reported failed without aborting the batch. Every `MtpEndpoint` operation goes through `with_handle`; no handle is held across an `await` on anything other than that operation.

### 6.12 Retry classes

```rust
pub(crate) enum RetryClass { Safe, Resolve, Reconcile, Never }
```

| Operation | Class | Behavior |
|---|---|---|
| list, stat, read window | Safe | retry up to 3 times, 500 ms doubling, on `is_retryable()` errors |
| any op on `StaleHandle` | Resolve | invalidate, re-list, re-resolve, retry once |
| mkdir | Reconcile | re-list parent; if the folder now exists, success |
| remove | Reconcile | re-list parent; if the object is now absent, success |
| upload | Reconcile | delete partial handle if reported, re-stat dest; if a same-size object exists, treat as done, else retry once |
| anything on `Disconnected`, `DeviceReset`, `NoDevice`, `PermissionDenied`, `ExclusiveAccess` | Never | abort the batch: emit `Finished` with the partial report, return `Err` |
| anything on `Cancelled` | Never | interrupt the batch: emit `Interrupted` and `Finished`, return `Ok(report)` with `interrupted` set |
| `AccessDenied` (per object: read-only storage, write-protected object) | Never | fail the file, continue |

The executor never retries a write blindly. A lost response after `SendObjectInfo` is the classic way to create duplicates, and `Reconcile` exists for that.

### 6.13 Cancellation state machine

Downloads are windowed, so no MTP session is held between windows and dropping a `WindowedDownload` is safe by construction. That is the reason windowed is the default even though continuous transfers avoid one round-trip per 8 MiB.

```text
Running ──Ctrl-C──> Draining: finish the current window (at most 8 MiB, about 80 ms)
Draining ─────────> Persisting: flush the .mtpx-part, write the sidecar with bytes so far
Persisting ───────> Reporting: emit Interrupted { remaining_files }, then Finished { report.interrupted = true }
Reporting ────────> Closed: close the session cleanly, return Ok(report) with report.interrupted
```

A second Ctrl-C during `Draining` exits the process immediately; the sidecar may then be behind the `.mtpx-part`, which the next run detects (`bytes` mismatch) and restarts that file.

Uploads mid-flight on cancel: `upload_with_progress` returns `ControlFlow::Break`, the partial remote object is deleted, the file is reported failed with "interrupted".

A continuous-download strategy (`Storage::download` with `ByteRange`) may be added later behind `TransferStrategy::Continuous` if a benchmark on real hardware shows a material gain. It would need `FileDownload::cancel` wired into the state machine first.

### 6.14 Discovery, doctor, errors

```rust
pub struct DeviceSummary { pub serial: Option<String>, pub label: String, pub vendor_id: u16, pub product_id: u16, pub location_id: u64, pub speed: Option<UsbSpeed> }
pub enum DeviceSelector { Only, Serial(String), Index(usize) }
pub fn list_devices() -> Result<Vec<DeviceSummary>>;

pub struct ExclusiveHolder { pub pid: u32, pub name: String }   // macOS: ioreg UsbExclusiveOwner
pub async fn diagnose() -> Diagnosis;   // checks: devices visible, exclusive holder, AFT running, udev rule, link speed, session opens, root lists
```

```rust
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    NoDevice,
    AmbiguousDevice(Vec<DeviceSummary>),
    StorageRequired(Vec<StorageSummary>),
    StorageNotFound(String),
    ExclusiveAccess { holder: Option<ExclusiveHolder> },
    PermissionDenied,
    RemotePathNotFound(DevicePath),
    NotADirectory(DevicePath),
    InvalidPath(#[from] PathError),
    Conflicts(Vec<RelPath>),
    SourceVanished(RelPath),
    LengthMismatch { path: RelPath, expected: u64, actual: u64 },
    Disconnected,
    Cancelled,
    Unsupported(&'static str),
    Mtp(#[from] mtp_rs::Error),
    Io(#[from] std::io::Error),
}
```

The core never prints, never exits, never reads env vars or config files.

### 6.15 Feature flags

- `virtual-device`: re-exports `mtp-rs`'s virtual device and adds `Device::open_virtual(dir)`. Used by tests and by the CLI's hidden `--virtual <dir>` flag in debug builds.
- `tracing`: forwards `mtp-rs` tracing.

## 7. CLI (`mtpx`)

### 7.1 Commands

```text
mtpx devices
mtpx ls [REMOTE] [-l] [-R]
mtpx pull <REMOTE> <LOCAL> [--overwrite | --skip-existing] [--move] [--dry-run]
mtpx push <LOCAL> <REMOTE> [--overwrite | --skip-existing] [--move] [--dry-run] [--verify]
mtpx sync <REMOTE> <LOCAL> [--delete] [--move] [--dry-run] [--compare size|size-mtime]
mtpx sync <PROFILE>
mtpx rm <REMOTE> [-r] [--yes]
mtpx mkdir <REMOTE>
mtpx doctor [--fix] [--force]
mtpx completions <SHELL>
```

Global flags: `--device <SERIAL|INDEX>`, `--storage <NAME|INDEX>`, `--json`, `--quiet`, `--verbose` (repeatable), `--no-color`, `--no-interactive`, `--yes`, `--include <GLOB>`, `--exclude <GLOB>`, `--since <DATE|DURATION>`, `--until`.

Semantics:

- `pull`/`push`: copy a file or tree. Equal files are skipped. Differing existing files fail the plan with the list, unless `--overwrite` (SourceWins) or `--skip-existing` (Skip). `--move` deletes each source file after its copy is length-verified.
- `sync`: new and differing files are copied (source wins), equal files skipped, nothing deleted unless `--delete`. `--delete` prints the list and asks on a TTY unless `--yes`. `--move` means "offload": the device ends up without the synced files.
- `--dry-run`: prints the plan and totals, exit 0, touches nothing on either side.
- `rm -r` asks with count and total size unless `--yes`.
- Multiple storages and no `--storage`: picker on TTY, exit 5 with the list otherwise.

### 7.2 Output

TTY:

```text
Moto g52 (USB 2.0 High Speed, Internal)
Scanning /DCIM/Camera ... 1,284 objects
Scanning ~/Pictures/motog52 ... 1,102 files

Plan: copy 182 files (4.3 GB, 81 MB resumable), skip 1,102, delete 0

IMG_20260914_182233.jpg    ████████████████████  12.4 MB  38.1 MB/s
Overall  ▕████████░░░░░░░░░░░░▏ 1.2 / 4.3 GB  41%  32.5 MB/s  eta 1m 35s  (61/182)

Done in 2m 12s: 182 copied (4.3 GB, 33.1 MB/s avg), 1,102 skipped, 0 failed
```

- Two `indicatif` bars: current file, overall (bytes, percent, 3 second moving-average speed, ETA, file count).
- Non-TTY: one line per file event, no bars, no colors.
- `--quiet`: errors and the final summary line.
- `NO_COLOR` and `--no-color` respected. ASCII fallback for non-UTF-8 locales.

`--json` is a separate, versioned contract:

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JsonEventV1 { ScanStarted { .. }, FileStarted { .. }, /* mirrors ProgressEvent */ }

impl From<&ProgressEvent> for JsonEventV1 { .. }
```

Every line carries `"version": 1`. Field names are documented in `docs/cli.md`. The internal `ProgressEvent` can change without breaking `mtpx sync camera --json | jq`.

### 7.3 Errors and diagnostics

`miette` reports with a `help` section.

- `ExclusiveAccess` on macOS:

  ```text
  error: the device is held by another process
    ptpcamerad (pid 412) has exclusive access to "Moto g52"

  help: macOS claims MTP devices for Image Capture.
    Run `mtpx doctor --fix` to release it for this session.
    If Android File Transfer is installed, quit it.
    See docs/troubleshooting.md for permanent options.
  ```

  The error message never suggests disabling an OS service. That option lives in the troubleshooting doc with its trade-offs.

- `NoDevice`: "no MTP device found. Unlock the phone and choose File transfer / MTP in the USB notification."
- `AmbiguousDevice` / `StorageRequired`: picker on TTY, otherwise the list and the flag to use.
- `PermissionDenied` on Linux: the udev rule snippet.
- `Conflicts`: the list, then "use --overwrite to replace them or --skip-existing to leave them".
- `LengthMismatch`: the file, "the partial was kept; the next run resumes it".

`mtpx doctor` is diagnostic only. `--fix` on macOS: identify the holder, `SIGTERM` it, wait 500 ms, retry opening up to 5 times. `SIGKILL` only with `--force`. Never sudo, never permanent.

### 7.4 Exit codes

| Code | Meaning |
|---|---|
| 0 | success (sync with nothing to do is success) |
| 1 | generic error |
| 2 | usage error |
| 3 | no device or ambiguous device |
| 4 | device access denied (exclusive access, permissions) |
| 5 | remote path or storage not found, storage required |
| 6 | one or more transfers failed (summary printed) |
| 7 | conflicts found (plan refused) |
| 130 | interrupted |

### 7.5 Config and profiles

Resolved with `directories` (`~/Library/Application Support/mtpx/config.toml` on macOS).

```toml
[defaults]
device = "ZY22H5V3TK"

[devices.ZY22H5V3TK]
storage = "Internal"

[profiles.camera]
source = "/DCIM/Camera"
dest = "~/Pictures/motog52"
exclude = ["*.tmp", ".thumbnails/**"]

[profiles.offload]
source = "/DCIM/Camera"
dest = "/Volumes/Archive/phone"
move = true
```

CLI flags override profile values. `mtpx sync` with no argument and one profile runs it; with several, lists them.

### 7.6 Signals

Ctrl-C triggers the cancellation state machine (6.13). Summary says "interrupted, N files remaining, re-run to resume". Exit 130.

## 8. Performance

MTP is one serial session per device. No per-file parallelism exists. Throughput comes from:

- One recursive scan per side, plan in memory, no per-decision round-trips.
- Overlapping USB reads with disk writes through a bounded channel and `spawn_blocking`.
- 8 MiB windows: one extra round-trip per 8 MiB, about 1% at USB 2.0 rates, in exchange for safe cancellation.
- Cached folder handles in the resolver so `mkdir` and uploads do not re-list parents.
- `BufWriter` 1 MiB on local writes.

Targets on a USB 2.0 phone: sustained transfer near the link limit (30 to 40 MB/s), 5,000 objects scanned in under 10 seconds, 100k entries planned in under 50 ms, peak memory under 64 MiB regardless of file sizes. `benches/planner.rs` (criterion) guards the planner.

## 9. Testing

### 9.1 Unit

- `path.rs`: parse and normalize tables, storage prefixes, trailing slashes, `..` rejection, Unicode.
- `filter.rs`: include/exclude precedence, `since` parsing (`7d`, ISO dates).
- `planner.rs`: new, identical, size differs, mtime within and outside tolerance, extraneous with each `DeletePolicy`, resume with matching and mismatching fingerprint, `Mkdir` before children, post-order deletes, filters removing whole subtrees, `ConflictPolicy::Fail` returning `Conflicts`.
- `partial.rs`: sidecar round-trip, fingerprint equality rules.
- `retry.rs`: class selection per error and operation.

### 9.2 Integration, no hardware

Every test uses `mtp-rs`'s virtual device over a `tempdir`. Runs on all three platforms in CI.

- pull a tree: byte equality and mtime.
- pull twice: second run copies nothing.
- sync `--delete`: only extraneous files removed, post-order verified by observing the virtual backing dir.
- cancel after N bytes: `.mtpx-part` and sidecar exist; second run resumes at N; final equality.
- resume with a replaced source of the same name and size but different mtime: partial discarded, restart from zero.
- `--move`: source objects gone only for verified files.
- stale handle: rename the backing file between scan and copy so the virtual device re-keys; copy still succeeds after re-resolve.
- length mismatch (backing file truncated after scan): exit 6, partial kept.
- device-skipped objects appear in the report.
- push a tree; failed upload deletes the partial object.

### 9.3 CLI

`assert_cmd` against a debug build with `--virtual <dir>`: exit codes, every `--json` line deserializes into `JsonEventV1`, `--dry-run` leaves both sides byte-identical, `--no-interactive` with two devices exits 3, conflicts exit 7.

### 9.4 Hardware

`docs/manual-checklist.md` for the Moto g52: `doctor`, `ls`, pull one file, sync `/DCIM/Camera`, Ctrl-C mid-transfer, resume, `--move` a throwaway folder, unplug mid-transfer. Milestone 1 is not done until this passes.

## 10. Code standards

The goal is code a senior engineer reads once and understands. Concretely:

- Rust 2024, MSRV 1.85, `rust-toolchain.toml`.
- Workspace lints: `clippy::pedantic` and `clippy::nursery` warn, `unsafe_code = "forbid"`, `missing_docs` deny in core, `unwrap_used` and `expect_used` deny outside tests.
- Functions under 20 lines. A function that needs a comment to explain its flow is two functions.
- One concept per module. Modules over 300 lines get split.
- Named constants for every number: `DOWNLOAD_WINDOW`, `PUMP_DEPTH`, `MTIME_TOLERANCE`, `PROGRESS_INTERVAL`.
- Early returns, no nested `match` deeper than two levels, `?` everywhere, no `Box<dyn Error>`.
- Types encode invariants: `RemotePath` cannot contain `..`, `ModifiedTime` cannot carry sub-second precision, `Plan` is only constructible through `plan()`.
- Comments only for a non-obvious why: the mtime tolerance, the windowed default, the per-file move ordering, the reconcile-before-retry rule. Nothing that restates the code.
- No traits with one implementation, no generics without two concrete users, no `Arc<Mutex<>>` where ownership works.
- Errors carry the data needed to render a useful message; formatting lives in the CLI.
- `rustfmt` default (`imports_granularity` is nightly-only, so imports are grouped by hand). `cargo deny` for licenses (MIT, Apache-2.0, BSD, ISC, Zlib) and advisories.
- Conventional commits. `CHANGELOG.md` in Keep a Changelog format.
- `mtpx-core` stays `0.x` until the API has proven itself across pull, push, sync, resume and move on real hardware. `1.0` is an API decision, not a UI milestone.

## 11. Dependencies

Core: `mtp-rs`, `tokio` (rt-multi-thread, sync, fs, macros), `futures`, `bytes`, `thiserror`, `globset`, `filetime`, `serde` + `serde_json` (sidecar only), `tracing`.

CLI: `clap` (derive, env, wrap_help), `clap_complete`, `clap_mangen`, `indicatif`, `console`, `dialoguer`, `miette` (fancy), `serde_json`, `toml`, `directories`, `humansize`, `humantime`, `tracing-subscriber`, `tokio::signal`.

Dev: `tempfile`, `assert_cmd`, `predicates`, `criterion`, `mtp-rs` with `virtual-device`.

## 12. Release engineering

- `ci.yml`: matrix macOS, Ubuntu, Windows: fmt, clippy `-D warnings`, test, doc `-D warnings`, `cargo deny`, MSRV build.
- `release.yml` with `cargo-dist`: tag `v*` builds archives for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, GitHub Release, Homebrew tap `brianrey/tap`, crates.io publish of `mtpx-core` then `mtpx`.
- Install paths: `brew install brianrey/tap/mtpx`, `cargo install mtpx`, `cargo binstall mtpx`, archives.
- Completions and man page generated with `clap_complete` and `clap_mangen`, bundled in archives.

## 13. README outline

1. Name, tagline, badges.
2. Terminal recording (`vhs`) of `mtpx sync` with progress bars. The single most important asset for traction.
3. Why, in three sentences: Android File Transfer is discontinued, MTP mounts are flaky, `adb` needs developer mode, nothing does incremental sync.
4. Install.
5. Quick start: `devices`, `ls`, `pull`, `sync`, `sync --move`, `--dry-run`. One line each.
6. Sync semantics table: new, equal, changed, extraneous, with and without `--delete`; the "same size means unchanged" note.
7. Resume and safety: `.mtpx-part`, fingerprint, per-file move.
8. Profiles.
9. macOS note: `ptpcamerad`, `doctor --fix`.
10. Comparison table.
11. Library: a 15 line `mtpx-core` example using `Device`.
12. How it works: the plan/execute diagram.
13. Contributing, credits (`mtp-rs` by David Veszelovszki), license.

## 14. Milestones

Milestone 1 is a vertical slice that validates the dangerous assumptions on real hardware before anything is polished.

- **M1, vertical slice**: `devices`, `ls`, `pull` (file and directory), `sync` (no delete), `--dry-run`, basic two-bar progress, resolver with stale-handle recovery, windowed download with cancellation, `.mtpx-part` with sidecar and resume, planner tests, virtual-device integration tests, CI on three platforms. Exit criterion: `mtpx sync /DCIM/Camera ~/Documents/backups/motog52` on the Moto g52 survives Ctrl-C, unplug, and re-run.
- **M2, safety and control**: `sync --delete` (post-order), `--move`, `ConflictPolicy` flags, `--compare size-mtime`, filters, `StorageSelector` picker, `doctor` (diagnostic) and `--fix`.
- **M3, surface**: `push` with partial cleanup and `--verify`, `rm`, `mkdir`, `--json` (`JsonEventV1`), profiles, exit codes finalized, `docs/cli.md`.
- **M4, release**: completions, man page, `cargo-dist`, Homebrew tap, README with recording, crates.io publish as `0.1.0`.
- **Later**: `watch` (hotplug: run a profile when the phone connects), continuous-download strategy if benchmarks justify it, `mtpx-tui` on the same core, `mtpx-core 1.0` when the API has stopped moving.

## 15. Decisions log

- Handles never leave the resolver. Snapshots and plans are logical (paths plus metadata). Verified against `mtp-rs`: `StaleHandle` docs state Android re-keys IDs across media rescans.
- Windowed downloads by default. Verified: `FileDownload` is `#[must_use]` because dropping it mid-transfer can corrupt the USB session; `WindowedDownload` holds nothing between windows. Continuous mode is a later, benchmarked, opt-in strategy.
- Resume requires a matching fingerprint. Length alone can splice two different files.
- Deletes are post-order. Device behavior on non-empty folder deletion is inconsistent; post-order is correct everywhere.
- `pull` refuses to overwrite differing files by default. Equal files are skipped, not conflicts.
- Public API is a facade (`Device`) plus value types. `Endpoint` stays private until proven.
- JSON output is `JsonEventV1`, a separate versioned type.
- `doctor --fix` is `SIGTERM` and retry; `SIGKILL` needs `--force`; no sudo; no permanent OS changes suggested in errors.
- Storage is selected, never assumed, when a device has more than one.
- Core `1.0` is decoupled from the TUI.
- Name: `mtpx` (CLI), `mtpx-core` (library). Both free on crates.io as of 2026-09-16.
- A cancelled run is an outcome, not an error. `run` returns `Ok(Report { interrupted: true })`; `Report.interrupted` exists for exactly this and the CLI maps it to exit 130. Only a lost device or session (`Disconnected`, `DeviceReset`, `NoDevice`, `PermissionDenied`, `ExclusiveAccess`) returns `Err`.
- A stale handle rebuilds the whole resolver cache, not just the parent's subtree. Android re-keys every object on a media rescan, so nothing cached survives it; a subtree invalidation leaves stale grandparents in place.
- Retries in M1 cover opening a read, and the backoff sleeps in 100 ms slices so a cancel is noticed promptly. A transient error mid-file fails that file; the sidecar lets the next run resume it. Window-level retry inside the MTP pump is M2.
- Known risk from `mtp-rs` docs: some Android devices (Pixel) wedge after a cancelled read without reporting `DeviceReset`; the next operation hangs. M2 adds an operation timeout; M1 relies on windowed downloads, whose wedge is the recoverable one.
