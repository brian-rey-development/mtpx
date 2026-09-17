# What this PR does

<!-- One paragraph: behavior change, user-visible or internal. -->

## Checks

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --features virtual-device`
- [ ] `cargo doc --no-deps --workspace --features virtual-device` (CI runs it with `-D warnings`)
- [ ] `cargo deny check`
- [ ] `CHANGELOG.md` updated under `[Unreleased]` (if user-visible)

## Hardware validation (if it touches transfer, scan, or device code)

<!-- Steps from `docs/manual-checklist.md` with a results file under `docs/hardware-runs/`, or "not needed, virtual-device covers it". -->
