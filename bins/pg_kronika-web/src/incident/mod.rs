//! Transport-free incident domain core.
//!
//! Reader preparation and HTTP/JSON adapters live outside this module.

mod cluster;
mod dispatch;
mod engine;
mod evidence;
mod lens;
mod model;
mod series;

// Input preparation and consumers live outside this module (`incident_input`),
// so the reader-facing input surface and the engine entry point are exported.
pub(crate) use model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
pub(crate) use series::{Series, SeriesError, SeriesInsertError, SeriesSet};

// The engine entry point and its result types have no non-test consumer until
// the HTTP endpoint lands; the input adapter's tests already drive them.
#[allow(
    unused_imports,
    reason = "engine surface awaits the incident endpoint; exercised by adapter tests"
)]
pub(crate) use engine::{ClockRelation, EngineOutcome, Incident, IncidentConfig, analyze};
#[allow(
    unused_imports,
    reason = "the lens catalog and its endpoint land in a later step"
)]
pub(crate) use lens::Lens;
