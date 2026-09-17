//! `mtpx`: rsync for your phone, over MTP.

mod cli;
mod commands;
mod exit;
mod ui;

use clap::Parser;
use commands::{Console, Output, UiOptions};
use indicatif::{MultiProgress, ProgressDrawTarget};
use miette::MietteHandlerOpts;
use mtpx_core::{CancelToken, Error};
use std::{io::Write, process::ExitCode};
use tokio::sync::watch;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};
use ui::stderr;

const INTERRUPTED_BEFORE_TRANSFER: &str = "Interrupted before the transfer started";
const INTERRUPTED_LISTING: &str = "Interrupted";

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let ui = commands::ui_options(&cli.global);
    init_diagnostics(ui.color);
    let progress = draw_target(ui.output);
    init_tracing(cli.global.verbose, progress.clone());
    let (console, cancel) = watch_interrupts(ui, progress);
    let transfers = cli.command.transfers();
    match commands::run(cli, console, cancel).await {
        Ok(outcome) => exit::code_for_outcome(&outcome),
        Err(error) => {
            stderr::line(&failure_text(&error, transfers));
            exit::code_for_error(&error)
        }
    }
}

/// Failed-command stderr. A pre-transfer Ctrl-C is one plain line, not a diagnostic; mid-transfer never reaches here.
fn failure_text(error: &Error, transfers: bool) -> String {
    match (error, transfers) {
        (Error::Cancelled, true) => INTERRUPTED_BEFORE_TRANSFER.to_owned(),
        (Error::Cancelled, false) => INTERRUPTED_LISTING.to_owned(),
        _ => format!("{:?}", ui::diagnostic::report(error)),
    }
}

/// The one draw target on stderr; hidden unless bars are wanted, so `suspend` around a log
/// line is a plain write everywhere else.
fn draw_target(output: Output) -> MultiProgress {
    let target = if output == Output::Bars {
        ProgressDrawTarget::stderr()
    } else {
        ProgressDrawTarget::hidden()
    };
    MultiProgress::with_draw_target(target)
}

/// Wires Ctrl-C to the cancel token the core polls and to the flag the renderer announces.
fn watch_interrupts(ui: UiOptions, progress: MultiProgress) -> (Console, CancelToken) {
    let cancel = CancelToken::new();
    let (interrupt_tx, interrupt) = watch::channel(false);
    tokio::spawn(watch_ctrl_c(cancel.clone(), interrupt_tx));
    let console = Console {
        ui,
        progress,
        interrupt,
    };
    (console, cancel)
}

fn init_diagnostics(color: bool) {
    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(MietteHandlerOpts::new().color(color).build())
    }));
}

/// Log lines go through the draw target so they land above the bars, and a stderr that went
/// away is not reported: the report itself would panic on the same closed pipe.
fn init_tracing(verbose: u8, progress: MultiProgress) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(log_filter(verbose))
        .with_writer(stderr::LogWriter::new(progress))
        .log_internal_errors(false)
        .try_init();
}

/// An explicit `-v` sets the level outright; `RUST_LOG` only applies when no `-v` is given.
fn log_filter(verbose: u8) -> EnvFilter {
    let level = match verbose {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    if verbose > 0 {
        return EnvFilter::new(level.to_string());
    }
    EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy()
}

/// The first Ctrl-C asks the executor to stop after the current window and tells the renderer,
/// which owns stderr, to say so; the second gives up waiting for it.
async fn watch_ctrl_c(cancel: CancelToken, interrupt: watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(
            %error,
            "Ctrl-C handler unavailable; an interrupt will kill the process and the file in progress restarts from zero next run"
        );
        return;
    }
    cancel.cancel();
    let _ = interrupt.send(true);
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

    #[test]
    fn each_extra_v_lowers_the_log_level_down_to_trace() {
        use tracing_subscriber::{Registry, layer::Layer};
        let level = |verbose| Layer::<Registry>::max_level_hint(&log_filter(verbose));
        assert_eq!(level(1), Some(LevelFilter::INFO));
        assert_eq!(level(2), Some(LevelFilter::DEBUG));
        assert_eq!(level(3), Some(LevelFilter::TRACE));
        assert_eq!(level(9), Some(LevelFilter::TRACE));
    }

    #[test]
    fn lines_and_quiet_modes_never_draw_on_stderr() {
        assert!(draw_target(Output::Lines).is_hidden());
        assert!(draw_target(Output::Quiet).is_hidden());
    }
}
