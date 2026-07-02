//! Steps for `features/user_tables.feature` (types `1_013_003` / `1_014_002`).
//!
//! Both scenarios use two isolated databases so no scenario shares table state
//! with another. Each row is selected by its label plus `datname` through the
//! shared multi-key row step, and each side's counters are checked against the
//! shared oracle step's `in the second database` form; this module has no
//! metric-specific steps of its own.
