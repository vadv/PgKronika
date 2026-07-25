//! Single-writer assembly of immutable timeline index views.
//!
//! The writer retains exact sealed descriptors and one `LiveBuilder`. Refresh
//! derives fact identities from catalog metadata, while selected request
//! leaders load bodies through bounded shared work.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_reader::{
    FactBuildKey, FactKey, FactStore, FallbackConfig, FileKind, GcConfig, GcMark, GcOutcome, LIMIT,
    LiveBuilder, LiveConfigError, LiveFoldError, LocalDirSnapshot, RefreshDelta, SealedFactError,
    SealedLocator, SegmentContext, SegmentDescriptor, SegmentFacts,
};

use super::admission::{ColdAdmissionConfig, ColdWorkWeight};
use super::loader::OverviewFactLoader;
use super::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;
use super::view::{DescriptorEntry, DescriptorView, PromotionCandidate};

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
    /// The process-wide cold-work bounds are invalid.
    ColdAdmission,
    /// The selected-segment request policy is outside the v1 range.
    SelectedSegmentLimit {
        /// Rejected configured value.
        configured: usize,
        /// Absolute v1 ceiling.
        maximum: usize,
    },
    /// The single-writer mutex was poisoned by a previous panic.
    WriterPoisoned,
}

impl std::fmt::Display for OverviewBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(f, "overview configuration: {error}"),
            Self::Live(error) => write!(f, "overview live fold: {error}"),
            Self::ActiveRead(error) => write!(f, "overview active read: {error}"),
            Self::ColdAdmission => f.write_str("overview cold admission limits are invalid"),
            Self::SelectedSegmentLimit {
                configured,
                maximum,
            } => write!(
                f,
                "overview selected-segment limit {configured} must be in 1..={maximum}"
            ),
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
            Self::ColdAdmission | Self::SelectedSegmentLimit { .. } | Self::WriterPoisoned => None,
        }
    }
}

/// Refresh passes between fact-cache garbage collections.
///
/// This is a scan interval, not retention grace. Deletion requires absence in
/// the configured number of distinct authoritative GC scans plus wall grace.
const GC_INTERVAL_PASSES: u64 = 60;

/// The only mutable owner of sealed facts and live fold state.
#[derive(Debug)]
pub(crate) struct OverviewWriter {
    store: FactStore,
    namespace: Vec<u8>,
    sealed: BTreeMap<SealedLocator, DescriptorEntry>,
    unavailable: BTreeMap<SealedLocator, SegmentDescriptor>,
    live: LiveBuilder,
    passes_since_gc: u64,
    view_generation: u64,
}

impl OverviewWriter {
    /// Builds a writer with explicit durable and fallback storage policy.
    #[cfg(test)]
    pub(crate) fn new(
        cache_root: PathBuf,
        namespace: Vec<u8>,
        fallback: FallbackConfig,
    ) -> Result<Self, OverviewBuildError> {
        Self::with_gc_config(cache_root, namespace, fallback, GcConfig::default())
    }

    /// Builds a writer with explicit fallback and durable GC/quota policies.
    pub(crate) fn with_gc_config(
        cache_root: PathBuf,
        namespace: Vec<u8>,
        fallback: FallbackConfig,
        gc: GcConfig,
    ) -> Result<Self, OverviewBuildError> {
        let live =
            LiveBuilder::new(namespace.clone(), LIMIT).map_err(OverviewBuildError::Config)?;
        Ok(Self {
            store: FactStore::with_configs(cache_root, fallback, gc),
            namespace,
            sealed: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            live,
            passes_since_gc: 0,
            view_generation: 0,
        })
    }

    /// The persistent-cache write mode and backoff diagnostics.
    pub(crate) fn persist_mode(&self) -> kronika_reader::PersistModeSnapshot {
        self.store.persist_mode()
    }

    /// Runs the single due recovery probe independently of fact construction.
    pub(crate) fn probe_persistence(&self) -> kronika_reader::PersistenceProbeOutcome {
        super::resilience::run_due_probe(&self.store)
    }

    pub(crate) fn fact_loader(
        &self,
        config: ColdAdmissionConfig,
    ) -> Result<OverviewFactLoader, OverviewBuildError> {
        OverviewFactLoader::new(self.store.clone(), self.namespace.clone(), config)
            .map_err(|_error| OverviewBuildError::ColdAdmission)
    }

    /// Requests a bounded GC scan at most once per [`GC_INTERVAL_PASSES`]
    /// successful refreshes.
    ///
    /// The mark contains every retained sealed build identity. Any unavailable
    /// sealed source makes the mark non-authoritative, which forbids both
    /// grace advancement and deletion.
    pub(crate) fn collect_fact_garbage(&mut self) -> Option<GcOutcome> {
        self.passes_since_gc = self.passes_since_gc.saturating_add(1);
        if self.passes_since_gc < GC_INTERVAL_PASSES {
            return None;
        }
        self.passes_since_gc = 0;
        let mark = if self.unavailable.is_empty() {
            GcMark::authoritative(
                self.view_generation,
                self.sealed.values().map(DescriptorEntry::fact_build_key),
            )
        } else {
            GcMark::unavailable(self.view_generation)
        };
        Some(self.store.collect_garbage(&mark))
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
    ) -> Result<DescriptorView, OverviewBuildError> {
        let prior_live = Arc::new(self.live.publish());
        let mut sealed = self.sealed.clone();
        let mut unavailable = self.unavailable.clone();
        self.refresh_sealed(snapshot, &mut sealed, &mut unavailable);

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
        self.view_generation = snapshot.view_generation();
        let promotion_locators = delta
            .sealed_added
            .iter()
            .map(|descriptor| descriptor.locator)
            .filter(|locator| self.sealed.contains_key(locator))
            .take(ABSOLUTE_MAX_SELECTED_SEGMENTS)
            .collect::<BTreeSet<_>>();
        let promotion = PromotionCandidate::new(prior_live, promotion_locators);
        Ok(self.current_view(self.view_generation, promotion))
    }

    /// Seeds an empty writer from a snapshot bootstrap delta.
    pub(crate) fn assemble(
        &mut self,
        snapshot: &LocalDirSnapshot,
        delta: &RefreshDelta,
    ) -> Result<DescriptorView, OverviewBuildError> {
        self.assemble_with_live(snapshot, delta)
    }

    fn refresh_sealed(
        &self,
        snapshot: &LocalDirSnapshot,
        sealed: &mut BTreeMap<SealedLocator, DescriptorEntry>,
        unavailable: &mut BTreeMap<SealedLocator, SegmentDescriptor>,
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
        unavailable.retain(|locator, descriptor| baseline.get(locator) == Some(descriptor));
        for descriptor in baseline.values() {
            if sealed.contains_key(&descriptor.locator) {
                unavailable.remove(&descriptor.locator);
                continue;
            }
            match self.describe_sealed(snapshot, descriptor) {
                Ok(entry) => {
                    sealed.insert(descriptor.locator, entry);
                    unavailable.remove(&descriptor.locator);
                }
                Err(_error) => {
                    metrics::counter!("kronika_web_overview_sealed_failures_total").increment(1);
                    unavailable.insert(descriptor.locator, *descriptor);
                }
            }
        }
    }

    fn describe_sealed(
        &self,
        snapshot: &LocalDirSnapshot,
        descriptor: &SegmentDescriptor,
    ) -> Result<DescriptorEntry, SealedFactError> {
        let context = self.context(descriptor)?;
        let unit = snapshot.open_sealed_by_descriptor(descriptor)?;
        let (identity, lineage) =
            SegmentFacts::provenance(&unit, &context).map_err(SealedFactError::Build)?;
        let fact_key = FactKey::for_identity(&identity, FileKind::SegmentFacts);
        Ok(DescriptorEntry::new(
            *descriptor,
            FactBuildKey::new(fact_key, lineage.id()),
            ColdWorkWeight::for_unit(&unit),
            identity.source_scope_id,
        ))
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

    fn current_view(
        &self,
        view_generation: u64,
        promotion: Option<PromotionCandidate>,
    ) -> DescriptorView {
        DescriptorView::new(
            view_generation,
            self.sealed.values().cloned().collect(),
            self.unavailable.values().copied().collect(),
            Arc::new(self.live.publish()),
            promotion,
        )
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
    use std::time::Duration;

    use kronika_analytics::overview::{CountLimits, CoverageSpan, OracleLimits, RawOracle};
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

    use crate::overview::selection::SelectedSealedPlan;
    use crate::overview::view::{IndexView, SourceStatus};

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

    async fn load_selected(
        writer: &OverviewIndex,
        snapshot: &LocalDirSnapshot,
        descriptors: DescriptorView,
        sources: &[u64],
        range: CoverageSpan,
    ) -> Arc<IndexView> {
        let plan = SelectedSealedPlan::build(
            Arc::new(descriptors),
            sources,
            range,
            ABSOLUTE_MAX_SELECTED_SEGMENTS,
        )
        .expect("selected plan");
        writer
            .fact_loader(ColdAdmissionConfig::default())
            .expect("fact loader")
            .load_selected(Arc::new(snapshot.clone()), &plan)
            .await
            .expect("loaded selected view")
    }

    #[tokio::test]
    async fn an_empty_store_assembles_an_empty_current_view() {
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
        let descriptors = index.assemble(&snapshot, &delta).expect("view");
        let view = load_selected(
            &index,
            &snapshot,
            descriptors,
            &[7],
            CoverageSpan::new(0, 1).expect("range"),
        )
        .await;
        assert!(view.coverage_envelope().is_empty());
        assert_eq!(view.source_status(), SourceStatus::CompleteForContract);
    }

    #[tokio::test]
    async fn a_sealed_segment_is_loaded_only_after_its_plan_is_admitted() {
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
        let descriptors = index.assemble(&snapshot, &delta).expect("view");
        assert_eq!(
            ovf_files(cache.path()),
            0,
            "descriptor publication does not build a fact file"
        );
        let view = load_selected(
            &index,
            &snapshot,
            descriptors,
            &[7],
            CoverageSpan::new(0, 10_000).expect("range"),
        )
        .await;
        assert!(
            !view.coverage_envelope().is_empty(),
            "the admitted sealed segment binds coverage into the query view"
        );
        assert_eq!(ovf_files(cache.path()), 1);
    }

    fn ovf_files(cache_root: &std::path::Path) -> usize {
        fn walk(dir: &std::path::Path, count: &mut usize) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, count);
                } else if path.extension().is_some_and(|ext| ext == "ovf") {
                    *count += 1;
                }
            }
        }
        let mut count = 0;
        walk(cache_root, &mut count);
        count
    }

    #[tokio::test]
    async fn gc_reclaims_the_fact_file_of_a_dropped_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("cache dir");
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::with_gc_config(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
            GcConfig::new(1_000, 2, Duration::ZERO, Duration::ZERO, None, None)
                .expect("test GC policy"),
        )
        .expect("writer");
        let descriptors = index.assemble(&snapshot, &delta).expect("view");
        drop(
            load_selected(
                &index,
                &snapshot,
                descriptors,
                &[7],
                CoverageSpan::new(0, 10_000).expect("range"),
            )
            .await,
        );
        assert_eq!(
            ovf_files(cache.path()),
            1,
            "the sealed segment published a fact file"
        );

        // While the segment is live its fact file survives GC: the live-set
        // identity must equal what publication wrote.
        for _ in 0..GC_INTERVAL_PASSES {
            let _ = index.collect_fact_garbage();
        }
        assert_eq!(
            ovf_files(cache.path()),
            1,
            "a live segment's fact file survives GC"
        );

        // The segment disappears from the source; the view drops it but the
        // fact file lingers until two GC scans carrying different successful
        // source-view generations have both marked it absent.
        std::fs::remove_file(dir.path().join("143000.pgm")).expect("drop segment");
        let drop_delta = snapshot.refresh_incremental_delta().expect("drop delta");
        index
            .assemble_with_live(&snapshot, &drop_delta)
            .expect("view without the segment");

        let mut first_absence = None;
        for _ in 0..GC_INTERVAL_PASSES {
            first_absence = index.collect_fact_garbage().or(first_absence);
        }
        assert_eq!(
            first_absence.expect("first absence scan").deleted,
            0,
            "one authoritative absence scan cannot delete"
        );
        write_segment(dir.path(), "143001.pgm", 3_000, 4_000);
        let next_delta = snapshot
            .refresh_incremental_delta()
            .expect("advance source view with a distinct retained set");
        let next_view = index
            .assemble_with_live(&snapshot, &next_delta)
            .expect("next successful view");
        drop(
            load_selected(
                &index,
                &snapshot,
                next_view,
                &[7],
                CoverageSpan::new(0, 10_000).expect("range"),
            )
            .await,
        );
        let mut second_absence = None;
        for _ in 0..GC_INTERVAL_PASSES {
            second_absence = index.collect_fact_garbage().or(second_absence);
        }
        let outcome = second_absence.expect("second absence scan");
        assert_eq!(
            outcome.deleted, 1,
            "the dropped segment's fact file is unlinked"
        );
        assert_eq!(
            ovf_files(cache.path()),
            1,
            "only the newly added live segment's fact file remains"
        );
    }

    #[test]
    fn gc_runs_only_on_the_interval_boundary() {
        let cache = tempfile::tempdir().expect("cache dir");
        let mut index = OverviewIndex::new(
            cache.path().to_path_buf(),
            b"deployment".to_vec(),
            FallbackConfig::default(),
        )
        .expect("writer");
        for _ in 0..(GC_INTERVAL_PASSES - 1) {
            assert!(
                index.collect_fact_garbage().is_none(),
                "no GC before the boundary"
            );
        }
        assert!(
            index.collect_fact_garbage().is_some(),
            "GC runs on the boundary pass"
        );
    }

    #[test]
    fn repeat_assembly_is_deterministic_in_plan_identity() {
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
        let range = CoverageSpan::new(0, 10_000).expect("range");
        let first =
            SelectedSealedPlan::build(Arc::new(first), &[7], range, ABSOLUTE_MAX_SELECTED_SEGMENTS)
                .expect("first plan");
        let second = SelectedSealedPlan::build(
            Arc::new(second),
            &[7],
            range,
            ABSOLUTE_MAX_SELECTED_SEGMENTS,
        )
        .expect("second plan");
        assert_eq!(first.fact_set_id(), second.fact_set_id());
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

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the test verifies one append-to-seal transition across three publications"
    )]
    async fn append_then_seal_keeps_one_coherent_event_set() {
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
        let first_descriptors = writer
            .assemble(&snapshot, &bootstrap)
            .expect("first live view");
        let range = CoverageSpan::new(0, 10_000).expect("range");
        let first_view = load_selected(&writer, &snapshot, first_descriptors, &[7], range).await;
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
        let second_descriptors = writer
            .assemble_with_live(&snapshot, &appended)
            .expect("appended live view");
        let second_view = load_selected(&writer, &snapshot, second_descriptors, &[7], range).await;
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
        let sealed_descriptors = writer
            .assemble_with_live(&snapshot, &sealed_delta)
            .expect("sealed view");
        assert!(
            sealed_descriptors
                .promotion_for(sealed_delta.sealed_added[0].locator)
                .is_some(),
            "the seal publication retains one bounded prior-live candidate"
        );
        let sealed_view = load_selected(&writer, &snapshot, sealed_descriptors, &[7], range).await;
        assert_eq!(
            sealed_view
                .query(range, QUERY_LIMITS)
                .expect("sealed query")
                .observations()
                .len(),
            2,
            "seal reconciliation neither drops nor duplicates live observations"
        );
        assert_eq!(
            ovf_files(cache.path()),
            1,
            "the reconciled sealed facts are durably published"
        );
    }
}
