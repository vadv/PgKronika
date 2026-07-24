//! Single-writer assembly of immutable timeline index views.
//!
//! The writer retains admitted sealed facts and one `LiveBuilder`. Refresh work
//! is proportional to semantic deltas: unchanged sealed files are not reopened,
//! and only newly completed journal parts are folded. Requests receive immutable
//! `IndexView` values and never perform PGM extraction.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_reader::{
    FactLoad, FactOrigin, FactStore, FallbackConfig, LIMIT, LiveBuilder, LiveConfigError,
    LiveFoldError, LiveView, LocalDirSnapshot, PersistError, RefreshDelta, SealOutcome,
    SealedFactError, SealedLocator, SegmentContext, SegmentDescriptor, reconcile_seal,
};

use super::view::{IndexView, SealedEntry};

/// Deployment naming-contract identity for overview facts.
///
/// The contract binds the registry/extractor version into segment identity; a
/// fixed value scopes every segment in this deployment consistently.
const OVERVIEW_NAMING_CONTRACT: NamingContractId = NamingContractId([1; 16]);

/// One refresh failure that prevents a coherent timeline publication.
#[derive(Debug)]
pub enum OverviewBuildError {
    /// The configured namespace or live bounds are invalid.
    Config(LiveConfigError),
    /// A completed active part could not be opened or folded.
    Live(LiveFoldError),
    /// A completed active part could not be reopened from the pinned snapshot.
    ActiveRead(kronika_reader::ReadError),
    /// The single-writer mutex was poisoned by a previous panic.
    WriterPoisoned,
}

impl std::fmt::Display for OverviewBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(f, "overview configuration: {error}"),
            Self::Live(error) => write!(f, "overview live fold: {error}"),
            Self::ActiveRead(error) => write!(f, "overview active read: {error}"),
            Self::WriterPoisoned => f.write_str("overview writer lock is poisoned"),
        }
    }
}

impl std::error::Error for OverviewBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Live(error) => Some(error),
            Self::ActiveRead(error) => Some(error),
            Self::WriterPoisoned => None,
        }
    }
}

/// Cumulative single-writer load diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OverviewDiagnostics {
    pub(crate) durable_hits: u64,
    pub(crate) fallback_hits: u64,
    pub(crate) rebuilt: u64,
    pub(crate) promotions: u64,
    pub(crate) persistence_failures: u64,
    pub(crate) sealed_failures: u64,
}

impl OverviewDiagnostics {
    const fn record_load(&mut self, load: &FactLoad) {
        match load.origin() {
            FactOrigin::CacheHit => self.durable_hits = self.durable_hits.saturating_add(1),
            FactOrigin::FallbackHit => self.fallback_hits = self.fallback_hits.saturating_add(1),
            FactOrigin::Rebuilt => self.rebuilt = self.rebuilt.saturating_add(1),
        }
        if load.persist_error().is_some() {
            self.persistence_failures = self.persistence_failures.saturating_add(1);
        }
    }

    const fn record_persist_error(&mut self, error: Option<PersistError>) {
        if error.is_some() {
            self.persistence_failures = self.persistence_failures.saturating_add(1);
        }
    }
}

/// The only mutable owner of sealed facts and live fold state.
#[derive(Debug)]
pub(crate) struct OverviewWriter {
    store: FactStore,
    namespace: Vec<u8>,
    sealed: BTreeMap<SealedLocator, SealedEntry>,
    unavailable: BTreeSet<SealedLocator>,
    live: LiveBuilder,
    diagnostics: OverviewDiagnostics,
}

impl OverviewWriter {
    /// Builds a writer with explicit durable and fallback storage policy.
    pub(crate) fn new(
        cache_root: std::path::PathBuf,
        namespace: Vec<u8>,
        fallback: FallbackConfig,
    ) -> Result<Self, OverviewBuildError> {
        let live =
            LiveBuilder::new(namespace.clone(), LIMIT).map_err(OverviewBuildError::Config)?;
        Ok(Self {
            store: FactStore::with_fallback_config(cache_root, fallback),
            namespace,
            sealed: BTreeMap::new(),
            unavailable: BTreeSet::new(),
            live,
            diagnostics: OverviewDiagnostics::default(),
        })
    }

    /// Applies one reader delta and returns the next immutable view.
    ///
    /// Sealed state and the live builder are committed only after the live
    /// boundary completes. A failed build leaves this writer at its last
    /// publishable state, so the refresh owner can retain the previous view.
    pub(crate) fn assemble_with_live(
        &mut self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
    ) -> Result<IndexView, OverviewBuildError> {
        let mut sealed = self.sealed.clone();
        let mut unavailable = self.unavailable.clone();
        let mut diagnostics = self.diagnostics;
        self.refresh_sealed(
            snapshot,
            delta,
            &mut sealed,
            &mut unavailable,
            &mut diagnostics,
        );

        let mut live = self.live.clone();
        if let Err(first_error) = fold_refresh(&mut live, snapshot, delta) {
            let mut rebuilt = LiveBuilder::new(self.namespace.clone(), LIMIT)
                .map_err(OverviewBuildError::Config)?;
            let baseline = full_live_baseline(delta);
            fold_refresh(&mut rebuilt, snapshot, &baseline).map_err(|_error| first_error)?;
            live = rebuilt;
        }

        self.sealed = sealed;
        self.unavailable = unavailable;
        self.live = live;
        self.diagnostics = diagnostics;
        Ok(self.current_view(snapshot.view_generation()))
    }

    /// Seeds an empty writer from a snapshot bootstrap delta.
    pub(crate) fn assemble(
        &mut self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
    ) -> Result<IndexView, OverviewBuildError> {
        self.assemble_with_live(snapshot, delta)
    }

    fn refresh_sealed(
        &self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
        sealed: &mut BTreeMap<SealedLocator, SealedEntry>,
        unavailable: &mut BTreeSet<SealedLocator>,
        diagnostics: &mut OverviewDiagnostics,
    ) {
        let baseline = snapshot
            .sealed_descriptors()
            .iter()
            .map(|descriptor| (descriptor.locator, *descriptor))
            .collect::<BTreeMap<_, _>>();
        sealed.retain(|locator, entry| {
            baseline
                .get(locator)
                .is_some_and(|descriptor| descriptor == entry.descriptor())
        });
        unavailable.retain(|locator| baseline.contains_key(locator));

        let prior_live = self.live.publish();
        let added = delta
            .sealed_added
            .iter()
            .map(|descriptor| descriptor.locator)
            .collect::<BTreeSet<_>>();
        for descriptor in baseline.values() {
            if sealed.contains_key(&descriptor.locator) {
                unavailable.remove(&descriptor.locator);
                continue;
            }
            let result = if added.contains(&descriptor.locator) {
                self.reconcile_added(snapshot, descriptor, &prior_live, diagnostics)
            } else {
                self.load_sealed(snapshot, descriptor, diagnostics)
            };
            match result {
                Ok(entry) => {
                    sealed.insert(descriptor.locator, entry);
                    unavailable.remove(&descriptor.locator);
                }
                Err(_error) => {
                    diagnostics.sealed_failures = diagnostics.sealed_failures.saturating_add(1);
                    unavailable.insert(descriptor.locator);
                }
            }
        }
    }

    fn load_sealed(
        &self,
        snapshot: &LocalDirSnapshot,
        descriptor: &SegmentDescriptor,
        diagnostics: &mut OverviewDiagnostics,
    ) -> Result<SealedEntry, SealedFactError> {
        let context = self.context(descriptor)?;
        let load =
            snapshot.load_sealed_facts_by_descriptor(descriptor, &self.store, &context, &LIMIT)?;
        diagnostics.record_load(&load);
        Ok(SealedEntry::new(*descriptor, load.into_shared_facts()))
    }

    fn reconcile_added(
        &self,
        snapshot: &LocalDirSnapshot,
        descriptor: &SegmentDescriptor,
        prior_live: &LiveView,
        diagnostics: &mut OverviewDiagnostics,
    ) -> Result<SealedEntry, SealedFactError> {
        let context = self.context(descriptor)?;
        let unit = snapshot.open_sealed_by_descriptor(descriptor)?;
        let outcome = reconcile_seal(prior_live, &unit, &context, &self.store, &LIMIT)
            .map_err(SealedFactError::Build)?;
        let facts = match outcome {
            SealOutcome::Promoted {
                facts,
                persist_error,
            } => {
                diagnostics.promotions = diagnostics.promotions.saturating_add(1);
                diagnostics.record_persist_error(persist_error);
                facts
            }
            SealOutcome::Rebuilt(load) => {
                diagnostics.record_load(&load);
                load.into_shared_facts()
            }
        };
        Ok(SealedEntry::new(*descriptor, facts))
    }

    fn context(&self, descriptor: &SegmentDescriptor) -> Result<SegmentContext, SealedFactError> {
        SegmentContext::new(
            self.namespace.clone(),
            OVERVIEW_NAMING_CONTRACT,
            SegmentLocator(*descriptor.locator.as_bytes()),
        )
        .map_err(|_error| SealedFactError::ContextLocatorMismatch {
            locator: descriptor.locator,
        })
    }

    fn current_view(&self, view_generation: u64) -> IndexView {
        let mut sealed = self.sealed.values().cloned().collect::<Vec<_>>();
        sealed.sort_by_key(|entry| {
            let descriptor = entry.descriptor();
            (descriptor.min_ts, descriptor.locator)
        });
        IndexView::new(
            view_generation,
            sealed,
            Arc::new(self.live.publish()),
            !self.unavailable.is_empty(),
        )
    }

    pub(crate) const fn diagnostics(&self) -> OverviewDiagnostics {
        self.diagnostics
    }
}

fn fold_refresh(
    builder: &mut LiveBuilder,
    snapshot: &LocalDirSnapshot,
    delta: &RefreshDelta,
) -> Result<(), OverviewBuildError> {
    builder
        .begin_refresh(delta)
        .map_err(OverviewBuildError::Live)?;
    for part in &delta.journal.completed_parts {
        let unit = snapshot
            .open_active_part(part)
            .map_err(OverviewBuildError::ActiveRead)?;
        builder
            .fold_part(part, &unit)
            .map_err(OverviewBuildError::Live)?;
    }
    builder.complete_refresh().map_err(OverviewBuildError::Live)
}

fn full_live_baseline(delta: &RefreshDelta) -> RefreshDelta {
    let mut baseline = delta.clone();
    baseline.journal.bootstrap = true;
    baseline.journal.completed_parts = baseline.journal.current_parts.clone();
    baseline
}

/// Compatibility name retained for internal callers while the writer role is
/// made explicit in state ownership.
pub(crate) type OverviewIndex = OverviewWriter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    use kronika_analytics::overview::{CountLimits, CoverageSpan, OracleLimits, RawOracle};
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

    use crate::overview::view::SourceStatus;

    const QUERY_LIMITS: OracleLimits = OracleLimits {
        max_observations: 32,
        max_coverage_spans: 32,
        count_limits: CountLimits {
            max_input_entries: 32,
            max_joint_keys: 32,
            max_signal_keys: 32,
        },
    };

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

    fn lifecycle_part(rows: &[PgLogLifecycleV1]) -> Vec<u8> {
        let min_ts = rows
            .iter()
            .map(|row| row.ts.0)
            .min()
            .expect("non-empty part");
        let max_ts = rows
            .iter()
            .map(|row| row.ts.0)
            .max()
            .expect("non-empty part");
        let body = PgLogLifecycleV1::encode(rows).expect("encode lifecycle");
        build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: u32::try_from(rows.len()).expect("row count"),
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    fn lifecycle_row(ts: i64, pid: i32) -> PgLogLifecycleV1 {
        PgLogLifecycleV1 {
            ts: Ts(ts),
            kind: 0,
            pid: Some(pid),
            signal: Some(9),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        }
    }

    fn framed(part: &[u8]) -> Vec<u8> {
        let mut bytes = FrameHeader {
            part_len: u64::try_from(part.len()).expect("part length"),
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(part);
        bytes
    }

    #[test]
    fn an_empty_store_assembles_an_empty_current_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
        )
        .expect("writer");
        let view = index.assemble(&snapshot, &delta).expect("view");
        assert!(view.coverage_envelope().is_empty());
        assert_eq!(view.source_status(), SourceStatus::CompleteForContract);
    }

    #[test]
    fn a_sealed_segment_is_bound_into_the_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
        )
        .expect("writer");
        let view = index.assemble(&snapshot, &delta).expect("view");
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
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let first_delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
        )
        .expect("writer");
        let first = index.assemble(&snapshot, &first_delta).expect("first view");
        let second_delta = snapshot
            .refresh_incremental_delta()
            .expect("unchanged delta");
        let second = index
            .assemble_with_live(&snapshot, &second_delta)
            .expect("second view");
        assert_eq!(
            first.fact_set_id(),
            second.fact_set_id(),
            "an unchanged snapshot assembles an identical fact-set id"
        );
    }

    #[test]
    fn an_invalid_namespace_fails_instead_of_aliasing() {
        let cache = tempfile::tempdir().expect("cache dir");
        assert!(matches!(
            OverviewIndex::new(
                cache.path().to_path_buf(),
                Vec::new(),
                FallbackConfig::default()
            ),
            Err(OverviewBuildError::Config(
                LiveConfigError::EmptyStoreNamespace
            ))
        ));
        assert!(matches!(
            OverviewIndex::new(
                cache.path().to_path_buf(),
                vec![b'x'; 4097],
                FallbackConfig::default()
            ),
            Err(OverviewBuildError::Config(
                LiveConfigError::StoreNamespaceTooLong
            ))
        ));
    }

    #[test]
    fn append_then_seal_keeps_one_coherent_event_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        let first = lifecycle_row(1_500, 41);
        let second = lifecycle_row(2_500, 42);
        let first_part = lifecycle_part(std::slice::from_ref(&first));
        std::fs::write(dir.path().join("active.parts"), framed(&first_part))
            .expect("write first frame");

        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let bootstrap = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut writer = OverviewIndex::new(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
        )
        .expect("writer");
        let first_view = writer
            .assemble(&snapshot, &bootstrap)
            .expect("first live view");
        let range = CoverageSpan::new(0, 10_000).expect("range");
        assert_eq!(
            first_view
                .query(range, QUERY_LIMITS)
                .expect("first query")
                .observations()
                .len(),
            1
        );

        let second_part = lifecycle_part(std::slice::from_ref(&second));
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("active.parts"))
            .expect("open journal")
            .write_all(&framed(&second_part))
            .expect("append second frame");
        let appended = snapshot.refresh_incremental_delta().expect("append delta");
        assert_eq!(appended.journal.completed_parts.len(), 1);
        let second_view = writer
            .assemble_with_live(&snapshot, &appended)
            .expect("appended live view");
        assert_eq!(
            second_view
                .query(range, QUERY_LIMITS)
                .expect("second query")
                .observations()
                .len(),
            2
        );

        let first_body =
            PgLogLifecycleV1::encode(std::slice::from_ref(&first)).expect("first body");
        let second_body =
            PgLogLifecycleV1::encode(std::slice::from_ref(&second)).expect("second body");
        let sealed = build_part(
            &[
                SectionInput {
                    type_id: 1_028_001,
                    rows: 1,
                    body: &first_body,
                },
                SectionInput {
                    type_id: 1_028_001,
                    rows: 1,
                    body: &second_body,
                },
            ],
            PartMeta {
                min_ts: first.ts.0,
                max_ts: second.ts.0,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("1000.pgm"), sealed).expect("write sealed segment");
        std::fs::write(dir.path().join("active.parts"), []).expect("reset journal");
        let sealed_delta = snapshot.refresh_incremental_delta().expect("seal delta");
        assert_eq!(sealed_delta.sealed_added.len(), 1);
        let sealed_view = writer
            .assemble_with_live(&snapshot, &sealed_delta)
            .expect("sealed view");
        assert_eq!(
            sealed_view
                .query(range, QUERY_LIMITS)
                .expect("sealed query")
                .observations()
                .len(),
            2,
            "seal reconciliation neither drops nor duplicates live observations"
        );
        assert_eq!(writer.diagnostics().promotions, 1);
    }
}
