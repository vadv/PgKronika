//! Query layer: logical sections and column union views.

mod logical;
mod section;
mod value;

pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use section::{Cursor, QueryError, SectionPage, section, sections};
pub use value::{Gap, GapReason, OutRow, Value};
