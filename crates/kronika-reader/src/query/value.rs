//! Output value model and the `Cell` -> `Value` mapping.
//!
//! [`Value`] is the width-neutral output type the JSON layer consumes: integer
//! widths collapse to `I64`/`U64`, and dictionary ids resolve to their bytes.
//! [`OutRow`] carries one row's cells keyed by union-column name; absent columns
//! are [`Value::Null`]. [`Gap`] describes a coverage hole in a result and is
//! populated by later query stages.

use crate::{Cell, Dictionary, Resolved};

/// One output cell, after dictionary resolution and width normalization.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A `NULL`, an unresolved id, or the `StrId(0)` sentinel.
    Null,
    /// Signed integer, widened from any signed cell width.
    I64(i64),
    /// Unsigned integer, widened from any unsigned cell width.
    U64(u64),
    /// 64-bit float.
    F64(f64),
    /// Boolean.
    Bool(bool),
    /// Timestamp, unix microseconds.
    Ts(i64),
    /// A resolved `dict.strings` value, lossily decoded as UTF-8.
    Str(String),
    /// A resolved `dict.blobs` value; `text` is a prefix of the original when
    /// `truncated`.
    Blob {
        /// Stored bytes, lossily decoded as UTF-8.
        text: String,
        /// Length of the full original value, bytes.
        full_len: u64,
        /// Whether `text` is only a prefix of the original.
        truncated: bool,
    },
    /// A list of signed 32-bit integers.
    ListI32(Vec<i32>),
}

/// One output row: union-column name to [`Value`], in logical-section column
/// order. Columns absent from a row's layout version are [`Value::Null`].
pub type OutRow = Vec<(String, Value)>;

/// A coverage hole in a result, over the half-open time range `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// Start of the gap, unix microseconds, inclusive.
    pub from: i64,
    /// End of the gap, unix microseconds, exclusive.
    pub to: i64,
    /// Why the range is missing.
    pub reason: GapReason,
}

/// Why a [`Gap`] range carries no rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapReason {
    /// A journal frame in the range failed to decode.
    CorruptJournalFrame,
    /// A sealed segment in the range failed to decode.
    CorruptSegment,
    /// No segment or journal part covers the range.
    NoCoverage,
}

/// Map one decoded cell to an output value, resolving string ids through `dict`.
/// Returns the value plus the id that failed to resolve: `Some(id)` iff the cell
/// was `StrId(id)` with `id != 0` and `dict.resolve(id)` returned `None`
/// (dictionary gap for that segment). `None` in every other case, including the
/// `StrId(0)` sentinel and `Cell::Null`, which are legitimate nulls.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the row-materialization caller lands in a later change; only unit tests exercise it now"
    )
)]
pub(crate) fn cell_to_value(cell: &Cell, dict: &Dictionary) -> (Value, Option<u64>) {
    match cell {
        Cell::I16(v) => (Value::I64(i64::from(*v)), None),
        Cell::I32(v) => (Value::I64(i64::from(*v)), None),
        Cell::I64(v) => (Value::I64(*v), None),
        Cell::U32(v) => (Value::U64(u64::from(*v)), None),
        Cell::U64(v) => (Value::U64(*v), None),
        Cell::F64(v) => (Value::F64(*v), None),
        Cell::Bool(v) => (Value::Bool(*v), None),
        Cell::Ts(v) => (Value::Ts(*v), None),
        Cell::ListI32(v) => (Value::ListI32(v.clone()), None),
        // `StrId(0)` is the "no string" sentinel, a legitimate null like `Null`.
        Cell::Null | Cell::StrId(0) => (Value::Null, None),
        Cell::StrId(id) => match dict.resolve(*id) {
            Some(Resolved::String(bytes)) => (
                Value::Str(String::from_utf8_lossy(bytes).into_owned()),
                None,
            ),
            Some(Resolved::Blob {
                bytes,
                full_len,
                truncated,
            }) => (
                Value::Blob {
                    text: String::from_utf8_lossy(bytes).into_owned(),
                    full_len,
                    truncated,
                },
                None,
            ),
            None => (Value::Null, Some(*id)),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{Cell, Dictionary, Stored};

    use super::{Value, cell_to_value};

    /// A `Dictionary` from `(id, Stored)` pairs, using the crate's own storage
    /// type — the real production dictionary, not a mock.
    fn dict_from(entries: Vec<(u64, Stored)>) -> Dictionary {
        Dictionary {
            by_id: entries.into_iter().collect::<HashMap<_, _>>(),
        }
    }

    /// Build a real `Dictionary` via the foundation's write path.
    fn dict_via_writer(values: &[&[u8]]) -> (Dictionary, Vec<u64>) {
        use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
        use kronika_writer::{Interner, dict};

        let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
        let ids: Vec<u64> = values
            .iter()
            .map(|value| interner.intern(value).expect("intern").get())
            .collect();
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");

        let sections: Vec<_> = dict_sections
            .iter()
            .map(|section| SectionInput {
                type_id: section.type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect();
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("d.pgm");
        std::fs::write(&path, &bytes).expect("write");

        let segment = crate::Segment::open(&path).expect("open");
        let dictionary = segment.dictionary().expect("read dictionary");
        (dictionary, ids)
    }

    fn empty_dict() -> Dictionary {
        Dictionary::default()
    }

    #[test]
    fn i16_widens_to_i64() {
        let (value, signal) = cell_to_value(&Cell::I16(-1), &empty_dict());
        assert_eq!(value, Value::I64(-1));
        assert_eq!(signal, None);
    }

    #[test]
    fn i32_widens_to_i64() {
        let (value, signal) = cell_to_value(&Cell::I32(i32::MIN), &empty_dict());
        assert_eq!(value, Value::I64(i64::from(i32::MIN)));
        assert_eq!(signal, None);
    }

    #[test]
    fn i64_passes_through() {
        let (value, signal) = cell_to_value(&Cell::I64(i64::MIN), &empty_dict());
        assert_eq!(value, Value::I64(i64::MIN));
        assert_eq!(signal, None);
    }

    #[test]
    fn u32_widens_to_u64() {
        let (value, signal) = cell_to_value(&Cell::U32(u32::MAX), &empty_dict());
        assert_eq!(value, Value::U64(u64::from(u32::MAX)));
        assert_eq!(signal, None);
    }

    #[test]
    fn u64_passes_through() {
        let (value, signal) = cell_to_value(&Cell::U64(u64::MAX), &empty_dict());
        assert_eq!(value, Value::U64(u64::MAX));
        assert_eq!(signal, None);
    }

    #[test]
    fn f64_passes_through() {
        let (value, signal) = cell_to_value(&Cell::F64(1.5), &empty_dict());
        assert_eq!(value, Value::F64(1.5));
        assert_eq!(signal, None);
    }

    #[test]
    fn bool_passes_through() {
        let (value, signal) = cell_to_value(&Cell::Bool(true), &empty_dict());
        assert_eq!(value, Value::Bool(true));
        assert_eq!(signal, None);
    }

    #[test]
    fn ts_passes_through() {
        let (value, signal) = cell_to_value(&Cell::Ts(1_700_000_000_000_000), &empty_dict());
        assert_eq!(value, Value::Ts(1_700_000_000_000_000));
        assert_eq!(signal, None);
    }

    #[test]
    fn null_cell_is_null_value() {
        let (value, signal) = cell_to_value(&Cell::Null, &empty_dict());
        assert_eq!(value, Value::Null);
        assert_eq!(signal, None);
    }

    #[test]
    fn str_id_zero_is_the_null_sentinel_not_a_signal() {
        let (value, signal) = cell_to_value(&Cell::StrId(0), &empty_dict());
        assert_eq!(value, Value::Null);
        assert_eq!(signal, None);
    }

    #[test]
    fn list_i32_keeps_order_and_values() {
        let (value, signal) = cell_to_value(&Cell::ListI32(vec![3, 1, 2, -7]), &empty_dict());
        assert_eq!(value, Value::ListI32(vec![3, 1, 2, -7]));
        assert_eq!(signal, None);
    }

    #[test]
    fn str_id_resolves_to_a_string_via_the_real_dictionary() {
        let (dictionary, ids) = dict_via_writer(&[b"db-host-01", b"node-7"]);
        let (value, signal) = cell_to_value(&Cell::StrId(ids[0]), &dictionary);
        assert_eq!(value, Value::Str("db-host-01".to_owned()));
        assert_eq!(signal, None);

        let (value, signal) = cell_to_value(&Cell::StrId(ids[1]), &dictionary);
        assert_eq!(value, Value::Str("node-7".to_owned()));
        assert_eq!(signal, None);
    }

    #[test]
    fn truncated_blob_carries_full_len_and_truncated_flag() {
        let dictionary = dict_from(vec![(
            42,
            Stored::Blob {
                bytes: b"SELECT * FROM".to_vec(),
                full_len: 4096,
                truncated: true,
            },
        )]);
        let (value, signal) = cell_to_value(&Cell::StrId(42), &dictionary);
        let Value::Blob {
            text,
            full_len,
            truncated,
        } = value
        else {
            panic!("expected Value::Blob, got {value:?}");
        };
        assert_eq!(text, "SELECT * FROM");
        assert!(truncated);
        assert_eq!(full_len, 4096);
        assert!(full_len > text.len() as u64);
        assert_eq!(signal, None);
    }

    #[test]
    fn missing_str_id_yields_null_and_signals_the_id() {
        let (value, signal) = cell_to_value(&Cell::StrId(777), &empty_dict());
        assert_eq!(value, Value::Null);
        assert_eq!(signal, Some(777));
    }
}
