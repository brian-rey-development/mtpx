//! The clap command tree: what the user types, parsed and validated before anything runs.

use crate::exit;
use clap::{ArgAction, Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use mtpx_core::{ConflictPolicy, DevicePath, DeviceSelector, StorageSelector, TransferOptions};
use std::path::PathBuf;

const REMOTE_DIR_HELP: &str = "Remote directory, as /path or storage:/path";
const REMOTE_ENTRY_HELP: &str = "Remote file or directory, as /path or storage:/path";
const VERBOSE_HELP: &str =
    "Log more; repeat for debug and trace output. RUST_LOG applies only when no -v is given";
/// Marks a `--device` value as a serial even when it is all digits.
const SERIAL_PREFIX: &str = "serial:";

/// Top-level invocation: global flags plus one subcommand.
#[derive(Debug, Parser)]
#[command(name = "mtpx", version, about, after_help = exit::HELP)]
pub struct Cli {
    /// Flags accepted before or after the subcommand.
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// Flags every subcommand accepts.
#[derive(Debug, Args)]
#[command(next_help_heading = "Global options")]
pub struct Global {
    /// Device to open: an index from mtpx devices, or a USB serial (prefix with serial: when the serial is all digits)
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

    #[arg(short, long, global = true, action = ArgAction::Count, help = VERBOSE_HELP)]
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

    /// Objects, relative to the backing directory, that the virtual device refuses to describe
    #[cfg(feature = "virtual-device")]
    #[arg(
        long,
        global = true,
        hide = true,
        value_name = "PATH",
        requires = "virtual"
    )]
    pub virtual_refuse: Vec<String>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List attached MTP devices
    Devices,
    #[command(flatten)]
    WithDevice(DeviceCommand),
}

/// Subcommands that run against one open device.
#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// List a remote directory
    Ls(LsArgs),
    /// Copy a remote file or directory to a local directory
    Pull(PullArgs),
    /// Make a local directory mirror a remote file or directory (source wins, nothing deleted)
    Sync(SyncArgs),
}

#[derive(Debug, Args)]
pub struct LsArgs {
    #[arg(default_value = "/", help = REMOTE_DIR_HELP)]
    pub path: DevicePath,
    /// Show kind, size and modification time
    #[arg(short, long)]
    pub long: bool,
    /// List the whole subtree
    #[arg(short = 'R', long)]
    pub recursive: bool,
}

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

#[derive(Debug, Args)]
pub struct SyncArgs {
    #[arg(help = REMOTE_ENTRY_HELP)]
    pub remote: DevicePath,
    /// Local directory to mirror into
    pub local: PathBuf,
    /// Print the plan and touch nothing
    #[arg(long)]
    pub dry_run: bool,
}

impl Command {
    /// Whether the command moves files, so an interrupt reads differently.
    pub const fn transfers(&self) -> bool {
        matches!(
            self,
            Self::WithDevice(DeviceCommand::Pull(_) | DeviceCommand::Sync(_))
        )
    }
}

impl PullArgs {
    /// Conflicts fail the plan unless one of the flags says what to do with them.
    pub const fn options(&self) -> TransferOptions {
        let mut opts = TransferOptions::pull();
        if self.overwrite {
            opts.conflict = ConflictPolicy::SourceWins;
        } else if self.skip_existing {
            opts.conflict = ConflictPolicy::Skip;
        }
        opts
    }
}

impl Global {
    /// Which device `--device` names; the only attached one when absent.
    pub fn device_selector(&self) -> DeviceSelector {
        let Some(value) = self.device.as_deref() else {
            return DeviceSelector::Only;
        };
        value.strip_prefix(SERIAL_PREFIX).map_or_else(
            || DeviceSelector::from(value),
            |serial| DeviceSelector::Serial(serial.to_owned()),
        )
    }

    /// Which storage `--storage` names, when given.
    pub fn storage_selector(&self) -> Option<StorageSelector> {
        self.storage.as_deref().map(StorageSelector::from)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use clap::{CommandFactory, error::ErrorKind};

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mtpx").chain(args.iter().copied()))
    }

    fn ls_args(args: &[&str]) -> LsArgs {
        let Command::WithDevice(DeviceCommand::Ls(args)) = parse(args).unwrap().command else {
            panic!("expected ls");
        };
        args
    }

    #[test]
    fn ls_defaults_to_the_root_without_long_or_recursive() {
        let args = ls_args(&["ls"]);
        assert_eq!(args.path.to_string(), "/");
        assert!(!args.long);
        assert!(!args.recursive);
    }

    #[test]
    fn ls_long_has_a_short_and_a_long_form() {
        assert!(ls_args(&["ls", "-l"]).long);
        assert!(ls_args(&["ls", "--long"]).long);
    }

    #[test]
    fn help_ends_with_the_exit_codes_under_their_own_heading() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("Exit codes:"), "{help}");
        assert!(help.contains("Global options:"), "{help}");
    }

    #[test]
    fn subcommand_help_lists_its_own_flags_before_the_global_ones() {
        let help = parse(&["pull", "--help"]).unwrap_err().to_string();
        let own = help.find("--overwrite").unwrap();
        let global = help.find("Global options:").unwrap();
        let device = help.find("--device").unwrap();
        assert!(own < global && global < device, "{help}");
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
    fn only_pull_and_sync_transfer() {
        assert!(parse(&["pull", "/a", "out"]).unwrap().command.transfers());
        assert!(parse(&["sync", "/a", "out"]).unwrap().command.transfers());
        assert!(!parse(&["ls"]).unwrap().command.transfers());
        assert!(!parse(&["devices"]).unwrap().command.transfers());
    }

    #[test]
    fn a_serial_prefix_keeps_an_all_digit_serial_from_becoming_an_index() {
        let prefixed = parse(&["--device", "serial:12345678", "devices"]).unwrap();
        assert_eq!(
            prefixed.global.device_selector(),
            DeviceSelector::Serial("12345678".into())
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
            let Command::WithDevice(DeviceCommand::Pull(pull)) = parse(args).unwrap().command
            else {
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
