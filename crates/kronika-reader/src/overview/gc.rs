//! Retention-safe garbage collection of committed fact files.
//!
//! A sealed segment that leaves the source snapshot also leaves its `.ovf`
//! fact file behind. The refresh loop drops the segment from the published
//! view but never reclaims the disk. This walks the cache tree and unlinks
//! fact files whose segment is no longer live, after a grace window so a
//! segment that briefly disappears and returns is not rebuilt needlessly.
//!
//! GC runs on the single refresh thread that owns publication, so unlink
//! never races a concurrent writer of the same key.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Consecutive GC passes a non-live fact file is kept before unlinking.
const GRACE_GENERATIONS: u32 = 2;

/// Work performed by one GC pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcOutcome {
    /// Fact files seen on disk.
    pub scanned: u64,
    /// Fact files still backing a live segment.
    pub live: u64,
    /// Non-live fact files held inside the grace window.
    pub pending: u64,
    /// Fact files unlinked this pass.
    pub deleted: u64,
    /// Bytes reclaimed by unlinks this pass.
    pub freed_bytes: u64,
}

/// Walks `<cache_root>/overview/v1/*/*/*.ovf`, holding non-live files for
/// [`GRACE_GENERATIONS`] passes before unlinking them.
///
/// `live` holds the absolute paths of fact files that still back a segment
/// in the current snapshot. `pending` carries the per-file miss counter
/// across passes; entries for files that no longer exist are dropped so it
/// cannot grow without bound.
pub(super) fn collect(
    cache_root: &Path,
    live: &HashSet<PathBuf>,
    pending: &mut HashMap<PathBuf, u32>,
) -> GcOutcome {
    let mut outcome = GcOutcome::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for path in fact_files(cache_root) {
        outcome.scanned += 1;
        seen.insert(path.clone());
        if live.contains(&path) {
            outcome.live += 1;
            pending.remove(&path);
            continue;
        }
        let misses = pending.entry(path.clone()).or_insert(0);
        *misses += 1;
        if *misses >= GRACE_GENERATIONS {
            let freed = std::fs::metadata(&path).map_or(0, |meta| meta.len());
            if std::fs::remove_file(&path).is_ok() {
                outcome.deleted += 1;
                outcome.freed_bytes += freed;
                pending.remove(&path);
            }
        } else {
            outcome.pending += 1;
        }
    }

    pending.retain(|path, _| seen.contains(path));
    outcome
}

/// Every `.ovf` under `<cache_root>/overview/v1/<scope>/<prefix>/`.
///
/// A missing or partially built tree yields no paths rather than an error:
/// GC is best effort and a cache root that is not yet populated is normal.
fn fact_files(cache_root: &Path) -> Vec<PathBuf> {
    let root = cache_root.join("overview").join("v1");
    let mut files = Vec::new();
    for scope in child_dirs(&root) {
        for prefix in child_dirs(&scope) {
            let Ok(entries) = std::fs::read_dir(&prefix) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "ovf") && path.is_file() {
                    files.push(path);
                }
            }
        }
    }
    files
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, scope: &str, name: &str, bytes: usize) -> PathBuf {
        let dir = root
            .join("overview")
            .join("v1")
            .join(scope)
            .join(name.split_at(2).0);
        std::fs::create_dir_all(&dir).expect("create prefix dir");
        let path = dir.join(format!("{name}.ovf"));
        std::fs::write(&path, vec![0_u8; bytes]).expect("write fact file");
        path
    }

    #[test]
    fn missing_cache_root_scans_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut pending = HashMap::new();
        let outcome = collect(root.path(), &HashSet::new(), &mut pending);
        assert_eq!(outcome, GcOutcome::default(), "an empty tree does no work");
    }

    #[test]
    fn live_file_is_kept_and_never_pends() {
        let root = tempfile::tempdir().expect("tempdir");
        let live_path = touch(root.path(), "aa", "abcd-1111", 10);
        let live: HashSet<PathBuf> = std::iter::once(live_path.clone()).collect();
        let mut pending = HashMap::new();
        for _ in 0..5 {
            let outcome = collect(root.path(), &live, &mut pending);
            assert_eq!(outcome.live, 1, "the live file is counted live");
            assert_eq!(outcome.deleted, 0, "a live file is never deleted");
        }
        assert!(live_path.exists(), "the live file survives every pass");
        assert!(pending.is_empty(), "a live file leaves no pending state");
    }

    #[test]
    fn non_live_file_is_deleted_after_the_grace_window() {
        let root = tempfile::tempdir().expect("tempdir");
        let stale = touch(root.path(), "aa", "dead-2222", 40);
        let live = HashSet::new();
        let mut pending = HashMap::new();

        let first = collect(root.path(), &live, &mut pending);
        assert_eq!(first.pending, 1, "first miss holds the file in grace");
        assert_eq!(first.deleted, 0, "first miss does not delete");
        assert!(stale.exists(), "the file survives the first pass");

        let second = collect(root.path(), &live, &mut pending);
        assert_eq!(second.deleted, 1, "the second miss unlinks");
        assert_eq!(second.freed_bytes, 40, "reclaimed bytes are reported");
        assert!(!stale.exists(), "the stale file is gone");
        assert!(pending.is_empty(), "a deleted file leaves no pending state");
    }

    #[test]
    fn a_returning_segment_resets_its_grace() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = touch(root.path(), "aa", "back-3333", 10);
        let mut pending = HashMap::new();

        collect(root.path(), &HashSet::new(), &mut pending);
        assert_eq!(pending.get(&path), Some(&1), "one miss recorded");

        let live: HashSet<PathBuf> = std::iter::once(path.clone()).collect();
        collect(root.path(), &live, &mut pending);
        assert!(pending.is_empty(), "returning live resets the counter");
        assert!(path.exists(), "the file that came back is kept");
    }

    #[test]
    fn pending_does_not_leak_for_vanished_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = touch(root.path(), "aa", "gone-4444", 10);
        let mut pending = HashMap::new();
        collect(root.path(), &HashSet::new(), &mut pending);
        assert_eq!(pending.len(), 1, "the miss is tracked");

        std::fs::remove_file(&path).expect("external unlink");
        let outcome = collect(root.path(), &HashSet::new(), &mut pending);
        assert_eq!(outcome.scanned, 0, "nothing left to scan");
        assert!(pending.is_empty(), "pending drops the vanished path");
    }
}
