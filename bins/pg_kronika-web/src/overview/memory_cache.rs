//! Byte-bounded decoded fact cache between durable facts and response bodies.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kronika_reader::{FactBuildKey, SegmentFacts};

#[derive(Debug)]
struct Entry {
    facts: Arc<SegmentFacts>,
    last_used: u64,
}

#[derive(Debug, Default)]
struct CacheInner {
    entries: HashMap<FactBuildKey, Entry>,
    clock: u64,
    bytes: usize,
}

/// Process-local L2 cache of immutable decoded segment facts.
///
/// The byte ceiling is primary. The entry ceiling also bounds empty/small
/// values and hash-table metadata. A retained value is charged at its full
/// logical resident size even while a request holds another `Arc`.
#[derive(Debug, Clone)]
pub(crate) struct DecodedFactCache {
    inner: Arc<Mutex<CacheInner>>,
    max_bytes: usize,
    max_entries: usize,
}

impl DecodedFactCache {
    pub(crate) fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner::default())),
            max_bytes,
            max_entries,
        }
    }

    /// Returns an exact decoded fact set without entering cold admission.
    pub(crate) fn get(&self, key: FactBuildKey) -> Option<Arc<SegmentFacts>> {
        let Ok(mut inner) = self.inner.lock() else {
            record_lookup("miss", "lock_poisoned");
            return None;
        };
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        let Some(entry) = inner.entries.get_mut(&key) else {
            drop(inner);
            record_lookup("miss", "absent");
            return None;
        };
        entry.last_used = now;
        let facts = Arc::clone(&entry.facts);
        record_gauges(&inner);
        drop(inner);
        record_lookup("hit", "none");
        Some(facts)
    }

    /// Retains an exact decoded fact set if it fits the configured L2 budget.
    pub(crate) fn insert(&self, key: FactBuildKey, facts: Arc<SegmentFacts>) {
        let resident_bytes = facts.resident_bytes();
        if resident_bytes.is_none() {
            metrics::counter!("overview_overflow_total", "kind" => "decoded_fact_charge")
                .increment(1);
        }
        if self.max_bytes == 0 || self.max_entries == 0 || resident_bytes.is_none() {
            metrics::counter!(
                "overview_cache_evictions_total",
                "class" => "decoded_facts",
                "reason" => "entry_unadmitted"
            )
            .increment(1);
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        inner.entries.insert(
            key,
            Entry {
                facts,
                last_used: now,
            },
        );
        inner.bytes = logical_resident_charge(&inner.entries).unwrap_or(usize::MAX);

        let mut entry_evictions = 0_u64;
        while inner.entries.len() > self.max_entries {
            entry_evictions =
                entry_evictions.saturating_add(u64::from(evict_lru(&mut inner.entries)));
        }
        if entry_evictions != 0 {
            inner.entries.shrink_to_fit();
            inner.bytes = logical_resident_charge(&inner.entries).unwrap_or(usize::MAX);
        }

        let mut byte_evictions = 0_u64;
        while inner.bytes > self.max_bytes && !inner.entries.is_empty() {
            byte_evictions =
                byte_evictions.saturating_add(u64::from(evict_lru(&mut inner.entries)));
            inner.entries.shrink_to_fit();
            inner.bytes = logical_resident_charge(&inner.entries).unwrap_or(usize::MAX);
        }
        record_gauges(&inner);
        drop(inner);

        if entry_evictions != 0 {
            metrics::counter!(
                "overview_cache_evictions_total",
                "class" => "decoded_facts",
                "reason" => "entry_limit"
            )
            .increment(entry_evictions);
        }
        if byte_evictions != 0 {
            metrics::counter!(
                "overview_cache_evictions_total",
                "class" => "decoded_facts",
                "reason" => "byte_limit"
            )
            .increment(byte_evictions);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("cache lock").entries.len()
    }

    #[cfg(test)]
    fn resident_bytes(&self) -> usize {
        self.inner.lock().expect("cache lock").bytes
    }
}

fn evict_lru(entries: &mut HashMap<FactBuildKey, Entry>) -> bool {
    let Some(oldest) = entries.values().map(|entry| entry.last_used).min() else {
        return false;
    };
    let mut removed = false;
    entries.retain(|_, entry| {
        if !removed && entry.last_used == oldest {
            removed = true;
            false
        } else {
            true
        }
    });
    removed
}

fn logical_resident_charge(entries: &HashMap<FactBuildKey, Entry>) -> Option<usize> {
    if entries.is_empty() {
        return Some(0);
    }
    let bucket = size_of::<(FactBuildKey, Entry)>().checked_add(1)?;
    let table = size_of::<CacheInner>()
        .checked_add(entries.capacity().checked_mul(2)?.checked_mul(bucket)?)?
        .checked_add(4_usize.checked_mul(size_of::<usize>())?)?;
    entries.values().try_fold(table, |total, entry| {
        total
            .checked_add(entry.facts.resident_bytes()?)?
            .checked_add(6_usize.checked_mul(size_of::<usize>())?)
    })
}

fn record_lookup(result: &'static str, reason: &'static str) {
    metrics::counter!(
        "overview_fact_lookup_total",
        "layer" => "l2",
        "result" => result,
        "reason" => reason
    )
    .increment(1);
}

#[allow(
    clippy::cast_precision_loss,
    reason = "configured memory ceilings remain below exact f64 integer range"
)]
fn record_gauges(inner: &CacheInner) {
    debug_assert_eq!(
        Some(inner.bytes),
        logical_resident_charge(&inner.entries),
        "the L2 gauge must equal its conservative logical charge"
    );
    metrics::gauge!("overview_cache_entries", "class" => "decoded_facts")
        .set(inner.entries.len() as f64);
    metrics::gauge!("overview_cache_bytes", "class" => "decoded_facts").set(inner.bytes as f64);
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{NamingContractId, SegmentLocator};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_reader::{FactKey, FileKind, LIMIT, PgmUnit, SegmentContext};
    use kronika_registry::Section as _;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::*;

    fn fixture(seed: u8) -> (FactBuildKey, Arc<SegmentFacts>) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: i64::from(seed),
                max_ts: i64::from(seed) + 1,
                source_id: u64::from(seed),
            },
        );
        let unit = PgmUnit::open(bytes).expect("open fixture");
        let context = SegmentContext::new(
            b"cache-test".to_vec(),
            NamingContractId([1; 16]),
            SegmentLocator([seed; 32]),
        )
        .expect("context");
        let facts = Arc::new(
            SegmentFacts::extract(&unit, &context, &LIMIT).expect("extract fixture facts"),
        );
        let key = FactBuildKey::new(
            FactKey::for_identity(facts.identity(), FileKind::SegmentFacts),
            facts.lineage().id(),
        );
        (key, facts)
    }

    #[test]
    fn exact_key_round_trips_the_same_arc() {
        let (key, facts) = fixture(1);
        let cache = DecodedFactCache::new(16 * 1024 * 1024, 4);
        cache.insert(key, Arc::clone(&facts));
        let hit = cache.get(key).expect("cache hit");
        assert!(Arc::ptr_eq(&facts, &hit));
    }

    #[test]
    fn entry_limit_evicts_the_least_recently_used_fact() {
        let (first_key, first) = fixture(1);
        let (second_key, second) = fixture(2);
        let cache = DecodedFactCache::new(16 * 1024 * 1024, 1);
        cache.insert(first_key, first);
        cache.insert(second_key, second);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(first_key).is_none());
        assert!(cache.get(second_key).is_some());
    }

    #[test]
    fn byte_limit_counts_container_and_arc_overhead() {
        let (key, facts) = fixture(3);
        let sizing = DecodedFactCache::new(usize::MAX, 1);
        sizing.insert(key, Arc::clone(&facts));
        let exact = sizing.resident_bytes();
        assert!(exact > facts.resident_bytes().expect("fact charge"));

        let cache = DecodedFactCache::new(exact - 1, 1);
        cache.insert(key, facts);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.resident_bytes(), 0);
    }
}
