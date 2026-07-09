//! Query layer: logical sections and column union views.

mod cursor;
mod logical;
mod section;
mod value;

pub use cursor::Cursor;
pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use section::{QueryError, SectionPage, section, sections};
pub use value::{Gap, OutRow, Value};
