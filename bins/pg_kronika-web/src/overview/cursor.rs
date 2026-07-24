//! Opaque, authenticated pagination cursor for `/v1/timeline/events`.
//!
//! A cursor is a server-authenticated continuation token: it pins the view
//! generation and query it was issued against, plus the last item served. The
//! next page must present the same generation and query hash, so a cursor from
//! a changed range, filter, or policy is rejected rather than silently
//! continuing over different data.
//!
//! The authentication key is per-process. After a restart the key is new, so
//! every previously issued cursor fails its check and honestly expires; a
//! stateless continuation onto a fresh process is impossible.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// The cursor encoding version.
const CURSOR_VERSION: u16 = 1;

/// Length of the authenticated payload, in bytes.
const PAYLOAD_LEN: usize = 2 + 8 + 32 + 8 + 32;

/// Length of the appended message authentication code, in bytes.
const MAC_LEN: usize = 32;

/// The per-process cursor authentication key.
static SECRET: OnceLock<[u8; 32]> = OnceLock::new();

fn secret() -> &'static [u8; 32] {
    SECRET.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"pgk-overview-cursor-secret-v1");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        hasher.update(nanos.to_le_bytes());
        hasher.update(std::process::id().to_le_bytes());
        // A heap address adds entropy the wall clock alone lacks.
        let marker = Box::new(0_u8);
        let address = std::ptr::from_ref::<u8>(&marker).addr();
        hasher.update(address.to_le_bytes());
        hasher.finalize().into()
    })
}

/// The decoded position of a `/events` cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventsCursor {
    /// The view generation the first page pinned.
    pub(crate) view_generation: u64,
    /// The hash binding range, filters, order, and policy versions.
    pub(crate) query_hash: [u8; 32],
    /// The sort timestamp of the last item served.
    pub(crate) last_ts_us: i64,
    /// The id of the last item served, breaking timestamp ties.
    pub(crate) last_event_id: [u8; 32],
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
}

impl EventsCursor {
    /// Encodes an authenticated opaque continuation token.
    pub(crate) fn encode(self) -> String {
        let mut payload = Vec::with_capacity(PAYLOAD_LEN + MAC_LEN);
        payload.extend_from_slice(&CURSOR_VERSION.to_le_bytes());
        payload.extend_from_slice(&self.view_generation.to_le_bytes());
        payload.extend_from_slice(&self.query_hash);
        payload.extend_from_slice(&self.last_ts_us.to_le_bytes());
        payload.extend_from_slice(&self.last_event_id);
        let mac = hmac_sha256(secret(), &payload);
        payload.extend_from_slice(&mac);
        URL_SAFE_NO_PAD.encode(payload)
    }

    /// Decodes and authenticates a token, then checks it against the current
    /// query.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Invalid`] on a decode or authentication failure,
    /// [`CursorError::QueryMismatch`] when the bound query differs, and
    /// [`CursorError::ViewGone`] when the pinned generation is in the future of
    /// the current view.
    pub(crate) fn decode(
        token: &str,
        expected_query_hash: [u8; 32],
        current_view_generation: u64,
    ) -> Result<Self, CursorError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_error| CursorError::Invalid)?;
        if bytes.len() != PAYLOAD_LEN + MAC_LEN {
            return Err(CursorError::Invalid);
        }
        let (payload, mac) = bytes.split_at(PAYLOAD_LEN);
        let expected_mac = hmac_sha256(secret(), payload);
        if expected_mac.ct_eq(mac).unwrap_u8() != 1 {
            return Err(CursorError::Invalid);
        }
        let version = u16::from_le_bytes([payload[0], payload[1]]);
        if version != CURSOR_VERSION {
            return Err(CursorError::Invalid);
        }
        let generation_bytes: [u8; 8] = payload[2..10]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let view_generation = u64::from_le_bytes(generation_bytes);
        let query_hash: [u8; 32] = payload[10..42]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let ts_bytes: [u8; 8] = payload[42..50]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;
        let last_ts_us = i64::from_le_bytes(ts_bytes);
        let last_event_id: [u8; 32] = payload[50..82]
            .try_into()
            .map_err(|_error| CursorError::Invalid)?;

        if query_hash.ct_eq(&expected_query_hash).unwrap_u8() != 1 {
            return Err(CursorError::QueryMismatch);
        }
        if view_generation != current_view_generation {
            return Err(CursorError::ViewGone);
        }
        Ok(Self {
            view_generation,
            query_hash,
            last_ts_us,
            last_event_id,
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

    fn cursor() -> EventsCursor {
        EventsCursor {
            view_generation: 7,
            query_hash: [3; 32],
            last_ts_us: 1_234,
            last_event_id: [9; 32],
        }
    }

    #[test]
    fn a_round_trip_recovers_the_position() {
        let token = cursor().encode();
        let decoded = EventsCursor::decode(&token, [3; 32], 7).expect("valid cursor");
        assert_eq!(decoded, cursor());
    }

    #[test]
    fn a_tampered_token_fails_authentication() {
        let mut token = cursor().encode().into_bytes();
        let last = token.len() - 1;
        token[last] ^= 0x01;
        let tampered = String::from_utf8(token).expect("ascii token");
        assert_eq!(
            EventsCursor::decode(&tampered, [3; 32], 7),
            Err(CursorError::Invalid)
        );
    }

    #[test]
    fn a_changed_query_is_rejected() {
        let token = cursor().encode();
        assert_eq!(
            EventsCursor::decode(&token, [4; 32], 7),
            Err(CursorError::QueryMismatch)
        );
    }

    #[test]
    fn a_future_generation_is_gone() {
        let token = cursor().encode();
        assert_eq!(
            EventsCursor::decode(&token, [3; 32], 6),
            Err(CursorError::ViewGone)
        );
    }
}
