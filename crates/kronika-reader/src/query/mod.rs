//! Query layer: logical sections and column union views.

mod cursor;
mod diff;
mod gauge;
mod logical;
mod section;
mod value;

pub use cursor::Cursor;
pub use diff::{ColumnDiff, DiffAt, SeriesDiff, diff_section};
pub use gauge::{ColumnValues, SeriesValues, gauge_section};
pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use section::{QueryError, SectionPage, section, sections};
pub use value::{Gap, OutRow, Value};
