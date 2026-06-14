//! The `Section` trait: the typed codec contract `#[derive(Section)]` writes.
//!
//! Generic code — a shared roundtrip test, a future typed ingest helper — works
//! over `T: Section` instead of naming each type. Selection by `type_id` does
//! not go through this trait: it uses [`decode_any`](crate::decode_any),
//! driven by [`registry`](crate::registry), so a new section type costs one
//! registry entry and no per-type `match` (README.md, "Section Trait").

use crate::codec::{CodecError, VerifiedSection};
use crate::contract::TypeContract;

/// A section type: its registry contract plus the Parquet codec for its rows.
///
/// Sealed: the supertrait is crate-private and only this crate's
/// `#[derive(Section)]` emits it, so `T: Section` always means a registry type
/// whose [`CONTRACT`](Section::CONTRACT) is valid by construction (the derive
/// routes the id through the crate-private `TypeId` constructor). A downstream
/// crate cannot implement it to forge a codec for an existing contract.
pub trait Section: crate::sealed::Sealed + Sized {
    /// The registry contract for this type.
    const CONTRACT: TypeContract;

    /// Encode `rows` into a Parquet section body.
    ///
    /// # Errors
    ///
    /// [`CodecError::TooManyRows`] above the row cap; [`CodecError::Arrow`] or
    /// [`CodecError::Parquet`] if Arrow rejects the batch or writing fails.
    fn encode(rows: &[Self]) -> Result<Vec<u8>, CodecError>;

    /// Decode a section body back into typed rows.
    ///
    /// Takes a [`VerifiedSection`] so the CRC-before-decode boundary is in the
    /// type: the bytes were checked against the catalog before reaching here.
    ///
    /// This builds typed rows by transposing the columnar Parquet — one indexed
    /// read per cell — which fits the typed/convenience use and the roundtrip
    /// tests. The bulk reader path is [`decode_any`](crate::decode_any): it stays
    /// columnar (`RecordBatch`) and does no per-row gather, so a large section is
    /// decoded there, not here.
    ///
    /// # Errors
    ///
    /// A memory-bound [`CodecError`] if a cap is exceeded, [`CodecError::Parquet`]
    /// on malformed Parquet, or a column error if the file does not match
    /// [`CONTRACT`](Section::CONTRACT).
    fn decode(section: VerifiedSection) -> Result<Vec<Self>, CodecError>;

    /// The `(min, max)` of this type's timestamp column across `rows`, or `None`
    /// when the type has no timestamp column.
    ///
    /// The writer folds these into the part and segment catalog time range, which
    /// drives time-range reads and merge idempotency (README.md, "Section Trait").
    /// The derive reads the `#[column(t)]` field, so a type without one — a
    /// dictionary or event section — returns `None`.
    fn ts_range(rows: &[Self]) -> Option<(i64, i64)>;
}

/// Encode `rows`, decode the section back, and assert they roundtrip — the
/// shared codec test the trait exists to enable, so each type's test is one
/// line, not a custom encode/decode check.
#[cfg(test)]
pub(crate) fn assert_roundtrips<T>(rows: &[T])
where
    T: Section + PartialEq + std::fmt::Debug,
{
    let bytes = T::encode(rows).expect("encode");
    let decoded = T::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
    assert_eq!(decoded.as_slice(), rows);
}
