//! Bounded in-memory fallback for admitted sealed-segment facts.
//!
//! Persistent fact files remain the primary cache. A caller may retain an
//! [`Arc<SegmentFacts>`] here only after source extraction and full admission
//! have succeeded and durable publication has failed for a recoverable storage
//! reason. The caller computes the canonical encoded length before taking the
//! mutex that owns this cache; cache operations perform no encoding or I/O.
//!
//! [`FallbackFactLru`] is deliberately not internally synchronized. A
//! [`FactStore`](super::publish::FactStore) owns it behind one mutex, holds that
//! lock only for `get`, insertion, and stats snapshots, and performs source and
//! filesystem work without the lock.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use kronika_analytics::overview::{SegmentIdentity, SegmentLineageId};

use super::container::HeaderIdentity;
use super::factkey::{FactKey, FileKind};
use super::facts::SegmentFacts;
use super::limits::Bounds;

/// Default total admitted segment duration retained by the fallback.
pub const DEFAULT_FALLBACK_SEGMENT_HOURS: u64 = 24;

/// Hard operator-configurable segment-duration ceiling.
pub const MAX_FALLBACK_SEGMENT_HOURS: u64 = 31 * 24;

/// Default canonical fact bytes retained by the fallback.
pub const DEFAULT_FALLBACK_BYTES: u64 = 64 * 1024 * 1024;

/// Hard operator-configurable canonical-byte ceiling.
pub const MAX_FALLBACK_BYTES: u64 = 256 * 1024 * 1024;

const MICROSECONDS_PER_HOUR: u128 = 3_600_000_000;

/// Invalid bounds for the in-memory fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackConfigError {
    /// A zero segment-hour budget would never retain an admitted segment.
    ZeroSegmentHours,
    /// The segment-hour budget exceeds the defensive hard ceiling.
    SegmentHoursAboveMaximum,
    /// A zero byte budget would never retain a canonical fact file.
    ZeroBytes,
    /// The byte budget exceeds the defensive hard ceiling.
    BytesAboveMaximum,
}

impl std::fmt::Display for FallbackConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::ZeroSegmentHours => "fallback segment-hour budget must be non-zero",
            Self::SegmentHoursAboveMaximum => {
                "fallback segment-hour budget exceeds the hard ceiling"
            }
            Self::ZeroBytes => "fallback byte budget must be non-zero",
            Self::BytesAboveMaximum => "fallback byte budget exceeds the hard ceiling",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FallbackConfigError {}

/// Validated dual budgets for the in-memory fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackConfig {
    segment_hours: u64,
    bytes: u64,
}

impl FallbackConfig {
    /// Validates operator-selected segment-hour and canonical-byte budgets.
    ///
    /// # Errors
    ///
    /// Returns [`FallbackConfigError`] when either budget is zero or exceeds
    /// its defensive hard ceiling.
    pub const fn new(segment_hours: u64, bytes: u64) -> Result<Self, FallbackConfigError> {
        if segment_hours == 0 {
            return Err(FallbackConfigError::ZeroSegmentHours);
        }
        if segment_hours > MAX_FALLBACK_SEGMENT_HOURS {
            return Err(FallbackConfigError::SegmentHoursAboveMaximum);
        }
        if bytes == 0 {
            return Err(FallbackConfigError::ZeroBytes);
        }
        if bytes > MAX_FALLBACK_BYTES {
            return Err(FallbackConfigError::BytesAboveMaximum);
        }
        Ok(Self {
            segment_hours,
            bytes,
        })
    }

    /// Maximum sum of admitted segment-hour weights.
    #[must_use]
    pub const fn segment_hours(self) -> u64 {
        self.segment_hours
    }

    /// Maximum sum of canonical encoded fact bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            segment_hours: DEFAULT_FALLBACK_SEGMENT_HOURS,
            bytes: DEFAULT_FALLBACK_BYTES,
        }
    }
}

/// Full in-memory identity of one admitted sealed-segment fact set.
///
/// The durable key binds source scope, the PGM tail/catalog descriptor, fact
/// kind, schema, extractor, and registry versions. The lineage additionally
/// binds the stable sealed locator and naming contract carried by the admitted
/// observations. Consequently source changes, replacements, version changes,
/// and distinct sealed occurrences miss naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct FallbackFactKey {
    durable: FactKey,
    lineage: SegmentLineageId,
}

impl FallbackFactKey {
    /// Combines a durable content key with the admitted segment lineage.
    #[must_use]
    pub(super) const fn new(durable: FactKey, lineage: SegmentLineageId) -> Self {
        Self { durable, lineage }
    }

    /// Derives the complete lookup identity before any PGM body is read.
    #[must_use]
    pub(super) fn for_expected(identity: &HeaderIdentity, lineage: &SegmentIdentity) -> Self {
        Self::new(
            FactKey::for_identity(identity, FileKind::SegmentFacts),
            lineage.id(),
        )
    }

    /// Derives the complete fallback identity from admitted facts.
    #[must_use]
    pub(super) fn for_facts(facts: &SegmentFacts) -> Self {
        Self::for_expected(facts.identity(), facts.lineage())
    }

    /// Content-addressed durable fact key.
    #[must_use]
    pub(super) const fn durable(self) -> FactKey {
        self.durable
    }

    /// Stable admitted segment lineage.
    #[must_use]
    #[cfg(test)]
    pub(super) const fn lineage(self) -> SegmentLineageId {
        self.lineage
    }
}

/// Outcome of a publication-failure fallback insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FallbackInsert {
    /// The admitted facts are resident under their complete identity.
    Retained,
    /// One entry exceeds a budget, so the caller receives it without fallback
    /// residency.
    Oversized,
}

/// Saturating counters and exact current-residency gauges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackStats {
    /// Fallback lookups served from memory.
    pub hits: u64,
    /// Fallback lookups with no matching complete identity.
    pub misses: u64,
    /// Publication-failure entries retained, including same-key replacement.
    pub inserts: u64,
    /// Resident entries removed to restore either configured budget.
    pub evictions: u64,
    /// Single entries rejected because they cannot fit.
    pub oversized: u64,
    /// Recoverable publication failures offered to the fallback.
    pub publication_failure_fallbacks: u64,
    /// Exact number of currently resident entries.
    pub resident_entries: u64,
    /// Exact sum of currently resident segment-hour weights.
    pub resident_segment_hours: u64,
    /// Exact sum of currently resident canonical fact bytes.
    pub resident_bytes: u64,
}

#[derive(Debug, Default)]
struct FallbackCounters {
    hits: u64,
    misses: u64,
    inserts: u64,
    evictions: u64,
    oversized: u64,
    publication_failure_fallbacks: u64,
}

struct ResidentFacts {
    facts: Arc<SegmentFacts>,
    admitted_bounds: Bounds,
    segment_hours: u64,
    canonical_bytes: u64,
    last_touch: u64,
}

/// Deterministic, dual-budget LRU of fully admitted immutable segment facts.
///
/// Methods require `&mut self`, making synchronization ownership explicit.
/// Same-key concurrent builds may race before the owner mutex is acquired, but
/// their serialized insertions replace one resident entry and cannot double
/// account its weight. The most recent insertion wins with identical complete
/// identity; every read updates recency.
pub(super) struct FallbackFactLru {
    config: FallbackConfig,
    entries: BTreeMap<FallbackFactKey, ResidentFacts>,
    resident_segment_hours: u64,
    resident_bytes: u64,
    last_touch: u64,
    counters: FallbackCounters,
}

impl std::fmt::Debug for FallbackFactLru {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FallbackFactLru")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl FallbackFactLru {
    /// Creates an empty fallback with validated immutable budgets.
    #[must_use]
    pub(super) fn new(config: FallbackConfig) -> Self {
        Self {
            config,
            entries: BTreeMap::new(),
            resident_segment_hours: 0,
            resident_bytes: 0,
            last_touch: 0,
            counters: FallbackCounters::default(),
        }
    }

    /// Returns admitted facts for an exact identity and refreshes its recency.
    #[must_use]
    pub(super) fn get(
        &mut self,
        key: &FallbackFactKey,
        requested_bounds: Bounds,
    ) -> Option<Arc<SegmentFacts>> {
        let Some(facts) = self
            .entries
            .get(key)
            .filter(|entry| requested_bounds.admits_profile(entry.admitted_bounds))
            .map(|entry| Arc::clone(&entry.facts))
        else {
            saturating_increment(&mut self.counters.misses);
            return None;
        };

        saturating_increment(&mut self.counters.hits);
        let touch = self.next_touch();
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_touch = touch;
        }
        Some(facts)
    }

    /// Retains admitted facts after a recoverable durable-publication failure.
    ///
    /// `canonical_byte_len` is the exact non-zero length produced by canonical
    /// encoding before the owner locks this cache. Source extraction,
    /// admission, encoding, and publication errors stay outside this method.
    /// An entry that cannot fit by itself is returned by the caller but is not
    /// inserted here.
    pub(super) fn insert_after_publication_failure(
        &mut self,
        facts: Arc<SegmentFacts>,
        canonical_byte_len: NonZeroU64,
        admitted_bounds: Bounds,
    ) -> FallbackInsert {
        saturating_increment(&mut self.counters.publication_failure_fallbacks);

        let segment_hours = segment_hour_weight(
            facts.identity().source_min_ts_us,
            facts.identity().source_max_ts_us,
        );
        let canonical_bytes = canonical_byte_len.get();
        if segment_hours > self.config.segment_hours || canonical_bytes > self.config.bytes {
            saturating_increment(&mut self.counters.oversized);
            return FallbackInsert::Oversized;
        }

        let key = FallbackFactKey::for_facts(&facts);
        self.remove_replaced(&key);
        let last_touch = self.next_touch();
        let Some(resident_segment_hours) = self.resident_segment_hours.checked_add(segment_hours)
        else {
            saturating_increment(&mut self.counters.oversized);
            return FallbackInsert::Oversized;
        };
        let Some(resident_bytes) = self.resident_bytes.checked_add(canonical_bytes) else {
            saturating_increment(&mut self.counters.oversized);
            return FallbackInsert::Oversized;
        };
        self.resident_segment_hours = resident_segment_hours;
        self.resident_bytes = resident_bytes;
        self.entries.insert(
            key,
            ResidentFacts {
                facts,
                admitted_bounds,
                segment_hours,
                canonical_bytes,
                last_touch,
            },
        );
        saturating_increment(&mut self.counters.inserts);
        self.evict_to_budgets();
        FallbackInsert::Retained
    }

    /// Returns saturating lifetime counters and exact residency gauges.
    #[must_use]
    pub(super) fn stats(&self) -> FallbackStats {
        FallbackStats {
            hits: self.counters.hits,
            misses: self.counters.misses,
            inserts: self.counters.inserts,
            evictions: self.counters.evictions,
            oversized: self.counters.oversized,
            publication_failure_fallbacks: self.counters.publication_failure_fallbacks,
            resident_entries: u64::try_from(self.entries.len()).unwrap_or(u64::MAX),
            resident_segment_hours: self.resident_segment_hours,
            resident_bytes: self.resident_bytes,
        }
    }

    pub(super) fn discard_durable(&mut self, durable: FactKey) {
        let keys = self
            .entries
            .keys()
            .copied()
            .filter(|key| key.durable() == durable)
            .collect::<Vec<_>>();
        for key in keys {
            self.remove_replaced(&key);
        }
    }

    fn remove_replaced(&mut self, key: &FallbackFactKey) {
        if let Some(previous) = self.entries.remove(key) {
            self.resident_segment_hours -= previous.segment_hours;
            self.resident_bytes -= previous.canonical_bytes;
        }
    }

    fn evict_to_budgets(&mut self) {
        while self.resident_segment_hours > self.config.segment_hours
            || self.resident_bytes > self.config.bytes
        {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry.last_touch, **key))
                .map(|(key, _entry)| *key)
            else {
                break;
            };
            let Some(evicted) = self.entries.remove(&key) else {
                break;
            };
            self.resident_segment_hours -= evicted.segment_hours;
            self.resident_bytes -= evicted.canonical_bytes;
            saturating_increment(&mut self.counters.evictions);
        }
    }

    fn next_touch(&mut self) -> u64 {
        if self.last_touch == u64::MAX {
            self.compact_recency();
        }
        self.last_touch = self.last_touch.saturating_add(1);
        self.last_touch
    }

    fn compact_recency(&mut self) {
        let mut order = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.last_touch, *key))
            .collect::<Vec<_>>();
        order.sort_unstable();

        let mut touch = 0_u64;
        for (_old_touch, key) in order {
            touch = touch.saturating_add(1);
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.last_touch = touch;
            }
        }
        self.last_touch = touch;
    }
}

fn segment_hour_weight(min_ts_us: i64, max_ts_us: i64) -> u64 {
    if min_ts_us > max_ts_us {
        return 1;
    }
    let duration_us =
        u128::try_from(i128::from(max_ts_us) - i128::from(min_ts_us)).unwrap_or(u128::MAX);
    let hours = duration_us.saturating_add(MICROSECONDS_PER_HOUR - 1) / MICROSECONDS_PER_HOUR;
    u64::try_from(hours.max(1)).unwrap_or(u64::MAX)
}

fn saturating_increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};

    use kronika_analytics::overview::{
        NamingContractId, SegmentLineageId, SegmentLocator, SourceScopeId,
    };
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

    use super::super::descriptors::SourceDescriptor;
    use super::super::facts::SegmentContext;
    use super::super::limits::LIMIT;
    use super::*;
    use crate::unit::PgmUnit;

    fn config(segment_hours: u64, bytes: u64) -> FallbackConfig {
        FallbackConfig::new(segment_hours, bytes).expect("valid fallback config")
    }

    fn canonical_len(bytes: u64) -> NonZeroU64 {
        NonZeroU64::new(bytes).expect("non-zero canonical length")
    }

    fn key(durable_byte: u8, lineage_byte: u8) -> FallbackFactKey {
        FallbackFactKey::new(
            FactKey::for_current_segment(
                SourceScopeId([durable_byte; 32]),
                SourceDescriptor([durable_byte; 32]),
            ),
            SegmentLineageId([lineage_byte; 32]),
        )
    }

    fn facts(locator_byte: u8, min_ts_us: i64, max_ts_us: i64) -> Arc<SegmentFacts> {
        let row = PgLogLifecycleV1 {
            ts: Ts(min_ts_us),
            kind: 2,
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        };
        let body = PgLogLifecycleV1::encode(&[row]).expect("encode lifecycle section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: min_ts_us,
                max_ts: max_ts_us,
                source_id: 7,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        let context = SegmentContext::new(
            b"fallback-test".to_vec(),
            NamingContractId([0x33; 16]),
            SegmentLocator([locator_byte; 32]),
        )
        .expect("valid segment context");
        Arc::new(SegmentFacts::extract(&unit, &context, &LIMIT).expect("extract facts"))
    }

    #[test]
    fn config_is_bounded_and_defaults_are_valid() {
        assert_eq!(
            FallbackConfig::new(0, 1),
            Err(FallbackConfigError::ZeroSegmentHours)
        );
        assert_eq!(
            FallbackConfig::new(MAX_FALLBACK_SEGMENT_HOURS + 1, 1),
            Err(FallbackConfigError::SegmentHoursAboveMaximum)
        );
        assert_eq!(
            FallbackConfig::new(1, 0),
            Err(FallbackConfigError::ZeroBytes)
        );
        assert_eq!(
            FallbackConfig::new(1, MAX_FALLBACK_BYTES + 1),
            Err(FallbackConfigError::BytesAboveMaximum)
        );
        assert_eq!(
            FallbackConfig::default(),
            config(DEFAULT_FALLBACK_SEGMENT_HOURS, DEFAULT_FALLBACK_BYTES)
        );
    }

    #[test]
    fn segment_hour_weight_rounds_up_and_treats_empty_as_one_hour() {
        assert_eq!(segment_hour_weight(0, 0), 1);
        assert_eq!(segment_hour_weight(8, 7), 1);
        assert_eq!(
            segment_hour_weight(
                0,
                i64::try_from(MICROSECONDS_PER_HOUR).expect("hour fits") - 1
            ),
            1
        );
        assert_eq!(
            segment_hour_weight(0, i64::try_from(MICROSECONDS_PER_HOUR).expect("hour fits")),
            1
        );
        assert_eq!(
            segment_hour_weight(
                0,
                i64::try_from(MICROSECONDS_PER_HOUR * 2).expect("two hours fit")
            ),
            2
        );
        assert!(segment_hour_weight(i64::MIN, i64::MAX) > 1);
    }

    #[test]
    fn complete_key_distinguishes_durable_content_and_lineage() {
        assert_ne!(key(1, 1), key(2, 1));
        assert_ne!(key(1, 1), key(1, 2));
        let current = FactKey::derive(
            SourceScopeId([1; 32]),
            SourceDescriptor([1; 32]),
            FileKind::SegmentFacts,
            1,
            1,
            1,
        );
        let next_extractor = FactKey::derive(
            SourceScopeId([1; 32]),
            SourceDescriptor([1; 32]),
            FileKind::SegmentFacts,
            1,
            2,
            1,
        );
        assert_ne!(
            FallbackFactKey::new(current, SegmentLineageId([1; 32])),
            FallbackFactKey::new(next_extractor, SegmentLineageId([1; 32]))
        );

        let first = facts(1, 1_500, 1_700);
        let replacement = facts(2, 1_500, 1_700);
        let first_key = FallbackFactKey::for_facts(&first);
        let replacement_key = FallbackFactKey::for_facts(&replacement);
        assert_eq!(first_key.durable(), replacement_key.durable());
        assert_ne!(first_key.lineage(), replacement_key.lineage());
        assert_ne!(first_key, replacement_key);
    }

    #[test]
    fn segment_hour_and_byte_budgets_evict_least_recently_used() {
        let first = facts(1, 1_500, 1_700);
        let second = facts(2, 1_500, 1_700);
        let third = facts(3, 1_500, 1_700);
        let first_key = FallbackFactKey::for_facts(&first);
        let second_key = FallbackFactKey::for_facts(&second);
        let third_key = FallbackFactKey::for_facts(&third);
        let mut by_hours = FallbackFactLru::new(config(2, 1_000));
        assert_eq!(
            by_hours
                .insert_after_publication_failure(Arc::clone(&first), canonical_len(10), LIMIT,),
            FallbackInsert::Retained
        );
        by_hours.insert_after_publication_failure(Arc::clone(&second), canonical_len(10), LIMIT);
        by_hours.insert_after_publication_failure(Arc::clone(&third), canonical_len(10), LIMIT);
        assert!(by_hours.get(&first_key, LIMIT).is_none());
        assert!(by_hours.get(&second_key, LIMIT).is_some());
        assert!(by_hours.get(&third_key, LIMIT).is_some());
        assert_eq!(by_hours.stats().resident_segment_hours, 2);

        let mut by_bytes = FallbackFactLru::new(config(10, 20));
        by_bytes.insert_after_publication_failure(first, canonical_len(10), LIMIT);
        by_bytes.insert_after_publication_failure(second, canonical_len(10), LIMIT);
        by_bytes.insert_after_publication_failure(third, canonical_len(10), LIMIT);
        assert!(by_bytes.get(&first_key, LIMIT).is_none());
        assert_eq!(by_bytes.stats().resident_bytes, 20);
        assert_eq!(by_bytes.stats().evictions, 1);
    }

    #[test]
    fn recent_read_updates_recency() {
        let first = facts(1, 1_500, 1_700);
        let second = facts(2, 1_500, 1_700);
        let third = facts(3, 1_500, 1_700);
        let first_key = FallbackFactKey::for_facts(&first);
        let second_key = FallbackFactKey::for_facts(&second);
        let third_key = FallbackFactKey::for_facts(&third);
        let mut cache = FallbackFactLru::new(config(2, 1_000));
        cache.insert_after_publication_failure(first, canonical_len(10), LIMIT);
        cache.insert_after_publication_failure(second, canonical_len(10), LIMIT);
        assert!(cache.get(&first_key, LIMIT).is_some());
        cache.insert_after_publication_failure(third, canonical_len(10), LIMIT);

        assert!(cache.get(&first_key, LIMIT).is_some());
        assert!(cache.get(&second_key, LIMIT).is_none());
        assert!(cache.get(&third_key, LIMIT).is_some());
    }

    #[test]
    fn tighter_request_does_not_reuse_a_more_permissive_admission() {
        let admitted = facts(1, 1_500, 1_700);
        let admitted_key = FallbackFactKey::for_facts(&admitted);
        let mut cache = FallbackFactLru::new(config(2, 100));
        cache.insert_after_publication_failure(admitted, canonical_len(10), LIMIT);
        let tighter = Bounds {
            items_per_block: LIMIT.items_per_block - 1,
            ..LIMIT
        };

        assert!(cache.get(&admitted_key, tighter).is_none());
        assert!(cache.get(&admitted_key, LIMIT).is_some());
    }

    #[test]
    fn admitted_empty_interval_is_charged_one_segment_hour() {
        let admitted = facts(9, 8, 7);
        let mut cache = FallbackFactLru::new(config(1, 100));
        assert_eq!(
            cache.insert_after_publication_failure(admitted, canonical_len(10), LIMIT),
            FallbackInsert::Retained
        );
        assert_eq!(cache.stats().resident_segment_hours, 1);
    }

    #[test]
    fn oversized_entry_is_returned_without_residency() {
        let admitted = facts(1, 1_500, 1_700);
        let admitted_key = FallbackFactKey::for_facts(&admitted);
        let mut cache = FallbackFactLru::new(config(1, 10));
        assert_eq!(
            cache.insert_after_publication_failure(admitted, canonical_len(11), LIMIT),
            FallbackInsert::Oversized
        );
        assert!(cache.get(&admitted_key, LIMIT).is_none());
        assert_eq!(
            cache.stats(),
            FallbackStats {
                misses: 1,
                oversized: 1,
                publication_failure_fallbacks: 1,
                ..FallbackStats::default()
            }
        );

        let two_hours = facts(
            2,
            0,
            i64::try_from(MICROSECONDS_PER_HOUR + 1).expect("hour plus one microsecond fits"),
        );
        let mut cache = FallbackFactLru::new(config(1, 1_000));
        assert_eq!(
            cache.insert_after_publication_failure(two_hours, canonical_len(10), LIMIT),
            FallbackInsert::Oversized
        );
        assert_eq!(cache.stats().resident_entries, 0);
    }

    #[test]
    fn duplicate_key_replaces_arc_without_double_accounting() {
        let first = facts(1, 1_500, 1_700);
        let second = Arc::new(first.as_ref().clone());
        let admitted_key = FallbackFactKey::for_facts(&first);
        let mut cache = FallbackFactLru::new(config(2, 100));
        cache.insert_after_publication_failure(first, canonical_len(10), LIMIT);
        cache.insert_after_publication_failure(Arc::clone(&second), canonical_len(10), LIMIT);

        let loaded = cache
            .get(&admitted_key, LIMIT)
            .expect("resident replacement");
        assert!(Arc::ptr_eq(&loaded, &second));
        assert_eq!(cache.stats().resident_entries, 1);
        assert_eq!(cache.stats().resident_segment_hours, 1);
        assert_eq!(cache.stats().resident_bytes, 10);
        assert_eq!(cache.stats().inserts, 2);
        assert_eq!(cache.stats().evictions, 0);
    }

    #[test]
    fn durable_publication_discards_every_lineage_without_an_eviction() {
        let first = facts(1, 1_500, 1_700);
        let second = facts(2, 1_500, 1_700);
        let durable_key = FallbackFactKey::for_facts(&first).durable();
        assert_eq!(FallbackFactKey::for_facts(&second).durable(), durable_key);
        let mut cache = FallbackFactLru::new(config(2, 100));
        cache.insert_after_publication_failure(first, canonical_len(10), LIMIT);
        cache.insert_after_publication_failure(second, canonical_len(10), LIMIT);

        cache.discard_durable(durable_key);

        assert_eq!(cache.stats().resident_entries, 0);
        assert_eq!(cache.stats().resident_segment_hours, 0);
        assert_eq!(cache.stats().resident_bytes, 0);
        assert_eq!(cache.stats().evictions, 0);
    }

    #[test]
    fn externally_synchronized_duplicate_callers_keep_exact_residency() {
        let admitted = facts(1, 1_500, 1_700);
        let admitted_key = FallbackFactKey::for_facts(&admitted);
        let cache = Arc::new(Mutex::new(FallbackFactLru::new(config(2, 100))));
        let barrier = Arc::new(Barrier::new(4));
        let mut threads = Vec::with_capacity(4);
        for _index in 0..4 {
            let admitted = Arc::clone(&admitted);
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                cache
                    .lock()
                    .expect("fallback mutex")
                    .insert_after_publication_failure(admitted, canonical_len(10), LIMIT);
            }));
        }
        for thread in threads {
            thread.join().expect("fallback caller");
        }

        let mut cache = cache.lock().expect("fallback mutex");
        let loaded = cache.get(&admitted_key, LIMIT).expect("resident facts");
        assert!(Arc::ptr_eq(&loaded, &admitted));
        assert_eq!(cache.stats().resident_entries, 1);
        assert_eq!(cache.stats().resident_segment_hours, 1);
        assert_eq!(cache.stats().resident_bytes, 10);
        assert_eq!(cache.stats().inserts, 4);
        assert_eq!(cache.stats().publication_failure_fallbacks, 4);
    }

    #[test]
    fn counters_saturate() {
        let admitted = facts(1, 1_500, 1_700);
        let admitted_key = FallbackFactKey::for_facts(&admitted);
        let mut cache = FallbackFactLru::new(config(2, 100));
        cache.insert_after_publication_failure(admitted, canonical_len(10), LIMIT);
        cache.counters.hits = u64::MAX;
        assert!(cache.get(&admitted_key, LIMIT).is_some());
        assert_eq!(cache.stats().hits, u64::MAX);
    }
}
