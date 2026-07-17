//! Source-independent analytics: counter diffs and spike scoring.
//!
//! Holds no knowledge of `PostgreSQL`, Linux, the registry, the reader, or any
//! transport. Counter deltas (`diff`) and window scoring (`anomaly`) are pure
//! functions over numeric samples.

pub mod anomaly;
pub mod diff;

pub use anomaly::{
    Direction, Episode, Evaluated, NotEvaluatedReason, ScoreParams, Scored, episodes, score_window,
};
pub use diff::{DiffPoint, Reason, Scalar, diff_pair};
