//! Assembles an [`IndexView`] from a store snapshot.
//!
//! The assembler binds every sealed segment's facts to one live generation,
//! producing the atomic view. The refresh cycle is the single writer: it
//! assembles a fresh view and publishes it into the shared `ArcSwap`, so every
//! request reads one immutable view without decoding PGM bodies. Sealed facts
//! are loaded through the persistent fact store, so a repeat assembly over an
//! unchanged snapshot serves from cache.
//!
//! A sealed segment that fails to load is skipped, and the view reports a
//! source gap rather than silently claiming completeness. Active parts are
//! folded by [`OverviewIndex::assemble_with_live`], driven from the refresh
//! cycle's semantic delta; the query path never folds.

use std::sync::Arc;

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_reader::{
    FactStore, JournalDelta, LIMIT, LiveBuilder, LiveView, LocalDirSnapshot, OpenUnit,
    PartDescriptor, PartTransition, PgmUnit, RefreshDelta, SealedLocator, SegmentContext,
    SegmentDescriptor, catalog_digest,
};

use super::view::{IndexView, SealedEntry};

/// Deployment naming-contract identity for overview facts.
///
/// The contract binds the registry/extractor version into segment identity; a
/// fixed value scopes every segment in this deployment consistently.
const OVERVIEW_NAMING_CONTRACT: NamingContractId = NamingContractId([1; 16]);

/// Fallback store namespace when the configured one is empty.
const FALLBACK_NAMESPACE: &[u8] = b"pgkronika";

/// Upper bound on a stored namespace, comfortably under the reader's limit.
const MAX_NAMESPACE_BYTES: usize = 256;

/// Owns the persistent fact store and namespace used to assemble views.
#[derive(Debug, Clone)]
pub(crate) struct OverviewIndex {
    store: FactStore,
    namespace: Arc<[u8]>,
    warming_live: Arc<LiveView>,
}

impl OverviewIndex {
    /// Roots the fact store at `cache_root` under the given store namespace.
    pub(crate) fn new(cache_root: std::path::PathBuf, namespace: Vec<u8>) -> Self {
        let namespace = sanitize_namespace(namespace);
        let warming_live = Arc::new(
            LiveBuilder::new(namespace.clone(), LIMIT)
                .expect("a sanitized namespace and constant bounds are a valid live config")
                .publish(),
        );
        Self {
            store: FactStore::new(cache_root),
            namespace: Arc::from(namespace),
            warming_live,
        }
    }

    /// Assembles the atomic view over the sealed set, with an unfolded live
    /// generation (`Empty` when the journal has no active parts, else
    /// `Warming`).
    ///
    /// This is the seed and the fallback: it never folds active parts, so it is
    /// safe to call from request handlers and from `AppState` construction.
    pub(crate) fn assemble(&self, snapshot: &LocalDirSnapshot) -> IndexView {
        let (sealed, sealed_gap, has_live) = self.load_sealed_set(snapshot);
        let live = self.unfolded_live(snapshot, has_live);
        IndexView::new(snapshot.view_generation(), sealed, live, sealed_gap)
    }

    /// Assembles the atomic view, folding the delta's active parts into the
    /// live generation.
    ///
    /// The refresh cycle owns the mutable snapshot that produces `delta` and the
    /// units it opens, so this is the only path that reaches a `Current` live
    /// view with live events. On any fold failure it falls back to the unfolded
    /// live rather than publishing a partial fold as complete.
    pub(crate) fn assemble_with_live(
        &self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
    ) -> IndexView {
        let (sealed, sealed_gap, has_live) = self.load_sealed_set(snapshot);
        let live = self
            .fold_live(snapshot, delta)
            .unwrap_or_else(|| self.unfolded_live(snapshot, has_live));
        IndexView::new(snapshot.view_generation(), sealed, live, sealed_gap)
    }

    /// Loads every sealed segment best-effort; returns the ordered entries,
    /// whether any load failed (a source gap), and whether live parts exist.
    fn load_sealed_set(&self, snapshot: &LocalDirSnapshot) -> (Vec<SealedEntry>, bool, bool) {
        let mut sealed = Vec::new();
        let mut sealed_gap = false;
        let mut has_live = false;
        for (idx, meta) in snapshot.units().iter().enumerate() {
            if meta.live {
                has_live = true;
                continue;
            }
            match self.load_sealed(snapshot, idx) {
                Some(entry) => sealed.push(entry),
                None => sealed_gap = true,
            }
        }
        sealed.sort_by(|left, right| {
            let left = left.descriptor();
            let right = right.descriptor();
            (left.min_ts, left.locator).cmp(&(right.min_ts, right.locator))
        });
        (sealed, sealed_gap, has_live)
    }

    fn load_sealed(&self, snapshot: &LocalDirSnapshot, idx: usize) -> Option<SealedEntry> {
        let catalog = snapshot.unit_catalog(idx)?;
        let digest = catalog_digest(catalog);
        let locator = SealedLocator::from_file_name_bytes(digest.as_bytes());
        let context = SegmentContext::new(
            self.namespace.to_vec(),
            OVERVIEW_NAMING_CONTRACT,
            SegmentLocator(*locator.as_bytes()),
        )
        .ok()?;
        let load = snapshot
            .load_sealed_facts(idx, &self.store, &context, &LIMIT)
            .ok()?;
        let descriptor = SegmentDescriptor::from_catalog(locator, catalog);
        Some(SealedEntry::new(descriptor, load.shared_facts()))
    }

    /// Builds an unfolded live view: `Empty` when there are no active parts,
    /// otherwise the `Warming` template.
    fn unfolded_live(&self, snapshot: &LocalDirSnapshot, has_live: bool) -> Arc<LiveView> {
        if has_live {
            return Arc::clone(&self.warming_live);
        }
        let Ok(mut builder) = LiveBuilder::new(self.namespace.to_vec(), LIMIT) else {
            return Arc::clone(&self.warming_live);
        };
        let delta = empty_delta(snapshot);
        if builder.begin_refresh(&delta).is_ok() && builder.complete_refresh().is_ok() {
            Arc::new(builder.publish())
        } else {
            Arc::clone(&self.warming_live)
        }
    }

    /// Folds the delta's current active parts into a fresh live builder.
    ///
    /// Returns `None` (so the caller falls back to the unfolded live) when the
    /// delta is not authoritative, a part cannot be opened, or a fold step
    /// rejects the part — a partial fold is never published as complete.
    fn fold_live(
        &self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
    ) -> Option<Arc<LiveView>> {
        if !delta.journal.current_parts_complete {
            return None;
        }
        let mut builder = LiveBuilder::new(self.namespace.to_vec(), LIMIT).ok()?;
        builder.begin_refresh(delta).ok()?;
        let sealed_units = snapshot.units().iter().filter(|meta| !meta.live).count();
        for (offset, part) in delta.journal.current_parts.iter().enumerate() {
            let unit_idx = sealed_units.checked_add(offset)?;
            let OpenUnit::Active(unit) = snapshot.open_unit(unit_idx).ok()? else {
                return None;
            };
            fold_one(&mut builder, part, &unit)?;
        }
        builder.complete_refresh().ok()?;
        Some(Arc::new(builder.publish()))
    }
}

fn fold_one(
    builder: &mut LiveBuilder,
    part: &PartDescriptor,
    unit: &PgmUnit<Vec<u8>>,
) -> Option<()> {
    builder.fold_part(part, unit).ok().map(|_effect| ())
}

/// Normalizes a namespace to a non-empty, bounded byte string.
fn sanitize_namespace(namespace: Vec<u8>) -> Vec<u8> {
    let mut namespace = namespace;
    if namespace.is_empty() {
        return FALLBACK_NAMESPACE.to_vec();
    }
    namespace.truncate(MAX_NAMESPACE_BYTES);
    namespace
}

/// An empty authoritative refresh for a journal with no active parts.
const fn empty_delta(snapshot: &LocalDirSnapshot) -> RefreshDelta {
    RefreshDelta {
        previous_view_generation: snapshot.view_generation(),
        new_view_generation: snapshot.view_generation(),
        view_changed: false,
        sealed_added: Vec::new(),
        sealed_removed: Vec::new(),
        journal: JournalDelta {
            bootstrap: true,
            generation_id: snapshot.journal_generation(),
            previous_valid_len: 0,
            new_valid_len: 0,
            completed_parts: Vec::new(),
            current_parts: Vec::new(),
            current_parts_complete: true,
            transition: PartTransition::Append,
            tail_pending: None,
            damages: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use crate::overview::view::SourceStatus;

    fn write_segment(dir: &std::path::Path, file: &str, min_ts: i64, max_ts: i64) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        );
        std::fs::write(dir.join(file), &bytes).expect("write segment");
    }

    #[test]
    fn an_empty_store_assembles_an_empty_current_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        let snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let index = OverviewIndex::new(cache.path().to_path_buf(), b"deployment".to_vec());
        let view = index.assemble(&snapshot);
        assert!(view.coverage_envelope().is_empty());
        assert_eq!(view.source_status(), SourceStatus::CompleteForContract);
    }

    #[test]
    fn a_sealed_segment_is_bound_into_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let index = OverviewIndex::new(cache.path().to_path_buf(), b"deployment".to_vec());
        let view = index.assemble(&snapshot);
        assert!(
            !view.coverage_envelope().is_empty(),
            "the sealed segment binds coverage into the view"
        );
    }

    #[test]
    fn repeat_assembly_is_deterministic_in_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let index = OverviewIndex::new(cache.path().to_path_buf(), b"deployment".to_vec());
        let first = index.assemble(&snapshot);
        let second = index.assemble(&snapshot);
        assert_eq!(
            first.fact_set_id(),
            second.fact_set_id(),
            "an unchanged snapshot assembles an identical fact-set id"
        );
    }

    #[test]
    fn a_namespace_is_sanitized_to_a_non_empty_bounded_string() {
        assert_eq!(
            sanitize_namespace(Vec::new()),
            FALLBACK_NAMESPACE.to_vec(),
            "an empty namespace falls back to the default"
        );
        assert_eq!(
            sanitize_namespace(vec![b'x'; MAX_NAMESPACE_BYTES * 2]).len(),
            MAX_NAMESPACE_BYTES,
            "an oversized namespace is truncated to the bound"
        );
        assert_eq!(
            sanitize_namespace(b"deployment".to_vec()),
            b"deployment".to_vec(),
            "a valid namespace passes through unchanged"
        );
    }
}
