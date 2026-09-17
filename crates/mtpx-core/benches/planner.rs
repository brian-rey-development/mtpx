//! Planner throughput on a large tree: 100k files across 1k directories, half already on the destination.

use criterion::{Criterion, criterion_group, criterion_main};
use mtpx_core::{
    __bench::{NameFolding, Partials, plan},
    Entry, EntryKind, PathError, RelPath, Snapshot, TransferOptions,
};
use std::hint::black_box;

const DIRS: u32 = 1_000;
const FILES_PER_DIR: u32 = 100;
const FILE_SIZE: u64 = 4 * 1024 * 1024;

fn dir(index: u32) -> Result<Entry, PathError> {
    let path = RelPath::new([format!("d{index:04}")])?;
    Ok(Entry::new(path, EntryKind::Dir, 0, None))
}

fn file(dir_index: u32, file_index: u32) -> Result<Entry, PathError> {
    let path = RelPath::new([format!("d{dir_index:04}"), format!("f{file_index:03}.bin")])?;
    Ok(Entry::new(path, EntryKind::File, FILE_SIZE, None))
}

fn tree(keep_file: impl Fn(u32) -> bool) -> Result<Snapshot, PathError> {
    let mut entries = Vec::new();
    for d in 0..DIRS {
        entries.push(dir(d)?);
        for f in (0..FILES_PER_DIR).filter(|f| keep_file(*f)) {
            entries.push(file(d, f)?);
        }
    }
    Ok(Snapshot::new("/bench", entries, vec![]))
}

fn fixtures() -> Result<(Snapshot, Snapshot), PathError> {
    Ok((tree(|_| true)?, tree(|f| f % 2 == 0)?))
}

fn plan_100k_files(c: &mut Criterion) {
    let (source, dest) =
        fixtures().unwrap_or_else(|e| panic!("bench fixture paths are static: {e}"));
    let partials = Partials::new();
    let opts = TransferOptions::sync();
    c.bench_function("plan 100k files, half present", |b| {
        b.iter(|| {
            plan(
                black_box(&source),
                black_box(&dest),
                NameFolding::Exact,
                black_box(&partials),
                black_box(&opts),
            )
        });
    });
}

#[allow(missing_docs, reason = "criterion's macros emit undocumented items")]
mod harness {
    use super::{criterion_group, plan_100k_files};
    criterion_group!(benches, plan_100k_files);
}
criterion_main!(harness::benches);
