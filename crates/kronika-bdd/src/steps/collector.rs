//! Steps for `features/collector.feature` (`pg_stat_bgwriter+pg_stat_checkpointer`).
//!
//! The feature pins the two `pg_stat_bgwriter` / `pg_stat_checkpointer` layouts:
//! pre-PG17 stores backend buffer counters and no checkpointer reset, while
//! PG17+ stores checkpointer restartpoint fields and drops backend counters.

use anyhow::{Result, bail};
use cucumber::then;
use kronika_registry::{Cell, Row};

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

const BGWRITER_CHECKPOINTER_LABEL: &str = "pg_stat_bgwriter+pg_stat_checkpointer";

/// Assert the pre-PG17 nullable-column shape.
#[then(
    "section pg_stat_bgwriter+pg_stat_checkpointer uses the pre-PG17 bgwriter/checkpointer layout"
)]
fn pre_pg17_layout(world: &mut BddWorld) -> Result<()> {
    let (rows, logs) = bgwriter_rows(world)?;
    let row = single_row(&rows, &logs)?;
    check_common_counters(row)?;
    expect_i64(row, "buffers_backend")?;
    expect_i64(row, "buffers_backend_fsync")?;
    expect_null(row, "restartpoints_timed")?;
    expect_null(row, "restartpoints_req")?;
    expect_null(row, "restartpoints_done")?;
    expect_null(row, "checkpointer_stats_reset")?;
    Ok(())
}

/// Assert the PG17+ nullable-column shape.
#[then("section pg_stat_bgwriter+pg_stat_checkpointer uses the PG17+ bgwriter/checkpointer layout")]
fn pg17_layout(world: &mut BddWorld) -> Result<()> {
    let (rows, logs) = bgwriter_rows(world)?;
    let row = single_row(&rows, &logs)?;
    check_common_counters(row)?;
    expect_null(row, "buffers_backend")?;
    expect_null(row, "buffers_backend_fsync")?;
    expect_i64(row, "restartpoints_timed")?;
    expect_i64(row, "restartpoints_req")?;
    expect_i64(row, "restartpoints_done")?;
    expect_ts(row, "checkpointer_stats_reset")?;
    Ok(())
}

fn bgwriter_rows(world: &BddWorld) -> Result<(Vec<Row>, String)> {
    let segment = world.harness.segment()?.clone();
    let logs = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, BGWRITER_CHECKPOINTER_TYPE_ID)?;
    Ok((rows, logs))
}

fn single_row<'a>(rows: &'a [Row], logs: &str) -> Result<&'a Row> {
    let [row] = rows else {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {BGWRITER_CHECKPOINTER_LABEL}: expected one row, got {}",
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

fn check_common_counters(row: &Row) -> Result<()> {
    expect_ts(row, "ts")?;
    expect_i64(row, "checkpoints_timed")?;
    expect_i64(row, "checkpoints_req")?;
    expect_f64(row, "checkpoint_write_time")?;
    expect_f64(row, "checkpoint_sync_time")?;
    expect_i64(row, "buffers_checkpoint")?;
    expect_i64(row, "buffers_clean")?;
    expect_i64(row, "maxwritten_clean")?;
    expect_i64(row, "buffers_alloc")?;
    expect_ts(row, "bgwriter_stats_reset")?;
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

fn expect_null(row: &Row, column: &str) -> Result<()> {
    match row.get(column) {
        Some(Cell::Null) => Ok(()),
        Some(other) => bail!("{column}: expected NULL, got {}", dump::render_cell(other)),
        None => bail!("{column}: column absent from decoded row"),
    }
}

#[cfg(test)]
mod tests {
    use super::{check_common_counters, expect_i64, expect_null, expect_ts};
    use kronika_registry::{Cell, Row};

    fn common_row() -> Row {
        Row::from([
            ("ts", Cell::Ts(10)),
            ("checkpoints_timed", Cell::I64(0)),
            ("checkpoints_req", Cell::I64(0)),
            ("checkpoint_write_time", Cell::F64(0.0)),
            ("checkpoint_sync_time", Cell::F64(0.0)),
            ("buffers_checkpoint", Cell::I64(0)),
            ("buffers_clean", Cell::I64(0)),
            ("maxwritten_clean", Cell::I64(0)),
            ("buffers_alloc", Cell::I64(1)),
            ("bgwriter_stats_reset", Cell::Ts(1)),
        ])
    }

    #[test]
    fn common_counter_check_accepts_nonnegative_values() {
        check_common_counters(&common_row()).expect("common counters pass");
    }

    #[test]
    fn common_counter_check_rejects_negative_counter() {
        let mut row = common_row();
        row.insert("buffers_alloc", Cell::I64(-1));
        assert!(check_common_counters(&row).is_err());
    }

    #[test]
    fn helpers_distinguish_present_values_from_nulls() {
        let row = Row::from([
            ("some_counter", Cell::I64(0)),
            ("some_ts", Cell::Ts(1)),
            ("gone", Cell::Null),
        ]);
        assert_eq!(expect_i64(&row, "some_counter").unwrap(), 0);
        assert_eq!(expect_ts(&row, "some_ts").unwrap(), 1);
        assert!(expect_null(&row, "gone").is_ok());
        assert!(expect_i64(&row, "gone").is_err());
    }
}
