//! Per-segment dictionaries: `dict.strings`, `dict.blobs`, `dict.hot_strings`.
//!
//! This is the in-memory model of the dictionary contract (README.md,
//! "String Ids and Dictionaries"): which dictionary a value lands in,
//! how oversized values are truncated, and which inputs are collisions.
//! Encoding the dictionaries into on-disk section bytes is a later step;
//! this module owns only the contract:
//!
//! - every issued [`StrId`] resolves within the same [`SegmentDicts`];
//! - one id never lives in `strings` and `blobs` at the same time;
//! - `hot_strings` is a subset of `strings` — it is a duplicating tail
//!   cache, not a third source of truth;
//! - a value larger than the truncation limit keeps only a prefix, while
//!   `str_id` and `full_sha256` are computed over the full original value.
//!
//! Placement is decided by the *set of requirements* attached to a value
//! (registry-forced blob, hot), never by call order: interning the same
//! values with the same requirements in any order yields identical
//! dictionaries. Incompatible hard requirements are a typed error, since
//! the segment would violate the contract whichever dictionary won.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::StrId;

/// Default boundary between `dict.strings` and `dict.blobs`, bytes.
/// A starting value, not a settled format decision — see README.md,
/// "Open Questions", for what the trade-off is and what would settle it.
pub const DEFAULT_BLOB_THRESHOLD: usize = 4 * 1024;

/// Default truncation limit for large values, bytes. A starting value,
/// not a settled format decision — see README.md, "Open Questions".
pub const DEFAULT_TRUNCATE_LIMIT: usize = 1024 * 1024;

/// Size knobs of the dictionaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictLimits {
    /// Values shorter than this go to `dict.strings`, the rest to
    /// `dict.blobs`.
    blob_threshold: usize,
    /// Values longer than this keep only a prefix of exactly this length
    /// in the segment (`dict.blobs` truncation).
    truncate_limit: usize,
}

impl DictLimits {
    /// Build limits, validating `0 < blob_threshold <= truncate_limit`.
    ///
    /// The ordering matters: truncation is defined only for `dict.blobs`,
    /// so every value longer than the truncation limit must already be
    /// routed to blobs by the threshold.
    ///
    /// # Errors
    ///
    /// [`InvalidLimits`] when the ordering above does not hold.
    pub const fn new(blob_threshold: usize, truncate_limit: usize) -> Result<Self, InvalidLimits> {
        if blob_threshold == 0 || blob_threshold > truncate_limit {
            return Err(InvalidLimits {
                blob_threshold,
                truncate_limit,
            });
        }
        Ok(Self {
            blob_threshold,
            truncate_limit,
        })
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
}

impl Default for DictLimits {
    fn default() -> Self {
        Self {
            blob_threshold: DEFAULT_BLOB_THRESHOLD,
            truncate_limit: DEFAULT_TRUNCATE_LIMIT,
        }
    }
}

/// Rejected [`DictLimits`]: the threshold/limit ordering does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidLimits {
    /// The rejected `dict.strings` / `dict.blobs` boundary.
    pub blob_threshold: usize,
    /// The rejected truncation limit.
    pub truncate_limit: usize,
}

impl fmt::Display for InvalidLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dictionary limits must satisfy 0 < blob_threshold <= truncate_limit, \
             got blob_threshold {} and truncate_limit {}",
            self.blob_threshold, self.truncate_limit
        )
    }
}

impl Error for InvalidLimits {}

/// Why a value was rejected by the dictionaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictError {
    /// One `str_id` corresponds to different byte values. The writer must
    /// abort the segment (README.md, "String Ids and Dictionaries").
    /// `id == 0` means the input hashed to zero, which the format
    /// reserves as "no value" and mandates treating as a collision.
    Collision {
        /// The contested raw id; `0` for the zero-hash case.
        id: u64,
    },
    /// Hard requirements on one value are incompatible: it is required
    /// both in `dict.blobs` (registry-forced or oversized) and in
    /// `dict.hot_strings` (which must be a subset of `dict.strings`).
    PlacementConflict {
        /// The id whose requirements cannot all be satisfied.
        id: StrId,
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
        }
    }
}

impl Error for DictError {}

/// One `dict.blobs` row, mirroring the on-disk schema (README.md,
/// "String Ids and Dictionaries").
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
    /// The value lives in `dict.strings` and is stored in full.
    Str(&'a [u8]),
    /// The value lives in `dict.blobs` and may be truncated.
    Blob(BlobEntry<'a>),
}

/// Sizes of the dictionaries, for the collector's self-metrics.
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

/// Requirements attached to one interning call. Requirements accumulate
/// per value and are never withdrawn, which is what makes the final
/// placement independent of call order.
#[derive(Debug, Clone, Copy, Default)]
struct Requirements {
    /// The registry requires this value in `dict.blobs` regardless of
    /// size, e.g. query plans.
    blob: bool,
    /// The strict part of the hot contract: the value must be readable
    /// from the tail cache (chart headers, catalog `source_id`).
    hot_hard: bool,
    /// The best-effort part of the hot contract: duplicate into the tail
    /// cache if placement allows, silently skip otherwise.
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
    /// Which dictionary the value belongs to under current requirements.
    const fn is_blob(&self) -> bool {
        self.req.blob || self.oversized
    }

    /// Whether the value is in `dict.hot_strings`. A soft hot request on
    /// a blob-placed value degrades to nothing, per the best-effort part
    /// of the contract; a hard one is rejected before getting here.
    const fn is_hot(&self) -> bool {
        !self.is_blob() && (self.req.hot_hard || self.req.hot_soft)
    }
}

/// The three dictionaries of one segment.
///
/// All `intern*` methods deduplicate: the same bytes always yield the
/// same [`StrId`] and one stored copy. The only fatal outcomes are a
/// [`DictError::Collision`] and a [`DictError::PlacementConflict`]; both
/// leave the dictionaries unchanged.
#[derive(Debug, Default)]
pub struct SegmentDicts {
    limits: DictLimits,
    entries: BTreeMap<StrId, Stored>,
}

impl SegmentDicts {
    /// Empty dictionaries with the given limits.
    #[must_use]
    pub const fn new(limits: DictLimits) -> Self {
        Self {
            limits,
            entries: BTreeMap::new(),
        }
    }

    /// The limits this set was built with.
    #[must_use]
    pub const fn limits(&self) -> DictLimits {
        self.limits
    }

    /// Intern a value with size-based routing: shorter than the threshold
    /// goes to `dict.strings`, the rest to `dict.blobs`.
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] — the id is already taken by different
    /// bytes, or the input hashed to zero.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(bytes, Requirements::default())
    }

    /// Intern a value that the registry requires in `dict.blobs`
    /// regardless of size, e.g. a query plan.
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] as for [`Self::intern`];
    /// [`DictError::PlacementConflict`] if the value is already required
    /// in `dict.hot_strings`.
    pub fn intern_blob(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(
            bytes,
            Requirements {
                blob: true,
                ..Requirements::default()
            },
        )
    }

    /// Intern a value of the strict hot contract: chart header strings
    /// (`unit`, `series_names`, `entity`) and the catalog `source_id`
    /// must be resolvable from the tail cache.
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] as for [`Self::intern`];
    /// [`DictError::PlacementConflict`] if the value lands in
    /// `dict.blobs` — by the registry requirement or by size. The strict
    /// hot contract is only satisfiable for short strings, so an
    /// oversized one is a bug in the calling type, not a degradation.
    pub fn intern_hot(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.insert(
            bytes,
            Requirements {
                hot_hard: true,
                ..Requirements::default()
            },
        )
    }

    /// Intern a value of the best-effort hot contract: short event labels
    /// are duplicated into the tail cache, large values silently stay
    /// only in the full dictionaries.
    ///
    /// Returns the id and whether the value is in `dict.hot_strings`
    /// after this call.
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] as for [`Self::intern`].
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

    /// Number of interned values across both dictionaries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `dict.strings` rows in `str_id` order (the on-disk sort order).
    pub fn strings(&self) -> impl Iterator<Item = (StrId, &[u8])> {
        self.entries
            .iter()
            .filter(|(_, stored)| !stored.is_blob())
            .map(|(id, stored)| (*id, stored.bytes.as_slice()))
    }

    /// `dict.blobs` rows in `str_id` order (the on-disk sort order).
    pub fn blobs(&self) -> impl Iterator<Item = BlobEntry<'_>> {
        self.entries
            .iter()
            .filter(|(_, stored)| stored.is_blob())
            .map(|(id, stored)| Self::blob_entry(*id, stored))
    }

    /// `dict.hot_strings` rows in `str_id` order. Always a subset of
    /// [`Self::strings`].
    pub fn hot_strings(&self) -> impl Iterator<Item = (StrId, &[u8])> {
        self.entries
            .iter()
            .filter(|(_, stored)| stored.is_hot())
            .map(|(id, stored)| (*id, stored.bytes.as_slice()))
    }

    /// Sizes of the dictionaries, for self-metrics.
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

    /// The single write path. Public `intern*` methods only hash and
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

/// The zero hash never becomes a real id: zero is the on-disk "no value"
/// sentinel, so the format mandates treating it as a collision
/// (README.md, "String Ids and Dictionaries"). No xxh3 preimage of zero
/// is known, so this conversion is split out to keep the rule directly
/// testable.
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
    fn soft_hot_degrades_without_error() {
        let value = b"label";

        // Soft hot first, forced blob later: the value moves to blobs and
        // the soft mark degrades to nothing, in either call order.
        let mut dicts = SegmentDicts::new(small_limits());
        let (id, hot) = dicts.intern_hot_best_effort(value).expect("soft hot");
        assert!(hot, "short value lands in strings, so it is hot");
        dicts
            .intern_blob(value)
            .expect("forced blob wins over soft hot");
        assert_eq!(dicts.hot_strings().count(), 0);
        assert!(matches!(dicts.resolve(id), Some(Resolved::Blob(_))));

        let mut dicts = SegmentDicts::new(small_limits());
        dicts.intern_blob(value).expect("forced blob first");
        let (_, hot) = dicts.intern_hot_best_effort(value).expect("soft hot later");
        assert!(!hot, "blob-placed value silently skips the hot cache");
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
