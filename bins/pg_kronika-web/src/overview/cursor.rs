//! Opaque, authenticated pagination cursor for `/v1/timeline/events`.
//!
//! A cursor is a server-authenticated continuation token. It binds a canonical
//! source set, query and three-part event position to one count/logical-byte/TTL
//! bounded pinned [`IndexView`]. A continuation resolves that exact immutable
//! view even when refresh has published a newer one.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::view::IndexView;

/// The cursor encoding version.
const CURSOR_VERSION: u16 = 2;

/// Length of the authenticated payload, in bytes.
const PAYLOAD_LEN: usize = 2 + 16 + 8 + 8 + 32 + 32 + 32 + 8 + 32 + 32;

/// Length of the appended message authentication code, in bytes.
const MAC_LEN: usize = 32;

fn secret() -> std::io::Result<[u8; 32]> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn key_id(secret: &[u8; 32]) -> [u8; 16] {
    let digest = Sha256::digest(secret);
    digest[..16].try_into().expect("fixed digest prefix")
}

/// Bounded pinned-view policy for event cursors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CursorConfig {
    pub(crate) max_views: usize,
    pub(crate) max_bytes: usize,
    pub(crate) ttl_secs: u64,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            max_views: 64,
            max_bytes: 512 * 1024 * 1024,
            ttl_secs: 300,
        }
    }
}

struct PinnedView {
    view: Arc<IndexView>,
    charge: usize,
    expires_at: u64,
    last_used: u64,
}

#[derive(Default)]
struct RegistryInner {
    views: BTreeMap<RegistryKey, PinnedView>,
    recency: BTreeSet<(u64, RegistryKey)>,
    bytes: usize,
    clock: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RegistryKey {
    fact_set_id: [u8; 32],
    source_set_hash: [u8; 32],
}

/// Application-owned cursor key and bounded pinned-view registry.
pub(crate) struct CursorRegistry {
    secret: [u8; 32],
    key_id: [u8; 16],
    config: CursorConfig,
    inner: Mutex<RegistryInner>,
}

impl std::fmt::Debug for CursorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorRegistry")
            .field("secret", &"<redacted>")
            .field("key_id", &self.key_id)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl CursorRegistry {
    /// Creates a registry using operating-system entropy.
    pub(crate) fn new(config: CursorConfig) -> std::io::Result<Self> {
        if config.max_views == 0 || config.max_bytes == 0 || config.ttl_secs == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cursor count, logical-byte, and TTL bounds must be non-zero",
            ));
        }
        let secret = secret()?;
        Ok(Self {
            key_id: key_id(&secret),
            secret,
            config,
            inner: Mutex::new(RegistryInner::default()),
        })
    }

    /// Pins a view and returns its authenticated lease.
    pub(crate) fn pin(
        &self,
        view: Arc<IndexView>,
        source_set_hash: [u8; 32],
        now_secs: u64,
    ) -> Result<CursorLease, CursorError> {
        if self.config.max_views == 0 || self.config.ttl_secs == 0 {
            return Err(capacity_unavailable());
        }
        let charge = view
            .resident_bytes()
            .filter(|charge| *charge != 0)
            .ok_or_else(capacity_unavailable)?;
        if charge > self.config.max_bytes {
            return Err(capacity_unavailable());
        }
        let expires_at = now_secs
            .checked_add(self.config.ttl_secs)
            .ok_or_else(capacity_unavailable)?;
        let fact_set_id = view.fact_set_id();
        let registry_key = RegistryKey {
            fact_set_id,
            source_set_hash,
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = prune_expired(&mut inner, now_secs);
        inner.clock = inner.clock.wrapping_add(1);
        let last_used = inner.clock;
        if let Some(previous) = inner.views.remove(&registry_key) {
            inner.recency.remove(&(previous.last_used, registry_key));
            inner.bytes = inner
                .bytes
                .checked_sub(previous.charge)
                .expect("cursor registry byte charge must match retained views");
        }
        let mut evicted = 0_u64;
        while inner.views.len() >= self.config.max_views
            || inner
                .bytes
                .checked_add(charge)
                .is_none_or(|bytes| bytes > self.config.max_bytes)
        {
            let Some((_, victim)) = inner.recency.pop_first() else {
                record_registry_metrics(&inner, expired, evicted);
                return Err(capacity_unavailable());
            };
            if let Some(removed) = inner.views.remove(&victim) {
                inner.bytes = inner
                    .bytes
                    .checked_sub(removed.charge)
                    .expect("cursor registry byte charge must match retained views");
                evicted = evicted.saturating_add(1);
            }
        }
        inner.bytes = inner
            .bytes
            .checked_add(charge)
            .ok_or_else(capacity_unavailable)?;
        inner.recency.insert((last_used, registry_key));
        inner.views.insert(
            registry_key,
            PinnedView {
                view,
                charge,
                expires_at,
                last_used,
            },
        );
        record_registry_metrics(&inner, expired, evicted);
        drop(inner);
        metrics::counter!("kronika_web_timeline_cursor_pins_total").increment(1);
        Ok(CursorLease {
            fact_set_id,
            issued_at: now_secs,
            expires_at,
        })
    }

    /// Resolves and touches one unexpired pinned view.
    pub(crate) fn resolve(
        &self,
        fact_set_id: [u8; 32],
        source_set_hash: [u8; 32],
        now_secs: u64,
    ) -> Result<Arc<IndexView>, CursorError> {
        let registry_key = RegistryKey {
            fact_set_id,
            source_set_hash,
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = prune_expired(&mut inner, now_secs);
        inner.clock = inner.clock.wrapping_add(1);
        let last_used = inner.clock;
        let (previous, view) = {
            let pinned = inner
                .views
                .get_mut(&registry_key)
                .ok_or(CursorError::ViewGone)?;
            let previous = pinned.last_used;
            pinned.last_used = last_used;
            (previous, Arc::clone(&pinned.view))
        };
        inner.recency.remove(&(previous, registry_key));
        inner.recency.insert((last_used, registry_key));
        record_registry_metrics(&inner, expired, 0);
        drop(inner);
        metrics::counter!("kronika_web_timeline_cursor_resolves_total").increment(1);
        Ok(view)
    }

    /// Removes expired views even when no cursor request arrives.
    pub(crate) fn prune(&self, now_secs: u64) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = prune_expired(&mut inner, now_secs);
        record_registry_metrics(&inner, expired, 0);
        drop(inner);
    }
}

fn capacity_unavailable() -> CursorError {
    metrics::counter!("kronika_web_timeline_cursor_capacity_rejections_total").increment(1);
    CursorError::CapacityUnavailable
}

fn prune_expired(inner: &mut RegistryInner, now_secs: u64) -> u64 {
    let expired = inner
        .views
        .iter()
        .filter_map(|(id, pinned)| (pinned.expires_at <= now_secs).then_some(*id))
        .collect::<Vec<_>>();
    let expired_count = u64::try_from(expired.len()).unwrap_or(u64::MAX);
    for id in expired {
        if let Some(removed) = inner.views.remove(&id) {
            inner.recency.remove(&(removed.last_used, id));
            inner.bytes = inner
                .bytes
                .checked_sub(removed.charge)
                .expect("cursor registry byte charge must match retained views");
        }
    }
    expired_count
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Prometheus gauges use f64; cursor limits stay far below 2^53"
)]
fn record_registry_metrics(inner: &RegistryInner, expired: u64, evicted: u64) {
    metrics::gauge!("kronika_web_timeline_cursor_views").set(inner.views.len() as f64);
    metrics::gauge!("kronika_web_timeline_cursor_bytes").set(inner.bytes as f64);
    if expired != 0 {
        metrics::counter!("kronika_web_timeline_cursor_expired_total").increment(expired);
    }
    if evicted != 0 {
        metrics::counter!("kronika_web_timeline_cursor_evictions_total").increment(evicted);
    }
}

/// Authenticated lifetime of one pinned view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorLease {
    pub(crate) fact_set_id: [u8; 32],
    pub(crate) issued_at: u64,
    pub(crate) expires_at: u64,
}

/// The decoded position of a `/events` cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventsCursor {
    /// The pinned view lease.
    pub(crate) lease: CursorLease,
    /// Canonical selected-source-set hash.
    pub(crate) source_set_hash: [u8; 32],
    /// The hash binding range, filters, order, and policy versions.
    pub(crate) query_hash: [u8; 32],
    /// The sort timestamp of the last item served.
    pub(crate) last_ts_us: i64,
    /// The id of the last item served, breaking timestamp ties.
    pub(crate) last_event_id: [u8; 32],
    /// The physical event-instance ID, breaking semantic event-ID ties.
    pub(crate) last_event_instance_id: [u8; 32],
}

/// A failure while validating a presented cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    /// The token failed to decode, or its authentication code was wrong.
    Invalid,
    /// The query the cursor was issued against no longer matches.
    QueryMismatch,
    /// The pinned view generation is gone.
    ViewGone,
    /// The authenticated cursor lease expired or belongs to a prior process.
    Expired,
    /// The view could not fit in the configured cursor registry.
    CapacityUnavailable,
}

impl EventsCursor {
    /// Encodes an authenticated opaque continuation token.
    pub(crate) fn encode(self, registry: &CursorRegistry) -> String {
        let mut payload = Vec::with_capacity(PAYLOAD_LEN + MAC_LEN);
        payload.extend_from_slice(&CURSOR_VERSION.to_le_bytes());
        payload.extend_from_slice(&registry.key_id);
        payload.extend_from_slice(&self.lease.issued_at.to_le_bytes());
        payload.extend_from_slice(&self.lease.expires_at.to_le_bytes());
        payload.extend_from_slice(&self.lease.fact_set_id);
        payload.extend_from_slice(&self.source_set_hash);
        payload.extend_from_slice(&self.query_hash);
        payload.extend_from_slice(&self.last_ts_us.to_le_bytes());
        payload.extend_from_slice(&self.last_event_id);
        payload.extend_from_slice(&self.last_event_instance_id);
        let mac = hmac_sha256(&registry.secret, &payload);
        payload.extend_from_slice(&mac);
        URL_SAFE_NO_PAD.encode(payload)
    }

    /// Decodes and authenticates a token, then checks it against the current
    /// query.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Invalid`] on a decode or authentication failure,
    /// [`CursorError::QueryMismatch`] when the bound query or source set
    /// differs, and [`CursorError::Expired`] when the lease is no longer
    /// usable.
    pub(crate) fn decode(
        token: &str,
        registry: &CursorRegistry,
        expected_query_hash: [u8; 32],
        expected_source_set_hash: [u8; 32],
        now_secs: u64,
    ) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_error| CursorError::Invalid)?;
        if bytes.len() != PAYLOAD_LEN + MAC_LEN {
            return Err(CursorError::Invalid);
        }
        let (payload, mac) = bytes.split_at(PAYLOAD_LEN);
        let version = u16::from_le_bytes([payload[0], payload[1]]);
        if version != CURSOR_VERSION {
            return Err(CursorError::Invalid);
        }
        let presented_key_id: [u8; 16] = payload[2..18]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        if presented_key_id.ct_eq(&registry.key_id).unwrap_u8() != 1 {
            return Err(CursorError::Expired);
        }
        let expected_mac = hmac_sha256(&registry.secret, payload);
        if expected_mac.ct_eq(mac).unwrap_u8() != 1 {
            return Err(CursorError::Invalid);
        }
        let issued_at = u64::from_le_bytes(
            payload[18..26]
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        );
        let expires_at = u64::from_le_bytes(
            payload[26..34]
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        );
        let fact_set_id: [u8; 32] = payload[34..66]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let source_set_hash: [u8; 32] = payload[66..98]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let query_hash: [u8; 32] = payload[98..130]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let last_ts_us = i64::from_le_bytes(
            payload[130..138]
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        );
        let last_event_id: [u8; 32] = payload[138..170]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let last_event_instance_id: [u8; 32] = payload[170..202]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;

        if query_hash.ct_eq(&expected_query_hash).unwrap_u8() != 1
            || source_set_hash.ct_eq(&expected_source_set_hash).unwrap_u8() != 1
        {
            return Err(CursorError::QueryMismatch);
        }
        if issued_at > expires_at || expires_at <= now_secs {
            return Err(CursorError::Expired);
        }
        Ok(Self {
            lease: CursorLease {
                fact_set_id,
                issued_at,
                expires_at,
            },
            source_set_hash,
            query_hash,
            last_ts_us,
            last_event_id,
            last_event_instance_id,
        })
    }
}

/// Computes HMAC-SHA256 over `message` with `key` (RFC 2104).
fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
    const BLOCK_LEN: usize = 64;
    let mut ipad = [0x36_u8; BLOCK_LEN];
    let mut opad = [0x5c_u8; BLOCK_LEN];
    for (index, byte) in key.iter().enumerate() {
        ipad[index] ^= byte;
        opad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner: [u8; 32] = inner.finalize().into();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_reader::{
        JournalDelta, JournalGenerationId, LIMIT, LiveBuilder, PartTransition, RefreshDelta,
    };

    fn registry() -> CursorRegistry {
        CursorRegistry::new(CursorConfig::default()).expect("cursor entropy")
    }

    fn empty_view(view_generation: u64) -> Arc<IndexView> {
        let mut builder = LiveBuilder::new(b"cursor-test".to_vec(), LIMIT).expect("live builder");
        let delta = RefreshDelta {
            previous_view_generation: view_generation.saturating_sub(1),
            new_view_generation: view_generation,
            view_changed: true,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: JournalDelta {
                bootstrap: true,
                generation_id: JournalGenerationId(view_generation),
                previous_valid_len: 0,
                new_valid_len: 0,
                completed_parts: Vec::new(),
                current_parts: Vec::new(),
                current_parts_complete: true,
                transition: PartTransition::Append,
                tail_pending: None,
                damages: Vec::new(),
            },
        };
        builder.begin_refresh(&delta).expect("begin refresh");
        builder.complete_refresh().expect("complete refresh");
        Arc::new(IndexView::new(
            view_generation,
            Vec::new(),
            Arc::new(builder.publish()),
            false,
        ))
    }

    fn cursor(now_secs: u64) -> EventsCursor {
        EventsCursor {
            lease: CursorLease {
                fact_set_id: [7; 32],
                issued_at: now_secs,
                expires_at: now_secs + 300,
            },
            source_set_hash: [2; 32],
            query_hash: [3; 32],
            last_ts_us: 1_234,
            last_event_id: [9; 32],
            last_event_instance_id: [8; 32],
        }
    }

    #[test]
    fn a_round_trip_recovers_the_position() {
        let registry = registry();
        let token = cursor(100).encode(&registry);
        let decoded =
            EventsCursor::decode(&token, &registry, [3; 32], [2; 32], 101).expect("valid cursor");
        assert_eq!(decoded, cursor(100));
    }

    #[test]
    fn a_tampered_token_fails_authentication() {
        let registry = registry();
        let mut token = cursor(100).encode(&registry).into_bytes();
        let last = token.len() - 1;
        token[last] ^= 0x01;
        let tampered = String::from_utf8(token).expect("ascii token");
        assert_eq!(
            EventsCursor::decode(&tampered, &registry, [3; 32], [2; 32], 101),
            Err(CursorError::Invalid)
        );
    }

    #[test]
    fn a_changed_query_is_rejected() {
        let registry = registry();
        let token = cursor(100).encode(&registry);
        assert_eq!(
            EventsCursor::decode(&token, &registry, [4; 32], [2; 32], 101),
            Err(CursorError::QueryMismatch)
        );
    }

    #[test]
    fn an_expired_lease_is_rejected() {
        let registry = registry();
        let token = cursor(100).encode(&registry);
        assert_eq!(
            EventsCursor::decode(&token, &registry, [3; 32], [2; 32], 401),
            Err(CursorError::Expired)
        );
    }

    #[test]
    fn a_prior_process_key_is_reported_as_expired() {
        let first = registry();
        let second = registry();
        let token = cursor(100).encode(&first);
        assert_eq!(
            EventsCursor::decode(&token, &second, [3; 32], [2; 32], 101),
            Err(CursorError::Expired)
        );
    }

    #[test]
    fn a_registry_key_includes_the_canonical_source_set() {
        let registry = registry();
        let view = empty_view(1);
        let lease = registry
            .pin(Arc::clone(&view), [1; 32], 100)
            .expect("pin view");
        assert!(registry.resolve(lease.fact_set_id, [1; 32], 101).is_ok());
        assert!(matches!(
            registry.resolve(lease.fact_set_id, [2; 32], 101),
            Err(CursorError::ViewGone)
        ));
    }

    #[test]
    fn a_future_generation_is_gone() {
        let registry = registry();
        let source_set_hash = [1; 32];
        registry
            .pin(empty_view(6), source_set_hash, 100)
            .expect("pin current view");
        let future_fact_set_id = empty_view(7).fact_set_id();
        assert!(matches!(
            registry.resolve(future_fact_set_id, source_set_hash, 101),
            Err(CursorError::ViewGone)
        ));
    }

    #[test]
    fn the_count_bound_evicts_the_least_recently_used_view() {
        let registry = CursorRegistry::new(CursorConfig {
            max_views: 1,
            max_bytes: usize::MAX,
            ttl_secs: 30,
        })
        .expect("registry");
        let first = registry
            .pin(empty_view(1), [1; 32], 100)
            .expect("first pin");
        let second = registry
            .pin(empty_view(2), [1; 32], 101)
            .expect("second pin");
        assert!(matches!(
            registry.resolve(first.fact_set_id, [1; 32], 102),
            Err(CursorError::ViewGone)
        ));
        assert!(registry.resolve(second.fact_set_id, [1; 32], 102).is_ok());
    }

    #[test]
    fn pruning_releases_a_view_at_the_exact_expiry_boundary() {
        let registry = CursorRegistry::new(CursorConfig {
            max_views: 2,
            max_bytes: usize::MAX,
            ttl_secs: 10,
        })
        .expect("registry");
        let lease = registry.pin(empty_view(1), [1; 32], 100).expect("pin");
        assert!(registry.resolve(lease.fact_set_id, [1; 32], 109).is_ok());
        registry.prune(110);
        assert!(matches!(
            registry.resolve(lease.fact_set_id, [1; 32], 110),
            Err(CursorError::ViewGone)
        ));
    }

    #[test]
    fn the_view_logical_byte_charge_is_checked_at_the_exact_boundary() {
        let view = empty_view(1);
        let charge = view.resident_bytes().expect("view charge");
        assert!(charge > 0);

        let below = CursorRegistry::new(CursorConfig {
            max_views: 1,
            max_bytes: charge - 1,
            ttl_secs: 10,
        })
        .expect("registry");
        assert!(matches!(
            below.pin(Arc::clone(&view), [1; 32], 100),
            Err(CursorError::CapacityUnavailable)
        ));

        let exact = CursorRegistry::new(CursorConfig {
            max_views: 1,
            max_bytes: charge,
            ttl_secs: 10,
        })
        .expect("registry");
        let lease = exact.pin(view, [1; 32], 100).expect("exact charge fits");
        assert!(exact.resolve(lease.fact_set_id, [1; 32], 101).is_ok());
    }

    #[test]
    fn debug_output_redacts_the_authentication_secret() {
        use std::fmt::Write as _;

        let registry = registry();
        let mut secret_hex = String::new();
        for byte in registry.secret {
            write!(&mut secret_hex, "{byte:02x}").expect("write string");
        }
        let rendered = format!("{registry:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&secret_hex));
    }

    #[test]
    fn zero_registry_limits_fail_at_startup() {
        let error = CursorRegistry::new(CursorConfig {
            max_views: 0,
            max_bytes: 1,
            ttl_secs: 1,
        })
        .expect_err("zero view limit");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
