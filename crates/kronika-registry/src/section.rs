//! The `Section` trait: the typed codec contract `#[derive(Section)]` writes.
//!
//! Generic code — a shared roundtrip test, a future typed ingest helper — works
//! over `T: Section` instead of naming each type. Runtime dispatch by `type_id`
//! does not go through this trait: it uses [`decode_any`](crate::decode_any),
//! driven by [`registry`](crate::registry), so a new section type costs one
//! registry entry and no per-type `match` (README.md, "Section Trait").

use crate::codec::CodecError;
use crate::contract::TypeContract;

/// A section type: its registry contract plus the Parquet codec for its rows.
///
/// Implemented only by `#[derive(Section)]`, which lives inside this crate, so
/// every implementor's [`CONTRACT`](Section::CONTRACT) is valid by construction
/// (the derive routes the id through the crate-private `TypeId` constructor).
pub trait Section: Sized {
    /// The registry contract for this type.
    const CONTRACT: TypeContract;

    /// Encode `rows` into a Parquet section body.
    ///
    /// # Errors
    ///
    /// [`CodecError::TooManyRows`] above the row cap; [`CodecError::Arrow`] or
    /// [`CodecError::Parquet`] if Arrow rejects the batch or writing fails.
    fn encode(rows: &[Self]) -> Result<Vec<u8>, CodecError>;

    /// Decode a Parquet section body back into typed rows.
    ///
    /// # Errors
    ///
    /// A memory-bound [`CodecError`] if a cap is exceeded, [`CodecError::Parquet`]
    /// on malformed Parquet, or a column error if the file does not match
    /// [`CONTRACT`](Section::CONTRACT).
    fn decode(bytes: &[u8]) -> Result<Vec<Self>, CodecError>;
}
