//! PGM container primitives: tail pointer, end catalog, HOT block headers, `active.parts` frames, dictionaries, `str_id`, CRC32C. No Parquet, no I/O.
//!
//! See `docs/architecture.md` for the workspace layout and the dependency
//! rules between crates, and `docs/segment-format.md` for the PGM format.
