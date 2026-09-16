//! The logical plan: what a transfer will do, decided before any byte moves.

use crate::{entry::ModifiedTime, path::RelPath};

/// Why a file is going to be copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyReason {
    /// The destination has no file at this path.
    New,
    /// Both sides have the path but the sizes differ.
    SizeDiffers,
}

/// Why a file is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// Both sides already agree.
    Identical,
    /// The sides differ and the conflict policy says not to touch it.
    Conflict,
}

/// One step of a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Create a directory on the destination.
    Mkdir {
        /// Directory to create.
        path: RelPath,
    },
    /// Copy a file from source to destination.
    Copy {
        /// File to copy.
        path: RelPath,
        /// Full size of the source file.
        size: u64,
        /// Source modification time to apply after the copy.
        modified: Option<ModifiedTime>,
        /// Offset a matching partial already holds; zero for a fresh copy.
        resume_from: u64,
        /// Why the planner decided to copy.
        reason: CopyReason,
    },
    /// Leave a file untouched.
    Skip {
        /// File being skipped.
        path: RelPath,
        /// Why it is skipped.
        reason: SkipReason,
    },
}

/// Totals derived from a plan's actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanSummary {
    /// Number of `Copy` actions.
    pub files_to_copy: u64,
    /// Bytes that still need to move, after subtracting resumable partials.
    pub bytes_to_copy: u64,
    /// Bytes already present in partials that will not be re-read.
    pub resumable_bytes: u64,
    /// Number of `Skip` actions.
    pub to_skip: u64,
}

impl PlanSummary {
    fn add(mut self, action: &Action) -> Self {
        match action {
            Action::Copy {
                size, resume_from, ..
            } => {
                debug_assert!(resume_from <= size, "resume_from must not exceed size");
                self.files_to_copy += 1;
                self.bytes_to_copy += size.saturating_sub(*resume_from);
                self.resumable_bytes += resume_from;
            }
            Action::Skip { .. } => self.to_skip += 1,
            Action::Mkdir { .. } => {}
        }
        self
    }
}

/// An ordered list of actions plus the totals they add up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    actions: Vec<Action>,
    summary: PlanSummary,
}

impl Plan {
    /// Wraps the actions in execution order and derives their totals.
    pub(crate) fn new(actions: Vec<Action>) -> Self {
        let summary = actions
            .iter()
            .fold(PlanSummary::default(), PlanSummary::add);
        Self { actions, summary }
    }

    /// The actions in execution order.
    #[must_use]
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// The totals for the whole plan.
    #[must_use]
    pub const fn summary(&self) -> PlanSummary {
        self.summary
    }

    /// Number of actions, including skips and directory creations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Whether the plan has no actions at all; use `summary().files_to_copy` for "nothing to copy".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn rel(name: &str) -> RelPath {
        RelPath::new([name]).unwrap()
    }

    fn skip(name: &str, reason: SkipReason) -> Action {
        Action::Skip {
            path: rel(name),
            reason,
        }
    }

    fn copy(name: &str, size: u64, resume_from: u64) -> Action {
        Action::Copy {
            path: rel(name),
            size,
            modified: None,
            resume_from,
            reason: CopyReason::New,
        }
    }

    #[test]
    fn summary_counts_copies_bytes_resume_and_skips() {
        let plan = Plan::new(vec![
            Action::Mkdir { path: rel("d") },
            copy("a", 100, 0),
            copy("b", 500, 200),
            skip("c", SkipReason::Identical),
            skip("e", SkipReason::Conflict),
        ]);
        assert_eq!(
            plan.summary(),
            PlanSummary {
                files_to_copy: 2,
                bytes_to_copy: 400,
                resumable_bytes: 200,
                to_skip: 2,
            }
        );
        assert_eq!(plan.len(), 5);
        assert!(!plan.is_empty());
    }

    #[test]
    fn plan_with_only_mkdirs_and_skips_has_nothing_to_copy_but_is_not_empty() {
        let plan = Plan::new(vec![
            Action::Mkdir { path: rel("d") },
            skip("c", SkipReason::Identical),
        ]);
        assert!(!plan.is_empty());
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.summary().files_to_copy, 0);
        assert_eq!(
            plan.summary(),
            PlanSummary {
                to_skip: 1,
                ..PlanSummary::default()
            }
        );
    }

    #[test]
    fn plan_with_no_actions_is_empty() {
        let plan = Plan::new(vec![]);
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
        assert_eq!(plan.summary(), PlanSummary::default());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "resume_from")]
    fn resume_offset_past_the_end_panics_in_debug() {
        let _ = Plan::new(vec![copy("a", 10, 11)]);
    }
}
