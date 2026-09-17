//! The decision procedure: diff two snapshots into an ordered `Plan`, touching neither side.

use crate::{
    entry::{Entry, EntryKind, Snapshot},
    error::{Error, Result},
    internal::partial::PartialInfo,
    options::{ConflictPolicy, TransferOptions},
    path::RelPath,
    plan::{Action, CopyReason, Plan, SkipReason},
};
use std::collections::{HashMap, hash_map::Entry as MapEntry};

/// Map from destination-relative path to the partial download found there.
#[cfg_attr(not(feature = "bench-internals"), allow(unreachable_pub))]
pub type Partials = HashMap<RelPath, PartialInfo>;

/// Whether the destination tells `a.jpg` and `A.jpg` apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "bench-internals"), allow(unreachable_pub))]
pub enum NameFolding {
    /// Names are compared byte for byte.
    Exact,
    /// The destination folds letter case, so `a.jpg` and `A.jpg` are one file there.
    CaseInsensitive,
}

/// Computes what must happen to make `dest` contain `source`, without touching either side.
/// `folding` is how the destination compares names, as its scan detected.
///
/// # Errors
/// `Error::Conflicts` with every conflicting path, in source order, when `opts.conflict`
/// is `Fail` and at least one same-path entry differs or two source names collide under
/// `folding`.
#[cfg_attr(not(feature = "bench-internals"), allow(unreachable_pub))]
pub fn plan(
    source: &Snapshot,
    dest: &Snapshot,
    folding: NameFolding,
    partials: &Partials,
    opts: &TransferOptions,
) -> Result<Plan> {
    let dest = DestIndex::new(dest, folding);
    if opts.conflict == ConflictPolicy::Fail {
        let conflicts = collect_conflicts(source, &dest, folding);
        if !conflicts.is_empty() {
            return Err(Error::Conflicts(conflicts));
        }
    }
    let mut decider = Decider::new(&dest, folding, partials, opts.conflict);
    for entry in source.entries() {
        decider.decide(entry);
    }
    Ok(decider.into_plan())
}

fn collect_conflicts(
    source: &Snapshot,
    dest: &DestIndex<'_>,
    folding: NameFolding,
) -> Vec<RelPath> {
    let mut claims = Claims::new(folding);
    source
        .entries()
        .iter()
        .filter(|entry| {
            let collides = claims.collides(entry);
            collides
                || dest
                    .get(&entry.path)
                    .is_some_and(|existing| is_conflict(entry, existing))
        })
        .map(|entry| entry.path.clone())
        .collect()
}

/// The destination's entries, looked up the way the destination itself compares names.
enum DestIndex<'a> {
    Exact(&'a Snapshot),
    Folded(HashMap<Vec<String>, &'a Entry>),
}

impl<'a> DestIndex<'a> {
    fn new(dest: &'a Snapshot, folding: NameFolding) -> Self {
        match folding {
            NameFolding::Exact => Self::Exact(dest),
            NameFolding::CaseInsensitive => {
                let mut folded = HashMap::with_capacity(dest.len());
                for entry in dest.entries() {
                    folded.entry(fold(&entry.path)).or_insert(entry);
                }
                Self::Folded(folded)
            }
        }
    }

    fn get(&self, path: &RelPath) -> Option<&'a Entry> {
        match self {
            Self::Exact(dest) => dest.get(path),
            Self::Folded(folded) => folded.get(&fold(path)).copied(),
        }
    }
}

fn fold(path: &RelPath) -> Vec<String> {
    path.segments()
        .iter()
        .map(|segment| segment.to_lowercase())
        .collect()
}

/// Which folded names the source has already claimed, so a later entry that folds to the
/// same name is a collision the destination would silently merge.
struct Claims {
    folding: NameFolding,
    seen: HashMap<Vec<String>, EntryKind>,
}

impl Claims {
    fn new(folding: NameFolding) -> Self {
        Self {
            folding,
            seen: HashMap::new(),
        }
    }

    /// Records `entry` and reports whether an earlier entry already owns its folded name.
    /// Two directories merge rather than collide; anything else cannot share the slot.
    fn collides(&mut self, entry: &Entry) -> bool {
        if self.folding == NameFolding::Exact {
            return false;
        }
        match self.seen.entry(fold(&entry.path)) {
            MapEntry::Vacant(slot) => {
                slot.insert(entry.kind);
                false
            }
            MapEntry::Occupied(owner) => {
                !(entry.kind == EntryKind::Dir && *owner.get() == EntryKind::Dir)
            }
        }
    }
}

/// One pass over the source in sorted order, which puts every directory right before its
/// contents, so a directory that cannot be created blocks its whole subtree in place.
struct Decider<'a> {
    dest: &'a DestIndex<'a>,
    partials: &'a Partials,
    conflict: ConflictPolicy,
    claims: Claims,
    blocked: Option<(RelPath, SkipReason)>,
    mkdirs: Vec<Action>,
    files: Vec<Action>,
}

impl<'a> Decider<'a> {
    fn new(
        dest: &'a DestIndex<'a>,
        folding: NameFolding,
        partials: &'a Partials,
        conflict: ConflictPolicy,
    ) -> Self {
        Self {
            dest,
            partials,
            conflict,
            claims: Claims::new(folding),
            blocked: None,
            mkdirs: Vec::new(),
            files: Vec::new(),
        }
    }

    fn decide(&mut self, entry: &Entry) {
        if let Some(reason) = self.blocking_reason(&entry.path) {
            self.files.push(skip(entry, reason));
            return;
        }
        if self.claims.collides(entry) {
            self.refuse(entry, SkipReason::NameCollision);
            return;
        }
        let existing = self.dest.get(&entry.path);
        match entry.kind {
            EntryKind::Dir => self.decide_dir(entry, existing),
            EntryKind::File => {
                let partial = self.partials.get(&entry.path);
                self.files
                    .push(decide_file(entry, existing, partial, self.conflict));
            }
        }
    }

    fn decide_dir(&mut self, dir: &Entry, existing: Option<&Entry>) {
        match decide_dir(existing) {
            DirDecision::Mkdir => self.mkdirs.push(Action::Mkdir {
                path: dir.path.clone(),
            }),
            DirDecision::Exists => {}
            DirDecision::Blocked => self.refuse(dir, SkipReason::KindConflict),
        }
    }

    fn blocking_reason(&mut self, path: &RelPath) -> Option<SkipReason> {
        let (prefix, reason) = self.blocked.as_ref()?;
        if path.starts_with(prefix) {
            return Some(*reason);
        }
        self.blocked = None;
        None
    }

    /// Skips `entry`; a directory takes everything beneath it along, since nothing can land
    /// inside a directory that will not exist.
    fn refuse(&mut self, entry: &Entry, reason: SkipReason) {
        if entry.kind == EntryKind::Dir {
            self.blocked = Some((entry.path.clone(), reason));
        }
        self.files.push(skip(entry, reason));
    }

    /// Mkdirs sort before copies, so every directory exists before its files land.
    fn into_plan(self) -> Plan {
        let mut actions = self.mkdirs;
        actions.extend(self.files);
        Plan::new(actions)
    }
}

enum DirDecision {
    Mkdir,
    Exists,
    Blocked,
}

/// A file in a directory's place blocks it under every policy: the executor never replaces
/// a file with a directory.
fn decide_dir(existing: Option<&Entry>) -> DirDecision {
    match existing {
        None => DirDecision::Mkdir,
        Some(existing) if existing.kind == EntryKind::Dir => DirDecision::Exists,
        Some(_) => DirDecision::Blocked,
    }
}

fn decide_file(
    file: &Entry,
    existing: Option<&Entry>,
    partial: Option<&PartialInfo>,
    conflict: ConflictPolicy,
) -> Action {
    let Some(existing) = existing else {
        return copy(file, CopyReason::New, resume_offset(file, partial));
    };
    if existing.kind != file.kind {
        return skip(file, SkipReason::KindConflict);
    }
    // A partial never outranks an identical file already on the destination.
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

/// Bytes a partial lets the copy skip; zero unless it came from this exact object. A partial
/// holding the whole file resumes at its size, so the copy only finalises it.
fn resume_offset(file: &Entry, partial: Option<&PartialInfo>) -> u64 {
    partial
        .filter(|partial| partial.fingerprint.matches(file) && partial.bytes <= file.size)
        .map_or(0, |partial| partial.bytes)
}

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
    use crate::{
        internal::partial::Fingerprint,
        plan::PlanSummary,
        test_support::{at, rel},
    };

    fn file(path: &str, size: u64) -> Entry {
        Entry::new(rel(path), EntryKind::File, size, None)
    }

    fn file_modified_at(path: &str, size: u64, seconds: u64) -> Entry {
        Entry::new(rel(path), EntryKind::File, size, Some(at(seconds)))
    }

    fn dir(path: &str) -> Entry {
        Entry::new(rel(path), EntryKind::Dir, 0, None)
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

    fn plan_folding(
        source: Vec<Entry>,
        dest: Vec<Entry>,
        folding: NameFolding,
        conflict: ConflictPolicy,
    ) -> Result<Plan> {
        plan(
            &snapshot(source),
            &snapshot(dest),
            folding,
            &Partials::new(),
            &opts(conflict),
        )
    }

    fn plan_with(source: Vec<Entry>, dest: Vec<Entry>, conflict: ConflictPolicy) -> Plan {
        plan_folding(source, dest, NameFolding::Exact, conflict).unwrap()
    }

    fn plan_resuming(source: Vec<Entry>, dest: Vec<Entry>, partials: &Partials) -> Plan {
        plan(
            &snapshot(source),
            &snapshot(dest),
            NameFolding::Exact,
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
        let err = plan_folding(source, dest, NameFolding::Exact, ConflictPolicy::Fail).unwrap_err();
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

    /// `Fail` is deliberate: were modification times ever part of the comparison, `plan`
    /// would return `Conflicts` and this test would fail loudly under every policy.
    #[test]
    fn a_same_size_file_with_a_different_mtime_is_still_identical() {
        let plan = plan_with(
            vec![file_modified_at("a.bin", 10, 1)],
            vec![file_modified_at("a.bin", 10, 2)],
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
    fn a_source_file_over_a_dest_directory_is_skipped_as_a_kind_conflict_under_every_policy() {
        let source = || vec![file("x", 10)];
        let dest = || vec![dir("x")];
        assert_eq!(conflicts_of(source(), dest()), [rel("x")]);
        for policy in [ConflictPolicy::SourceWins, ConflictPolicy::Skip] {
            let plan = plan_with(source(), dest(), policy);
            assert_eq!(
                plan.actions(),
                [skip("x", SkipReason::KindConflict)],
                "{policy:?}"
            );
            assert_eq!(plan.summary().bytes_to_copy, 0, "{policy:?}");
        }
    }

    #[test]
    fn a_source_directory_over_a_dest_file_is_skipped_as_a_kind_conflict_under_every_policy() {
        let source = || vec![dir("x")];
        let dest = || vec![file("x", 10)];
        assert_eq!(conflicts_of(source(), dest()), [rel("x")]);
        for policy in [ConflictPolicy::SourceWins, ConflictPolicy::Skip] {
            let plan = plan_with(source(), dest(), policy);
            assert_eq!(
                plan.actions(),
                [skip("x", SkipReason::KindConflict)],
                "{policy:?}"
            );
        }
    }

    #[test]
    fn a_directory_blocked_by_a_dest_file_skips_its_whole_subtree() {
        let source = || {
            vec![
                dir("x"),
                dir("x/sub"),
                file("x/a", 10),
                file("x/sub/b", 20),
                file("y", 1),
            ]
        };
        let dest = || vec![file("x", 5)];
        for policy in [ConflictPolicy::SourceWins, ConflictPolicy::Skip] {
            let plan = plan_with(source(), dest(), policy);
            assert_eq!(
                plan.actions(),
                [
                    skip("x", SkipReason::KindConflict),
                    skip("x/a", SkipReason::KindConflict),
                    skip("x/sub", SkipReason::KindConflict),
                    skip("x/sub/b", SkipReason::KindConflict),
                    copy_new("y", 1, 0),
                ],
                "{policy:?}"
            );
            assert_eq!(
                plan.summary(),
                PlanSummary {
                    files_to_copy: 1,
                    bytes_to_copy: 1,
                    resumable_bytes: 0,
                    files_to_skip: 4,
                },
                "{policy:?}"
            );
        }
    }

    #[test]
    fn a_blocked_directory_does_not_block_a_sibling_sharing_its_name_prefix() {
        let plan = plan_with(
            vec![dir("x"), file("x/a", 1), file("xy", 1)],
            vec![file("x", 5)],
            ConflictPolicy::SourceWins,
        );
        assert_eq!(
            plan.actions(),
            [
                skip("x", SkipReason::KindConflict),
                skip("x/a", SkipReason::KindConflict),
                copy_new("xy", 1, 0),
            ]
        );
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
        stale.fingerprint.modified = Some(at(1));
        let partials = partials(vec![("a.bin", stale)]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
        assert_eq!(plan.actions(), [copy_new("a.bin", 100, 0)]);
    }

    #[test]
    fn a_complete_partial_resumes_at_its_full_size() {
        let partials = partials(vec![("a.bin", partial(100, 100))]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
        assert_eq!(plan.actions(), [copy_new("a.bin", 100, 100)]);
        assert_eq!(
            plan.summary(),
            PlanSummary {
                files_to_copy: 1,
                bytes_to_copy: 0,
                resumable_bytes: 100,
                files_to_skip: 0,
            }
        );
    }

    #[test]
    fn a_partial_longer_than_the_file_is_ignored() {
        let partials = partials(vec![("a.bin", partial(100, 101))]);
        let plan = plan_resuming(vec![file("a.bin", 100)], vec![], &partials);
        assert_eq!(plan.actions(), [copy_new("a.bin", 100, 0)]);
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
                files_to_skip: 1,
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

    #[test]
    fn names_differing_only_by_case_are_two_copies_on_an_exact_destination() {
        let plan = plan_folding(
            vec![file("A.jpg", 1), file("a.jpg", 2)],
            vec![],
            NameFolding::Exact,
            ConflictPolicy::Fail,
        )
        .unwrap();
        assert_eq!(
            plan.actions(),
            [copy_new("A.jpg", 1, 0), copy_new("a.jpg", 2, 0)]
        );
    }

    #[test]
    fn names_differing_only_by_case_collide_on_a_folding_destination() {
        let plan = plan_folding(
            vec![file("A.jpg", 1), file("a.jpg", 2)],
            vec![],
            NameFolding::CaseInsensitive,
            ConflictPolicy::SourceWins,
        )
        .unwrap();
        assert_eq!(
            plan.actions(),
            [
                copy_new("A.jpg", 1, 0),
                skip("a.jpg", SkipReason::NameCollision)
            ]
        );
        assert_eq!(plan.summary().files_to_copy, 1);
        assert_eq!(plan.summary().files_to_skip, 1);
    }

    #[test]
    fn a_name_collision_is_a_conflict_under_the_fail_policy() {
        let err = plan_folding(
            vec![file("A.jpg", 1), file("a.jpg", 2)],
            vec![],
            NameFolding::CaseInsensitive,
            ConflictPolicy::Fail,
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Conflicts(paths) if *paths == [rel("a.jpg")]),
            "{err:?}"
        );
    }

    #[test]
    fn a_folding_destination_matches_a_differently_cased_existing_file() {
        let plan = plan_folding(
            vec![file("A.jpg", 10)],
            vec![file("a.jpg", 10)],
            NameFolding::CaseInsensitive,
            ConflictPolicy::Fail,
        )
        .unwrap();
        assert_eq!(plan.actions(), [skip("A.jpg", SkipReason::Identical)]);
        let plan = plan_folding(
            vec![dir("Photos"), file("Photos/A.jpg", 10)],
            vec![dir("photos"), file("photos/a.jpg", 11)],
            NameFolding::CaseInsensitive,
            ConflictPolicy::SourceWins,
        )
        .unwrap();
        assert_eq!(plan.actions(), [copy_differs("Photos/A.jpg", 10, 0)]);
    }

    #[test]
    fn under_folding_two_directories_merge_but_a_directory_over_a_file_blocks_its_subtree() {
        let plan = plan_folding(
            vec![
                dir("Music"),
                file("Music/a.mp3", 1),
                dir("music"),
                file("music/b.mp3", 1),
                file("X", 1),
                dir("x"),
                file("x/inner", 1),
            ],
            vec![],
            NameFolding::CaseInsensitive,
            ConflictPolicy::SourceWins,
        )
        .unwrap();
        assert_eq!(
            plan.actions(),
            [
                mkdir("Music"),
                mkdir("music"),
                copy_new("Music/a.mp3", 1, 0),
                copy_new("X", 1, 0),
                copy_new("music/b.mp3", 1, 0),
                skip("x", SkipReason::NameCollision),
                skip("x/inner", SkipReason::NameCollision),
            ]
        );
    }
}
