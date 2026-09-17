//! `mtpx pull` and `mtpx sync`: plan, show, run, all through one progress renderer.

use crate::{
    commands::{self, Ctx, Outcome},
    ui::{progress::Renderer, table},
};
use mtpx_core::{DevicePath, ProgressEvent, PullJob, Result, TransferOptions};
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
    // The renderer starts before `plan_pull` because the core awaits every non-progress event
    // send, so a receiver must already be draining.
    let renderer = tokio::spawn(Renderer::run(rx, ctx.ui, started));
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
    let job = plan(ctx, &ctx.resolve(remote), local, opts, events).await?;
    if dry_run {
        return Ok(Outcome::DryRun(job.plan().clone()));
    }
    let report = job.run(&ctx.cancel, events).await?;
    Ok(Outcome::Transferred(report))
}

async fn plan<'d>(
    ctx: &'d Ctx,
    remote: &DevicePath,
    local: &Path,
    opts: &TransferOptions,
    events: &Sender<ProgressEvent>,
) -> Result<PullJob<'d>> {
    let device = &ctx.device;
    match device
        .plan_pull(remote, local, opts, &ctx.cancel, events)
        .await
    {
        Err(error) => {
            let picked = ctx.pick_storage(remote, error)?;
            device
                .plan_pull(&picked, local, opts, &ctx.cancel, events)
                .await
        }
        planned => planned,
    }
}
