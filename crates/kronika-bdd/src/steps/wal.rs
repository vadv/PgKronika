//! Steps for `features/pg_stat_wal.feature` (`pg_stat_wal.pg15_17` / `pg_stat_wal.pg18`).
//!
//! PG15-17 use the V1 layout with write/sync counters. PG18 uses V2 after those
//! fields moved out of `pg_stat_wal`.

use anyhow::{Result, bail};
use cucumber::then;
use kronika_registry::{Cell, Row};

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;

const PG_STAT_WAL_V1_TYPE_ID: u32 = 1_007_001;
const PG_STAT_WAL_V2_TYPE_ID: u32 = 1_007_002;
const PG_STAT_WAL_V1_LABEL: &str = "pg_stat_wal.pg15_17";
const PG_STAT_WAL_V2_LABEL: &str = "pg_stat_wal.pg18";

/// Assert the PG15-17 `pg_stat_wal` layout.
#[then("section pg_stat_wal.pg15_17 uses the PG15-17 pg_stat_wal layout")]
fn wal_v1_layout(world: &mut BddWorld) -> Result<()> {
    let (rows, logs) = wal_rows(world, PG_STAT_WAL_V1_TYPE_ID)?;
    let row = single_row(PG_STAT_WAL_V1_LABEL, &rows, &logs)?;
    check_generation_counters(row)?;
    expect_i64(row, "wal_write")?;
    expect_i64(row, "wal_sync")?;
    expect_f64(row, "wal_write_time")?;
    expect_f64(row, "wal_sync_time")?;
    expect_optional_ts(row, "stats_reset")?;
    Ok(())
}

/// Assert the PG18 `pg_stat_wal` layout.
#[then("section pg_stat_wal.pg18 uses the PG18 pg_stat_wal layout")]
fn wal_v2_layout(world: &mut BddWorld) -> Result<()> {
    let (rows, logs) = wal_rows(world, PG_STAT_WAL_V2_TYPE_ID)?;
    let row = single_row(PG_STAT_WAL_V2_LABEL, &rows, &logs)?;
    check_generation_counters(row)?;
    expect_absent(row, "wal_write")?;
    expect_absent(row, "wal_sync")?;
    expect_absent(row, "wal_write_time")?;
    expect_absent(row, "wal_sync_time")?;
    expect_optional_ts(row, "stats_reset")?;
    Ok(())
}

fn wal_rows(world: &BddWorld, type_id: u32) -> Result<(Vec<Row>, String)> {
    let segment = world.harness.segment()?.clone();
    let logs = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    Ok((rows, logs))
}

fn single_row<'a>(section_label: &str, rows: &'a [Row], logs: &str) -> Result<&'a Row> {
    let [row] = rows else {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {section_label}: expected one row, got {}",
                    rows.len()
                ),
                rows,
                logs,
                &[],
            )
        );
    };
    Ok(row)
}

fn check_generation_counters(row: &Row) -> Result<()> {
    expect_ts(row, "ts")?;
    expect_i64(row, "wal_records")?;
    expect_i64(row, "wal_fpi")?;
    expect_i64(row, "wal_bytes")?;
    expect_i64(row, "wal_buffers_full")?;
    Ok(())
}

fn expect_i64(row: &Row, column: &str) -> Result<i64> {
    match row.get(column) {
        Some(Cell::I64(value)) if *value >= 0 => Ok(*value),
        Some(other) => bail!(
            "{column}: expected a non-negative i64, got {}",
            dump::render_cell(other)
        ),
        None => bail!("{column}: column absent from decoded row"),
    }
}

fn expect_f64(row: &Row, column: &str) -> Result<f64> {
    match row.get(column) {
        Some(Cell::F64(value)) if value.is_finite() && *value >= 0.0 => Ok(*value),
        Some(other) => bail!(
            "{column}: expected a non-negative finite f64, got {}",
            dump::render_cell(other)
        ),
        None => bail!("{column}: column absent from decoded row"),
    }
}

fn expect_ts(row: &Row, column: &str) -> Result<i64> {
    match row.get(column) {
        Some(Cell::Ts(value)) if *value > 0 => Ok(*value),
        Some(other) => bail!(
            "{column}: expected a positive timestamp, got {}",
            dump::render_cell(other)
        ),
        None => bail!("{column}: column absent from decoded row"),
    }
}

fn expect_optional_ts(row: &Row, column: &str) -> Result<()> {
    match row.get(column) {
        Some(Cell::Null) => Ok(()),
        Some(Cell::Ts(value)) if *value > 0 => Ok(()),
        Some(other) => bail!(
            "{column}: expected NULL or a positive timestamp, got {}",
            dump::render_cell(other)
        ),
        None => bail!("{column}: column absent from decoded row"),
    }
}

fn expect_absent(row: &Row, column: &str) -> Result<()> {
    anyhow::ensure!(
        row.get(column).is_none(),
        "{column}: column is present but PG18 layout must not carry it"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_generation_counters, expect_absent, expect_optional_ts};
    use kronika_registry::{Cell, Row};

    fn v2_row_with_bytes(wal_bytes: i64) -> Row {
        crate::harness::test_row(&[
            ("ts", Cell::Ts(10)),
            ("wal_records", Cell::I64(1)),
            ("wal_fpi", Cell::I64(0)),
            ("wal_bytes", Cell::I64(wal_bytes)),
            ("wal_buffers_full", Cell::I64(0)),
            ("stats_reset", Cell::Null),
        ])
    }

    #[test]
    fn generation_counter_check_accepts_valid_row() {
        check_generation_counters(&v2_row_with_bytes(1024)).expect("generation counters pass");
    }

    #[test]
    fn generation_counter_check_rejects_negative_wal_bytes() {
        let row = v2_row_with_bytes(-1);
        assert!(check_generation_counters(&row).is_err());
    }

    #[test]
    fn optional_ts_accepts_null_or_positive_timestamp() {
        let row = v2_row_with_bytes(1024);
        assert!(expect_optional_ts(&row, "stats_reset").is_ok());
        let row = crate::harness::test_row(&[("stats_reset", Cell::Ts(1))]);
        assert!(expect_optional_ts(&row, "stats_reset").is_ok());
    }

    #[test]
    fn absent_column_check_rejects_present_column() {
        let row = crate::harness::test_row(&[("wal_write", Cell::I64(0))]);
        assert!(expect_absent(&row, "wal_write").is_err());
    }
}
