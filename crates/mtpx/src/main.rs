//! `mtpx`: rsync for your phone, over MTP.

mod cli;
mod commands;
mod exit;
mod ui;

use clap::Parser;
use miette::MietteHandlerOpts;
use mtpx_core::CancelToken;
use std::{io::Write, process::ExitCode};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    init_diagnostics(ui::theme::color_enabled(cli.global.no_color));
    init_tracing(cli.global.verbose);
    let cancel = CancelToken::new();
    tokio::spawn(watch_ctrl_c(cancel.clone()));
    match commands::run(cli, cancel).await {
        Ok(outcome) => exit::code_for_outcome(&outcome),
        Err(error) => {
            eprintln!("{:?}", ui::diagnostic::report(&error));
            exit::code_for_error(&error)
        }
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
