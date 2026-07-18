//! Transport-free incident domain core.
//!
//! Reader preparation and HTTP/JSON adapters live outside this module.

mod active;
mod cluster;
mod dispatch;
mod engine;
mod evidence;
mod lens;
mod lenses;
mod model;
mod series;
mod typed;

// Input preparation and consumers live outside this module (`incident_input`),
// so the reader-facing input surface and the engine entry point are exported.
pub(crate) use model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
pub(crate) use series::{Series, SeriesError, SeriesInsertError, SeriesSet};
pub(crate) use typed::TypedInputs;

pub(crate) use active::{active_catalog, active_catalog_ids};
pub(crate) use dispatch::LimitAxis;
pub(crate) use engine::{
    AnalyzeError, ClockRelation, EngineOutcome, EngineSkip, Incident, IncidentConfig, analyze,
};
pub(crate) use evidence::Finding;
#[allow(
    unused_imports,
    reason = "engine tests use Lens while the HTTP endpoint exposes clustering only"
)]
pub(crate) use lens::Lens;
pub(crate) use lenses::dormant_catalog;
