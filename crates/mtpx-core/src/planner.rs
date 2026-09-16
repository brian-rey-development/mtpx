//! The decision procedure: diff two snapshots into an ordered `Plan`, touching neither side.

use crate::{
    entry::{Entry, EntryKind, Snapshot},
    error::{Error, Result},
    internal::partial::PartialInfo,
    options::{ConflictPolicy, TransferOptions},
    path::RelPath,
    plan::{Action, CopyReason, Plan, SkipReason},
};
use std::collections::HashMap;

/// Map from destination-relative path to the partial download found there.
pub type Partials = HashMap<RelPath, PartialInfo>;

/// Computes what must happen to make `dest` contain `source`, without touching either side.
///
/// # Errors
/// `Error::Conflicts` with every conflicting path, in source order, when `opts.conflict`
/// is `Fail` and at least one same-path entry differs.
#[cfg_attr(
    not(any(test, feature = "bench-internals")),
    expect(dead_code, reason = "called by the Phase 5 executor")
)]
// pub rather than pub(crate) only so the bench-internals seam can re-export it; the module itself is private.
pub fn plan(
    source: &Snapshot,
    dest: &Snapshot,
    partials: &Partials,
    opts: &TransferOptions,
) -> Result<Plan> {
    if opts.conflict == ConflictPolicy::Fail {
        let conflicts = collect_conflicts(source, dest);
        if !conflicts.is_empty() {
            return Err(Error::Conflicts(conflicts));
        }
    }
    // Every Mkdir goes before every Copy. Snapshots sort parents before children, so this
    // is the cheapest order that still guarantees a directory exists before its files land.
    let mut actions = dir_actions(source, dest, opts.conflict);
    actions.extend(file_actions(source, dest, partials, opts.conflict));
    Ok(Plan::new(actions))
}

fn collect_conflicts(source: &Snapshot, dest: &Snapshot) -> Vec<RelPath> {
    source
        .entries()
        .iter()
        .filter(|entry| {
            dest.get(&entry.path)
                .is_some_and(|existing| is_conflict(entry, existing))
        })
        .map(|entry| entry.path.clone())
        .collect()
}

fn dir_actions(source: &Snapshot, dest: &Snapshot, conflict: ConflictPolicy) -> Vec<Action> {
    source
        .dirs()
        .filter_map(|dir| decide_dir(dir, dest.get(&dir.path), conflict))
        .collect()
}

fn file_actions(
    source: &Snapshot,
    dest: &Snapshot,
    partials: &Partials,
    conflict: ConflictPolicy,
) -> Vec<Action> {
    source
        .files()
        .map(|file| {
            decide_file(
                file,
                dest.get(&file.path),
                partials.get(&file.path),
                conflict,
            )
        })
        .collect()
}

/// A directory is created when nothing is there yet, or when the policy lets it replace a file.
fn decide_dir(dir: &Entry, existing: Option<&Entry>, conflict: ConflictPolicy) -> Option<Action> {
    let mkdir = Action::Mkdir {
        path: dir.path.clone(),
    };
    match existing {
        None => Some(mkdir),
        Some(existing) if existing.kind == EntryKind::Dir => None,
        Some(_) => (conflict == ConflictPolicy::SourceWins).then_some(mkdir),
    }
}

/// The single decision point for one source file.
fn decide_file(
    file: &Entry,
    existing: Option<&Entry>,
    partial: Option<&PartialInfo>,
    conflict: ConflictPolicy,
) -> Action {
    let Some(existing) = existing else {
        return copy(file, CopyReason::New, resume_offset(file, partial));
    };
    // A partial never turns an identical file into a copy: equal size means there is nothing
    // to transfer, so the partial is orphaned data for the local endpoint to clean up, not a
    // resume hint.
    if !is_conflict(file, existing) {
        return skip(file, SkipReason::Identical);
    }
    match conflict {
        ConflictPolicy::Skip => skip(file, SkipReason::Conflict),
        // Fail cannot reach a conflict here: `plan` returns before building any action.
        ConflictPolicy::Fail | ConflictPolicy::SourceWins => {
            copy(file, CopyReason::SizeDiffers, resume_offset(file, partial))
        }
    }
}

/// Bytes a partial lets the copy skip; zero unless it came from this exact object and is incomplete.
fn resume_offset(file: &Entry, partial: Option<&PartialInfo>) -> u64 {
    partial
        .filter(|partial| partial.fingerprint.matches(file) && partial.bytes < file.size)
        .map_or(0, |partial| partial.bytes)
}

/// Same path on both sides but not the same thing: kinds differ, or sizes differ.
fn is_conflict(entry: &Entry, existing: &Entry) -> bool {
    entry.kind != existing.kind || entry.size != existing.size
}

fn copy(file: &Entry, reason: CopyReason, resume_from: u64) -> Action {
    Action::Copy {
        path: file.path.clone(),
        size: file.size,
        modified: file.modified,
        resume_from,
        reason,
    }
}

fn skip(file: &Entry, reason: SkipReason) -> Action {
    Action::Skip {
        path: file.path.clone(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::too_many_lines)]

    use super::*;
    use crate::{entry::ModifiedTime, internal::partial::Fingerprint, plan::PlanSummary};
    use std::time::{Duration, UNIX_EPOCH};

    fn rel(path: &str) -> RelPath {
        RelPath::new(path.split('/')).unwrap()
    }

    fn file(path: &str, size: u64) -> Entry {
        Entry {
            path: rel(path),
            kind: EntryKind::File,
            size,
            modified: None,
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            path: rel(path),
            kind: EntryKind::Dir,
            size: 0,
            modified: None,
        }
    }

    fn snapshot(entries: Vec<Entry>) -> Snapshot {
        Snapshot::new("/", entries, vec![])
    }

    fn partial(size: u64, bytes: u64) -> PartialInfo {
        PartialInfo {
            fingerprint: Fingerprint {
                size,
                modified: None,
            },
            bytes,
        }
    }

    fn partials(items: Vec<(&str, PartialInfo)>) -> Partials {
        items.into_iter().map(|(p, info)| (rel(p), info)).collect()
    }

    fn opts(conflict: ConflictPolicy) -> TransferOptions {
        TransferOptions { conflict }
    }

    fn plan_with(source: Vec<Entry>, dest: Vec<Entry>, conflict: ConflictPolicy) -> Plan {
        plan(
            &snapshot(source),
            &snapshot(dest),
            &Partials::new(),
            &opts(conflict),
        )
        .unwrap()
    }

    fn plan_resuming(source: Vec<Entry>, dest: Vec<Entry>, partials: &Partials) -> Plan {
        plan(
            &snapshot(source),
            &snapshot(dest),
            partials,
            &opts(ConflictPolicy::SourceWins),
        )
        .unwrap()
    }

    fn copy_new(path: &str, size: u64, resume_from: u64) -> Action {
        Action::Copy {
            path: rel(path),
            size,
            modified: None,
            resume_from,
            reason: CopyReason::New,
        }
    }

    fn copy_differs(path: &str, size: u64, resume_from: u64) -> Action {
        Action::Copy {
            path: rel(path),
            size,
            modified: None,
            resume_from,
            reason: CopyReason::SizeDiffers,
        }
    }

    fn skip(path: &str, reason: SkipReason) -> Action {
        Action::Skip {
            path: rel(path),
            reason,
        }
    }

    fn mkdir(path: &str) -> Action {
        Action::Mkdir { path: rel(path) }
    }

    fn conflicts_of(source: Vec<Entry>, dest: Vec<Entry>) -> Vec<RelPath> {
        let err = plan(
            &snapshot(source),
            &snapshot(dest),
            &Partials::new(),
            &opts(ConflictPolicy::Fail),
        )
        .unwrap_err();
        match err {
            Error::Conflicts(paths) => paths,
            other => panic!("expected Conflicts, got {other:?}"),
        }
    }

    #[test]
    fn a_file_missing_from_dest_is_copied_from_zero() {
        let plan = plan_with(vec![file("a.bin", 10)], vec![], ConflictPolicy::Fail);
        assert_eq!(plan.actions(), [copy_new("a.bin", 10, 0)]);
    }

    #[test]
    fn a_file_with_equal_size_on_both_sides_is_skipped_as_identical() {
        let plan = plan_with(
            vec![file("a.bin", 10)],
            vec![file("a.bin", 10)],
            ConflictPolicy::Fail,
        );
        assert_eq!(plan.actions(), [skip("a.bin", SkipReason::Identical)]);
    }

    #[test]
    fn fail_policy_returns_every_conflicting_path_in_source_order_and_no_plan() {
        let source = vec![
            file("a.bin", 10),
            file("b.bin", 20),
            file("c.bin", 30),
            file("d.bin", 40),
        ];
        let dest = vec![file("a.bin", 11), file("b.bin", 20), file("d.bin", 41)];
        assert_eq!(conflicts_of(source, dest), [rel("a.bin"), rel("d.bin")]);
    }

    #[test]
    fn fail_policy_reports_size_and_kind_conflicts_interleaved_in_source_order() {
        let source = vec![file("a.bin", 10), dir("b"), file("c.bin", 10)];
        let dest = vec![file("a.bin", 11), file("b", 1), file("c.bin", 12)];
        assert_eq!(
            conflicts_of(source, dest),
            [rel("a.bin"), rel("b"), rel("c.bin")]
        );
    }

    #[test]
    fn source_wins_policy_copies_a_file_whose_size_differs() {
        let plan = plan_with(
            vec![file("a.bin", 10)],
            vec![file("a.bin", 11)],
            ConflictPolicy::SourceWins,
        );
        assert_eq!(plan.actions(), [copy_differs("a.bin", 10, 0)]);
    }

    #[test]
    fn skip_policy_leaves_a_file_whose_size_differs_alone() {
        let plan = plan_with(
            vec![file("a.bin", 10)],
            vec![file("a.bin", 11)],
            ConflictPolicy::Skip,
        );
        assert_eq!(plan.actions(), [skip("a.bin", SkipReason::Conflict)]);
    }

    #[test]
    fn a_source_file_over_a_dest_directory_is_a_conflict_under_every_policy() {
        let source = || vec![file("x", 10)];
        let dest = || vec![dir("x")];
        assert_eq!(conflicts_of(source(), dest()), [rel("x")]);
        let wins = plan_with(source(), dest(), ConflictPolicy::SourceWins);
        assert_eq!(wins.actions(), [copy_differs("x", 10, 0)]);
        let skipped = plan_with(source(), dest(), ConflictPolicy::Skip);
        assert_eq!(skipped.actions(), [skip("x", SkipReason::Conflict)]);
    }

    #[test]
    fn a_source_directory_over_a_dest_file_is_a_conflict_under_every_policy() {
        let source = || vec![dir("x")];
        let dest = || vec![file("x", 10)];
        assert_eq!(conflicts_of(source(), dest()), [rel("x")]);
        let wins = plan_with(source(), dest(), ConflictPolicy::SourceWins);
        assert_eq!(wins.actions(), [mkdir("x")]);
        let skipped = plan_with(source(), dest(), ConflictPolicy::Skip);
        assert!(skipped.is_empty());
    }

    #[test]
    fn missing_directories_get_mkdir_and_existing_ones_do_not() {
        let plan = plan_with(
            vec![dir("old"), dir("new"), dir("new/inner")],
            vec![dir("old")],
            ConflictPolicy::Fail,
        );
        assert_eq!(plan.actions(), [mkdir("new"), mkdir("new/inner")]);
    }

    #[test]
    fn three_levels_of_nesting_produce_mkdirs_outermost_first_then_the_copy() {
        let plan = plan_with(
            vec![dir("a"), dir("a/b"), file("a/b/f", 1)],
            vec![],
            ConflictPolicy::Fail,
        );
        assert_eq!(
            plan.actions(),
            [mkdir("a"), mkdir("a/b"), copy_new("a/b/f", 1, 0)]
        );
    }

    #[test]
    fn every_mkdir_precedes_the_copies_beneath_it() {
        let plan = plan_with(
            vec![
                file("a.bin", 1),
                dir("p"),
                file("p/x.bin", 1),
                dir("p/q"),
                file("p/q/y.bin", 1),
            ],
            vec![],
            ConflictPolicy::Fail,
        );
        let position = |action: &Action| plan.actions().iter().position(|a| a == action).unwrap();
        assert!(position(&mkdir("p")) < position(&copy_new("p/x.bin", 1, 0)));
        assert!(position(&mkdir("p/q")) < position(&copy_new("p/q/y.bin", 1, 0)));
        assert!(position(&mkdir("p/q")) < position(&copy_new("a.bin", 1, 0)));
    }

    #[test]
    fn a_partial_with_a_matching_fingerprint_sets_the_resume_offset() {
        let partials = partials(vec![("a.bin", partial(100, 40))]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
        assert_eq!(plan.actions(), [copy_new("a.bin", 100, 40)]);
    }

    #[test]
    fn a_partial_keeps_its_offset_when_the_copy_is_an_overwrite() {
        let partials = partials(vec![("a.bin", partial(100, 40))]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![file("a.bin", 7)], &partials);
        assert_eq!(plan.actions(), [copy_differs("a.bin", 100, 40)]);
    }

    #[test]
    fn a_partial_whose_modified_time_differs_is_ignored() {
        let mut stale = partial(100, 40);
        stale.fingerprint.modified = Some(ModifiedTime::from_system(
            UNIX_EPOCH + Duration::from_secs(1),
        ));
        let partials = partials(vec![("a.bin", stale)]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
        assert_eq!(plan.actions(), [copy_new("a.bin", 100, 0)]);
    }

    #[test]
    fn a_partial_holding_the_whole_file_or_more_is_ignored() {
        for bytes in [100, 101] {
            let partials = partials(vec![("a.bin", partial(100, bytes))]);
            let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
            assert_eq!(plan.actions(), [copy_new("a.bin", 100, 0)]);
        }
    }

    #[test]
    fn a_stale_partial_never_turns_an_identical_file_into_a_copy() {
        let partials = partials(vec![("a.bin", partial(100, 40))]);
        let plan = plan_resuming(
            vec![file("a.bin", 100)],
            vec![file("a.bin", 100)],
            &partials,
        );
        assert_eq!(plan.actions(), [skip("a.bin", SkipReason::Identical)]);
    }

    #[test]
    fn summary_adds_up_for_a_mixed_plan() {
        let partials = partials(vec![("p/resume.bin", partial(500, 200))]);
        let plan = plan_resuming(
            vec![
                dir("p"),
                file("p/new.bin", 100),
                file("p/resume.bin", 500),
                file("p/same.bin", 30),
                file("p/differs.bin", 60),
            ],
            vec![file("p/same.bin", 30), file("p/differs.bin", 61)],
            &partials,
        );
        assert_eq!(
            plan.summary(),
            PlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 100 + 300 + 60,
                resumable_bytes: 200,
                to_skip: 1,
            }
        );
        assert_eq!(plan.len(), 5);
    }

    #[test]
    fn files_only_on_the_dest_produce_no_actions() {
        let plan = plan_with(
            vec![file("a.bin", 1)],
            vec![file("a.bin", 1), dir("extra"), file("extra/z.bin", 9)],
            ConflictPolicy::Fail,
        );
        assert_eq!(plan.actions(), [skip("a.bin", SkipReason::Identical)]);
    }

    #[test]
    fn an_empty_source_gives_an_empty_plan() {
        let plan = plan_with(vec![], vec![file("a.bin", 1)], ConflictPolicy::Fail);
        assert!(plan.is_empty());
        assert_eq!(plan.summary(), PlanSummary::default());
    }
}
