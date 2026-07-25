//! Request-scoped sealed-fact loading over exact-key shared work.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use kronika_analytics::overview::{MetricFactor, NamingContractId, SegmentLocator};
use kronika_reader::{
    BuildError, CacheRebuildReason, FactLoad, FactOrigin, FactStore, FallbackStats, LIMIT,
    LiveView, LocalDirSnapshot, SealOutcome, SealedFactError, SegmentContext, SegmentFacts,
    SourceError, reconcile_seal,
};
use tokio::task::JoinSet;

use super::admission::{
    ColdAdmission, ColdAdmissionConfig, ColdAdmissionConfigError, ColdAdmissionError,
};
use super::memory_cache::DecodedFactCache;
use super::selection::SelectedSealedPlan;
use super::singleflight::{FactSingleflight, SingleflightError};
use super::view::{DescriptorEntry, IndexView, SealedEntry};

const OVERVIEW_NAMING_CONTRACT: NamingContractId = NamingContractId([1; 16]);

type FactResult = Result<Arc<SegmentFacts>, FactLoadFailure>;

/// A selected request could not enter or complete sealed-fact work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactLoadFailure {
    Source(SealedFactError),
    ColdBuildOverloaded {
        retry_after_seconds: u64,
        reason: &'static str,
    },
    WorkerFailed,
    IdentityMismatch,
}

/// Shared loader for request-specific immutable fact views.
#[derive(Debug, Clone)]
pub(crate) struct OverviewFactLoader {
    store: FactStore,
    namespace: Arc<[u8]>,
    admission: ColdAdmission,
    decoded: DecodedFactCache,
    flights: FactSingleflight<FactResult>,
    fallback_metrics: Arc<Mutex<FallbackStats>>,
    per_request_parallelism: usize,
    retry_after_seconds: u64,
}

impl OverviewFactLoader {
    pub(crate) fn new(
        store: FactStore,
        namespace: Vec<u8>,
        config: ColdAdmissionConfig,
        decoded_cache_bytes: usize,
        decoded_cache_entries: usize,
    ) -> Result<Self, ColdAdmissionConfigError> {
        let admission = ColdAdmission::new(config)?;
        let config = admission.config();
        let max_flights = usize::try_from(config.max_workers)
            .unwrap_or(usize::MAX)
            .saturating_add(config.max_queue);
        Ok(Self {
            store,
            namespace: namespace.into(),
            admission,
            decoded: DecodedFactCache::new(decoded_cache_bytes, decoded_cache_entries),
            flights: FactSingleflight::new(max_flights),
            fallback_metrics: Arc::new(Mutex::new(FallbackStats::default())),
            per_request_parallelism: config.per_request_parallelism,
            retry_after_seconds: config.retry_after_seconds,
        })
    }

    pub(crate) async fn load_selected(
        &self,
        snapshot: Arc<LocalDirSnapshot>,
        plan: &SelectedSealedPlan,
    ) -> Result<Arc<IndexView>, FactLoadFailure> {
        let entries = plan
            .entries()
            .map(|entry| {
                (
                    entry.clone(),
                    plan.promotion_for(entry.descriptor().locator),
                )
            })
            .collect::<Vec<_>>();
        let mut loaded = vec![None; entries.len()];
        let mut source_gaps = plan.source_gaps().to_vec();
        let mut workers = JoinSet::new();
        let mut next = 0;

        while next < entries.len() && workers.len() < self.per_request_parallelism {
            spawn_load(
                &mut workers,
                next,
                entries[next].0.clone(),
                entries[next].1.clone(),
                Arc::clone(&snapshot),
                self.clone(),
            );
            next += 1;
        }

        while let Some(joined) = workers.join_next().await {
            let (index, result) = joined.map_err(|_error| FactLoadFailure::WorkerFailed)?;
            match result {
                Ok(entry) => loaded[index] = Some(entry),
                Err(FactLoadFailure::Source(error)) => {
                    record_source_failure(error);
                    source_gaps.push(plan.gap_for(entries[index].0.descriptor()));
                    metrics::counter!(
                        "overview_source_read_failures_total",
                        "outcome" => "partial"
                    )
                    .increment(1);
                }
                Err(error) => return Err(error),
            }
            if next < entries.len() {
                spawn_load(
                    &mut workers,
                    next,
                    entries[next].0.clone(),
                    entries[next].1.clone(),
                    Arc::clone(&snapshot),
                    self.clone(),
                );
                next += 1;
            }
        }

        let loaded = loaded.into_iter().flatten().collect();
        source_gaps.sort_unstable();
        source_gaps.dedup();
        let fact_set_id = plan.fact_set_id_with_gaps(&source_gaps);
        Ok(Arc::new(IndexView::from_selected(
            plan.view(),
            loaded,
            source_gaps,
            fact_set_id,
            plan.source_descriptors().to_vec(),
            plan.store_data_through_us(),
        )))
    }

    async fn load_one(
        &self,
        snapshot: Arc<LocalDirSnapshot>,
        entry: DescriptorEntry,
        promotion: Option<Arc<LiveView>>,
    ) -> Result<SealedEntry, FactLoadFailure> {
        let key = entry.fact_build_key();
        if let Some(facts) = self.decoded.get(key) {
            return SealedEntry::from_descriptor(&entry, facts)
                .ok_or(FactLoadFailure::IdentityMismatch);
        }
        let cached = self.load_cached(Arc::clone(&snapshot), entry.clone()).await;
        self.record_fallback_metrics();
        if let Some(facts) = cached? {
            self.decoded.insert(key, Arc::clone(&facts));
            return SealedEntry::from_descriptor(&entry, facts)
                .ok_or(FactLoadFailure::IdentityMismatch);
        }

        let worker_entry = entry.clone();
        let store = self.store.clone();
        let namespace = Arc::clone(&self.namespace);
        let admission = self.admission.clone();
        let decoded = self.decoded.clone();
        let retry_after_seconds = self.retry_after_seconds;
        let facts =
            self.flights
                .run(
                    key,
                    move || async move {
                        if let Some(facts) = decoded.get(key) {
                            return Ok(facts);
                        }
                        if let Some(facts) = load_cached(
                            Arc::clone(&snapshot),
                            worker_entry.clone(),
                            store.clone(),
                            Arc::clone(&namespace),
                        )
                        .await?
                        {
                            decoded.insert(key, Arc::clone(&facts));
                            return Ok(facts);
                        }
                        let _permit = admission
                            .acquire(worker_entry.cold_weight())
                            .await
                            .map_err(|error| map_admission_error(error, retry_after_seconds))?;
                        let started = Instant::now();
                        let descriptor = *worker_entry.descriptor();
                        let result = tokio::task::spawn_blocking(move || {
                            let context = SegmentContext::new(
                                namespace.as_ref().to_vec(),
                                OVERVIEW_NAMING_CONTRACT,
                                SegmentLocator(*descriptor.locator.as_bytes()),
                            )
                            .map_err(|_error| {
                                FactLoadFailure::Source(SealedFactError::ContextLocatorMismatch {
                                    locator: descriptor.locator,
                                })
                            })?;
                            let unit = snapshot
                                .open_sealed_by_descriptor(&descriptor)
                                .map_err(FactLoadFailure::Source)?;
                            let facts = if let Some(candidate) = promotion {
                                let outcome =
                                    reconcile_seal(&candidate, &unit, &context, &store, &LIMIT)
                                        .map_err(|error| {
                                            FactLoadFailure::Source(SealedFactError::Build(error))
                                        })?;
                                record_seal_outcome(outcome)
                            } else {
                                let load = store.load_or_build(&unit, &context, &LIMIT).map_err(
                                    |error| FactLoadFailure::Source(SealedFactError::Build(error)),
                                )?;
                                record_load(&load);
                                load.into_shared_facts()
                            };
                            Ok::<_, FactLoadFailure>(facts)
                        })
                        .await
                        .unwrap_or(Err(FactLoadFailure::WorkerFailed));
                        record_build(&result, started);
                        if let Ok(facts) = &result {
                            decoded.insert(key, Arc::clone(facts));
                        }
                        result
                    },
                    || Err(FactLoadFailure::WorkerFailed),
                )
                .await
                .map_err(|error| map_singleflight_error(error, self.retry_after_seconds));
        self.record_fallback_metrics();
        let facts = facts??;
        SealedEntry::from_descriptor(&entry, facts).ok_or(FactLoadFailure::IdentityMismatch)
    }

    async fn load_cached(
        &self,
        snapshot: Arc<LocalDirSnapshot>,
        entry: DescriptorEntry,
    ) -> Result<Option<Arc<SegmentFacts>>, FactLoadFailure> {
        load_cached(
            snapshot,
            entry,
            self.store.clone(),
            Arc::clone(&self.namespace),
        )
        .await
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "fallback budgets are bounded well below the exact f64 integer range"
    )]
    fn record_fallback_metrics(&self) {
        let current = self.store.fallback_stats();
        let Ok(mut previous) = self.fallback_metrics.lock() else {
            return;
        };
        let evictions = current.evictions.saturating_sub(previous.evictions);
        let oversized = current.oversized.saturating_sub(previous.oversized);
        if evictions != 0 {
            metrics::counter!(
                "overview_cache_evictions_total",
                "class" => "publication_fallback",
                "reason" => "budget"
            )
            .increment(evictions);
        }
        if oversized != 0 {
            metrics::counter!(
                "overview_cache_evictions_total",
                "class" => "publication_fallback",
                "reason" => "entry_unadmitted"
            )
            .increment(oversized);
        }
        metrics::gauge!("overview_cache_entries", "class" => "publication_fallback")
            .set(current.resident_entries as f64);
        metrics::gauge!("overview_cache_bytes", "class" => "publication_fallback")
            .set(current.resident_bytes as f64);
        *previous = current;
    }
}

fn spawn_load(
    workers: &mut JoinSet<(usize, Result<SealedEntry, FactLoadFailure>)>,
    index: usize,
    entry: DescriptorEntry,
    promotion: Option<Arc<LiveView>>,
    snapshot: Arc<LocalDirSnapshot>,
    loader: OverviewFactLoader,
) {
    workers.spawn(async move { (index, loader.load_one(snapshot, entry, promotion).await) });
}

async fn load_cached(
    snapshot: Arc<LocalDirSnapshot>,
    entry: DescriptorEntry,
    store: FactStore,
    namespace: Arc<[u8]>,
) -> Result<Option<Arc<SegmentFacts>>, FactLoadFailure> {
    tokio::task::spawn_blocking(move || {
        let descriptor = *entry.descriptor();
        let context = SegmentContext::new(
            namespace.as_ref().to_vec(),
            OVERVIEW_NAMING_CONTRACT,
            SegmentLocator(*descriptor.locator.as_bytes()),
        )
        .map_err(|_error| {
            FactLoadFailure::Source(SealedFactError::ContextLocatorMismatch {
                locator: descriptor.locator,
            })
        })?;
        let unit = snapshot
            .open_sealed_by_descriptor(&descriptor)
            .map_err(FactLoadFailure::Source)?;
        let load = store
            .load_cached(&unit, &context, &LIMIT)
            .map_err(|error| FactLoadFailure::Source(SealedFactError::Build(error)))?;
        Ok(load.map(|load| {
            record_load(&load);
            load.into_shared_facts()
        }))
    })
    .await
    .unwrap_or(Err(FactLoadFailure::WorkerFailed))
}

const fn map_admission_error(
    error: ColdAdmissionError,
    retry_after_seconds: u64,
) -> FactLoadFailure {
    FactLoadFailure::ColdBuildOverloaded {
        retry_after_seconds,
        reason: error.metric_reason(),
    }
}

const fn map_singleflight_error(
    _error: SingleflightError,
    retry_after_seconds: u64,
) -> FactLoadFailure {
    FactLoadFailure::ColdBuildOverloaded {
        retry_after_seconds,
        reason: "singleflight_capacity",
    }
}

fn record_load(load: &FactLoad) {
    match load.origin() {
        FactOrigin::CacheHit => {
            metrics::counter!("kronika_web_overview_durable_hits_total").increment(1);
            record_lookup("l1", "hit", "none");
        }
        FactOrigin::FallbackHit => {
            metrics::counter!("kronika_web_overview_fallback_hits_total").increment(1);
            record_lookup(
                "l1",
                "miss",
                load.rebuild_reason().map_or("none", rebuild_reason),
            );
            record_lookup(
                "l1f",
                "hit",
                load.rebuild_reason().map_or("none", rebuild_reason),
            );
        }
        FactOrigin::Rebuilt => {
            metrics::counter!("kronika_web_overview_rebuilt_total").increment(1);
            record_lookup(
                "l1",
                "miss",
                load.rebuild_reason().map_or("none", rebuild_reason),
            );
            record_lookup(
                "l1f",
                "miss",
                load.rebuild_reason().map_or("none", rebuild_reason),
            );
            metrics::counter!(
                "overview_raw_fallback_total",
                "reason" => load.rebuild_reason().map_or("none", rebuild_reason)
            )
            .increment(1);
            record_lookup(
                "source",
                "rebuild",
                load.rebuild_reason().map_or("none", rebuild_reason),
            );
            record_fact_quality(load.facts());
        }
    }
    if let Some(stats) = load.fact_read_stats() {
        metrics::counter!("overview_fact_read_bytes").increment(stats.stored_bytes_read);
    }
    metrics::counter!("overview_fact_write_bytes").increment(load.fact_write_bytes());
    let pgm = load.pgm_body_read_stats();
    metrics::counter!("overview_pgm_body_read_bytes").increment(pgm.stored_bytes_read);
    metrics::counter!("overview_pgm_sections_decoded").increment(pgm.read_calls);
    if let Some(error) = load.persist_error() {
        metrics::counter!("kronika_web_overview_persistence_failures_total").increment(1);
        metrics::counter!(
            "overview_persist_failures_total",
            "reason" => super::resilience::persist_reason(Some(error))
        )
        .increment(1);
    }
}

fn record_fact_quality(facts: &SegmentFacts) {
    let source = facts.identity().pgm_source_id.to_string();
    for observation in facts.observations() {
        metrics::counter!(
            "overview_retained_observations_total",
            "kind" => observation.payload().kind_code()
        )
        .increment(1);
    }
    for coverage in facts.loss_coverage().factor_coverage() {
        let factor =
            MetricFactor::from_id(coverage.factor_id).map_or("unknown", MetricFactor::wire_code);
        for reason in &coverage.loss_reasons {
            metrics::counter!(
                "overview_coverage_loss_total",
                "source" => source.clone(),
                "factor" => factor,
                "reason" => super::dto::loss_reason_name(*reason)
            )
            .increment(1);
        }
    }
}

fn record_source_failure(error: SealedFactError) {
    let reason = match error {
        SealedFactError::UnitOutOfRange { .. } => "unit_out_of_range",
        SealedFactError::LiveUnit { .. } => "live_unit",
        SealedFactError::StaleSnapshot { .. } => "stale_snapshot",
        SealedFactError::DescriptorUnavailable { .. } => "descriptor_unavailable",
        SealedFactError::StaleDescriptor { .. } => "stale_descriptor",
        SealedFactError::ContextLocatorMismatch { .. } => "context_locator_mismatch",
        SealedFactError::Build(BuildError::Source(SourceError::Io)) => "source_io",
        SealedFactError::Build(BuildError::Source(SourceError::Corrupt)) => "source_corrupt",
        SealedFactError::Build(BuildError::Source(SourceError::UnsupportedFormat)) => {
            "unsupported_format"
        }
        SealedFactError::Build(BuildError::Source(SourceError::UnsupportedLayout)) => {
            "unsupported_layout"
        }
        SealedFactError::Build(BuildError::LimitExceeded) => "fact_limit",
        SealedFactError::Build(BuildError::Overflow) => "arithmetic_overflow",
        SealedFactError::Build(BuildError::Internal) => "internal",
    };
    metrics::counter!("overview_source_failures_total", "reason" => reason).increment(1);
    match error {
        SealedFactError::Build(BuildError::LimitExceeded) => {
            metrics::counter!("overview_overflow_total", "kind" => "fact_limit").increment(1);
        }
        SealedFactError::Build(BuildError::Overflow) => {
            metrics::counter!("overview_overflow_total", "kind" => "arithmetic").increment(1);
        }
        _ => {}
    }
}

fn record_lookup(layer: &'static str, result: &'static str, reason: &'static str) {
    metrics::counter!(
        "overview_fact_lookup_total",
        "layer" => layer,
        "result" => result,
        "reason" => reason
    )
    .increment(1);
}

const fn rebuild_reason(reason: CacheRebuildReason) -> &'static str {
    match reason {
        CacheRebuildReason::Missing => "missing",
        CacheRebuildReason::Incompatible => "incompatible",
        CacheRebuildReason::Corrupt => "corrupt",
        CacheRebuildReason::WrongSource => "wrong_source",
        CacheRebuildReason::Oversized => "oversized",
        CacheRebuildReason::Io => "io",
    }
}

fn record_build(result: &FactResult, started: Instant) {
    metrics::histogram!("overview_fact_build_seconds").record(started.elapsed());
    metrics::counter!(
        "overview_fact_build_total",
        "result" => if result.is_ok() { "success" } else { "failure" },
        "source_type" => "segment"
    )
    .increment(1);
}

fn record_seal_outcome(outcome: SealOutcome) -> Arc<SegmentFacts> {
    match outcome {
        SealOutcome::Promoted {
            facts,
            persist_error,
            fact_write_bytes,
        } => {
            metrics::counter!("kronika_web_overview_promotions_total").increment(1);
            metrics::counter!("overview_fact_write_bytes").increment(fact_write_bytes);
            if let Some(error) = persist_error {
                metrics::counter!("kronika_web_overview_persistence_failures_total").increment(1);
                metrics::counter!(
                    "overview_persist_failures_total",
                    "reason" => super::resilience::persist_reason(Some(error))
                )
                .increment(1);
            }
            record_fact_quality(&facts);
            facts
        }
        SealOutcome::Rebuilt(load) => {
            record_load(&load);
            load.into_shared_facts()
        }
    }
}
