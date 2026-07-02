//! Steps for `features/replication_instance.feature` (type `1015001`).
//!
//! The singleton's `current_wal_lsn` only advances, so it is asserted as a
//! window: a floor read captured before the snapshot, a ceiling read taken by
//! the assertion step, and the recorded offset must lie between them. Both
//! oracle reads are docstring SQL in the `.feature`.

use anyhow::{Context, Result, bail};
use cucumber::{gherkin::Step, given, then};
use kronika_registry::{Cell, Row};

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::harness::oracle::window_contains;
use crate::steps::docstring;

const REPLICATION_INSTANCE_TYPE_ID: u32 = 1_015_001;
/// The window-checked column; also the floor's storage key.
const WAL_LSN_COLUMN: &str = "current_wal_lsn";

/// Run the docstring SQL before the snapshot and store the result as the
/// window floor for `current_wal_lsn`.
#[given("the current WAL LSN is captured as the replication instance window floor:")]
async fn capture_wal_window_floor(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let floor = scalar_i64(&world.harness.database_dsn()?, sql).await?;
    world.harness.set_window_floor(WAL_LSN_COLUMN, floor);
    Ok(())
}

/// Assert the recorded `current_wal_lsn` lies between the captured floor and a
/// ceiling read by the docstring SQL now, after the snapshot.
#[then("section 1015001 current_wal_lsn matches the replication instance window oracle up to:")]
async fn wal_lsn_within_window(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let floor = world.harness.window_floor(WAL_LSN_COLUMN)?;
    let ceiling = scalar_i64(&world.harness.database_dsn()?, sql).await?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, REPLICATION_INSTANCE_TYPE_ID)?;
    let value = match recorded_wal_lsn(&rows) {
        Ok(value) => value,
        Err(err) => bail!(
            "{}",
            dump::section_dump(
                &format!("section {REPLICATION_INSTANCE_TYPE_ID} {WAL_LSN_COLUMN}: {err}"),
                &rows,
                &failure_log,
                &[],
            )
        ),
    };
    if window_contains(floor, value, ceiling) {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!(
                "section {REPLICATION_INSTANCE_TYPE_ID} {WAL_LSN_COLUMN}: \
                 window oracle failed (floor <= recorded <= ceiling)"
            ),
            &rows,
            &failure_log,
            &[
                ("window floor", floor.to_string()),
                ("recorded", value.to_string()),
                ("window ceiling", ceiling.to_string()),
            ],
        )
    )
}

/// The single recorded `current_wal_lsn` of the decoded section rows.
fn recorded_wal_lsn(rows: &[Row]) -> Result<i64> {
    let [row] = rows else {
        bail!("expected exactly one row, got {}", rows.len());
    };
    match row.get(WAL_LSN_COLUMN) {
        Some(Cell::I64(value)) => Ok(*value),
        Some(other) => bail!(
            "recorded value is {}, not an int8 byte offset",
            dump::render_cell(other)
        ),
        None => bail!("column absent from the decoded row"),
    }
}

/// Run `sql` on a fresh connection to the scenario database and read the single
/// row's first column as `int8`.
async fn scalar_i64(dsn: &str, sql: &str) -> Result<i64> {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the window oracle read")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = async {
        let row = client
            .query_one(sql, &[])
            .await
            .context("window oracle read")?;
        row.try_get::<_, i64>(0)
            .context("window oracle value as int8")
    }
    .await;
    driver.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::recorded_wal_lsn;
    use kronika_registry::{Cell, Row};

    #[test]
    fn recorded_wal_lsn_reads_the_single_i64_cell() {
        let rows = vec![Row::from([("current_wal_lsn", Cell::I64(16_777_216))])];
        assert_eq!(recorded_wal_lsn(&rows).unwrap(), 16_777_216);
    }

    #[test]
    fn recorded_wal_lsn_rejects_a_null_cell() {
        let rows = vec![Row::from([("current_wal_lsn", Cell::Null)])];
        let err = recorded_wal_lsn(&rows).unwrap_err().to_string();
        assert!(
            err.contains("null"),
            "the error names the null value: {err}"
        );
    }

    #[test]
    fn recorded_wal_lsn_rejects_zero_or_many_rows() {
        assert!(recorded_wal_lsn(&[]).is_err(), "no rows is an error");
        let rows = vec![
            Row::from([("current_wal_lsn", Cell::I64(1))]),
            Row::from([("current_wal_lsn", Cell::I64(2))]),
        ];
        assert!(recorded_wal_lsn(&rows).is_err(), "two rows are an error");
    }

    #[test]
    fn recorded_wal_lsn_rejects_a_missing_column() {
        let rows = vec![Row::from([("is_in_recovery", Cell::Bool(false))])];
        let err = recorded_wal_lsn(&rows).unwrap_err().to_string();
        assert!(err.contains("absent"), "the error names the absence: {err}");
    }
}
