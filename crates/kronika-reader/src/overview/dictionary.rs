//! Targeted resolution of retained dictionary references.
//!
//! A segment interns text in two sections: short values in `dict.strings` and
//! larger ones, possibly truncated, in `dict.blobs`. A retained overview
//! reference — a normalized pattern, a database name — may land in either, and
//! the split threshold is not a stable contract. The resolver therefore checks
//! both section kinds and never assumes a reference is short enough to live only
//! in `dict.strings`.
//!
//! Only the requested IDs are retained, so resolving a handful of patterns does
//! not materialize the whole segment dictionary. When the same ID appears in
//! both kinds, the blob wins: it carries the truncation metadata. Section-level
//! early exit is deliberately not done, because a later blob section can upgrade
//! an ID already seen as a string.

use std::collections::{BTreeMap, BTreeSet};

use kronika_registry::{Bytes, CodecError};

use crate::{Stored, decode_dictionary};

/// One resolved dictionary value, owned and bounded to the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPattern {
    /// A `dict.strings` value, retained in full.
    Text(Vec<u8>),
    /// A `dict.blobs` value; `bytes` is a prefix when `truncated`.
    Blob {
        /// The stored bytes — a prefix of the original when truncated.
        bytes: Vec<u8>,
        /// The length of the full original value, bytes.
        full_len: u64,
        /// Whether `bytes` is only a prefix of the original.
        truncated: bool,
    },
}

impl ResolvedPattern {
    const fn is_blob(&self) -> bool {
        matches!(self, Self::Blob { .. })
    }

    fn from_stored(stored: Stored) -> Self {
        match stored {
            Stored::String(bytes) => Self::Text(bytes),
            Stored::Blob {
                bytes,
                full_len,
                truncated,
            } => Self::Blob {
                bytes,
                full_len,
                truncated,
            },
        }
    }
}

/// Resolves the requested `str_id`s against a segment's dictionary sections.
///
/// `dict_sections` yields the `(type_id, body)` of every `dict.strings` and
/// `dict.blobs` section, in catalog order. Only IDs in `wanted` are retained.
///
/// # Errors
/// Returns [`CodecError`] when a dictionary section body fails to decode.
pub fn resolve_targeted(
    dict_sections: impl IntoIterator<Item = (u32, Bytes)>,
    wanted: &BTreeSet<u64>,
) -> Result<BTreeMap<u64, ResolvedPattern>, CodecError> {
    let mut resolved: BTreeMap<u64, ResolvedPattern> = BTreeMap::new();
    for (type_id, body) in dict_sections {
        for (str_id, stored) in decode_dictionary(body, type_id)? {
            if !wanted.contains(&str_id) {
                continue;
            }
            let value = ResolvedPattern::from_stored(stored);
            match resolved.entry(str_id) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(value);
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    // A later blob section upgrades a value first seen as a string.
                    if value.is_blob() {
                        slot.insert(value);
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kronika_format::{DEFAULT_BLOB_THRESHOLD, DictLimits};
    use kronika_registry::Bytes;
    use kronika_writer::{Interner, dict};

    use super::{ResolvedPattern, resolve_targeted};

    /// Interns a short value (into `dict.strings`) and a long one (into
    /// `dict.blobs`), then returns the encoded dictionary section bodies.
    fn dictionary_sections() -> (Vec<(u32, Bytes)>, u64, u64) {
        let mut interner = Interner::new(DictLimits::new(4_096, 1 << 20).expect("limits"));
        let short_id = interner.intern(b"idx_orders_pkey").expect("intern short");
        let long_pattern = vec![b'x'; DEFAULT_BLOB_THRESHOLD + 64];
        let long_id = interner.intern(&long_pattern).expect("intern long");
        let sections = dict::encode(interner.window()).expect("encode dictionary");
        let bodies = sections
            .iter()
            .map(|section| (section.type_id, Bytes::from(section.body.clone())))
            .collect();
        (bodies, short_id.get(), long_id.get())
    }

    #[test]
    fn resolves_a_reference_from_either_section_kind() {
        let (sections, short_id, long_id) = dictionary_sections();
        let wanted = BTreeSet::from([short_id, long_id, 999_999]);
        let resolved = resolve_targeted(sections, &wanted).expect("resolve");

        assert_eq!(
            resolved.get(&short_id),
            Some(&ResolvedPattern::Text(b"idx_orders_pkey".to_vec()))
        );
        // The long pattern lives only in dict.blobs; a short-threshold assumption
        // would have missed it.
        assert!(
            matches!(resolved.get(&long_id), Some(ResolvedPattern::Blob { .. })),
            "long pattern must resolve from dict.blobs"
        );
        assert_eq!(resolved.get(&999_999), None, "absent id is not invented");
    }

    #[test]
    fn retains_only_the_requested_ids() {
        let (sections, short_id, _long_id) = dictionary_sections();
        let wanted = BTreeSet::from([short_id]);
        let resolved = resolve_targeted(sections, &wanted).expect("resolve");
        assert_eq!(resolved.len(), 1, "only the requested id is retained");
    }

    #[test]
    fn an_empty_request_resolves_nothing() {
        let (sections, _short_id, _long_id) = dictionary_sections();
        let resolved = resolve_targeted(sections, &BTreeSet::new()).expect("resolve");
        assert!(resolved.is_empty());
    }
}
