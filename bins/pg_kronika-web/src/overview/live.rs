//! Single-writer assembly of immutable timeline index views.
//!
//! The writer retains exact sealed descriptors and one `LiveBuilder`. Refresh
//! derives fact identities from catalog metadata, while selected request
//! leaders load bodies through bounded shared work.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kronika_reader::{
    FactBuildKey, FactKey, FactStore, FallbackConfig, FileKind, FoldEffect, GcConfig, GcMark,
    GcOutcome, LIMIT, LiveBuilder, LiveConfigError, LiveFoldError, LiveState, LocalDirSnapshot,
    RefreshDelta, SealedFactError, SealedLocator, SegmentDescriptor, SegmentFacts,
};

use super::admission::{ColdAdmissionConfig, ColdWorkWeight};
use super::loader::OverviewFactLoader;
use super::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;
use super::view::{DescriptorEntry, DescriptorView, PromotionCandidate};
use crate::OverviewColdConfig;

/// One refresh failure that prevents a coherent timeline publication.
#[derive(Debug)]
pub enum OverviewBuildError {
    /// The configured live bounds are invalid.
    Config(LiveConfigError),
    /// A completed active part could not be opened or folded.
    Live(LiveFoldError),
    /// A completed active part could not be reopened from the pinned snapshot.
    ActiveRead(kronika_reader::ReadError),
    /// The process-wide cold-work bounds are invalid.
    ColdAdmission,
    /// The bounded source-scrub cadence is zero.
    SourceScrubInterval,
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
            Self::SourceScrubInterval => {
                f.write_str("overview source scrub interval must be non-zero")
            }
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
            Self::ColdAdmission
            | Self::SourceScrubInterval
            | Self::SelectedSegmentLimit { .. }
            | Self::WriterPoisoned => None,
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
    sealed: BTreeMap<SealedLocator, DescriptorEntry>,
    unavailable: BTreeMap<SealedLocator, SegmentDescriptor>,
    scrub_damaged: BTreeMap<SealedLocator, SegmentDescriptor>,
    scrub_cursor: Option<(SealedLocator, u32)>,
    source_scrub_interval: Duration,
    next_source_scrub: Instant,
    live: LiveBuilder,
    passes_since_gc: u64,
    view_generation: u64,
    live_fold_stats: LiveFoldStats,
}

/// Exact source-body work performed while folding completed active parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LiveFoldStats {
    pub(crate) completed_parts: u64,
    pub(crate) pgm_body_reads: u64,
    pub(crate) pgm_body_bytes: u64,
}

impl LiveFoldStats {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            completed_parts: self.completed_parts.checked_add(other.completed_parts)?,
            pgm_body_reads: self.pgm_body_reads.checked_add(other.pgm_body_reads)?,
            pgm_body_bytes: self.pgm_body_bytes.checked_add(other.pgm_body_bytes)?,
        })
    }
}

impl OverviewWriter {
    /// Builds a writer with explicit durable and fallback storage policy.
    #[cfg(test)]
    pub(crate) fn new(
        data_dir: PathBuf,
        fallback: FallbackConfig,
    ) -> Result<Self, OverviewBuildError> {
        Self::with_gc_config(data_dir, fallback, GcConfig::default())
    }

    /// Builds a writer with explicit fallback and durable GC/quota policies.
    #[cfg(test)]
    pub(crate) fn with_gc_config(
        data_dir: PathBuf,
        fallback: FallbackConfig,
        gc: GcConfig,
    ) -> Result<Self, OverviewBuildError> {
        Self::with_runtime_config(data_dir, fallback, gc, Duration::from_mins(1))
    }

    /// Builds a writer with the complete production storage policy.
    pub(crate) fn with_runtime_config(
        data_dir: PathBuf,
        fallback: FallbackConfig,
        gc: GcConfig,
        source_scrub_interval: Duration,
    ) -> Result<Self, OverviewBuildError> {
        if source_scrub_interval.is_zero() {
            return Err(OverviewBuildError::SourceScrubInterval);
        }
        let next_source_scrub = Instant::now()
            .checked_add(source_scrub_interval)
            .ok_or(OverviewBuildError::SourceScrubInterval)?;
        let live = LiveBuilder::new(LIMIT).map_err(OverviewBuildError::Config)?;
        Ok(Self {
            store: FactStore::with_configs(data_dir, fallback, gc),
            sealed: BTreeMap::new(),
            unavailable: BTreeMap::new(),
            scrub_damaged: BTreeMap::new(),
            scrub_cursor: None,
            source_scrub_interval,
            next_source_scrub,
            live,
            passes_since_gc: 0,
            view_generation: 0,
            live_fold_stats: LiveFoldStats::default(),
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
        policy: OverviewColdConfig,
        decoded_cache_bytes: usize,
        decoded_cache_entries: usize,
    ) -> Result<OverviewFactLoader, OverviewBuildError> {
        let config = ColdAdmissionConfig::from_operator(policy)
            .map_err(|_error| OverviewBuildError::ColdAdmission)?;
        OverviewFactLoader::new(
            self.store.clone(),
            config,
            decoded_cache_bytes,
            decoded_cache_entries,
        )
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
        self.scrub_one_source_section(snapshot);
        let mut sealed = self.sealed.clone();
        let mut unavailable = self.unavailable.clone();
        self.refresh_sealed(snapshot, &mut sealed, &mut unavailable);

        let mut live = self.live.clone();
        let fold_stats = match fold_refresh(&mut live, snapshot, delta) {
            Ok(stats) => stats,
            Err(first_error) => {
                let mut rebuilt = LiveBuilder::new(LIMIT).map_err(OverviewBuildError::Config)?;
                let baseline = full_live_baseline(delta);
                let stats = fold_refresh(&mut rebuilt, snapshot, &baseline)
                    .map_err(|_error| first_error)?;
                live = rebuilt;
                stats
            }
        };

        self.sealed = sealed;
        self.unavailable = unavailable;
        self.live = live;
        self.live_fold_stats = self
            .live_fold_stats
            .checked_add(fold_stats)
            .ok_or(OverviewBuildError::Live(LiveFoldError::Overflow))?;
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

    #[cfg(feature = "qualification")]
    pub(crate) const fn qualification_live_fold_stats(&self) -> LiveFoldStats {
        self.live_fold_stats
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
        &mut self,
        snapshot: &LocalDirSnapshot,
        sealed: &mut BTreeMap<SealedLocator, DescriptorEntry>,
        unavailable: &mut BTreeMap<SealedLocator, SegmentDescriptor>,
    ) {
        let baseline = snapshot
            .sealed_descriptors()
            .iter()
            .map(|descriptor| (descriptor.locator, *descriptor))
            .collect::<BTreeMap<_, _>>();
        self.scrub_damaged
            .retain(|locator, descriptor| baseline.get(locator) == Some(descriptor));
        sealed.retain(|locator, entry| {
            baseline
                .get(locator)
                .is_some_and(|descriptor| descriptor == entry.descriptor())
        });
        unavailable.retain(|locator, descriptor| baseline.get(locator) == Some(descriptor));
        for descriptor in baseline.values() {
            if self.scrub_damaged.get(&descriptor.locator) == Some(descriptor) {
                sealed.remove(&descriptor.locator);
                unavailable.insert(descriptor.locator, *descriptor);
                continue;
            }
            if sealed.contains_key(&descriptor.locator) {
                unavailable.remove(&descriptor.locator);
                continue;
            }
            match Self::describe_sealed(snapshot, descriptor) {
                Ok(entry) => {
                    sealed.insert(descriptor.locator, entry);
                    unavailable.remove(&descriptor.locator);
                }
                Err(_error) => {
                    metrics::counter!("kronika_web_overview_sealed_failures_total").increment(1);
                    metrics::counter!(
                        "overview_source_failures_total",
                        "reason" => "sealed_descriptor"
                    )
                    .increment(1);
                    unavailable.insert(descriptor.locator, *descriptor);
                }
            }
        }
        record_damaged_sources(self.scrub_damaged.len());
    }

    fn scrub_one_source_section(&mut self, snapshot: &LocalDirSnapshot) {
        let now = Instant::now();
        if now < self.next_source_scrub {
            return;
        }
        self.next_source_scrub = now.checked_add(self.source_scrub_interval).unwrap_or(now);

        let descriptors = snapshot.sealed_descriptors();
        if descriptors.is_empty() {
            self.scrub_cursor = None;
            return;
        }
        let (position, ordinal) = self.scrub_position(descriptors);
        let descriptor = descriptors[position];
        let started = Instant::now();
        let unit = match snapshot.open_sealed_by_descriptor(&descriptor) {
            Ok(unit) => unit,
            Err(_error) => {
                self.mark_scrub_damage(descriptor);
                self.advance_scrub_cursor(descriptors, position);
                metrics::counter!(
                    "overview_source_failures_total",
                    "reason" => "scrub_open"
                )
                .increment(1);
                record_scrub("open_error", 0, started);
                return;
            }
        };
        if unit.catalog().entries.is_empty() {
            self.scrub_damaged.remove(&descriptor.locator);
            self.advance_scrub_cursor(descriptors, position);
            record_scrub("empty", 0, started);
            return;
        }
        let ordinal =
            ordinal.min(u32::try_from(unit.catalog().entries.len() - 1).unwrap_or(u32::MAX));
        let result = unit.scrub_overview_section(ordinal);
        let stats = unit.body_read_stats();
        match result {
            Ok(()) => {
                let next = usize::try_from(ordinal)
                    .ok()
                    .and_then(|ordinal| ordinal.checked_add(1));
                if next.is_some_and(|next| next < unit.catalog().entries.len()) {
                    self.scrub_cursor = Some((descriptor.locator, ordinal.saturating_add(1)));
                } else {
                    self.scrub_damaged.remove(&descriptor.locator);
                    self.advance_scrub_cursor(descriptors, position);
                }
                record_scrub("ok", stats.stored_bytes_read, started);
            }
            Err(_error) => {
                self.mark_scrub_damage(descriptor);
                self.advance_scrub_cursor(descriptors, position);
                metrics::counter!(
                    "overview_source_failures_total",
                    "reason" => "scrub_damage"
                )
                .increment(1);
                record_scrub("damage", stats.stored_bytes_read, started);
            }
        }
    }

    fn scrub_position(&self, descriptors: &[SegmentDescriptor]) -> (usize, u32) {
        let Some((locator, ordinal)) = self.scrub_cursor else {
            return (0, 0);
        };
        descriptors
            .iter()
            .position(|descriptor| descriptor.locator == locator)
            .map_or((0, 0), |position| (position, ordinal))
    }

    fn advance_scrub_cursor(&mut self, descriptors: &[SegmentDescriptor], position: usize) {
        let next = position.saturating_add(1);
        self.scrub_cursor = Some((descriptors.get(next).unwrap_or(&descriptors[0]).locator, 0));
    }

    fn mark_scrub_damage(&mut self, descriptor: SegmentDescriptor) {
        self.scrub_damaged.insert(descriptor.locator, descriptor);
        self.sealed.remove(&descriptor.locator);
        self.unavailable.insert(descriptor.locator, descriptor);
    }

    fn describe_sealed(
        snapshot: &LocalDirSnapshot,
        descriptor: &SegmentDescriptor,
    ) -> Result<DescriptorEntry, SealedFactError> {
        let unit = snapshot.open_sealed_by_descriptor(descriptor)?;
        let (identity, lineage) =
            SegmentFacts::provenance(&unit).map_err(SealedFactError::Build)?;
        let fact_key = FactKey::for_identity(&identity, FileKind::SegmentFacts);
        Ok(DescriptorEntry::new(
            *descriptor,
            FactBuildKey::new(fact_key, lineage.id()),
            ColdWorkWeight::for_unit(&unit),
        ))
    }

    fn current_view(
        &self,
        view_generation: u64,
        promotion: Option<PromotionCandidate>,
    ) -> DescriptorView {
        let view = DescriptorView::new(
            view_generation,
            self.sealed.values().cloned().collect(),
            self.unavailable.values().copied().collect(),
            Arc::new(self.live.publish()),
            promotion,
        );
        record_live_metrics(&view);
        view
    }
}

fn record_scrub(outcome: &'static str, bytes: u64, started: Instant) {
    metrics::counter!("overview_source_scrub_total", "outcome" => outcome).increment(1);
    metrics::counter!("overview_source_scrub_bytes_total").increment(bytes);
    metrics::histogram!("overview_source_scrub_seconds").record(started.elapsed());
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Prometheus gauges use f64 and the descriptor count is tightly bounded"
)]
fn record_damaged_sources(count: usize) {
    metrics::gauge!("overview_source_damaged_segments").set(count as f64);
}

fn fold_refresh(
    builder: &mut LiveBuilder,
    snapshot: &LocalDirSnapshot,
    delta: &RefreshDelta,
) -> Result<LiveFoldStats, OverviewBuildError> {
    let mut stats = LiveFoldStats::default();
    builder
        .begin_refresh(delta)
        .map_err(OverviewBuildError::Live)?;
    for part in &delta.journal.completed_parts {
        let unit = snapshot
            .open_active_part(part)
            .map_err(OverviewBuildError::ActiveRead)?;
        let effect = builder
            .fold_part(part, &unit)
            .map_err(OverviewBuildError::Live)?;
        if effect == FoldEffect::Folded {
            metrics::counter!("overview_live_folded_parts_total").increment(1);
        }
        let reads = unit.body_read_stats();
        stats.completed_parts = stats.completed_parts.saturating_add(1);
        stats.pgm_body_reads = stats.pgm_body_reads.saturating_add(reads.read_calls);
        stats.pgm_body_bytes = stats.pgm_body_bytes.saturating_add(reads.stored_bytes_read);
    }
    builder
        .complete_refresh()
        .map_err(OverviewBuildError::Live)?;
    Ok(stats)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Prometheus gauges use f64; timestamps remain exact enough for operational lag"
)]
fn record_live_metrics(view: &DescriptorView) {
    let live = view.live();
    let (state, reason) = match live.state() {
        LiveState::Empty => ("empty", "proven_empty"),
        LiveState::Warming => ("warming", "bootstrap"),
        LiveState::Current => ("current", "none"),
        LiveState::NeedsRebuild => ("needs_rebuild", "continuity"),
        LiveState::Incomplete => ("incomplete", "loss_or_limit"),
    };
    for (candidate_state, candidate_reason) in [
        ("empty", "proven_empty"),
        ("warming", "bootstrap"),
        ("current", "none"),
        ("needs_rebuild", "continuity"),
        ("incomplete", "loss_or_limit"),
    ] {
        metrics::gauge!(
            "overview_live_state",
            "state" => candidate_state,
            "reason" => candidate_reason
        )
        .set(f64::from(
            candidate_state == state && candidate_reason == reason,
        ));
    }
    let watermark = live.watermark_us().unwrap_or_default();
    metrics::gauge!("overview_live_data_through_us").set(watermark as f64);
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    let lag_seconds = live.watermark_us().map_or(0.0, |watermark| {
        let lag_us = u128::from(u64::try_from(watermark).unwrap_or_default());
        now_us.saturating_sub(lag_us) as f64 / 1_000_000.0
    });
    metrics::gauge!("overview_live_visibility_lag_seconds").set(lag_seconds);
    metrics::gauge!("overview_view_generation").set(view.view_generation() as f64);
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

    use kronika_analytics::overview::{
        CountLimits, CoverageSpan, OracleLimits, RawOracle, notable_event_id,
    };
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

    use crate::overview::selection::SelectedSealedPlan;
    use crate::overview::view::{IndexView, SourceStatus};
    use crate::tests::bgwriter_row;

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
        let body = BgwriterCheckpointer::encode(&[bgwriter_row(min_ts)]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 1,
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
            .fact_loader(OverviewColdConfig::default(), 16 * 1024 * 1024, 16)
            .expect("fact loader")
            .load_selected(Arc::new(snapshot.clone()), &plan)
            .await
            .expect("loaded selected view")
    }

    #[tokio::test]
    async fn an_empty_store_assembles_an_empty_current_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(dir.path().to_path_buf(), FallbackConfig::default())
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
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(dir.path().to_path_buf(), FallbackConfig::default())
            .expect("writer");
        let descriptors = index.assemble(&snapshot, &delta).expect("view");
        assert_eq!(
            ovf_files(dir.path()),
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
        assert_eq!(ovf_files(dir.path()), 1);
    }

    fn ovf_files(data_dir: &std::path::Path) -> usize {
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
        walk(data_dir, &mut count);
        count
    }

    #[tokio::test]
    async fn gc_reclaims_the_fact_file_of_a_dropped_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::with_gc_config(
            dir.path().to_path_buf(),
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
            ovf_files(dir.path()),
            1,
            "the sealed segment published a fact file"
        );

        // While the segment is live its fact file survives GC: the live-set
        // identity must equal what publication wrote.
        for _ in 0..GC_INTERVAL_PASSES {
            let _ = index.collect_fact_garbage();
        }
        assert_eq!(
            ovf_files(dir.path()),
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
            ovf_files(dir.path()),
            1,
            "only the newly added live segment's fact file remains"
        );
    }

    #[test]
    fn gc_runs_only_on_the_interval_boundary() {
        let cache = tempfile::tempdir().expect("cache dir");
        let mut index = OverviewIndex::new(cache.path().to_path_buf(), FallbackConfig::default())
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
        write_segment(dir.path(), "143000.pgm", 1_000, 2_000);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let first_delta = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut index = OverviewIndex::new(dir.path().to_path_buf(), FallbackConfig::default())
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
    fn writer_has_no_separate_namespace_or_cache_tree() {
        let dir = tempfile::tempdir().expect("data directory");
        OverviewIndex::new(dir.path().to_path_buf(), FallbackConfig::default())
            .expect("writer needs only the owned data directory");
        assert!(!dir.path().join("overview").exists());
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the test verifies one append-to-seal transition across three publications"
    )]
    async fn append_then_seal_keeps_one_coherent_event_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = lifecycle_row(1_500, 41);
        let second = lifecycle_row(2_500, 42);
        let first_part = lifecycle_part(std::slice::from_ref(&first));
        std::fs::write(dir.path().join("active.parts"), framed(&first_part))
            .expect("write first frame");

        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let bootstrap = snapshot
            .refresh_incremental_delta()
            .expect("bootstrap delta");
        let mut writer = OverviewIndex::new(dir.path().to_path_buf(), FallbackConfig::default())
            .expect("writer");
        let first_descriptors = writer
            .assemble(&snapshot, &bootstrap)
            .expect("first live view");
        let range = CoverageSpan::new(0, 10_000).expect("range");
        let first_view = load_selected(&writer, &snapshot, first_descriptors, &[7], range).await;
        let first_result = first_view.query(range, QUERY_LIMITS).expect("first query");
        assert_eq!(first_result.observations().len(), 1);
        let first_public_ids = first_result
            .observations()
            .iter()
            .map(|observation| notable_event_id(observation).expect("notable lifecycle event"))
            .collect::<Vec<_>>();

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
        let second_result = second_view
            .query(range, QUERY_LIMITS)
            .expect("second query");
        assert_eq!(second_result.observations().len(), 2);
        let second_public_ids = second_result
            .observations()
            .iter()
            .map(|observation| notable_event_id(observation).expect("notable lifecycle event"))
            .collect::<Vec<_>>();
        let live_observation_ids = second_result
            .observations()
            .iter()
            .map(kronika_analytics::overview::EventObservation::observation_id)
            .collect::<Vec<_>>();
        assert_eq!(
            &second_public_ids[..first_public_ids.len()],
            first_public_ids,
            "append must preserve the stable public identity of prior live rows"
        );

        let sealed_body = PgLogLifecycleV1::encode(&[first, second]).expect("sealed body");
        let sealed = build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: 2,
                body: &sealed_body,
            }],
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
        let sealed_result = sealed_view
            .query(range, QUERY_LIMITS)
            .expect("sealed query");
        assert_eq!(
            sealed_result.observations().len(),
            2,
            "seal reconciliation neither drops nor duplicates live observations"
        );
        let sealed_public_ids = sealed_result
            .observations()
            .iter()
            .map(|observation| notable_event_id(observation).expect("notable lifecycle event"))
            .collect::<Vec<_>>();
        let sealed_observation_ids = sealed_result
            .observations()
            .iter()
            .map(kronika_analytics::overview::EventObservation::observation_id)
            .collect::<Vec<_>>();
        assert_eq!(
            sealed_public_ids, second_public_ids,
            "ordinary live-to-sealed promotion must preserve semantic event IDs"
        );
        assert_ne!(
            sealed_observation_ids, live_observation_ids,
            "physical observation identity must still distinguish live and sealed lineages"
        );
        assert_eq!(
            ovf_files(dir.path()),
            1,
            "the reconciled sealed facts are durably published"
        );
    }
}
