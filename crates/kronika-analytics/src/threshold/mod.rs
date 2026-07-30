//! Explainable absolute-threshold classification over fixed-size numeric inputs.

mod catalog;
mod model;
mod policy;

pub use catalog::{Calibration, CatalogEntry, MetricId, Unit, catalog, catalog_entry, classify};
pub use model::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
pub use policy::{
    AgePolicy, Direction, FractionPolicy, FreeCapacityPolicy, InputKind, InvalidPolicy, Policy,
    RatioWithFloorPolicy, ScalarPolicy, WarningLimitPolicy, ZeroDisposition,
};
