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
| 4.4 | start the sync, pull the cable mid-file | an error naming the disconnect, exit 3 (or 4 if the OS revokes access first), the partial kept |
| 4.5 | replug, unlock, rerun | resumes and completes |
| 4.6 | start the sync, Ctrl-C twice quickly | the process exits at once with 130; the next run resumes that file from the last checkpoint (at most 64 MiB behind) |

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

Fill in after each hardware run and save the table under `docs/hardware-runs/<date>-<device>.md`
with the device, host OS, USB speed and commit at the top; the README Status section links
the latest run.

| Step | Result | Notes |
|---|---|---|
| 1.1 |  |  |
| 1.2 |  |  |
| 1.3 |  |  |
| 1.4 |  |  |
| 2.1 |  |  |
| 2.2 |  |  |
| 2.3 |  |  |
| 2.4 |  |  |
| 2.5 |  |  |
| 3.1 |  |  |
| 3.2 |  |  |
| 3.3 |  |  |
| 3.4 |  |  |
| 3.5 |  |  |
| 3.6 |  |  |
| 4.1 |  |  |
| 4.2 |  |  |
| 4.3 |  |  |
| 4.4 |  |  |
| 4.5 |  |  |
| 4.6 |  |  |
| 5.1 |  |  |
| 5.2 |  |  |
| 5.3 |  |  |
| 6 |  |  |

Completed runs live in `docs/hardware-runs/`.
