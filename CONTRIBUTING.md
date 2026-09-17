# Contributing to mtpx

Thanks for considering a contribution. By participating you agree to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Setup

```sh
git clone https://github.com/brian-rey-development/mtpx
cd mtpx
cargo install cargo-deny
cargo test --workspace --features virtual-device
```

`rustup` installs the channel pinned in `rust-toolchain.toml` on the first
`cargo` call; the MSRV is 1.85 and CI builds every target with it. Nothing needs
installing at the system level: `mtp-rs` reaches the phone through `nusb` on
Linux and macOS and through Windows Portable Devices on Windows, with no C
dependencies in the graph.

No phone is needed. The `virtual-device` feature runs the whole pull, resume,
and CLI path against an in-process MTP device backed by a temporary directory.

## Running without a phone

The same virtual device is reachable from the real binary through a flag that is
hidden from `--help` and only compiled with the feature:

```sh
cargo run -p mtpx --features virtual-device -- --virtual <dir> ls /
cargo run -p mtpx --features virtual-device -- --virtual <dir> sync /DCIM ~/tmp/out
```

`<dir>` is any local directory; its contents stand in for the phone's storage,
which is exactly how the CLI tests in `crates/mtpx/tests/cli/` drive the binary.

## Checks before you open a PR

Run all of these; CI runs the same on macOS, Ubuntu, and Windows:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --features virtual-device
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features virtual-device
cargo deny check
```

## Code standards

- Small functions (clippy `too-many-lines-threshold = 20` is enforced).
- No `unwrap` or `expect` in non-test code (`unwrap_used` and `expect_used`
  are denied workspace-wide). Return `Result` and let the caller decide.
- No `unsafe` (`unsafe_code` is forbidden).
- Every public item carries a doc comment that states its contract in one line:
  the hidden constraint, the device quirk, the error contract, or the invariant
  the next reader needs. Never a restatement of the name or signature.
  `missing_docs` is `warn` workspace-wide, and CI's `-D warnings` makes a
  missing doc an error.
- Validate at boundaries (CLI parsing, path parsing, MTP and filesystem
  errors); trust internal code.
- Every behavior change needs a test. Prefer the existing suites:
  `crates/mtpx-core/tests/` for engine behavior, `crates/mtpx/tests/cli/`
  for CLI behavior.

## Pull requests

- Branch from `main`; one change per PR.
- Title in Conventional Commit form: `feat:`, `fix:`, `docs:`, `test:`,
  `refactor:`, `chore:`. Example: `fix: resume a complete partial in place`.
- CI must be green on all three platforms.
- User-visible changes get a line in `CHANGELOG.md` under `[Unreleased]`.

## Hardware validation

Some changes need a real phone. `docs/manual-checklist.md` is the procedure;
record the results in a file under `docs/hardware-runs/` and link it from the PR.

## License

mtpx is dual licensed under MIT or Apache-2.0. Unless you explicitly state otherwise,
any contribution intentionally submitted for inclusion in the work by you, as defined
in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
