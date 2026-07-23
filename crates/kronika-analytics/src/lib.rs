//! Source-independent counter differences, anomaly scoring, and overview
//! contracts.
//!
//! [`diff`] interprets two numeric counter samples without extrapolation:
//! integer deltas stay exact, decreases are resets, and non-positive time
//! intervals are invalid. The caller adds series-wide coverage, first-point,
//! and collection-gate reasons because one pair cannot infer them.
//!
//! [`anomaly`] compares a current window with a reference window using a
//! modified z-score, then groups consecutive above-threshold positions into
//! episodes. Empty, discontinuous, undersized, or non-finite inputs produce an
//! explicit not-evaluated reason rather than a score.
//!
//! The functions allocate in proportion to their input slices or returned
//! episodes. These functions do not impose request ceilings; adapters such as
//! `pg_kronika-web` must bound samples, window positions, work, and output
//! before calling them. The [`diff`] and [`anomaly`] kernels have no
//! `PostgreSQL`, Linux, registry, reader, or transport knowledge.
//!
//! [`overview`] provides implementation-independent retained `PostgreSQL` event,
//! reduction, coverage, health, and bounded oracle-adapter contracts. It does
//! not implement PGM extraction, persistent indexing, or HTTP. See the
//! [crate README](../README.md).

pub mod anomaly;
pub mod diff;
pub mod overview;

pub use anomaly::{
    Direction, Episode, Evaluated, NotEvaluatedReason, ScoreParams, Scored, episodes, score_window,
};
pub use diff::{DiffPoint, Reason, Scalar, diff_pair};
