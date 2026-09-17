//! `mtpx ls`: one directory or a whole subtree, as a table on stdout.

use crate::{
    cli::LsArgs,
    commands::{self, Ctx, Outcome},
    ui::{format, stderr, table},
};
use mtpx_core::Result;

/// Lists `path` and prints it. Objects the device refused to describe are missing from the
/// listing, so they are counted on stderr even when quiet.
pub async fn run(ctx: &Ctx, args: &LsArgs) -> Result<Outcome> {
    let path = ctx.resolve(&args.path);
    let snapshot = ctx
        .with_storage(&path, async |p| {
            ctx.device.ls(p, args.recursive, &ctx.cancel).await
        })
        .await?;
    commands::print(&table::ls(&snapshot, args.long))?;
    let skipped = snapshot.skipped();
    if !skipped.is_empty() {
        stderr::line(&format!(
            "{} the device refused to describe were left out",
            format::objects(skipped.len() as u64)
        ));
        for line in format::skipped_lines(skipped) {
            stderr::line(&line);
        }
    }
    Ok(Outcome::Done)
}
