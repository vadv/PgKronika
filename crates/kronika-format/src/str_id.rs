//! Interned string id: `str_id = xxh3_64(bytes)`.
//!
//! Every text value in a segment — SQL, plans, object names, `cmdline`,
//! event payloads, chart series names — is referenced by its `str_id` and
//! stored once in the segment dictionaries (README.md, "String Ids and
//! Dictionaries").

use std::num::NonZeroU64;

use xxhash_rust::xxh3::xxh3_64;

/// Interned string id: the `xxh3_64` hash of the value bytes.
///
/// Zero is reserved as "no value" in on-disk fields (`source_id`, event
/// `ref_str`/`payload`), so a real id is always non-zero. An input that
/// happens to hash to zero must be treated as a collision by the writer
/// and never enters a dictionary (README.md, "String Ids and
/// Dictionaries").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrId(NonZeroU64);

impl StrId {
    /// Hash `bytes` with `xxh3_64`.
    ///
    /// The hash is computed over the raw value bytes, without encoding
    /// normalization. `None` means the input hashed to zero: the caller
    /// must treat that as a collision, not as a usable id.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Option<Self> {
        NonZeroU64::new(xxh3_64(bytes)).map(Self)
    }

    /// The id as the raw `u64` stored on disk.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Wrap a raw on-disk `u64`; `None` for the zero sentinel.
    ///
    /// This is the conversion boundary with fields that keep the raw
    /// representation, such as [`crate::Catalog::source_id`].
    #[must_use]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StrId;

    /// The canonical `XXH3_64bits` check value for empty input from the
    /// xxHash reference implementation. Catches accidentally swapping the
    /// algorithm (e.g. plain xxh64) the same way the CRC32C known vector
    /// does for checksums.
    #[test]
    fn known_vector_empty_input() {
        let id = StrId::of(b"").expect("empty input must hash to a non-zero id");
        assert_eq!(id.get(), 0x2D06_8005_38D3_94C2);
    }

    #[test]
    fn raw_roundtrip_and_zero_sentinel() {
        let id = StrId::of(b"pg_stat_activity").expect("non-zero id");
        assert_eq!(StrId::from_raw(id.get()), Some(id));
        assert_eq!(StrId::from_raw(0), None);
    }
}
