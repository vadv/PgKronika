//! In-memory dictionaries for one segment.
//!
//! Each text or byte value gets a [`StrId`] and is stored once in either
//! `dict.strings` or `dict.blobs`. `dict.hot_strings` duplicates selected
//! short strings so readers can resolve common labels without loading larger
//! dictionary entries.
//!
//! Placement is based on accumulated requirements for a value: size-based
//! routing, registry-forced blob placement, and strict or optional hot-cache
//! placement. The final placement is independent of call order.
//!
//! The core invariants are:
//!
//! - every issued [`StrId`] resolves inside the same [`SegmentDicts`];
//! - one id is never stored in both `dict.strings` and `dict.blobs`;
//! - `dict.hot_strings` is always a subset of `dict.strings`;
//! - truncated values keep `str_id`, `full_len`, and `full_sha256` for the
//!   full original value, not only for the stored prefix.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::StrId;

/// Default boundary between `dict.strings` and `dict.blobs`, bytes.
///
/// Starting value. Finalize it after measuring real segment data.
pub const DEFAULT_BLOB_THRESHOLD: usize = 4 * 1024;

/// Default truncation limit for large values, bytes. A starting value,
/// not a fixed format constant.
pub const DEFAULT_TRUNCATE_LIMIT: usize = 1024 * 1024;

/// Default cap on the total stored bytes of one dictionary set. A starting
/// value sized to match the default `active.parts` frame limit: a window
/// that hits this cap is flushed into one journal part.
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Size limits used while building dictionaries.
///
/// Values shorter than `blob_threshold` go to `dict.strings`. Values at or
/// above that threshold go to `dict.blobs`. Values longer than
/// `truncate_limit` keep only a prefix in the segment. The total stored
/// bytes of the set are capped by `max_total_bytes`: a new value past the
/// cap fails with [`DictError::Full`], the signal to flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictLimits {
    /// Values shorter than this go to `dict.strings`, the rest to
    /// `dict.blobs`.
    blob_threshold: usize,
    /// Values longer than this keep only a prefix of exactly this length
    /// in the segment (`dict.blobs` truncation).
    truncate_limit: usize,
    /// Cap on the total stored bytes across both dictionaries.
    max_total_bytes: usize,
}

impl DictLimits {
    /// Build dictionary limits with the default total-bytes cap.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidLimits`] unless `0 < blob_threshold <=
    /// truncate_limit <= max_total_bytes`.
    pub const fn new(blob_threshold: usize, truncate_limit: usize) -> Result<Self, InvalidLimits> {
        Self::validate(Self {
            blob_threshold,
            truncate_limit,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        })
    }

    /// Replace the total-bytes cap.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidLimits`] if the cap is smaller than
    /// `truncate_limit`: the set must always be able to hold at least one
    /// value of the maximum stored size, or a single large value could
    /// never be interned at all.
    pub const fn with_max_total_bytes(self, max_total_bytes: usize) -> Result<Self, InvalidLimits> {
        Self::validate(Self {
            max_total_bytes,
            ..self
        })
    }

    const fn validate(limits: Self) -> Result<Self, InvalidLimits> {
        if limits.blob_threshold == 0
            || limits.blob_threshold > limits.truncate_limit
            || limits.truncate_limit > limits.max_total_bytes
        {
            return Err(InvalidLimits {
                blob_threshold: limits.blob_threshold,
                truncate_limit: limits.truncate_limit,
                max_total_bytes: limits.max_total_bytes,
            });
        }
        Ok(limits)
    }

    /// The `dict.strings` / `dict.blobs` boundary, bytes.
    #[must_use]
    pub const fn blob_threshold(self) -> usize {
        self.blob_threshold
    }

    /// The truncation limit for large values, bytes.
    #[must_use]
    pub const fn truncate_limit(self) -> usize {
        self.truncate_limit
    }

    /// The cap on total stored bytes, bytes.
    #[must_use]
    pub const fn max_total_bytes(self) -> usize {
        self.max_total_bytes
    }
}

impl Default for DictLimits {
    fn default() -> Self {
        Self {
            blob_threshold: DEFAULT_BLOB_THRESHOLD,
            truncate_limit: DEFAULT_TRUNCATE_LIMIT,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// Rejected [`DictLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidLimits {
    /// The rejected `dict.strings` / `dict.blobs` boundary.
    pub blob_threshold: usize,
    /// The rejected truncation limit.
    pub truncate_limit: usize,
    /// The rejected total-bytes cap.
    pub max_total_bytes: usize,
}

impl fmt::Display for InvalidLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dictionary limits must satisfy 0 < blob_threshold <= truncate_limit \
             <= max_total_bytes, got {}, {} and {}",
            self.blob_threshold, self.truncate_limit, self.max_total_bytes
        )
    }
}

impl Error for InvalidLimits {}

/// Why a dictionary update was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictError {
    /// One `str_id` was assigned to different values.
    ///
    /// The writer must abandon the current segment. `id == 0` means the
    /// input hashed to the reserved zero id.
    Collision {
        /// The contested raw id; `0` for the zero-hash case.
        id: u64,
    },
    /// One value has incompatible placement requirements.
    ///
    /// A value cannot be required in `dict.blobs` and in `dict.hot_strings`,
    /// because hot strings must also be present in `dict.strings`.
    PlacementConflict {
        /// The id whose requirements cannot all be satisfied.
        id: StrId,
    },
    /// Storing a new value would push the total stored bytes past
    /// [`DictLimits::max_total_bytes`].
    ///
    /// This is flow control, not corruption: the writer should flush the
    /// window into a journal part and retry. Strict-hot values are exempt —
    /// they are registry-bounded by contract and must reach every part.
    Full {
        /// Total stored bytes already held.
        stored_bytes: usize,
        /// The configured cap.
        max: usize,
    },
}

impl fmt::Display for DictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collision { id: 0 } => {
                write!(
                    f,
                    "input hashed to the reserved zero str_id, treated as a collision"
                )
            }
            Self::Collision { id } => {
                write!(
                    f,
                    "str_id collision: {id:#018x} maps to different byte values"
                )
            }
            Self::PlacementConflict { id } => {
                write!(
                    f,
                    "str_id {:#018x} is required both in dict.blobs and in dict.hot_strings",
                    id.get()
                )
            }
            Self::Full { stored_bytes, max } => {
                write!(
                    f,
                    "dictionaries hold {stored_bytes} bytes of the {max} cap; flush the window"
                )
            }
        }
    }
}

impl Error for DictError {}

/// One `dict.blobs` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobEntry<'a> {
    /// Id of the full original value.
    pub str_id: StrId,
    /// Stored bytes: the full value, or its prefix when truncated.
    pub stored_bytes: &'a [u8],
    /// Length of the full original value, bytes.
    pub full_len: u64,
    /// Whether only a prefix of the value is stored.
    pub truncated: bool,
    /// SHA-256 of the full original value; present only when truncated.
    pub full_sha256: Option<[u8; 32]>,
}

/// A resolved dictionary value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved<'a> {
    /// The value is stored in `dict.strings` and kept in full.
    Str(&'a [u8]),
    /// The value is stored in `dict.blobs` and may be truncated.
    Blob(BlobEntry<'a>),
}

/// Current dictionary placement for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// `dict.strings`.
    Strings,
    /// `dict.blobs`.
    Blobs,
}

/// Hot-cache requirement requested for an entry.
///
/// Effective `dict.hot_strings` membership also requires
/// [`Placement::Strings`]. A soft mark on a blob-placed value leaves the value
/// out of the hot cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotMark {
    /// Never requested hot.
    None,
    /// Soft hot request (event labels).
    Soft,
    /// Strict hot (chart headers, catalog `source_id`).
    Hard,
}

/// Snapshot used by the writer when flushing a dictionary window.
#[derive(Debug, Clone, Copy)]
pub struct EntrySnapshot<'a> {
    /// Id of the full original value.
    pub str_id: StrId,
    /// Stored bytes: the full value, or its prefix when truncated.
    pub stored_bytes: &'a [u8],
    /// Length of the full original value, bytes.
    pub full_len: u64,
    /// Whether only a prefix of the value is stored.
    pub truncated: bool,
    /// SHA-256 of the full original value; present only when truncated.
    pub full_sha256: Option<[u8; 32]>,
    /// Current placement after applying the entry requirements.
    pub placement: Placement,
    /// The hot requirement requested for the entry.
    pub hot: HotMark,
    /// Whether the registry forced this value into `dict.blobs`
    /// regardless of size.
    pub blob_required: bool,
}

/// Current dictionary sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictStats {
    /// Number of `dict.strings` entries.
    pub string_count: usize,
    /// Number of `dict.blobs` entries.
    pub blob_count: usize,
    /// Number of `dict.hot_strings` entries.
    pub hot_count: usize,
    /// Total stored bytes of `dict.strings` values.
    pub string_bytes: u64,
    /// Total stored bytes of `dict.blobs` values (after truncation).
    pub blob_bytes: u64,
}

/// Requirements attached to one interning call.
///
/// Requirements accumulate per value and are never withdrawn. This makes final
/// placement independent of call order.
#[derive(Debug, Clone, Copy, Default)]
struct Requirements {
    /// The registry requires this value in `dict.blobs` regardless of
    /// size, e.g. query plans.
    blob: bool,
    /// The strict part of the hot contract: the value must be readable
    /// from `dict.hot_strings` (chart headers, catalog `source_id`).
    hot_hard: bool,
    /// Soft hot request: duplicate into `dict.hot_strings` when placement
    /// allows it; otherwise leave it out without failing.
    hot_soft: bool,
}

/// One stored value with its accumulated requirements.
#[derive(Debug)]
struct Stored {
    /// The full value, or its prefix of `truncate_limit` bytes.
    bytes: Vec<u8>,
    /// Length of the full original value.
    full_len: usize,
    /// SHA-256 of the full original value; `Some` iff truncated.
    full_sha256: Option<[u8; 32]>,
    /// Routed to blobs by size at insert time (`full_len >= threshold`).
    oversized: bool,
    req: Requirements,
}

impl Stored {
    /// Whether the value is placed in `dict.blobs` (forced or oversized).
    const fn is_blob(&self) -> bool {
        self.req.blob || self.oversized
    }

    /// Whether the value is in `dict.hot_strings`. A soft hot request on
    /// a blob-placed value does not add it to the hot cache; a hard one is
    /// rejected before getting here.
    const fn is_hot(&self) -> bool {
        !self.is_blob() && (self.req.hot_hard || self.req.hot_soft)
    }
}

/// The dictionaries of one segment.
///
/// All `intern*` methods deduplicate: the same bytes always yield the
/// same [`StrId`] and one stored copy. Failed calls — [`DictError`] in any
/// variant — leave the dictionaries unchanged.
///
/// Memory is bounded: total stored bytes are capped by
/// [`DictLimits::max_total_bytes`], and a new value past the cap fails
/// with [`DictError::Full`] — the signal to flush the window. Repeats and
/// requirement upgrades of stored values add no bytes; strict-hot values
/// are exempt from the cap because the hot contract requires them in every
/// part and the registry bounds their number.
#[derive(Debug, Default)]
pub struct SegmentDicts {
    limits: DictLimits,
    entries: BTreeMap<StrId, Stored>,
    /// Total stored bytes across both dictionaries; enforced against
    /// `limits.max_total_bytes`.
    stored_bytes: usize,
}

impl SegmentDicts {
    /// Create empty dictionaries with the given limits.
    #[must_use]
    pub const fn new(limits: DictLimits) -> Self {
        Self {
            limits,
            entries: BTreeMap::new(),
            stored_bytes: 0,
        }
    }

    /// Total stored bytes across both dictionaries.
    #[must_use]
    pub const fn stored_bytes(&self) -> usize {
        self.stored_bytes
    }

    /// Return the limits used by this dictionary set.
    #[must_use]
    pub const fn limits(&self) -> DictLimits {
        self.limits
    }

    /// Intern a value using size-based placement.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] if the computed id is already used
    /// for a different value, or if the input hashes to the reserved zero id.
    /// On error, dictionaries are left unchanged.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(bytes, Requirements::default())
    }

    /// Intern a value that must be stored in `dict.blobs` regardless of size.
    ///
    /// Use this for values whose registry entry forces blob placement.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] as in [`Self::intern`].
    /// Returns [`DictError::PlacementConflict`] if the same value is already
    /// required in `dict.hot_strings`. On error, dictionaries are left
    /// unchanged.
    pub fn intern_blob(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(
            bytes,
            Requirements {
                blob: true,
                ..Requirements::default()
            },
        )
    }

    /// Intern a value that must be available in `dict.hot_strings`.
    ///
    /// Use this only for short values such as chart headers and `source_id`.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] as in [`Self::intern`].
    /// Returns [`DictError::PlacementConflict`] if size or registry
    /// requirements place the value in `dict.blobs`. On error, dictionaries
    /// are left unchanged.
    pub fn intern_hot(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(
            bytes,
            Requirements {
                hot_hard: true,
                ..Requirements::default()
            },
        )
    }

    /// Intern a value and try to add it to `dict.hot_strings`.
    ///
    /// Returns the id and a boolean that is `true` when the value is present in
    /// `dict.hot_strings` after the call. Large values and blob-forced values
    /// keep their normal placement and return `false`.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] as in [`Self::intern`]. On error,
    /// dictionaries are left unchanged.
    pub fn intern_hot_best_effort(&mut self, bytes: &[u8]) -> Result<(StrId, bool), DictError> {
        let id = self.insert(
            bytes,
            Requirements {
                hot_soft: true,
                ..Requirements::default()
            },
        )?;
        let hot = self.entries.get(&id).is_some_and(Stored::is_hot);
        Ok((id, hot))
    }

    /// Resolve an id issued by this set.
    #[must_use]
    pub fn resolve(&self, id: StrId) -> Option<Resolved<'_>> {
        let stored = self.entries.get(&id)?;
        Some(if stored.is_blob() {
            Resolved::Blob(Self::blob_entry(id, stored))
        } else {
            Resolved::Str(&stored.bytes)
        })
    }

    /// Return the number of interned values across `dict.strings` and
    /// `dict.blobs`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate `dict.strings` rows in on-disk sort order.
    pub fn strings(&self) -> impl Iterator<Item = (StrId, &[u8])> {
        self.entries
            .iter()
            .filter(|(_, stored)| !stored.is_blob())
            .map(|(id, stored)| (*id, stored.bytes.as_slice()))
    }

    /// Iterate `dict.blobs` rows in on-disk sort order.
    pub fn blobs(&self) -> impl Iterator<Item = BlobEntry<'_>> {
        self.entries
            .iter()
            .filter(|(_, stored)| stored.is_blob())
            .map(|(id, stored)| Self::blob_entry(*id, stored))
    }

    /// Iterate `dict.hot_strings` rows in on-disk sort order.
    ///
    /// Every returned id is also present in [`Self::strings`].
    pub fn hot_strings(&self) -> impl Iterator<Item = (StrId, &[u8])> {
        self.entries
            .iter()
            .filter(|(_, stored)| stored.is_hot())
            .map(|(id, stored)| (*id, stored.bytes.as_slice()))
    }

    /// Per-entry snapshots in `str_id` order, for the writer.
    pub fn entries(&self) -> impl Iterator<Item = EntrySnapshot<'_>> {
        self.entries.iter().map(|(id, stored)| EntrySnapshot {
            str_id: *id,
            stored_bytes: &stored.bytes,
            full_len: stored.full_len as u64,
            truncated: stored.full_sha256.is_some(),
            full_sha256: stored.full_sha256,
            placement: if stored.is_blob() {
                Placement::Blobs
            } else {
                Placement::Strings
            },
            hot: if stored.req.hot_hard {
                HotMark::Hard
            } else if stored.req.hot_soft {
                HotMark::Soft
            } else {
                HotMark::None
            },
            blob_required: stored.req.blob,
        })
    }

    /// Return current dictionary sizes.
    #[must_use]
    pub fn stats(&self) -> DictStats {
        let mut stats = DictStats::default();
        for stored in self.entries.values() {
            let stored_len = stored.bytes.len() as u64;
            if stored.is_blob() {
                stats.blob_count += 1;
                stats.blob_bytes += stored_len;
            } else {
                stats.string_count += 1;
                stats.string_bytes += stored_len;
                if stored.is_hot() {
                    stats.hot_count += 1;
                }
            }
        }
        stats
    }

    fn blob_entry(id: StrId, stored: &Stored) -> BlobEntry<'_> {
        BlobEntry {
            str_id: id,
            stored_bytes: &stored.bytes,
            full_len: stored.full_len as u64,
            truncated: stored.full_sha256.is_some(),
            full_sha256: stored.full_sha256,
        }
    }

    fn insert(&mut self, bytes: &[u8], req: Requirements) -> Result<StrId, DictError> {
        let id = id_or_collision(StrId::of(bytes))?;
        self.try_insert(id, bytes, req)
    }

    /// The single insertion path. Public `intern*` methods only hash and
    /// delegate here, so the zero-id and collision rules are testable
    /// without a known xxh3 preimage.
    ///
    /// All checks run before any mutation: a returned error leaves the
    /// dictionaries exactly as they were.
    fn try_insert(
        &mut self,
        id: StrId,
        bytes: &[u8],
        req: Requirements,
    ) -> Result<StrId, DictError> {
        if let Some(existing) = self.entries.get(&id) {
            Self::check_same_value(id, existing, bytes)?;
            let merged = Requirements {
                blob: existing.req.blob || req.blob,
                hot_hard: existing.req.hot_hard || req.hot_hard,
                hot_soft: existing.req.hot_soft || req.hot_soft,
            };
            if merged.hot_hard && (merged.blob || existing.oversized) {
                return Err(DictError::PlacementConflict { id });
            }
            // get_mut only after every check has passed.
            if let Some(stored) = self.entries.get_mut(&id) {
                stored.req = merged;
            }
            return Ok(id);
        }

        let oversized = bytes.len() >= self.limits.blob_threshold;
        if req.hot_hard && (req.blob || oversized) {
            return Err(DictError::PlacementConflict { id });
        }
        // The total-bytes cap is checked before any mutation. Strict-hot
        // values are exempt: the hot contract requires them in every part
        // and the registry bounds their number, so rejecting one here
        // would break a contract to protect a budget it cannot threaten.
        let stored_len = bytes.len().min(self.limits.truncate_limit);
        if !req.hot_hard
            && self.stored_bytes.saturating_add(stored_len) > self.limits.max_total_bytes
        {
            return Err(DictError::Full {
                stored_bytes: self.stored_bytes,
                max: self.limits.max_total_bytes,
            });
        }
        let truncated = bytes.len() > self.limits.truncate_limit;
        let stored = Stored {
            bytes: if truncated {
                bytes[..self.limits.truncate_limit].to_vec()
            } else {
                bytes.to_vec()
            },
            full_len: bytes.len(),
            full_sha256: truncated.then(|| Sha256::digest(bytes).into()),
            oversized,
            req,
        };
        self.stored_bytes += stored_len;
        self.entries.insert(id, stored);
        Ok(id)
    }

    /// Collision check between an existing entry and a re-interned value.
    ///
    /// For a truncated entry the original bytes are gone, so equality is
    /// checked via `(full_len, full_sha256)`; comparing against the
    /// stored prefix would call every repeat of the same oversized value
    /// a collision.
    fn check_same_value(id: StrId, existing: &Stored, bytes: &[u8]) -> Result<(), DictError> {
        let same = existing.full_len == bytes.len()
            && existing.full_sha256.map_or_else(
                || existing.bytes == bytes,
                |sha| <[u8; 32]>::from(Sha256::digest(bytes)) == sha,
            );
        if same {
            Ok(())
        } else {
            Err(DictError::Collision { id: id.get() })
        }
    }
}

/// Convert the optional hash result into a dictionary id.
///
/// Zero is the on-disk "no value" sentinel. If `StrId::of` returns `None`,
/// the caller must treat it as a collision.
fn id_or_collision(hashed: Option<StrId>) -> Result<StrId, DictError> {
    hashed.ok_or(DictError::Collision { id: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_limits() -> DictLimits {
        DictLimits::new(8, 16).expect("8 <= 16")
    }

    fn id_of(bytes: &[u8]) -> StrId {
        StrId::of(bytes).expect("test value must not hash to zero")
    }

    #[test]
    fn rejects_inverted_limits() {
        assert!(DictLimits::new(0, 16).is_err());
        assert!(DictLimits::new(32, 16).is_err());
        assert!(DictLimits::new(16, 16).is_ok());
        // The cap must fit at least one value of the maximum stored size.
        assert!(
            DictLimits::new(8, 16)
                .expect("valid")
                .with_max_total_bytes(15)
                .is_err()
        );
    }

    #[test]
    fn total_bytes_cap_signals_full() {
        let limits = DictLimits::new(8, 16)
            .expect("valid")
            .with_max_total_bytes(16)
            .expect("cap fits one value");
        let mut dicts = SegmentDicts::new(limits);

        dicts.intern(b"0123456789").expect("ten bytes fit the cap");
        let err = dicts
            .intern(b"abcdefghij")
            .expect_err("ten more would exceed the cap");
        assert_eq!(
            err,
            DictError::Full {
                stored_bytes: 10,
                max: 16
            }
        );
        assert_eq!(dicts.len(), 1, "a rejected value is not stored");

        // Repeats of stored values add no bytes and stay allowed.
        dicts.intern(b"0123456789").expect("repeat is free");
        // Strict-hot values are exempt: registry-bounded by contract.
        dicts.intern_hot(b"hot").expect("hot bypasses the cap");
        assert_eq!(dicts.stored_bytes(), 13);
    }

    #[test]
    fn empty_string_is_a_regular_value() {
        let mut dicts = SegmentDicts::new(small_limits());
        let id = dicts.intern(b"").expect("empty string interns");
        assert_ne!(id.get(), 0);
        assert_eq!(dicts.resolve(id), Some(Resolved::Str(&[][..])));
    }

    #[test]
    fn zero_hash_is_a_collision() {
        // No xxh3 preimage of zero is known, so the rule is tested on the
        // extracted conversion that every public intern call goes through.
        assert_eq!(
            id_or_collision(None).expect_err("zero hash must be rejected"),
            DictError::Collision { id: 0 }
        );
        let real = StrId::of(b"value");
        assert_eq!(id_or_collision(real).ok(), real);
    }

    #[test]
    fn same_id_different_bytes_is_a_collision() {
        let mut dicts = SegmentDicts::new(small_limits());
        let id = dicts.intern(b"short").expect("interns");
        let err = dicts
            .try_insert(id, b"other", Requirements::default())
            .expect_err("different bytes under one id");
        assert_eq!(err, DictError::Collision { id: id.get() });
        // The failed call must not have changed the stored value.
        assert_eq!(dicts.resolve(id), Some(Resolved::Str(&b"short"[..])));
    }

    #[test]
    fn truncation_keeps_full_value_identity() {
        let mut dicts = SegmentDicts::new(small_limits());
        let value = b"this value is longer than sixteen bytes";
        let id = dicts.intern(value).expect("interns");
        assert_eq!(id, id_of(value), "id is computed over the full value");

        let Some(Resolved::Blob(entry)) = dicts.resolve(id) else {
            panic!("oversized value must resolve as a blob");
        };
        assert!(entry.truncated);
        assert_eq!(entry.full_len, value.len() as u64);
        assert_eq!(entry.stored_bytes, &value[..16]);
        let expected: [u8; 32] = Sha256::digest(value).into();
        assert_eq!(entry.full_sha256, Some(expected));
    }

    #[test]
    fn reinterning_an_oversized_value_is_not_a_collision() {
        let mut dicts = SegmentDicts::new(small_limits());
        let value = b"this value is longer than sixteen bytes";
        let first = dicts.intern(value).expect("interns");
        let second = dicts.intern(value).expect("same value re-interns");
        assert_eq!(first, second);
        assert_eq!(dicts.len(), 1);
    }

    #[test]
    fn truncated_entries_collide_via_full_value_identity() {
        let mut dicts = SegmentDicts::new(small_limits());
        // Same length, same stored prefix, different tail: only
        // (full_len, full_sha256) can tell these apart after truncation.
        let original = b"0123456789abcdef this is tail A";
        let impostor = b"0123456789abcdef this is tail B";
        assert_eq!(original.len(), impostor.len());

        let id = dicts.intern(original).expect("interns");
        let err = dicts
            .try_insert(id, impostor, Requirements::default())
            .expect_err("same id and length, different content");
        assert_eq!(err, DictError::Collision { id: id.get() });
    }

    #[test]
    fn threshold_and_truncation_boundaries() {
        let mut dicts = SegmentDicts::new(small_limits());

        // blob_threshold = 8: seven bytes is a string, eight is a blob.
        let seven = dicts.intern(&[7_u8; 7]).expect("interns");
        assert!(matches!(dicts.resolve(seven), Some(Resolved::Str(_))));
        let eight = dicts.intern(&[8_u8; 8]).expect("interns");
        assert!(matches!(dicts.resolve(eight), Some(Resolved::Blob(_))));

        // truncate_limit = 16: sixteen bytes is stored whole and carries
        // no sha, seventeen is cut to the limit.
        let sixteen = dicts.intern(&[16_u8; 16]).expect("interns");
        let Some(Resolved::Blob(entry)) = dicts.resolve(sixteen) else {
            panic!("sixteen bytes is a blob");
        };
        assert!(!entry.truncated);
        assert_eq!(entry.full_sha256, None);
        assert_eq!(entry.stored_bytes.len(), 16);

        let seventeen = dicts.intern(&[17_u8; 17]).expect("interns");
        let Some(Resolved::Blob(entry)) = dicts.resolve(seventeen) else {
            panic!("seventeen bytes is a blob");
        };
        assert!(entry.truncated);
        assert_eq!(entry.stored_bytes.len(), 16);
        assert_eq!(entry.full_len, 17);
    }

    #[test]
    fn hot_of_an_oversized_value_is_a_conflict() {
        let mut dicts = SegmentDicts::new(small_limits());
        let err = dicts
            .intern_hot(b"longer than the eight-byte threshold")
            .expect_err("strict hot cannot live in blobs");
        assert!(matches!(err, DictError::PlacementConflict { .. }));
        assert!(dicts.is_empty(), "a rejected value is not stored");
    }

    #[test]
    fn hard_hot_and_forced_blob_conflict_in_both_orders() {
        let value = b"plan";

        let mut dicts = SegmentDicts::new(small_limits());
        let id = dicts.intern_hot(value).expect("hot first");
        let err = dicts.intern_blob(value).expect_err("then blob");
        assert!(matches!(err, DictError::PlacementConflict { .. }));
        // The failed call must not have moved the value or dropped its
        // hot mark.
        assert!(matches!(dicts.resolve(id), Some(Resolved::Str(_))));
        assert_eq!(dicts.hot_strings().count(), 1);
        assert_eq!(dicts.stats().blob_count, 0);

        let mut dicts = SegmentDicts::new(small_limits());
        let id = dicts.intern_blob(value).expect("blob first");
        let err = dicts.intern_hot(value).expect_err("then hot");
        assert!(matches!(err, DictError::PlacementConflict { .. }));
        assert!(matches!(dicts.resolve(id), Some(Resolved::Blob(_))));
        assert_eq!(dicts.hot_strings().count(), 0);
    }

    #[test]
    fn soft_hot_skips_hot_cache_for_blob_without_error() {
        let value = b"label";

        // Soft hot first, forced blob later: the value moves to blobs and
        // the soft hot mark does not add it to the hot cache, in either
        // call order.
        let mut dicts = SegmentDicts::new(small_limits());
        let (id, hot) = dicts.intern_hot_best_effort(value).expect("soft hot");
        assert!(hot, "short value is string-placed, so it is hot");
        dicts
            .intern_blob(value)
            .expect("forced blob wins over soft hot");
        assert_eq!(dicts.hot_strings().count(), 0);
        assert!(matches!(dicts.resolve(id), Some(Resolved::Blob(_))));

        let mut dicts = SegmentDicts::new(small_limits());
        dicts.intern_blob(value).expect("forced blob first");
        let (_, hot) = dicts.intern_hot_best_effort(value).expect("soft hot later");
        assert!(!hot, "blob-placed value stays out of the hot cache");
        assert_eq!(dicts.hot_strings().count(), 0);
    }

    #[test]
    fn stats_count_both_dictionaries() {
        let mut dicts = SegmentDicts::new(small_limits());
        dicts.intern(b"a").expect("string");
        dicts.intern_hot(b"hot").expect("hot string");
        dicts.intern(b"longer than the threshold").expect("blob");
        let stats = dicts.stats();
        assert_eq!(stats.string_count, 2);
        assert_eq!(stats.hot_count, 1);
        assert_eq!(stats.blob_count, 1);
        assert_eq!(stats.string_bytes, 4);
        assert_eq!(stats.blob_bytes, 16);
    }
}
