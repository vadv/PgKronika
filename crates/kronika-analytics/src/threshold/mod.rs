//! Explainable absolute-threshold classification over fixed-size numeric inputs.

mod model;
mod policy;

pub use model::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
pub use policy::{
    AgePolicy, Direction, FractionPolicy, FreeCapacityPolicy, InputKind, InvalidPolicy, Policy,
    RatioWithFloorPolicy, ScalarPolicy, ZeroDisposition,
};
