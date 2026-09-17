//! `mtpx ls`: one directory or a whole subtree, as a table on stdout.

use crate::{
    cli::LsArgs,
    commands::{self, Ctx, Outcome, Output},
    ui::{format, table},
};
use mtpx_core::{DevicePath, Result, Snapshot};

/// Lists `path` and prints it; objects the device refused to describe are counted on stderr
/// unless quiet.
pub async fn run(ctx: &Ctx, args: &LsArgs) -> Result<Outcome> {
    let snapshot = list(ctx, &ctx.resolve(&args.path), args.recursive).await?;
    commands::print(&table::ls(&snapshot, args.long))?;
    let skipped = snapshot.skipped().len() as u64;
    if skipped > 0 && ctx.ui.output != Output::Quiet {
        eprintln!(
            "{} the device refused to describe were left out",
            format::objects(skipped)
        );
    }
    Ok(Outcome::Done)
}

async fn list(ctx: &Ctx, path: &DevicePath, recursive: bool) -> Result<Snapshot> {
    match ctx.device.ls(path, recursive, &ctx.cancel).await {
        Err(error) => {
            let picked = ctx.pick_storage(path, error)?;
            ctx.device.ls(&picked, recursive, &ctx.cancel).await
        }
        listed => listed,
    }
}
