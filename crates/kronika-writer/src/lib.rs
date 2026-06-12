//! Write path: string interner, per-type buffers, parts journal, merge and seal.
//!
//! The crate's scope and its boundary with `kronika-format` (which defines
//! the byte layout this crate produces) are documented in this crate's
//! README.md. Buffers, the `active.parts` journal, merge and seal arrive
//! in later steps.

mod interner;

pub use interner::Interner;
