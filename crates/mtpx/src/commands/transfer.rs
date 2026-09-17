//! `mtpx pull` and `mtpx sync`: plan, show, run, all through one progress renderer.

use crate::{
    commands::{self, Ctx, Outcome},
    ui::{progress::Renderer, table},
};
use mtpx_core::{DevicePath, ProgressEvent, Result, TransferOptions};
use std::{path::Path, time::Instant};
use tokio::sync::mpsc::{self, Sender};

/// Deep enough that a burst of `FileProgress` never stalls the executor on the renderer.
const EVENTS_CAPACITY: usize = 1024;

/// Plans the pull and, unless `dry_run`, runs it; the renderer owns stderr for the duration.
pub async fn run(
    ctx: &Ctx,
    remote: &DevicePath,
    local: &Path,
    opts: &TransferOptions,
    dry_run: bool,
) -> Result<Outcome> {
    let started = Instant::now();
    let (events, rx) = mpsc::channel(EVENTS_CAPACITY);
    // The renderer starts first: the core awaits every non-progress event send.
    let renderer = tokio::spawn(Renderer::run(rx, ctx.console.clone(), started));
    let outcome = execute(ctx, remote, local, opts, dry_run, &events).await;
    drop(events);
    if let Err(join_error) = renderer.await {
        tracing::error!(%join_error, "progress renderer panicked");
    }
    let outcome = outcome?;
    if let Outcome::DryRun(plan) = &outcome {
        commands::print(&table::plan(plan))?;
    }
    Ok(outcome)
}

async fn execute(
    ctx: &Ctx,
    remote: &DevicePath,
    local: &Path,
    opts: &TransferOptions,
    dry_run: bool,
    events: &Sender<ProgressEvent>,
) -> Result<Outcome> {
    let remote = ctx.resolve(remote);
    let job = ctx
        .with_storage(&remote, async |p| {
            ctx.device
                .plan_pull(p, local, opts, &ctx.cancel, events)
                .await
        })
        .await?;
    if dry_run {
        return Ok(Outcome::DryRun(job.plan().clone()));
    }
    let report = job.run(&ctx.cancel, events).await?;
    Ok(Outcome::Transferred(report))
}
