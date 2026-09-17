//! Dispatch from the parsed command line to the core facade, with the device opened once.

mod devices;
mod ls;
mod open;
mod transfer;

use crate::{
    cli::{Cli, Command, Global},
    ui::{prompt, stdout, theme},
};
use mtpx_core::{
    CancelToken, Device, DevicePath, Error, Plan, Report, Result, StorageSelector, StorageSummary,
    TransferOptions,
};
use std::io::{self, IsTerminal};

/// Where progress goes, decided once from the flags and the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Live bars on a terminal.
    Bars,
    /// One line per event, for pipes and logs.
    Lines,
    /// Only failures and the final summary.
    Quiet,
}

/// How output should look, decided once from the flags and the terminal.
#[derive(Debug, Clone, Copy)]
pub struct UiOptions {
    /// How progress is drawn.
    pub output: Output,
    /// Stderr is a terminal and prompts are allowed.
    pub prompts: bool,
    /// Styled output is wanted.
    pub color: bool,
}

/// What every command that touches a device gets.
pub struct Ctx {
    /// The open device.
    pub device: Device,
    /// Set by Ctrl-C.
    pub cancel: CancelToken,
    /// How output should look.
    pub ui: UiOptions,
    storage: Option<StorageSelector>,
}

/// What a command produced when it did not fail.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing to report beyond what was printed.
    Done,
    /// The plan that would have run.
    DryRun(Plan),
    /// A transfer ran, interrupted or not.
    Transferred(Report),
}

impl Ctx {
    /// Applies `--storage` to a path argument; the flag wins over the path's own prefix.
    pub fn resolve(&self, path: &DevicePath) -> DevicePath {
        self.storage.as_ref().map_or_else(
            || path.clone(),
            |storage| DevicePath {
                storage: storage.clone(),
                path: path.path.clone(),
            },
        )
    }

    /// Turns `StorageRequired` into a path on the storage the user picks, when a prompt is
    /// possible; any other error, or a dismissed prompt, comes back unchanged.
    pub fn pick_storage(&self, path: &DevicePath, error: Error) -> Result<DevicePath> {
        let Error::StorageRequired(storages) = error else {
            return Err(error);
        };
        if !self.ui.prompts {
            return Err(Error::StorageRequired(storages));
        }
        let Some(index) = prompt::pick_storage(&storages) else {
            return Err(Error::StorageRequired(storages));
        };
        Ok(on_storage(path, &storages, index))
    }
}

fn on_storage(path: &DevicePath, storages: &[StorageSummary], picked: usize) -> DevicePath {
    let index = storages.get(picked).map_or(picked, |storage| storage.index);
    DevicePath {
        storage: StorageSelector::Index(index),
        path: path.path.clone(),
    }
}

/// Prints `text` on stdout. A reader that closed the pipe early, as `ls | head` does, is not a
/// failure: the command goes on quietly and exits 0.
///
/// # Errors
/// Any I/O failure other than `BrokenPipe`.
pub fn print(text: &str) -> Result<()> {
    quiet_when_gone(stdout::write(text))
}

fn quiet_when_gone(written: io::Result<()>) -> Result<()> {
    match written {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(Error::from),
    }
}

/// Runs `cli` to completion; `devices` needs no open device, everything else opens one first.
pub async fn run(cli: Cli, cancel: CancelToken) -> Result<Outcome> {
    let ui = ui_options(&cli.global);
    if matches!(cli.command, Command::Devices) {
        return devices::run();
    }
    let ctx = Ctx {
        device: open::open_device(&cli.global, ui).await?,
        cancel,
        ui,
        storage: cli.global.storage_selector(),
    };
    let outcome = dispatch(&ctx, cli.command).await;
    if let Err(error) = ctx.device.close().await {
        tracing::warn!(%error, "closing the device failed");
    }
    outcome
}

async fn dispatch(ctx: &Ctx, command: Command) -> Result<Outcome> {
    match command {
        Command::Devices => devices::run(),
        Command::Ls(args) => ls::run(ctx, &args).await,
        Command::Pull(args) => {
            transfer::run(
                ctx,
                &args.remote,
                &args.local,
                &args.options(),
                args.dry_run,
            )
            .await
        }
        Command::Sync(args) => {
            let opts = TransferOptions::sync();
            transfer::run(ctx, &args.remote, &args.local, &opts, args.dry_run).await
        }
    }
}

fn ui_options(global: &Global) -> UiOptions {
    let terminal = std::io::stderr().is_terminal();
    UiOptions {
        output: output(global.quiet, terminal),
        prompts: terminal && !global.no_interactive,
        color: theme::color_enabled(global.no_color),
    }
}

const fn output(quiet: bool, terminal: bool) -> Output {
    match (quiet, terminal) {
        (true, _) => Output::Quiet,
        (false, true) => Output::Bars,
        (false, false) => Output::Lines,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn print_swallows_a_broken_pipe_and_keeps_other_errors() {
        let broken = io::Error::from(io::ErrorKind::BrokenPipe);
        assert!(matches!(quiet_when_gone(Err(broken)), Ok(())));
        let other = io::Error::other("disk full");
        assert!(matches!(quiet_when_gone(Err(other)), Err(Error::Io(_))));
        assert!(matches!(quiet_when_gone(Ok(())), Ok(())));
    }

    #[test]
    fn picked_storage_replaces_the_prefix_and_keeps_the_path() {
        let path: DevicePath = "sd:/DCIM".parse().unwrap();
        let storages = vec![StorageSummary {
            index: 1,
            name: "Internal".into(),
            free: 0,
            total: 0,
        }];
        let picked = on_storage(&path, &storages, 0);
        assert_eq!(picked.to_string(), "1:/DCIM");
    }

    #[test]
    fn quiet_wins_and_a_terminal_gets_bars() {
        assert_eq!(output(true, true), Output::Quiet);
        assert_eq!(output(true, false), Output::Quiet);
        assert_eq!(output(false, true), Output::Bars);
        assert_eq!(output(false, false), Output::Lines);
    }
}
