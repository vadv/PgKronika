//! Query layer: logical sections and column union views.

mod logical;
mod value;

pub use logical::{LogicalColumn, LogicalSection, logical_section};
pub use value::{Gap, GapReason, OutRow, Value};
