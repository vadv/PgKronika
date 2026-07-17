//! Spike scoring over derivative series.
//!
//! [`score_window`] rates how strongly a window of values deviates from a
//! reference set using the modified z-score (robust to outliers in the
//! reference); [`episodes`] folds a timeline of scores into contiguous
//! above-threshold episodes with a peak. Both are deterministic, I/O-free,
//! and carry every number needed to explain a verdict to a human.
//!
//! The crate knows nothing about metric sources or sections: callers feed
//! plain finite `f64` values (derivative rates, gauge readings) and interpret
//! the result.

mod episode;
mod score;

pub use episode::{Episode, episodes};
pub use score::{Direction, Evaluated, NotEvaluatedReason, ScoreParams, Scored, score_window};
