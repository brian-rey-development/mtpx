//! The clap command tree: what the user types, parsed and validated before anything runs.

use clap::{ArgAction, Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use mtpx_core::{ConflictPolicy, DevicePath, DeviceSelector, StorageSelector, TransferOptions};
use std::path::PathBuf;

const REMOTE_DIR_HELP: &str = "Remote directory, as /path or storage:/path";
const REMOTE_ENTRY_HELP: &str = "Remote file or directory, as /path or storage:/path";

/// Top-level invocation: global flags plus one subcommand.
#[derive(Debug, Parser)]
#[command(
    name = "mtpx",
    version,
    about = "rsync for your phone: fast, incremental transfers over MTP"
)]
pub struct Cli {
    /// Flags accepted before or after the subcommand.
    #[command(flatten)]
    pub global: Global,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags every subcommand accepts.
#[derive(Debug, Args)]
pub struct Global {
    /// Device to open, by USB serial or by index in mtpx devices
    #[arg(
        long,
        global = true,
        value_name = "SERIAL|INDEX",
        value_parser = NonEmptyStringValueParser::new()
    )]
    pub device: Option<String>,

    /// Storage to use, by name or index; overrides the storage: prefix of every path argument
    #[arg(
        long,
        global = true,
        value_name = "NAME|INDEX",
        value_parser = NonEmptyStringValueParser::new()
    )]
    pub storage: Option<String>,

    /// Print only failures and the final summary
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Log more; repeat for debug and trace output
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Disable colors and styling
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Never prompt; fail with the list instead when a device or storage must be chosen
    #[arg(long, global = true)]
    pub no_interactive: bool,

    /// Open an in-process virtual device backed by this directory instead of USB
    #[cfg(feature = "virtual-device")]
    #[arg(long, global = true, hide = true, value_name = "DIR")]
    pub r#virtual: Option<PathBuf>,
}

/// The M1 subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List attached MTP devices
    Devices,
    /// List a remote directory
    Ls(LsArgs),
    /// Copy a remote file or directory to a local directory
    Pull(PullArgs),
    /// Make a local directory mirror a remote one (source wins, nothing deleted)
    Sync(SyncArgs),
}

/// Arguments of `mtpx ls`.
#[derive(Debug, Args)]
pub struct LsArgs {
    #[arg(default_value = "/", help = REMOTE_DIR_HELP)]
    pub path: DevicePath,
    /// Show kind, size and modification time
    #[arg(short)]
    pub long: bool,
    /// List the whole subtree
    #[arg(short = 'R', long)]
    pub recursive: bool,
}

/// Arguments of `mtpx pull`.
#[derive(Debug, Args)]
pub struct PullArgs {
    #[arg(help = REMOTE_ENTRY_HELP)]
    pub remote: DevicePath,
    /// Local directory to copy into
    pub local: PathBuf,
    /// Replace local files that differ from the remote ones
    #[arg(long, conflicts_with = "skip_existing")]
    pub overwrite: bool,
    /// Leave local files that differ from the remote ones alone
    #[arg(long)]
    pub skip_existing: bool,
    /// Print the plan and touch nothing
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments of `mtpx sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    #[arg(help = REMOTE_DIR_HELP)]
    pub remote: DevicePath,
    /// Local directory to mirror into
    pub local: PathBuf,
    /// Print the plan and touch nothing
    #[arg(long)]
    pub dry_run: bool,
}

impl PullArgs {
    /// Conflicts fail the plan unless one of the flags says what to do with them.
    pub const fn options(&self) -> TransferOptions {
        let mut opts = TransferOptions::pull();
        if self.overwrite {
            opts.conflict = ConflictPolicy::SourceWins;
        }
        if self.skip_existing {
            opts.conflict = ConflictPolicy::Skip;
        }
        opts
    }
}

impl Global {
    /// Which device `--device` names; the only attached one when absent.
    pub fn device_selector(&self) -> DeviceSelector {
        self.device
            .as_deref()
            .map_or(DeviceSelector::Only, |value| {
                value.parse().map_or_else(
                    |_| DeviceSelector::Serial(value.to_owned()),
                    DeviceSelector::Index,
                )
            })
    }

    /// Which storage `--storage` names, when given.
    pub fn storage_selector(&self) -> Option<StorageSelector> {
        self.storage.as_deref().map(|value| {
            value.parse().map_or_else(
                |_| StorageSelector::Named(value.to_owned()),
                StorageSelector::Index,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mtpx").chain(args.iter().copied()))
    }

    #[test]
    fn ls_defaults_to_the_root_without_long_or_recursive() {
        let cli = parse(&["ls"]).unwrap();
        let Command::Ls(args) = cli.command else {
            panic!("expected ls");
        };
        assert_eq!(args.path.to_string(), "/");
        assert!(!args.long);
        assert!(!args.recursive);
    }

    #[test]
    fn pull_rejects_overwrite_together_with_skip_existing() {
        let err = parse(&["pull", "/a", "out", "--overwrite", "--skip-existing"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn an_invalid_remote_path_is_a_usage_error() {
        let err = parse(&["ls", "DCIM"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().contains("must start with '/'"), "{err}");
    }

    #[test]
    fn device_flag_maps_digits_to_an_index_and_anything_else_to_a_serial() {
        let by_index = parse(&["--device", "2", "devices"]).unwrap();
        assert_eq!(by_index.global.device_selector(), DeviceSelector::Index(2));
        let by_serial = parse(&["devices", "--device", "ZY22"]).unwrap();
        assert_eq!(
            by_serial.global.device_selector(),
            DeviceSelector::Serial("ZY22".into())
        );
        assert_eq!(
            parse(&["devices"]).unwrap().global.device_selector(),
            DeviceSelector::Only
        );
    }

    #[test]
    fn storage_flag_maps_digits_to_an_index_and_anything_else_to_a_name() {
        let by_index = parse(&["--storage", "1", "ls"]).unwrap();
        assert_eq!(
            by_index.global.storage_selector(),
            Some(StorageSelector::Index(1))
        );
        let by_name = parse(&["ls", "--storage", "SD card"]).unwrap();
        assert_eq!(
            by_name.global.storage_selector(),
            Some(StorageSelector::Named("SD card".into()))
        );
        assert_eq!(parse(&["ls"]).unwrap().global.storage_selector(), None);
    }

    #[test]
    fn pull_flags_pick_the_conflict_policy() {
        let policy = |args: &[&str]| {
            let Command::Pull(pull) = parse(args).unwrap().command else {
                panic!("expected pull");
            };
            pull.options().conflict
        };
        assert_eq!(policy(&["pull", "/a", "out"]), ConflictPolicy::Fail);
        assert_eq!(
            policy(&["pull", "/a", "out", "--overwrite"]),
            ConflictPolicy::SourceWins
        );
        assert_eq!(
            policy(&["pull", "/a", "out", "--skip-existing"]),
            ConflictPolicy::Skip
        );
    }

    #[test]
    fn empty_device_and_storage_values_are_usage_errors() {
        let err = parse(&["--device", "", "devices"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
        let err = parse(&["ls", "--storage", ""]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn verbose_counts_repeats_and_quiet_is_a_global_flag() {
        let cli = parse(&["sync", "/DCIM", "out", "-vv", "-q"]).unwrap();
        assert_eq!(cli.global.verbose, 2);
        assert!(cli.global.quiet);
    }
}
