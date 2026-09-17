//! `mtpx`: rsync for your phone, over MTP.

mod cli;
mod commands;
mod exit;
mod ui;

use clap::Parser;
use cli::Command;
use miette::MietteHandlerOpts;
use mtpx_core::{CancelToken, Error};
use std::{io::Write, process::ExitCode};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

const INTERRUPTED_BEFORE_TRANSFER: &str = "Interrupted before the transfer started";
const INTERRUPTED_LISTING: &str = "Interrupted";

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    init_diagnostics(ui::theme::color_enabled(cli.global.no_color));
    init_tracing(cli.global.verbose);
    let transfers = matches!(cli.command, Command::Pull(_) | Command::Sync(_));
    let cancel = CancelToken::new();
    tokio::spawn(watch_ctrl_c(cancel.clone()));
    match commands::run(cli, cancel).await {
        Ok(outcome) => exit::code_for_outcome(&outcome),
        Err(error) => {
            eprintln!("{}", failure_text(&error, transfers));
            exit::code_for_error(&error)
        }
    }
}

/// What stderr gets for a failed command. A Ctrl-C that lands before the executor runs (during
/// the scans, or in a listing) is not a fault worth a diagnostic report, just one plain line;
/// a Ctrl-C mid-transfer never reaches here, the executor reports it in its summary.
fn failure_text(error: &Error, transfers: bool) -> String {
    match (error, transfers) {
        (Error::Cancelled, true) => INTERRUPTED_BEFORE_TRANSFER.to_owned(),
        (Error::Cancelled, false) => INTERRUPTED_LISTING.to_owned(),
        _ => format!("{:?}", ui::diagnostic::report(error)),
    }
}

fn init_diagnostics(color: bool) {
    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(MietteHandlerOpts::new().color(color).build())
    }));
}

/// `-v` raises the default level; `RUST_LOG` still wins when set.
fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let filter = EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// The first Ctrl-C asks the executor to stop after the current window; the second gives up
/// waiting for it.
async fn watch_ctrl_c(cancel: CancelToken) {
    if tokio::signal::ctrl_c().await.is_err() {
        return;
    }
    cancel.cancel();
    eprintln!("interrupting, finishing the current window...");
    if tokio::signal::ctrl_c().await.is_ok() {
        let _ = std::io::stdout().flush();
        std::process::exit(i32::from(exit::INTERRUPTED));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupt_before_the_transfer_is_one_plain_line() {
        assert_eq!(
            failure_text(&Error::Cancelled, true),
            "Interrupted before the transfer started"
        );
        assert_eq!(failure_text(&Error::Cancelled, false), "Interrupted");
    }

    #[test]
    fn other_errors_keep_their_diagnostic_report() {
        let text = failure_text(&Error::NoDevice, true);
        assert!(text.contains("no MTP device found"), "{text}");
        assert!(text.contains("Unlock the phone"), "{text}");
    }
}
