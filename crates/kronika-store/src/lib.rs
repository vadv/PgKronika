//! Segment storage abstractions.
//!
//! [`LocalDir`] provides read-only access to a local directory of PGM segments.
//! It lists sealed `.pgm` files, validates their end catalogs without retaining
//! per-entry tables, and streams valid parts from the `active.parts` journal
//! without loading the whole journal.
//!
//! Each discovered sealed unit retains its exact [`kronika_layout::FileIdentity`]
//! and an `Arc`-shared, fixed-size [`CatalogSummary`]. The summary includes a
//! 512-bit Bloom filter for section types with non-zero rows. The filter has no
//! false negatives; a positive result is only a hint, and consumers must confirm
//! it against the full catalog opened lazily under their typed work budget.
//!
//! Typed filesystem traversal comes from `kronika-layout`; byte framing comes
//! from `kronika-format`. Section decoding lives in `kronika-reader`.
//!
//! Catalog, part, and retained-metadata sizes are checked before allocation.
//! Discovery checks the opened PGM identity before and after reading its catalog.
//! Unchanged scans and cloned scan results share the sealed collection and
//! summaries. Directory traversal, journal metadata, retained collections, and
//! summaries use the same 128 MiB `LayoutLimits::max_metadata_bytes` ceiling.
//! A cached sealed or live reference may become stale after publication or reset,
//! so consumers must refresh instead of treating changed bytes as the original
//! unit.
//!
//! `PgKronika` has not had a public release. Journal v1 is its first and only
//! journal format; there is no alternate-version reader or migration path.

mod catalog_summary;
mod local;
mod source;

pub use catalog_summary::{
    CatalogDigest, CatalogLayoutDigest, CatalogSummary, CatalogSummaryError, FirstCatalogEntry,
    PgmSourceDigest, catalog_digests,
};
pub use local::{LocalDir, is_active_journal_scan_error, read_catalog};
pub use source::{
    ActiveJournalWarningReason, ActivePart, InvalidPgmReason, JournalScan, LocalScan, SealedUnit,
    StoreError, StoreIoFailure, StoreIoOperation, StoreObject, StoreWarning, StoreWarningReason,
};
