//! Incident analysis over anomaly episodes: pure domain core.
//!
//! These modules hold no I/O, HTTP or JSON: the reader adapter, engine wiring
//! and endpoint are added in later steps. Clustering and the canonical key are
//! deterministic and unit-tested here without fixtures.

mod cluster;
mod dispatch;
mod evidence;
mod model;
