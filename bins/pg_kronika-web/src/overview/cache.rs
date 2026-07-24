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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Endpoint {
    /// `GET /v1/timeline/overview`.
    Overview,
    /// `GET /v1/timeline/events`.
    Events,
    /// `GET /v1/timeline/health`.
    Health,
}

/// The exact identity of a cached response body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
}

#[derive(Debug)]
struct CacheInner {
    entries: HashMap<ResponseKey, Entry>,
    clock: u64,
    bytes: usize,
}

/// A byte-bounded LRU cache of serialized response bodies.
#[derive(Debug, Clone)]
pub(crate) struct ResponseCache {
    inner: Arc<Mutex<CacheInner>>,
    max_bytes: usize,
}

impl ResponseCache {
    /// Creates a cache holding at most `max_bytes` of response bodies.
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner {
                entries: HashMap::new(),
                clock: 0,
                bytes: 0,
            })),
            max_bytes,
        }
    }

    /// Returns the cached body for `key`, bumping its recency.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock must stay held until the body Arc is cloned"
    )]
    pub(crate) fn get(&self, key: &ResponseKey) -> Option<Arc<[u8]>> {
        let mut inner = self.inner.lock().ok()?;
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        let entry = inner.entries.get_mut(key)?;
        entry.last_used = now;
        Some(Arc::clone(&entry.body))
    }

    /// Inserts `body` under `key`, evicting least-recently-used entries until
    /// the cache is within its byte budget.
    ///
    /// A body larger than the whole budget is not cached; the response is still
    /// returned to the caller, it is simply not retained.
    pub(crate) fn insert(&self, key: ResponseKey, body: Arc<[u8]>) {
        if body.len() > self.max_bytes {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clock = inner.clock.wrapping_add(1);
        let now = inner.clock;
        if let Some(previous) = inner.entries.remove(&key) {
            inner.bytes = inner.bytes.saturating_sub(previous.body.len());
        }
        let len = body.len();
        inner.entries.insert(
            key,
            Entry {
                body,
                last_used: now,
            },
        );
        inner.bytes = inner.bytes.saturating_add(len);
        while inner.bytes > self.max_bytes {
            let Some(victim) = inner
                .entries
                .iter()
                .min_by_key(|(_key, entry)| entry.last_used)
                .map(|(key, _entry)| key.clone())
            else {
                break;
            };
            if let Some(entry) = inner.entries.remove(&victim) {
                inner.bytes = inner.bytes.saturating_sub(entry.body.len());
            }
        }
    }

    /// The number of retained entries; test-only.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().expect("cache lock").entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fact_set_id: u8, page: Option<&str>) -> ResponseKey {
        ResponseKey {
            endpoint: Endpoint::Events,
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
        let cache = ResponseCache::new(1_024);
        let key = key(1, None);
        cache.insert(key.clone(), body(16));
        assert_eq!(cache.get(&key).expect("hit").len(), 16);
    }

    #[test]
    fn a_different_fact_set_id_misses() {
        let cache = ResponseCache::new(1_024);
        cache.insert(key(1, None), body(16));
        assert!(
            cache.get(&key(2, None)).is_none(),
            "a live generation change re-keys the entry"
        );
    }

    #[test]
    fn the_byte_budget_evicts_least_recently_used() {
        let cache = ResponseCache::new(100);
        cache.insert(key(1, None), body(60));
        cache.insert(key(2, None), body(30));
        // Touch entry 1 so entry 2 is the least recently used.
        assert!(cache.get(&key(1, None)).is_some());
        // Inserting a third body overflows the budget and evicts entry 2.
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
        let cache = ResponseCache::new(10);
        cache.insert(key(1, None), body(64));
        assert_eq!(
            cache.len(),
            0,
            "a body above the whole budget is not cached"
        );
    }
}
