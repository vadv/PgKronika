//! Segment storage abstractions.
//!
//! [`LocalDir`] provides read-only access to a local directory of PGM segments.
//! It lists sealed `.pgm` files, decodes their end catalogs cheaply (tail read
//! only, no section bodies), and streams valid parts from the `active.parts`
//! journal without loading the whole file.
//!
//! The crate depends only on `kronika-format`. Section decoding lives in
//! `kronika-reader`.
//!
//! Sealed-file failures and damaged live ranges are returned as typed warnings
//! while other valid units remain visible. Catalog and part lengths are capped
//! before allocation. A cached live-part reference may become stale after the
//! writer seals or resets the journal; consumers must refresh their snapshot
//! instead of treating the changed bytes as the original part.

mod local;
mod source;

pub use local::{LocalDir, read_catalog};
pub use source::{ActivePart, LocalScan, SealedUnit, StoreError, StoreWarning};
