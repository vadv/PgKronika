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

use std::collections::{BTreeSet, HashMap};
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

#[derive(Debug)]
struct Entry {
    body: Arc<[u8]>,
    last_used: u64,
    charge: usize,
}

#[derive(Debug)]
struct CacheInner {
    entries: HashMap<ResponseKey, Entry>,
    recency: BTreeSet<(u64, ResponseKey)>,
    clock: u64,
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
                recency: BTreeSet::new(),
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
    pub(crate) fn get(&self, key: &ResponseKey) -> Option<Arc<[u8]>> {
        if key.endpoint == Endpoint::Events {
            return None;
        }
        let mut inner = self.inner.lock().ok()?;
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        let (previous, body) = {
            let entry = inner.entries.get_mut(key)?;
            let previous = entry.last_used;
            entry.last_used = now;
            (previous, Arc::clone(&entry.body))
        };
        inner.recency.remove(&(previous, key.clone()));
        inner.recency.insert((now, key.clone()));
        Some(body)
    }

    /// Inserts `body` under `key`, evicting least-recently-used entries until
    /// the cache is within its byte budget.
    ///
    /// A body larger than the whole budget is not cached; the response is still
    /// returned to the caller, it is simply not retained.
    pub(crate) fn insert(&self, key: ResponseKey, body: Arc<[u8]>) {
        if key.endpoint == Endpoint::Events || self.max_entries == 0 {
            return;
        }
        let charge = response_charge(&key, body.len());
        if charge > self.max_bytes {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        if let Some(previous) = inner.entries.remove(&key) {
            inner.recency.remove(&(previous.last_used, key.clone()));
            inner.bytes = inner.bytes.saturating_sub(previous.charge);
        }
        inner.recency.insert((now, key.clone()));
        inner.entries.insert(
            key,
            Entry {
                body,
                last_used: now,
                charge,
            },
        );
        inner.bytes = inner.bytes.saturating_add(charge);
        while inner.bytes > self.max_bytes || inner.entries.len() > self.max_entries {
            let Some((last_used, victim)) = inner.recency.pop_first() else {
                break;
            };
            if let Some(entry) = inner.entries.remove(&victim) {
                debug_assert_eq!(
                    entry.last_used, last_used,
                    "recency index and cache entry must agree"
                );
                inner.bytes = inner.bytes.saturating_sub(entry.charge);
            }
        }
    }

    /// The number of retained entries; test-only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().expect("cache lock").entries.len()
    }
}

fn response_charge(key: &ResponseKey, body_len: usize) -> usize {
    size_of::<ResponseKey>()
        .saturating_add(size_of::<Entry>())
        .saturating_add(key.filters.len())
        .saturating_add(key.page.as_ref().map_or(0, String::len))
        .saturating_add(body_len)
        .saturating_add(64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fact_set_id: u8, page: Option<&str>) -> ResponseKey {
        ResponseKey {
            endpoint: Endpoint::Overview,
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

    fn body(size: usize) -> Arc<[u8]> {
        vec![0_u8; size].into()
    }

    #[test]
    fn a_stored_body_round_trips() {
        let cache = ResponseCache::new(1_024, 16);
        let key = key(1, None);
        cache.insert(key.clone(), body(16));
        assert_eq!(cache.get(&key).expect("hit").len(), 16);
    }

    #[test]
    fn a_different_fact_set_id_misses() {
        let cache = ResponseCache::new(1_024, 16);
        cache.insert(key(1, None), body(16));
        assert!(
            cache.get(&key(2, None)).is_none(),
            "a live generation change re-keys the entry"
        );
    }

    #[test]
    fn the_byte_budget_evicts_least_recently_used() {
        let cache = ResponseCache::new(1_024, 2);
        cache.insert(key(1, None), body(60));
        cache.insert(key(2, None), body(30));
        // Touch entry 1 so entry 2 is the least recently used.
        assert!(cache.get(&key(1, None)).is_some());
        // Inserting a third body exceeds the entry ceiling and evicts entry 2.
        cache.insert(key(3, None), body(30));
        assert!(cache.get(&key(1, None)).is_some(), "recently used survives");
        assert!(
            cache.get(&key(2, None)).is_none(),
            "least recently used evicted"
        );
        assert!(
            cache.get(&key(3, None)).is_some(),
            "the new entry is retained"
        );
    }

    #[test]
    fn an_oversized_body_is_not_retained() {
        let cache = ResponseCache::new(10, 16);
        cache.insert(key(1, None), body(64));
        assert_eq!(
            cache.len(),
            0,
            "a body above the whole budget is not cached"
        );
    }

    #[test]
    fn event_pages_are_never_retained() {
        let cache = ResponseCache::new(1_024, 16);
        let mut event_key = key(1, Some("cursor"));
        event_key.endpoint = Endpoint::Events;
        cache.insert(event_key.clone(), body(16));
        assert!(cache.get(&event_key).is_none());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn entry_ceiling_bounds_many_small_bodies() {
        let cache = ResponseCache::new(64 * 1024, 2);
        cache.insert(key(1, None), body(1));
        cache.insert(key(2, None), body(1));
        cache.insert(key(3, None), body(1));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&key(1, None)).is_none());
    }
}
