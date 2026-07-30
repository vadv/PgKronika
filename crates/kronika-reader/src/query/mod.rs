//! Query layer: logical sections and column union views.

mod cursor;
mod diff;
mod gating;
mod gauge;
mod latest;
mod logical;
mod section;
mod value;

pub use cursor::Cursor;
pub use diff::{ColumnDiff, DiffAt, SeriesDiff, diff_section};
pub use gating::{GateReading, apply_collection_gating, apply_gating, gate_readings, select_gate};
pub use gauge::{ColumnValues, SeriesValues, gauge_section};
pub use latest::latest_section_row;
pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use section::{
    QueryError, QueryLimits, QueryWorkLimits, QueryWorkResource, SealedQuerySession, SectionPage,
    section, section_with_limits, sections, sections_from_sealed_descriptor_with_limits,
    sections_with_limits,
};
pub use value::{Gap, OutRow, Value};
