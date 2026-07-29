//! Targeted resolution of retained dictionary references.
//!
//! A reference may occur in `dict.strings` or `dict.blobs`; the split threshold
//! is not part of the reader contract. The resolver reads both section kinds
//! and copies only requested values. Every id must occur exactly once across
//! both sections; duplicate ids, placement conflicts, or invalid truncation
//! metadata make the dictionary inconsistent.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use kronika_registry::{Bytes, CodecError};

use crate::{PgmUnit, ReadError, Stored, decode_dictionary_selected};

/// One resolved dictionary value, owned and bounded to the request.
#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for ResolvedPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(bytes) => f
                .debug_struct("Text")
                .field("stored_len", &bytes.len())
                .finish(),
            Self::Blob {
                bytes,
                full_len,
                truncated,
            } => f
                .debug_struct("Blob")
                .field("stored_len", &bytes.len())
                .field("full_len", full_len)
                .field("truncated", truncated)
                .finish(),
        }
    }
}

impl ResolvedPattern {
    const fn stored_len(&self) -> usize {
        match self {
            Self::Text(bytes) | Self::Blob { bytes, .. } => bytes.len(),
        }
    }

    const fn has_valid_storage(&self) -> bool {
        match self {
            Self::Text(_) => true,
            Self::Blob {
                bytes,
                full_len,
                truncated,
            } => {
                let stored_len = bytes.len() as u64;
                stored_len <= *full_len
                    && if *truncated {
                        stored_len < *full_len
                    } else {
                        stored_len == *full_len
                    }
            }
        }
    }

    fn from_stored(stored: Stored) -> Self {
        match stored {
            Stored::String(bytes) => Self::Text(bytes),
            Stored::Blob {
                bytes,
                full_len,
                truncated,
                ..
            } => Self::Blob {
                bytes,
                full_len,
                truncated,
            },
        }
    }
}

/// Work performed by a targeted PGM dictionary read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TargetedDictionaryStats {
    /// Dictionary section bodies read from the PGM.
    pub sections_read: u64,
    /// Stored section bytes read from the PGM.
    pub stored_bytes_read: u64,
    /// Dictionary rows inspected by the Parquet decoder.
    pub rows_scanned: u64,
    /// Uncompressed dictionary value bytes inspected.
    pub decoded_bytes: u64,
    /// Requested values retained in the result.
    pub values_retained: u64,
}

/// Selected dictionary values and their read counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetedDictionaryRead {
    /// Values keyed by `str_id`.
    pub values: BTreeMap<u64, ResolvedPattern>,
    /// Work performed to produce `values`.
    pub stats: TargetedDictionaryStats,
}

/// Resolves the requested `str_id`s against a segment's dictionary sections.
///
/// `dict_sections` yields the `(type_id, body)` of every `dict.strings` and
/// `dict.blobs` section, in catalog order. Only IDs in `wanted` are retained.
///
/// # Errors
/// Returns [`CodecError`] when a dictionary section body fails to decode or
/// the selected values exceed `bounds`.
pub fn resolve_targeted(
    dict_sections: impl IntoIterator<Item = (u32, Bytes)>,
    wanted: &BTreeSet<u64>,
    bounds: &crate::Bounds,
) -> Result<BTreeMap<u64, ResolvedPattern>, CodecError> {
    validate_request(wanted, bounds)?;
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut resolved: BTreeMap<u64, ResolvedPattern> = BTreeMap::new();
    let mut scanned_ids = BTreeSet::new();
    let mut scanned_types = BTreeSet::new();
    let mut retained_bytes = 0_u64;
    let mut rows_scanned = 0_u64;
    let mut decoded_bytes = 0_u64;
    for (type_id, body) in dict_sections {
        if !scanned_types.insert(type_id) {
            return Err(CodecError::SchemaMismatch);
        }
        let section_bounds = remaining_row_bounds(bounds, rows_scanned)?;
        let (entries, section_ids, section_rows, section_bytes) =
            decode_dictionary_selected(body, type_id, Some(wanted), &section_bounds)?;
        rows_scanned = rows_scanned
            .checked_add(section_rows)
            .ok_or(CodecError::TooManyRows {
                rows: usize::MAX,
                max: usize::try_from(bounds.items_per_block).unwrap_or(usize::MAX),
            })?;
        if rows_scanned > bounds.items_per_block {
            return Err(CodecError::TooManyRows {
                rows: usize::try_from(rows_scanned).unwrap_or(usize::MAX),
                max: usize::try_from(bounds.items_per_block).unwrap_or(usize::MAX),
            });
        }
        decoded_bytes =
            decoded_bytes
                .checked_add(section_bytes)
                .ok_or(CodecError::SectionTooLarge {
                    len: usize::MAX,
                    max: usize::try_from(bounds.decoded_file_bytes).unwrap_or(usize::MAX),
                })?;
        if decoded_bytes > bounds.decoded_file_bytes {
            return Err(CodecError::SectionTooLarge {
                len: usize::try_from(decoded_bytes).unwrap_or(usize::MAX),
                max: usize::try_from(bounds.decoded_file_bytes).unwrap_or(usize::MAX),
            });
        }
        for str_id in section_ids {
            if !scanned_ids.insert(str_id) {
                return Err(CodecError::SchemaMismatch);
            }
        }
        for (str_id, stored) in entries {
            insert_bounded(
                &mut resolved,
                str_id,
                ResolvedPattern::from_stored(stored),
                bounds,
                &mut retained_bytes,
            )?;
        }
    }
    Ok(resolved)
}

impl<R: kronika_format::ReadAt> PgmUnit<R> {
    /// Reads only dictionary section bodies and retains only requested IDs.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when a selected dictionary section cannot be read
    /// or decoded.
    pub fn resolve_overview_dictionary(
        &self,
        wanted: &BTreeSet<u64>,
        bounds: &crate::Bounds,
    ) -> Result<TargetedDictionaryRead, ReadError> {
        validate_request(wanted, bounds).map_err(ReadError::Codec)?;
        let mut values = BTreeMap::new();
        let mut scanned_ids = BTreeSet::new();
        let mut stats = TargetedDictionaryStats::default();
        let mut retained_bytes = 0_u64;
        if wanted.is_empty() {
            return Ok(TargetedDictionaryRead { values, stats });
        }

        for entry in &self.catalog().entries {
            if !matches!(
                entry.type_id,
                kronika_registry::DICT_STRINGS_TYPE_ID | kronika_registry::DICT_BLOBS_TYPE_ID
            ) {
                continue;
            }
            let section_bounds =
                remaining_row_bounds(bounds, stats.rows_scanned).map_err(ReadError::Codec)?;
            let projected_rows = stats
                .rows_scanned
                .checked_add(u64::from(entry.rows))
                .ok_or(ReadError::CounterOverflow)?;
            if projected_rows > bounds.items_per_block {
                return Err(ReadError::Codec(CodecError::TooManyRows {
                    rows: usize::try_from(projected_rows).unwrap_or(usize::MAX),
                    max: usize::try_from(bounds.items_per_block).unwrap_or(usize::MAX),
                }));
            }
            let body = self.verified_body(entry)?.into_bytes();
            stats.sections_read = stats
                .sections_read
                .checked_add(1)
                .ok_or(ReadError::CounterOverflow)?;
            stats.stored_bytes_read = stats
                .stored_bytes_read
                .checked_add(entry.len)
                .ok_or(ReadError::CounterOverflow)?;
            let (selected, section_ids, rows_scanned, decoded_bytes) =
                decode_dictionary_selected(body, entry.type_id, Some(wanted), &section_bounds)
                    .map_err(ReadError::Codec)?;
            Self::validate_catalog_row_count(entry, rows_scanned)?;
            for str_id in section_ids {
                if !scanned_ids.insert(str_id) {
                    return Err(ReadError::DictionaryConflict { str_id });
                }
            }
            stats.rows_scanned = stats
                .rows_scanned
                .checked_add(rows_scanned)
                .ok_or(ReadError::CounterOverflow)?;
            stats.decoded_bytes = stats
                .decoded_bytes
                .checked_add(decoded_bytes)
                .ok_or(ReadError::CounterOverflow)?;
            if stats.decoded_bytes > bounds.decoded_file_bytes {
                return Err(ReadError::Codec(CodecError::SectionTooLarge {
                    len: usize::try_from(stats.decoded_bytes).unwrap_or(usize::MAX),
                    max: usize::try_from(bounds.decoded_file_bytes).unwrap_or(usize::MAX),
                }));
            }
            for (str_id, stored) in selected {
                insert_bounded(
                    &mut values,
                    str_id,
                    ResolvedPattern::from_stored(stored),
                    bounds,
                    &mut retained_bytes,
                )
                .map_err(ReadError::Codec)?;
            }
        }
        stats.values_retained =
            u64::try_from(values.len()).map_err(|_error| ReadError::CounterOverflow)?;
        Ok(TargetedDictionaryRead { values, stats })
    }
}

fn validate_request(wanted: &BTreeSet<u64>, bounds: &crate::Bounds) -> Result<(), CodecError> {
    if !bounds.is_within_absolute_limits() || wanted.len() as u64 > bounds.items_per_block {
        return Err(CodecError::TooManyRows {
            rows: wanted.len(),
            max: usize::try_from(bounds.items_per_block).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

fn insert_bounded(
    resolved: &mut BTreeMap<u64, ResolvedPattern>,
    str_id: u64,
    value: ResolvedPattern,
    bounds: &crate::Bounds,
    retained_bytes: &mut u64,
) -> Result<(), CodecError> {
    if !value.has_valid_storage() {
        return Err(CodecError::SchemaMismatch);
    }
    let stored_len = value.stored_len() as u64;
    if stored_len > bounds.pattern_bytes {
        return Err(CodecError::SectionTooLarge {
            len: value.stored_len(),
            max: usize::try_from(bounds.pattern_bytes).unwrap_or(usize::MAX),
        });
    }
    match resolved.entry(str_id) {
        Entry::Vacant(entry) => {
            *retained_bytes =
                retained_bytes
                    .checked_add(stored_len)
                    .ok_or(CodecError::SectionTooLarge {
                        len: usize::MAX,
                        max: usize::try_from(bounds.string_table_bytes).unwrap_or(usize::MAX),
                    })?;
            if *retained_bytes > bounds.string_table_bytes {
                return Err(CodecError::SectionTooLarge {
                    len: usize::try_from(*retained_bytes).unwrap_or(usize::MAX),
                    max: usize::try_from(bounds.string_table_bytes).unwrap_or(usize::MAX),
                });
            }
            entry.insert(value);
        }
        Entry::Occupied(_) => return Err(CodecError::SchemaMismatch),
    }
    Ok(())
}

fn remaining_row_bounds(
    bounds: &crate::Bounds,
    rows_scanned: u64,
) -> Result<crate::Bounds, CodecError> {
    let remaining =
        bounds
            .items_per_block
            .checked_sub(rows_scanned)
            .ok_or(CodecError::TooManyRows {
                rows: usize::try_from(rows_scanned).unwrap_or(usize::MAX),
                max: usize::try_from(bounds.items_per_block).unwrap_or(usize::MAX),
            })?;
    Ok(crate::Bounds {
        items_per_block: remaining,
        ..*bounds
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;

    use kronika_format::{
        DEFAULT_BLOB_THRESHOLD, DictLimits, PartMeta, ReadAt, SectionInput, build_part,
    };
    use kronika_registry::Bytes;
    use kronika_writer::{Interner, dict};

    use crate::PgmUnit;

    use super::{ResolvedPattern, insert_bounded, resolve_targeted};

    /// Interns a short value (into `dict.strings`) and a long one (into
    /// `dict.blobs`), then returns the encoded dictionary section bodies.
    fn dictionary_sections() -> (Vec<(u32, u32, Bytes)>, u64, u64) {
        let mut interner = Interner::new(DictLimits::new(4_096, 1 << 20).expect("limits"));
        let short_id = interner.intern(b"idx_orders_pkey").expect("intern short");
        let long_pattern = vec![b'x'; DEFAULT_BLOB_THRESHOLD + 64];
        let long_id = interner.intern(&long_pattern).expect("intern long");
        let sections = dict::encode(interner.window()).expect("encode dictionary");
        let bodies = sections
            .iter()
            .map(|section| {
                (
                    section.type_id,
                    section.rows,
                    Bytes::from(section.body.clone()),
                )
            })
            .collect();
        (bodies, short_id.get(), long_id.get())
    }

    #[test]
    fn resolves_a_reference_from_either_section_kind() {
        let (sections, short_id, long_id) = dictionary_sections();
        let wanted = BTreeSet::from([short_id, long_id, 999_999]);
        let resolved = resolve_targeted(
            sections
                .into_iter()
                .map(|(type_id, _rows, body)| (type_id, body)),
            &wanted,
            &crate::LIMIT,
        )
        .expect("resolve");

        assert_eq!(
            resolved.get(&short_id),
            Some(&ResolvedPattern::Text(b"idx_orders_pkey".to_vec()))
        );
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
        let resolved = resolve_targeted(
            sections
                .into_iter()
                .map(|(type_id, _rows, body)| (type_id, body)),
            &wanted,
            &crate::LIMIT,
        )
        .expect("resolve");
        assert_eq!(resolved.len(), 1, "only the requested id is retained");
    }

    #[test]
    fn targeted_resolution_enforces_the_aggregate_scan_budget() {
        let (sections, short_id, _long_id) = dictionary_sections();
        let tight = crate::Bounds {
            items_per_block: 1,
            ..crate::LIMIT
        };
        let result = resolve_targeted(
            sections
                .into_iter()
                .map(|(type_id, _rows, body)| (type_id, body)),
            &BTreeSet::from([short_id]),
            &tight,
        );
        assert!(matches!(
            result,
            Err(kronika_registry::CodecError::TooManyRows { rows: 1, max: 0 })
        ));
    }

    #[test]
    fn equivalent_repeated_sections_are_rejected() {
        let (sections, short_id, _long_id) = dictionary_sections();
        let wanted = BTreeSet::from([short_id]);
        let repeated: Vec<_> = sections
            .into_iter()
            .flat_map(|(type_id, _rows, body)| [(type_id, body.clone()), (type_id, body)])
            .collect();
        assert!(matches!(
            resolve_targeted(repeated, &wanted, &crate::LIMIT),
            Err(kronika_registry::CodecError::SchemaMismatch)
        ));
    }

    #[test]
    fn cross_placement_repeat_is_rejected_transactionally() {
        let mut resolved = BTreeMap::new();
        let mut retained_bytes = 0;
        let text = b"same value".to_vec();
        insert_bounded(
            &mut resolved,
            7,
            ResolvedPattern::Text(text.clone()),
            &crate::LIMIT,
            &mut retained_bytes,
        )
        .expect("insert string");
        assert!(
            insert_bounded(
                &mut resolved,
                7,
                ResolvedPattern::Blob {
                    bytes: text.clone(),
                    full_len: text.len() as u64,
                    truncated: false,
                },
                &crate::LIMIT,
                &mut retained_bytes,
            )
            .is_err()
        );
        assert_eq!(resolved.get(&7), Some(&ResolvedPattern::Text(text.clone())));
        assert_eq!(retained_bytes, text.len() as u64);
    }

    #[test]
    fn contradictory_repeated_values_are_rejected() {
        let mut resolved = BTreeMap::new();
        let mut retained_bytes = 0;
        insert_bounded(
            &mut resolved,
            7,
            ResolvedPattern::Text(b"first".to_vec()),
            &crate::LIMIT,
            &mut retained_bytes,
        )
        .expect("insert first");
        assert!(
            insert_bounded(
                &mut resolved,
                7,
                ResolvedPattern::Text(b"second".to_vec()),
                &crate::LIMIT,
                &mut retained_bytes,
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_request_resolves_nothing() {
        let (sections, _short_id, _long_id) = dictionary_sections();
        let resolved = resolve_targeted(
            sections
                .into_iter()
                .map(|(type_id, _rows, body)| (type_id, body)),
            &BTreeSet::new(),
            &crate::LIMIT,
        )
        .expect("resolve");
        assert!(resolved.is_empty());
    }

    #[derive(Debug)]
    struct CountingReader {
        bytes: Vec<u8>,
        reads: Rc<RefCell<Vec<(u64, usize)>>>,
        byte_len_calls: Rc<Cell<u64>>,
    }

    impl ReadAt for CountingReader {
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
            self.reads.borrow_mut().push((offset, buf.len()));
            self.bytes.as_slice().read_exact_at(buf, offset)
        }

        fn byte_len(&self) -> std::io::Result<u64> {
            self.byte_len_calls.set(self.byte_len_calls.get() + 1);
            Ok(self.bytes.len() as u64)
        }
    }

    #[test]
    fn pgm_dictionary_resolution_skips_non_dictionary_sections() {
        let (dictionary, short_id, _long_id) = dictionary_sections();
        let unrelated = vec![0xA5; 128];
        let mut sections = vec![SectionInput {
            type_id: 1_006_001,
            rows: 1,
            body: &unrelated,
        }];
        sections.extend(dictionary.iter().map(|(type_id, rows, body)| SectionInput {
            type_id: *type_id,
            rows: *rows,
            body: body.as_ref(),
        }));
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: 1,
                max_ts: 2,
            },
        );
        let reads = Rc::new(RefCell::new(Vec::new()));
        let byte_len_calls = Rc::new(Cell::new(0));
        let unit = PgmUnit::open(CountingReader {
            bytes,
            reads: Rc::clone(&reads),
            byte_len_calls: Rc::clone(&byte_len_calls),
        })
        .expect("open");
        let reads_before = reads.borrow().len();

        let result = unit
            .resolve_overview_dictionary(&BTreeSet::from([short_id]), &crate::LIMIT)
            .expect("resolve");
        let body_reads = &reads.borrow()[reads_before..];
        let dictionary_entries: Vec<_> = unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.type_id,
                    kronika_registry::DICT_STRINGS_TYPE_ID | kronika_registry::DICT_BLOBS_TYPE_ID
                )
            })
            .collect();
        let expected_stored_bytes: u64 = dictionary_entries.iter().map(|entry| entry.len).sum();
        let expected_rows: u64 = dictionary_entries
            .iter()
            .map(|entry| u64::from(entry.rows))
            .sum();
        let expected_decoded_bytes =
            b"idx_orders_pkey".len() as u64 + (DEFAULT_BLOB_THRESHOLD + 64) as u64;
        assert_eq!(result.stats.sections_read, 2);
        assert_eq!(body_reads.len(), 2);
        assert!(body_reads.iter().all(|(_offset, length)| *length != 128));
        assert_eq!(
            body_reads
                .iter()
                .map(|(_offset, length)| *length as u64)
                .sum::<u64>(),
            expected_stored_bytes
        );
        assert_eq!(result.stats.stored_bytes_read, expected_stored_bytes);
        assert_eq!(result.stats.rows_scanned, expected_rows);
        assert_eq!(result.stats.decoded_bytes, expected_decoded_bytes);
        assert_eq!(result.values.len(), 1);
        assert_eq!(result.stats.values_retained, 1);
        assert_eq!(byte_len_calls.get(), 1);
    }

    #[test]
    fn an_empty_pgm_dictionary_request_reads_no_bodies() {
        let (dictionary, _short_id, _long_id) = dictionary_sections();
        let sections: Vec<_> = dictionary
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body: body.as_ref(),
            })
            .collect();
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: 1,
                max_ts: 2,
            },
        );
        let reads = Rc::new(RefCell::new(Vec::new()));
        let byte_len_calls = Rc::new(Cell::new(0));
        let unit = PgmUnit::open(CountingReader {
            bytes,
            reads: Rc::clone(&reads),
            byte_len_calls: Rc::clone(&byte_len_calls),
        })
        .expect("open");
        let reads_before = reads.borrow().len();

        let result = unit
            .resolve_overview_dictionary(&BTreeSet::new(), &crate::LIMIT)
            .expect("empty request");
        assert!(result.values.is_empty());
        assert_eq!(result.stats, super::TargetedDictionaryStats::default());
        assert_eq!(reads.borrow().len(), reads_before);
        assert_eq!(byte_len_calls.get(), 1);
    }

    #[test]
    fn pgm_dictionary_budget_rejects_the_next_catalog_entry_before_reading_it() {
        let (dictionary, short_id, _long_id) = dictionary_sections();
        let sections: Vec<_> = dictionary
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body: body.as_ref(),
            })
            .collect();
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: 1,
                max_ts: 2,
            },
        );
        let reads = Rc::new(RefCell::new(Vec::new()));
        let byte_len_calls = Rc::new(Cell::new(0));
        let unit = PgmUnit::open(CountingReader {
            bytes,
            reads: Rc::clone(&reads),
            byte_len_calls,
        })
        .expect("open");
        let reads_before = reads.borrow().len();
        let tight = crate::Bounds {
            items_per_block: 1,
            ..crate::LIMIT
        };

        let result = unit.resolve_overview_dictionary(&BTreeSet::from([short_id]), &tight);
        assert!(matches!(
            result,
            Err(crate::ReadError::Codec(
                kronika_registry::CodecError::TooManyRows { rows: 2, max: 1 }
            ))
        ));
        assert_eq!(
            reads.borrow().len() - reads_before,
            1,
            "the second dictionary body must be rejected from its catalog row claim"
        );
    }
}
