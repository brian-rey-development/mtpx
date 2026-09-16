# mtpx

rsync for your phone. Fast, incremental file transfers over MTP, in pure Rust.

## Status

Milestone 1 (devices, ls, pull, sync, dry-run, resume) is in development.
Nothing is released yet and the CLI surface will change without notice until 0.1.0.

## Install

From source, with a Rust toolchain (1.85 or newer):

```sh
cargo install --path crates/mtpx
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

## Credits

Built on [`mtp-rs`](https://github.com/vdavid/mtp-rs) by David Veszelovszki.
