//! Explainable absolute-threshold classification over fixed-size numeric inputs.

mod model;

pub use model::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
