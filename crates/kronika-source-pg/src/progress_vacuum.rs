//! `pg_stat_progress_vacuum` collection for type `1_012_001`.
//!
//! One row per backend running `VACUUM`; the view is empty when no vacuum runs.
//! The server major selects the SQL; fields absent from that catalog version
//! become `None`. Collection returns owned rows; the caller interns `datname`
//! and `phase` into the segment dictionary.

use kronika_registry::pg_stat_progress_vacuum::PgStatProgressVacuum;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/progress_vacuum.rs */ ",
            $sql,
        )
    };
}

/// The `pg_stat_progress_vacuum` column set for one server major.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressVacuumVersion {
    /// PG 10-16: tuple-count dead-tuple columns.
    Pre17,
    /// PG 17: byte-based TID store and index-progress counters.
    Pg17,
    /// PG 18: PG17 columns plus `delay_time`.
    Pg18,
}

/// Select the column set for a server major.
///
/// PG17 replaced the dead-tuple counters with a byte-based TID store; PG18 added
/// `delay_time`.
#[must_use]
pub const fn progress_vacuum_version(major: u32) -> ProgressVacuumVersion {
    if major >= 18 {
        ProgressVacuumVersion::Pg18
    } else if major >= 17 {
        ProgressVacuumVersion::Pg17
    } else {
        ProgressVacuumVersion::Pre17
    }
}

/// SQL for one column set.
///
/// Each query carries the kronika marker and selects only the columns that
/// version exposes. `ts` is one `statement_timestamp()` for the whole snapshot.
#[must_use]
pub const fn progress_vacuum_query(version: ProgressVacuumVersion) -> &'static str {
    match version {
        ProgressVacuumVersion::Pre17 => marked!(
            "SELECT pid, datname, relid, phase, \
             heap_blks_total, heap_blks_scanned, heap_blks_vacuumed, index_vacuum_count, \
             max_dead_tuples, num_dead_tuples, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_progress_vacuum"
        ),
        ProgressVacuumVersion::Pg17 => marked!(
            "SELECT pid, datname, relid, phase, \
             heap_blks_total, heap_blks_scanned, heap_blks_vacuumed, index_vacuum_count, \
             max_dead_tuple_bytes, dead_tuple_bytes, num_dead_item_ids, \
             indexes_total, indexes_processed, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_progress_vacuum"
        ),
        ProgressVacuumVersion::Pg18 => marked!(
            "SELECT pid, datname, relid, phase, \
             heap_blks_total, heap_blks_scanned, heap_blks_vacuumed, index_vacuum_count, \
             max_dead_tuple_bytes, dead_tuple_bytes, num_dead_item_ids, \
             indexes_total, indexes_processed, delay_time, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_progress_vacuum"
        ),
    }
}

/// Raw `pg_stat_progress_vacuum` row before string interning.
///
/// Labels are owned; the caller interns them. Columns absent from the selected
/// query are `None`.
#[derive(Debug, Clone)]
pub struct ProgressVacuumRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Backend process id.
    pub pid: i32,
    /// Database name.
    pub datname: String,
    /// Table OID.
    pub relid: u32,
    /// Vacuum phase.
    pub phase: String,
    /// Heap blocks in the table at scan start.
    pub heap_blks_total: i64,
    /// Heap blocks scanned in this vacuum.
    pub heap_blks_scanned: i64,
    /// Heap blocks vacuumed in this vacuum.
    pub heap_blks_vacuumed: i64,
    /// Index-vacuum cycles completed.
    pub index_vacuum_count: i64,
    /// Dead-tuple capacity, count (PG 10-16).
    pub max_dead_tuples: Option<i64>,
    /// Dead tuples collected, count (PG 10-16).
    pub num_dead_tuples: Option<i64>,
    /// Dead-tuple TID store capacity, bytes (PG17+).
    pub max_dead_tuple_bytes: Option<i64>,
    /// Dead-tuple TID store usage, bytes (PG17+).
    pub dead_tuple_bytes: Option<i64>,
    /// Dead item identifiers collected (PG17+).
    pub num_dead_item_ids: Option<i64>,
    /// Indexes to process (PG17+).
    pub indexes_total: Option<i64>,
    /// Indexes processed (PG17+).
    pub indexes_processed: Option<i64>,
    /// Cost-delay sleep time, ms (PG18+).
    pub delay_time: Option<f64>,
}

/// Build a `1_012_001` row, interning `datname` and `phase`.
///
/// # Errors
/// Returns the interner's error if a label cannot be interned.
pub fn to_progress_vacuum<E>(
    row: &ProgressVacuumRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatProgressVacuum, E> {
    Ok(PgStatProgressVacuum {
        ts: Ts(row.ts),
        pid: row.pid,
        datname: intern(row.datname.as_bytes())?,
        relid: row.relid,
        phase: intern(row.phase.as_bytes())?,
        heap_blks_total: row.heap_blks_total,
        heap_blks_scanned: row.heap_blks_scanned,
        heap_blks_vacuumed: row.heap_blks_vacuumed,
        index_vacuum_count: row.index_vacuum_count,
        max_dead_tuples: row.max_dead_tuples,
        num_dead_tuples: row.num_dead_tuples,
        max_dead_tuple_bytes: row.max_dead_tuple_bytes,
        dead_tuple_bytes: row.dead_tuple_bytes,
        num_dead_item_ids: row.num_dead_item_ids,
        indexes_total: row.indexes_total,
        indexes_processed: row.indexes_processed,
        delay_time: row.delay_time,
    })
}

fn row_from_pg(row: &tokio_postgres::Row, version: ProgressVacuumVersion) -> ProgressVacuumRow {
    let pre17 = matches!(version, ProgressVacuumVersion::Pre17);
    let pg17_plus = !pre17;
    let pg18 = matches!(version, ProgressVacuumVersion::Pg18);
    ProgressVacuumRow {
        ts: row.get("ts_us"),
        pid: row.get("pid"),
        datname: row.get("datname"),
        relid: row.get("relid"),
        phase: row.get("phase"),
        heap_blks_total: row.get("heap_blks_total"),
        heap_blks_scanned: row.get("heap_blks_scanned"),
        heap_blks_vacuumed: row.get("heap_blks_vacuumed"),
        index_vacuum_count: row.get("index_vacuum_count"),
        max_dead_tuples: pre17.then(|| row.get("max_dead_tuples")),
        num_dead_tuples: pre17.then(|| row.get("num_dead_tuples")),
        max_dead_tuple_bytes: pg17_plus.then(|| row.get("max_dead_tuple_bytes")),
        dead_tuple_bytes: pg17_plus.then(|| row.get("dead_tuple_bytes")),
        num_dead_item_ids: pg17_plus.then(|| row.get("num_dead_item_ids")),
        indexes_total: pg17_plus.then(|| row.get("indexes_total")),
        indexes_processed: pg17_plus.then(|| row.get("indexes_processed")),
        delay_time: pg18.then(|| row.get("delay_time")),
    }
}

/// Collect every in-progress vacuum, or an empty vector when none runs.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_progress_vacuum(
    client: &Client,
    major: u32,
) -> Result<Vec<ProgressVacuumRow>, tokio_postgres::Error> {
    let version = progress_vacuum_version(major);
    let rows = client.query(progress_vacuum_query(version), &[]).await?;
    Ok(rows.iter().map(|row| row_from_pg(row, version)).collect())
}

#[cfg(test)]
mod tests {
    use super::{
        ProgressVacuumRow, ProgressVacuumVersion, progress_vacuum_query, progress_vacuum_version,
        to_progress_vacuum,
    };
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_progress_vacuum expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn pre17_raw() -> ProgressVacuumRow {
        ProgressVacuumRow {
            ts: 2_000,
            pid: 4242,
            datname: "appdb".to_owned(),
            relid: 16_384,
            phase: "scanning heap".to_owned(),
            heap_blks_total: 10_000,
            heap_blks_scanned: 4_200,
            heap_blks_vacuumed: 4_000,
            index_vacuum_count: 1,
            max_dead_tuples: Some(291_271),
            num_dead_tuples: Some(120_000),
            max_dead_tuple_bytes: None,
            dead_tuple_bytes: None,
            num_dead_item_ids: None,
            indexes_total: None,
            indexes_processed: None,
            delay_time: None,
        }
    }

    #[test]
    fn version_follows_catalog_changes() {
        assert_eq!(progress_vacuum_version(13), ProgressVacuumVersion::Pre17);
        assert_eq!(progress_vacuum_version(16), ProgressVacuumVersion::Pre17);
        assert_eq!(progress_vacuum_version(17), ProgressVacuumVersion::Pg17);
        assert_eq!(progress_vacuum_version(18), ProgressVacuumVersion::Pg18);
    }

    #[test]
    fn query_includes_version_specific_columns() {
        let pre17 = progress_vacuum_query(ProgressVacuumVersion::Pre17);
        let pg17 = progress_vacuum_query(ProgressVacuumVersion::Pg17);
        let pg18 = progress_vacuum_query(ProgressVacuumVersion::Pg18);
        assert!(pre17.contains("max_dead_tuples"));
        assert!(!pre17.contains("dead_tuple_bytes"));
        assert!(pg17.contains("dead_tuple_bytes"));
        assert!(pg17.contains("indexes_total"));
        assert!(!pg17.contains("num_dead_tuples"));
        assert!(!pg17.contains("delay_time"));
        assert!(pg18.contains("delay_time"));
        for sql in [pre17, pg17, pg18] {
            assert!(sql.contains("pg_stat_progress_vacuum"));
            assert!(sql.contains("pg_kronika"));
        }
    }

    #[test]
    fn to_progress_vacuum_interns_labels_and_keeps_columns() {
        let out = to_progress_vacuum(&pre17_raw(), fake_intern).expect("intern");
        assert_eq!(out.pid, 4242);
        assert_eq!(out.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(out.phase, fake_intern(b"scanning heap").unwrap());
        assert_eq!(out.num_dead_tuples, Some(120_000));
        assert_eq!(out.dead_tuple_bytes, None);
        assert_eq!(out.delay_time, None);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_progress_vacuum(&pre17_raw(), boom), Err("full"));
    }
}
