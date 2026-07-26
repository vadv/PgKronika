//! Spike scoring and bounded change evidence over derivative series.
//!
//! [`score_window`] rates how strongly a window of values deviates from a
//! reference set using the modified z-score (robust to outliers in the
//! reference); [`episodes`] folds a timeline of scores into contiguous
//! above-threshold episodes with a peak. [`compare_distributions`] compares
//! count-normalized categorical mixtures, while [`compare_per_unit`] compares
//! work normalized by an operation count. The kernels are deterministic,
//! I/O-free, and carry every number needed to explain a verdict.
//!
//! The crate knows nothing about metric sources or sections: callers feed
//! plain finite `f64` values (derivative rates, gauge readings) and interpret
//! the result.

mod change;
mod episode;
mod score;

pub use change::{
    CategoryCount, CategoryShareChange, ChangeNotEvaluatedReason, DistributionEvidence,
    DistributionOutcome, DistributionParams, PerUnitEvidence, PerUnitOutcome, PerUnitParams,
    WorkTotals, compare_distributions, compare_per_unit,
};
pub use episode::{Episode, episodes};
pub use score::{Direction, Evaluated, NotEvaluatedReason, ScoreParams, Scored, score_window};
