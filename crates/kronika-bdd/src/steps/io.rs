//! Steps for `features/pg_stat_io.feature` (types `1_009_001` / `1_009_002`).
//!
//! The row is selected by its `(backend_type, object, context)` label triple
//! through the shared multi-key row step in [`super::common`]; this module has
//! no metric-specific steps of its own.
