//! Type `1_018_001`: per-database wraparound ages.
//!
//! One row per `pg_database` row: `age(datfrozenxid)` and
//! `mxid_age(datminmxid)`.

use crate::{Section, StrId, Ts};

/// Type `1_018_001`: wraparound age by database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_018_001,
    name = "wraparound",
    semantics = snapshot_full,
    sort_key("datname", "ts")
)]
pub struct WraparoundAge {
    /// Snapshot time, unix microseconds; one value for all rows of a snapshot.
    #[column(t)]
    pub ts: Ts,
    /// Database name, interned into the segment dictionary.
    #[column(l)]
    pub datname: StrId,
    /// XID age (`age(datfrozenxid)`).
    #[column(g)]
    pub age: i64,
    /// Multixact age (`mxid_age(datminmxid)`).
    #[column(g)]
    pub mxid_age: i64,
}

#[cfg(test)]
mod tests {
    use super::WraparoundAge;
    use crate::{Section, StrId, Ts, lint};

    fn row(ts: i64, datname: u64, age: i64, mxid_age: i64) -> WraparoundAge {
        WraparoundAge {
            ts: Ts(ts),
            datname: StrId(datname),
            age,
            mxid_age,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[WraparoundAge::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape_matches_the_source() {
        let c = WraparoundAge::CONTRACT;
        assert_eq!(c.type_id.get(), 1_018_001);
        assert_eq!(c.columns.len(), 4);
        assert_eq!(c.sort_key, ["datname", "ts"]);
        assert_eq!(c.column("datname").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("age").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("mxid_age").map(|col| col.nullable), Some(false));
    }

    #[test]
    fn roundtrip_preserves_values() {
        crate::assert_roundtrips(&[
            row(1_000_000, 1, 150_000_000, 5_000_000),
            row(1_000_000, 2, 42, 0),
        ]);
    }
}
