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
pub use latest::{
    SourceSummary, SourceSummaryError, SourceSummaryLimits, SourceSummaryResource,
    latest_section_row, source_summaries,
};
pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use section::{
    QueryError, QueryLimits, QueryWorkLimits, QueryWorkResource, SectionPage, section,
    section_with_limits, sections, sections_with_limits,
};
pub use value::{Gap, OutRow, Value};
