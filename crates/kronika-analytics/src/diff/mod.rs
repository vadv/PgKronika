//! Counter deltas and reset handling.
//!
//! Turns cumulative counters into per-interval deltas and rates. The core is
//! [`diff_pair`]: a pure function over two consecutive samples of one series,
//! with no knowledge of `PostgreSQL`. Series grouping and reader integration are
//! added on top in the query layer.

mod pair;

pub use pair::{DiffPoint, Reason, Scalar, diff_pair};
