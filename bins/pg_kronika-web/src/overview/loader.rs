//! Request-scoped sealed-fact loading over exact-key shared work.

use std::sync::Arc;

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_reader::{
    FactOrigin, FactStore, LIMIT, LiveView, LocalDirSnapshot, SealOutcome, SealedFactError,
    SegmentContext, SegmentFacts, reconcile_seal,
};
use tokio::task::JoinSet;

use super::admission::{
    ColdAdmission, ColdAdmissionConfig, ColdAdmissionConfigError, ColdAdmissionError,
};
use super::selection::SelectedSealedPlan;
use super::singleflight::{FactSingleflight, SingleflightError};
use super::view::{DescriptorEntry, IndexView, SealedEntry};

const OVERVIEW_NAMING_CONTRACT: NamingContractId = NamingContractId([1; 16]);

type FactResult = Result<Arc<SegmentFacts>, FactLoadFailure>;

/// A selected request could not enter or complete sealed-fact work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactLoadFailure {
    Source(SealedFactError),
    CapacityUnavailable,
    WorkerFailed,
    IdentityMismatch,
}

/// Shared loader for request-specific immutable fact views.
#[derive(Debug, Clone)]
pub(crate) struct OverviewFactLoader {
    store: FactStore,
    namespace: Arc<[u8]>,
    admission: ColdAdmission,
    flights: FactSingleflight<FactResult>,
    per_request_parallelism: usize,
}

impl OverviewFactLoader {
    pub(crate) fn new(
        store: FactStore,
        namespace: Vec<u8>,
        config: ColdAdmissionConfig,
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
            flights: FactSingleflight::new(max_flights),
            per_request_parallelism: config.per_request_parallelism,
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
        Ok(Arc::new(IndexView::from_selected(
            plan.view(),
            loaded,
            plan.sealed_gap(),
            plan.fact_set_id(),
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
        let worker_entry = entry.clone();
        let store = self.store.clone();
        let namespace = Arc::clone(&self.namespace);
        let admission = self.admission.clone();
        let facts =
            self.flights
                .run(
                    key,
                    move || async move {
                        let _permit = admission
                            .acquire(worker_entry.cold_weight())
                            .await
                            .map_err(map_admission_error)?;
                        let descriptor = *worker_entry.descriptor();
                        tokio::task::spawn_blocking(move || {
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
                        .unwrap_or(Err(FactLoadFailure::WorkerFailed))
                    },
                    || Err(FactLoadFailure::WorkerFailed),
                )
                .await
                .map_err(map_singleflight_error)??;
        SealedEntry::from_descriptor(&entry, facts).ok_or(FactLoadFailure::IdentityMismatch)
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

const fn map_admission_error(_error: ColdAdmissionError) -> FactLoadFailure {
    FactLoadFailure::CapacityUnavailable
}

const fn map_singleflight_error(_error: SingleflightError) -> FactLoadFailure {
    FactLoadFailure::CapacityUnavailable
}

fn record_load(load: &kronika_reader::FactLoad) {
    match load.origin() {
        FactOrigin::CacheHit => {
            metrics::counter!("kronika_web_overview_durable_hits_total").increment(1);
        }
        FactOrigin::FallbackHit => {
            metrics::counter!("kronika_web_overview_fallback_hits_total").increment(1);
        }
        FactOrigin::Rebuilt => {
            metrics::counter!("kronika_web_overview_rebuilt_total").increment(1);
        }
    }
    if load.persist_error().is_some() {
        metrics::counter!("kronika_web_overview_persistence_failures_total").increment(1);
    }
}

fn record_seal_outcome(outcome: SealOutcome) -> Arc<SegmentFacts> {
    match outcome {
        SealOutcome::Promoted {
            facts,
            persist_error,
        } => {
            metrics::counter!("kronika_web_overview_promotions_total").increment(1);
            if persist_error.is_some() {
                metrics::counter!("kronika_web_overview_persistence_failures_total").increment(1);
            }
            facts
        }
        SealOutcome::Rebuilt(load) => {
            record_load(&load);
            load.into_shared_facts()
        }
    }
}
