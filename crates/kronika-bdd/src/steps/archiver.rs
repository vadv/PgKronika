//! Archiver-specific step glue (`pg_stat_archiver`, type `1_008_001`).
//!
//! `pg_stat_archiver` is a singleton: one row per snapshot, no per-session key.
//! Generic assertion and oracle steps live in [`super::common`]; this module is
//! a placeholder for any archiver-specific step definitions that may be added later.
