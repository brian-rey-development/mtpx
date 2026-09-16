//! Knobs that change what a transfer does with files that already exist.

/// What to do when the destination already has a same-path file that is not equal to the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Refuse at plan time, before anything is touched, listing every conflict.
    Fail,
    /// Overwrite the destination copy.
    SourceWins,
    /// Leave the destination copy alone and report it as skipped.
    Skip,
}

/// Options shared by every transfer command; built with [`TransferOptions::pull`] or
/// [`TransferOptions::sync`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransferOptions {
    /// How same-path files that differ are handled.
    pub conflict: ConflictPolicy,
}

impl TransferOptions {
    /// Defaults for `pull`: conflicts abort before any byte moves.
    #[must_use]
    pub const fn pull() -> Self {
        Self {
            conflict: ConflictPolicy::Fail,
        }
    }

    /// Defaults for `sync`: the source is authoritative.
    #[must_use]
    pub const fn sync() -> Self {
        Self {
            conflict: ConflictPolicy::SourceWins,
        }
    }
}
