//! Byte-bounded exact response cache for the timeline endpoints (§13.3).
//!
//! The cache stores serialized response bodies keyed by an exact identity: the
//! endpoint, the schema version, the `FactSetId`, the requested range and step,
//! the normalized filters, and the relevant policy versions. Because the
//! `FactSetId` folds in the live journal generation and folded watermark, a live
//! change re-keys the entry — a short TTL is not needed to keep it honest.
//!
//! A hit takes a brief lock only to bump recency, then returns the body as an
//! `Arc` without copying under the lock, so a hit never blocks on the
//! heavy-analysis semaphore (§14.2).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Which timeline endpoint a cached body belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Endpoint {
    /// `GET /v1/timeline/overview`.
    Overview,
    /// `GET /v1/timeline/events`.
    Events,
    /// `GET /v1/timeline/health`.
    Health,
}

/// The exact identity of a cached response body.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ResponseKey {
    /// The endpoint that produced the body.
    pub(crate) endpoint: Endpoint,
    /// The response schema version.
    pub(crate) response_schema_version: u32,
    /// The fact-set identity of the view the body was built from.
    pub(crate) fact_set_id: [u8; 32],
    /// The requested range start.
    pub(crate) from_us: i64,
    /// The requested range end.
    pub(crate) to_us: i64,
    /// The effective health step, when the endpoint buckets.
    pub(crate) step_us: Option<u64>,
    /// The notable policy version applied to the body.
    pub(crate) notable_policy_version: u32,
    /// The health policy version applied to the body.
    pub(crate) health_policy_version: u32,
    /// The normalized response filters (severity, kind), canonical form.
    pub(crate) filters: String,
    /// The page identity for a paginated endpoint (the presented cursor).
    pub(crate) page: Option<String>,
}

/// An exact key proven eligible for serialized response caching.
///
/// Event pages cannot construct this type, so the cache storage API cannot
/// retain them accidentally.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CacheKey(ResponseKey);

impl CacheKey {
    /// Admits only non-paginated overview and health responses.
    pub(crate) fn new(key: ResponseKey) -> Option<Self> {
        (key.endpoint != Endpoint::Events && key.page.is_none()).then_some(Self(key))
    }
}

#[derive(Debug)]
struct Entry {
    body: Arc<[u8]>,
    last_used: u64,
}

#[derive(Debug)]
struct CacheInner {
    entries: HashMap<CacheKey, Entry>,
    clock: u64,
    /// Conservative logical resident bytes attributable to retained responses.
    bytes: usize,
}

/// A byte-bounded LRU cache of serialized response bodies.
#[derive(Debug, Clone)]
pub(crate) struct ResponseCache {
    inner: Arc<Mutex<CacheInner>>,
    max_bytes: usize,
    max_entries: usize,
}

impl ResponseCache {
    /// Creates a cache with explicit byte and entry ceilings.
    pub(crate) fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner {
                entries: HashMap::new(),
                clock: 0,
                bytes: 0,
            })),
            max_bytes,
            max_entries,
        }
    }

    /// Returns the cached body for `key`, bumping its recency.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock must stay held until the body Arc is cloned"
    )]
    pub(crate) fn get(&self, key: &CacheKey) -> Option<Arc<[u8]>> {
        let Ok(mut inner) = self.inner.lock() else {
            metrics::counter!("kronika_web_timeline_response_cache_misses_total").increment(1);
            return None;
        };
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        let body = {
            let Some(entry) = inner.entries.get_mut(key) else {
                drop(inner);
                metrics::counter!("kronika_web_timeline_response_cache_misses_total").increment(1);
                return None;
            };
            entry.last_used = now;
            Arc::clone(&entry.body)
        };
        record_cache_gauges(&inner);
        drop(inner);
        metrics::counter!("kronika_web_timeline_response_cache_hits_total").increment(1);
        Some(body)
    }

    /// Inserts `body` under `key`, evicting least-recently-used entries until
    /// the cache is within its byte budget.
    ///
    /// A body larger than the whole budget is not cached; the response is still
    /// returned to the caller, it is simply not retained.
    pub(crate) fn insert(&self, key: CacheKey, body: Arc<[u8]>) {
        if self.max_entries == 0 {
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
                body,
                last_used: now,
            },
        );

        let mut evicted = 0_u64;
        while inner.entries.len() > self.max_entries {
            evicted = evicted.saturating_add(u64::from(evict_lru(&mut inner.entries)));
        }
        if evicted != 0 {
            inner.entries.shrink_to_fit();
        }
        inner.bytes = logical_resident_charge(&inner.entries);

        while inner.bytes > self.max_bytes && !inner.entries.is_empty() {
            evicted = evicted.saturating_add(u64::from(evict_lru(&mut inner.entries)));
            // The table's spare buckets are part of the budget. Release them
            // before deciding whether another response must be evicted.
            inner.entries.shrink_to_fit();
            inner.bytes = logical_resident_charge(&inner.entries);
        }
        record_cache_gauges(&inner);
        drop(inner);
        if evicted != 0 {
            metrics::counter!("kronika_web_timeline_response_cache_evictions_total")
                .increment(evicted);
        }
    }

    /// The number of retained entries; test-only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().expect("cache lock").entries.len()
    }

    /// The conservative logical resident charge exported by the bytes gauge.
    #[cfg(test)]
    fn resident_bytes(&self) -> usize {
        self.inner.lock().expect("cache lock").bytes
    }
}

/// Removes the least-recently-used entry without retaining a second key copy.
fn evict_lru(entries: &mut HashMap<CacheKey, Entry>) -> bool {
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

/// Conservative logical bytes attributable to the retained cache contents.
///
/// `HashMap::capacity()` is its no-growth element capacity rather than its raw
/// bucket count. Charging two payload-plus-control buckets per capacity
/// deliberately over-accounts both the load-factor slack and the spare buckets.
/// Each dynamic allocation also carries a fixed allocator/header allowance.
fn logical_resident_charge(entries: &HashMap<CacheKey, Entry>) -> usize {
    if entries.is_empty() {
        return 0;
    }

    let bucket_charge = size_of::<(CacheKey, Entry)>().saturating_add(1);
    let table_charge = size_of::<CacheInner>()
        .saturating_add(
            entries
                .capacity()
                .saturating_mul(2)
                .saturating_mul(bucket_charge),
        )
        .saturating_add(32);

    entries.iter().fold(table_charge, |total, (key, entry)| {
        total
            .saturating_add(string_allocation_charge(&key.0.filters))
            .saturating_add(key.0.page.as_ref().map_or(0, string_allocation_charge))
            .saturating_add(arc_body_allocation_charge(entry.body.len()))
    })
}

const fn string_allocation_charge(value: &String) -> usize {
    if value.capacity() == 0 {
        0
    } else {
        value
            .capacity()
            .saturating_add(4_usize.saturating_mul(size_of::<usize>()))
    }
}

const fn arc_body_allocation_charge(body_len: usize) -> usize {
    body_len
        // Strong and weak counters in `ArcInner`.
        .saturating_add(2_usize.saturating_mul(size_of::<usize>()))
        // Conservative allocator metadata/alignment allowance.
        .saturating_add(4_usize.saturating_mul(size_of::<usize>()))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "configured cache bounds remain far below exact f64 integer range"
)]
fn record_cache_gauges(inner: &CacheInner) {
    debug_assert_eq!(
        inner.bytes,
        logical_resident_charge(&inner.entries),
        "the exported bytes gauge must equal the logical resident charge"
    );
    metrics::gauge!("kronika_web_timeline_response_cache_entries").set(inner.entries.len() as f64);
    metrics::gauge!("kronika_web_timeline_response_cache_bytes").set(inner.bytes as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_key(fact_set_id: u8, endpoint: Endpoint, page: Option<&str>) -> ResponseKey {
        ResponseKey {
            endpoint,
            response_schema_version: 1,
            fact_set_id: [fact_set_id; 32],
            from_us: 0,
            to_us: 1_000,
            step_us: None,
            notable_policy_version: 1,
            health_policy_version: 1,
            filters: String::new(),
            page: page.map(str::to_owned),
        }
    }

    fn key(fact_set_id: u8) -> CacheKey {
        CacheKey::new(response_key(fact_set_id, Endpoint::Overview, None)).expect("cacheable key")
    }

    fn body(size: usize) -> Arc<[u8]> {
        vec![0_u8; size].into()
    }

    #[test]
    fn a_stored_body_round_trips() {
        let cache = ResponseCache::new(4_096, 16);
        let key = key(1);
        cache.insert(key.clone(), body(16));
        assert_eq!(cache.get(&key).expect("hit").len(), 16);
    }

    #[test]
    fn a_different_fact_set_id_misses() {
        let cache = ResponseCache::new(1_024, 16);
        cache.insert(key(1), body(16));
        assert!(
            cache.get(&key(2)).is_none(),
            "a live generation change re-keys the entry"
        );
    }

    #[test]
    fn the_byte_budget_evicts_least_recently_used() {
        let sizing_cache = ResponseCache::new(usize::MAX, 2);
        sizing_cache.insert(key(1), body(60));
        sizing_cache.insert(key(2), body(30));
        let two_entry_budget = sizing_cache.resident_bytes();

        let cache = ResponseCache::new(two_entry_budget, 3);
        cache.insert(key(1), body(60));
        cache.insert(key(2), body(30));
        // Touch entry 1 so entry 2 is the least recently used.
        assert!(cache.get(&key(1)).is_some());
        // Inserting a third body exceeds the byte ceiling and evicts entry 2.
        cache.insert(key(3), body(30));
        assert!(cache.get(&key(1)).is_some(), "recently used survives");
        assert!(cache.get(&key(2)).is_none(), "least recently used evicted");
        assert!(cache.get(&key(3)).is_some(), "the new entry is retained");
    }

    #[test]
    fn a_tiny_one_entry_budget_retains_nothing() {
        let cache = ResponseCache::new(1, 1);
        cache.insert(key(1), body(1));
        assert_eq!(
            cache.len(),
            0,
            "container and allocation overhead count toward the byte ceiling"
        );
        assert_eq!(cache.resident_bytes(), 0);
    }

    #[test]
    fn a_one_entry_budget_is_inclusive_at_the_exact_boundary() {
        let sizing_cache = ResponseCache::new(usize::MAX, 1);
        sizing_cache.insert(key(1), body(17));
        let boundary = sizing_cache.resident_bytes();
        assert!(boundary > 17, "the charge includes cache overhead");

        let exact_cache = ResponseCache::new(boundary, 1);
        exact_cache.insert(key(1), body(17));
        assert_eq!(exact_cache.len(), 1);
        assert_eq!(exact_cache.resident_bytes(), boundary);

        let below_cache = ResponseCache::new(boundary - 1, 1);
        below_cache.insert(key(1), body(17));
        assert_eq!(below_cache.len(), 0);
        assert_eq!(below_cache.resident_bytes(), 0);
    }

    #[test]
    fn an_oversized_body_is_not_retained() {
        let cache = ResponseCache::new(10, 16);
        cache.insert(key(1), body(64));
        assert_eq!(
            cache.len(),
            0,
            "a body above the whole budget is not cached"
        );
    }

    #[test]
    fn event_pages_are_never_retained() {
        let cache = ResponseCache::new(1_024, 16);
        let event_key = response_key(1, Endpoint::Events, Some("cursor"));
        assert!(CacheKey::new(event_key).is_none());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn entry_ceiling_bounds_many_small_bodies() {
        let cache = ResponseCache::new(64 * 1024, 2);
        cache.insert(key(1), body(1));
        cache.insert(key(2), body(1));
        cache.insert(key(3), body(1));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&key(1)).is_none());
    }

    #[test]
    fn reported_bytes_equal_the_cache_charge() {
        let cache = ResponseCache::new(64 * 1024, 4);
        cache.insert(key(1), body(7));
        cache.insert(key(2), body(11));

        let (reported, charged) = {
            let inner = cache.inner.lock().expect("cache lock");
            (inner.bytes, logical_resident_charge(&inner.entries))
        };
        assert_eq!(reported, charged);
    }
}
