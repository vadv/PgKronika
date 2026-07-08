//! Segment storage abstractions.
//!
//! This crate provides read-only access to a local directory of PGM segments.
//! It lists sealed `.pgm` files, decodes their end catalogs cheaply (tail read
//! only, no section bodies), and streams valid parts from the `active.parts`
//! journal without loading the whole file.
//!
//! The crate depends only on `kronika-format`. Section decoding lives in
//! `kronika-reader`.

mod local;
mod source;

pub use local::{LocalDir, read_catalog};
pub use source::{ActivePart, LocalScan, SealedUnit, StoreError, StoreWarning};
