//! The logical plan: what a transfer will do, decided before any byte moves.

use crate::{entry::ModifiedTime, path::RelPath};

/// Why the planner decided a file must move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyReason {
    /// The destination has no file at this path.
    New,
    /// Both sides have the path but the sizes differ.
    SizeDiffers,
}

/// Why the planner leaves a path untouched; the executor reports it without reading either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// Same kind and size on both sides.
    Identical,
    /// The sizes differ and the policy leaves the destination alone.
    Conflict,
    /// A file on one side is a directory on the other, or sits beneath such a directory;
    /// nothing is deleted to make room for a different kind.
    KindConflict,
    /// The destination folds names, and another source entry already claims this one under
    /// folding; the first in source order wins.
    NameCollision,
}

/// One step of a plan. Parents come before children, so an executor can run the list in order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Create a directory that the source has and the destination lacks.
    Mkdir {
        /// Relative to the transfer root on both sides.
        path: RelPath,
    },
    /// Stream a file from source to destination, replacing whatever the destination holds.
    Copy {
        /// Relative to the transfer root on both sides.
        path: RelPath,
        /// Full size of the source file.
        size: u64,
        /// Source modification time to apply after the copy.
        modified: Option<ModifiedTime>,
        /// Offset a matching partial already holds; zero for a fresh copy.
        resume_from: u64,
        /// What made the copy necessary, for reporting.
        reason: CopyReason,
    },
    /// Leave the path alone on both sides.
    Skip {
        /// Relative to the transfer root on both sides.
        path: RelPath,
        /// What ruled the path out, for reporting.
        reason: SkipReason,
    },
}

/// Totals of a plan. Sizes come from the device, so every sum saturates rather than wraps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlanSummary {
    /// Copy actions, whether fresh or resumed.
    pub files_to_copy: u64,
    /// After subtracting resumable partials.
    pub bytes_to_copy: u64,
    /// Bytes already present in partials that will not be re-read.
    pub resumable_bytes: u64,
    /// Skip actions of every reason; directories are never counted.
    pub files_to_skip: u64,
}

impl PlanSummary {
    fn add(mut self, action: &Action) -> Self {
        match action {
            Action::Copy {
                size, resume_from, ..
            } => {
                debug_assert!(resume_from <= size, "resume_from must not exceed size");
                self.files_to_copy = self.files_to_copy.saturating_add(1);
                self.bytes_to_copy = self
                    .bytes_to_copy
                    .saturating_add(size.saturating_sub(*resume_from));
                self.resumable_bytes = self.resumable_bytes.saturating_add(*resume_from);
            }
            Action::Skip { .. } => self.files_to_skip = self.files_to_skip.saturating_add(1),
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
    pub(crate) fn new(actions: Vec<Action>) -> Self {
        let summary = actions
            .iter()
            .fold(PlanSummary::default(), PlanSummary::add);
        Self { actions, summary }
    }

    /// The steps in execution order: every `Mkdir` precedes the actions beneath it.
    #[must_use]
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// Totals computed once when the plan was built; they never diverge from `actions()`.
    #[must_use]
    pub const fn summary(&self) -> PlanSummary {
        self.summary
    }

    /// Number of actions, directories and skips included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// An empty plan has no actions at all; use `summary().files_to_copy` for "nothing to copy".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::test_support::rel;

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
                files_to_skip: 2,
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
                files_to_skip: 1,
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

    #[test]
    fn summary_saturates_on_absurd_device_sizes() {
        let plan = Plan::new(vec![copy("a", u64::MAX, 0), copy("b", u64::MAX, 0)]);
        assert_eq!(plan.summary().bytes_to_copy, u64::MAX);
        assert_eq!(plan.summary().files_to_copy, 2);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "resume_from")]
    fn resume_offset_past_the_end_panics_in_debug() {
        let _ = Plan::new(vec![copy("a", 10, 11)]);
    }
}
