//! Steps for `features/pg_stat_database.feature` (type `1_005_003`).
//!
//! `pg_stat_database` has one row per database in the cluster. The scenario's
//! own row is selected by `datname = [scenario database]` through the shared
//! multi-key row step in [`super::common`]; this module has no metric-specific
//! steps of its own.
