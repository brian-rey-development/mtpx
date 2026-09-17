# Manual hardware checklist (M1)

Run against a real phone before closing milestone 1. Reference device: Motorola moto g52
on macOS. Record the outcome of every step in the results table at the end; a failed step
is a finding, not a reason to stop.

Build once:

```sh
cargo build --release -p mtpx
alias mtpx="$PWD/target/release/mtpx"
```

Before every step: the phone is unlocked and the USB notification says "File transfer" (MTP).
A phone in "Charging only" mode answers nothing and every command times out.

## 1. Discovery and access

| Step | Command | Expected |
|---|---|---|
| 1.1 | `mtpx devices` | one row with the phone's label, serial and USB speed, exit 0 |
| 1.2 | `pkill ptpcamerad` is NOT run; `mtpx ls /` | either the listing, or exit 4 naming another process holding the device |
| 1.3 | unplug, `mtpx devices` | "no MTP device found" with the unlock hint, exit 3 |
| 1.4 | replug, lock the phone, `mtpx ls /` | note the exact behaviour (timeout, exit code, message) |

## 2. Listing

| Step | Command | Expected |
|---|---|---|
| 2.1 | `mtpx ls /` | the storage root: `DCIM/`, `Pictures/`, `Download/`, ... |
| 2.2 | `mtpx ls /DCIM/Camera -l` | kind, size, a real modification time (not `-`), name; sorted |
| 2.3 | `mtpx ls /DCIM -R \| wc -l` | the full count; stderr empty |
| 2.4 | `mtpx ls /Nope` | "remote path not found", exit 5 |
| 2.5 | `mtpx ls /DCIM/Camera/<a photo>` | "not a directory" with the hint, exit 5 |

## 3. Pull and sync

Use a fresh local directory, e.g. `~/Documents/backups/motog52`.

| Step | Command | Expected |
|---|---|---|
| 3.1 | `mtpx pull /DCIM/Camera/<one photo> /tmp/mtpx-one` | the file lands byte-exact (compare size with 2.2), its mtime equals the phone's |
| 3.2 | `mtpx sync /DCIM/Camera ~/Documents/backups/motog52 --dry-run` | action list on stdout, `Plan:` line on stderr, directory not created |
| 3.3 | `mtpx sync /DCIM/Camera ~/Documents/backups/motog52` | two bars, a `Done in ...` line; note the sustained MB/s |
| 3.4 | rerun 3.3 | `Plan: copy 0 files`, every file `identical`, exit 0, well under 10 s |
| 3.5 | `ls -la ~/Documents/backups/motog52 \| head` | no `.mtpx-part` files; mtimes match the phone |
| 3.6 | take a new photo on the phone, rerun 3.3 | exactly one file copied |

## 4. Interruption and resume

Pick a large file (a video works best). Start the sync and interrupt it while that file's bar
is moving.

| Step | Action | Expected |
|---|---|---|
| 4.1 | Ctrl-C once | "interrupting, finishing the current window...", then `Interrupted after ...: N copied, M remaining, re-run to resume`, exit 130 |
| 4.2 | `ls ~/Documents/backups/motog52/*.mtpx-part*` | one `.mtpx-part` and its `.json` sidecar |
| 4.3 | rerun the sync | that file's bar starts above 0 (resume), the run completes, the file is byte-exact and the part files are gone |
| 4.4 | start the sync, pull the cable mid-file | an error naming the disconnect, exit 1 or 4, the partial kept |
| 4.5 | replug, unlock, rerun | resumes and completes |
| 4.6 | start the sync, Ctrl-C twice quickly | the process exits at once with 130; the next run detects the stale partial and restarts that file |

## 5. Conflicts

| Step | Action | Expected |
|---|---|---|
| 5.1 | truncate one local copy (`truncate -s 100 <file>`), `mtpx pull /DCIM/Camera ~/Documents/backups/motog52` | exit 7, `1 file differs:` and the name, the flags in the help |
| 5.2 | same with `--skip-existing` | `skipped <file> (conflict)`, exit 0, the local file still 100 bytes |
| 5.3 | same with `--overwrite` | the file replaced, byte-exact |

## 6. Throughput

Sustained MB/s from 3.3 on a directory of at least 1 GB, plus the USB speed from 1.1.
USB 2.0 High Speed tops out around 35 to 40 MB/s in practice; USB 3 devices should do
better. If the number is far below that, note it and run with `-vv` to see the window timing.

## Results

Fill in and paste into the CHANGELOG under "M1 hardware validation".

| Step | Result | Notes |
|---|---|---|
| 1.1 | pass | motorola moto g52, ZY22FPNXWP, USB 2.0 High Speed |
| 1.2 | pass | ptpcamerad was not running; the listing came through |
| 1.3 | not run | needs hands on the cable |
| 1.4 | finding | charging-only mode: 30 s wait, `operation timed out`, exit 1, no help (fix C) |
| 2.1 | pass | 19 top-level directories with real modification times |
| 2.2 | pass | sizes and dates present; `ls \| head` panicked on broken pipe (fix E) |
| 2.3 | pass | 2,214 objects; the scan takes about 9 s (one metadata request per object in mtp-rs) |
| 2.4 | finding | exit 5 but the message and hint carry the storage name (fix D) |
| 2.5 | pass | exit 5 with the hint |
| 3.1 | pass | covered by 3.5: size and mtime match the phone |
| 3.2 | pass | actions on stdout, plan and SlowLink hint on stderr, directory not created |
| 3.3 | pass | 12.5 GB in one interrupted run plus one resume, 30.1 MB/s average |
| 3.4 | pass | 0 copied, 2,214 skipped; the summary said `Done in 0s` although the scans took ~9 s (fix F); dry run listed 2,214 skip lines (fix G) |
| 3.5 | pass | no part files; VID_20251220_203158938.mp4 has the phone's mtime and 15,541,666 bytes |
| 3.6 | not run |  |
| 4.1 | pass | SIGINT at 36 s: `Interrupted after 36s: 457 copied, 1,757 remaining`, exit 130 within 300 ms |
| 4.2 | finding | one part plus sidecar, but `bytes == size`: the interrupt landed after the last window of a one-window file (fix A) |
| 4.3 | finding | the rerun completed (1,757 copied, 0 failed, no parts left) but re-copied the complete partial from zero (fix B) |
| 4.4 | not run | needs hands on the cable |
| 4.5 | not run |  |
| 4.6 | not reproducible | the first SIGINT exits before a second one can be delivered; a SIGINT during the scan prints a bare `cancelled` (fix H) |
| 5.1 | not run | covered by the CLI suite against the virtual device |
| 5.2 | not run | same |
| 5.3 | not run | same |
| 6 | pass | 30 to 32 MB/s sustained on USB 2.0 High Speed, in range |

## Findings (2026-09-16, first full run)

Eight fixes came out of this run, labelled A to H in the table:

- A. A cancel that lands after a file's last window failed the file instead of finishing it.
- B. A complete partial (`bytes == size`) was re-copied from zero instead of finalised.
- C. An unresponsive phone (locked, charging-only) was a bare timeout with exit 1 and no help.
- D. Error messages and hints carried the storage name the user never typed.
- E. `mtpx ls | head` panicked on a broken pipe.
- F. The summary timed only the transfer, so an all-skip run said `Done in 0s`.
- G. A dry run listed every identical file as a `skip` line.
- H. Ctrl-C during the scan printed a bare `cancelled`.

Verified after the fixes, same phone: `ls /Nope` shows `/Nope` and `mtpx ls /`; `ls | head`
exits quietly; a synced dry run prints nothing on stdout; a 9 MB photo interrupted at two
windows planned `8.4 MB resumable` and resumed byte-exact; the summary reads
`Interrupted after 49s` for a 49 s command.

Two observations for M2, not defects:

- Listing latency is the phone's: 6,736 USB transfers for 2,214 objects (three per object,
  no retries), 9 s right after unlocking and 39 s later. Bulk metadata
  (`GetObjectPropList`) where the device supports it would cut this.
- A single-file pull lists the parent folder twice (locate, then scan); with a slow phone
  that doubled a 665 kB resume to 1m 17s. The locate listing could prime the scan.

Still to run by hand: 1.3 and 4.4/4.5 (cable), 3.6 (new photo).
