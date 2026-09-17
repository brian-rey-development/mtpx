//! Dispatch from the parsed command line to the core facade, with the device opened once.

mod devices;
mod ls;
mod open;
mod transfer;

use crate::{
    cli::{Cli, Command, DeviceCommand, Global},
    ui::{
        prompt::{self, Choice},
        stdout,
    },
};
use indicatif::MultiProgress;
use mtpx_core::{
    CancelToken, Device, DevicePath, Error, Plan, Report, Result, StorageSelector, StorageSummary,
    TransferOptions,
};
use std::io::{self, IsTerminal};
use tokio::sync::watch;

/// Where progress goes.
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
    pub output: Output,
    /// Stderr is a terminal and prompts are allowed.
    pub prompts: bool,
    pub color: bool,
}

/// The stderr side of a run: how output looks, the one draw target every writer shares so log
/// lines land above the bars, and the signal that announces the first Ctrl-C.
#[derive(Clone)]
pub struct Console {
    pub ui: UiOptions,
    /// Drawn on by the bars; suspended around every other stderr write.
    pub progress: MultiProgress,
    /// Flips to `true` on the first Ctrl-C.
    pub interrupt: watch::Receiver<bool>,
}

/// What every command that touches a device gets.
pub struct Ctx {
    pub device: Device,
    /// Set by Ctrl-C.
    pub cancel: CancelToken,
    pub console: Console,
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

    /// Runs `op` on `path`; when the device answers `StorageRequired` and a prompt is possible,
    /// runs it once more on the storage the user picks.
    pub async fn with_storage<T>(
        &self,
        path: &DevicePath,
        op: impl AsyncFn(&DevicePath) -> Result<T>,
    ) -> Result<T> {
        match op(path).await {
            Err(error) => op(&self.pick_storage(path, error)?).await,
            done => done,
        }
    }

    /// Turns `StorageRequired` into a path on the storage the user picks, when a prompt is
    /// possible; any other error, or a dismissed prompt, comes back unchanged.
    fn pick_storage(&self, path: &DevicePath, error: Error) -> Result<DevicePath> {
        let Error::StorageRequired(storages) = error else {
            return Err(error);
        };
        if !self.console.ui.prompts {
            return Err(Error::StorageRequired(storages));
        }
        match prompt::pick_storage(&storages) {
            Choice::Picked(index) => Ok(on_storage(path, &storages, index)),
            Choice::Dismissed => Err(Error::StorageRequired(storages)),
            Choice::Interrupted => Err(Error::Cancelled),
        }
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
pub async fn run(cli: Cli, console: Console, cancel: CancelToken) -> Result<Outcome> {
    let command = match cli.command {
        Command::Devices => return devices::run(),
        Command::WithDevice(command) => command,
    };
    let ctx = Ctx {
        device: open::open_device(&cli.global, console.ui).await?,
        cancel,
        console,
        storage: cli.global.storage_selector(),
    };
    let outcome = dispatch(&ctx, command).await;
    if let Err(error) = ctx.device.close().await {
        tracing::warn!(%error, "closing the device failed");
    }
    outcome
}

async fn dispatch(ctx: &Ctx, command: DeviceCommand) -> Result<Outcome> {
    match command {
        DeviceCommand::Ls(args) => ls::run(ctx, &args).await,
        DeviceCommand::Pull(args) => {
            transfer::run(
                ctx,
                &args.remote,
                &args.local,
                &args.options(),
                args.dry_run,
            )
            .await
        }
        DeviceCommand::Sync(args) => {
            let opts = TransferOptions::sync();
            transfer::run(ctx, &args.remote, &args.local, &opts, args.dry_run).await
        }
    }
}

/// Decided once from the flags, the environment and the terminal. A dumb terminal gets lines
/// and no prompts: indicatif hides every bar there and dialoguer needs cursor movement. Color
/// follows console's stderr detection (`NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM=dumb`).
pub fn ui_options(global: &Global) -> UiOptions {
    let terminal = io::stderr().is_terminal() && !console::is_dumb();
    UiOptions {
        output: output(global.quiet, terminal),
        prompts: terminal && !global.no_interactive,
        color: !global.no_color && console::colors_enabled_stderr(),
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
        let storages = vec![StorageSummary::new(1, "Internal".into(), 0, 0)];
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
