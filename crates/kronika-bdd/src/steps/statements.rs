//! Steps for `features/pg_stat_statements.feature` (types `1_002_001`..`1_002_006`).
//!
//! Row selection is by `queryid`, obtained from an independent oracle query on
//! `pg_stat_statements` (`WHERE query LIKE ...`). Query text is never compared
//! byte-for-byte: the extension normalizes constants to `$n`, so the `.feature`
//! patterns anchor on identifiers and aliases instead.

use anyhow::{Context, Result, bail};
use cucumber::then;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row, decode_section_labeled};
use crate::harness::dump;
use crate::harness::expected::{ExpectedColumn, ExpectedValue};
use crate::steps::common::parse_section_ref;

/// Open a dedicated connection to the scenario database for oracle queries.
async fn oracle_client(world: &BddWorld) -> Result<tokio_postgres::Client> {
    let dsn = world.harness.database_dsn()?;
    let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the pg_stat_statements oracle")?;
    tokio::spawn(async move { drop(conn.await) });
    Ok(client)
}

/// Look up `(queryid, calls, rows)` in `pg_stat_statements` by a LIKE pattern.
///
/// Scoped to the scenario database via `dbid`; exactly one row must match, so
/// an ambiguous pattern fails instead of silently picking a row.
async fn pgss_row_by_like(
    client: &tokio_postgres::Client,
    pattern: &str,
) -> Result<(i64, i64, i64)> {
    let sql = "SELECT queryid, calls, rows \
               FROM pg_stat_statements \
               WHERE query LIKE $1 \
               AND dbid = (SELECT oid FROM pg_database WHERE datname = current_database())";
    let pg_rows = client
        .query(sql, &[&pattern])
        .await
        .with_context(|| format!("pg_stat_statements oracle for pattern {pattern:?}"))?;
    match pg_rows.len() {
        0 => bail!("pg_stat_statements oracle: no row matches pattern {pattern:?}"),
        1 => {
            let r = &pg_rows[0];
            let queryid: Option<i64> = r.get(0);
            let calls: i64 = r.get(1);
            let rows: i64 = r.get(2);
            let queryid = queryid.with_context(|| {
                format!(
                    "pg_stat_statements oracle: queryid is NULL for pattern {pattern:?} \
                     (compute_query_id may be off)"
                )
            })?;
            Ok((queryid, calls, rows))
        }
        n => bail!(
            "pg_stat_statements oracle: {n} rows match pattern {pattern:?}; \
             use a more specific pattern"
        ),
    }
}

/// Assert the sealed section carries the row for the matched `pg_stat_statements`
/// query with the scenario's exact `calls` and `rows` counts.
///
/// The oracle first verifies the live view holds those counts, then the section
/// row (selected by the oracle's `queryid`) is compared column-by-column.
#[then(
    regex = r"^section ([\w.+-]+) has a row for pg_stat_statements query like '([^']+)' with calls = (\d+) and rows = (\d+)$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn pgss_row_with_counts(
    world: &mut BddWorld,
    section: String,
    pattern: String,
    expected_calls: i64,
    expected_rows: i64,
) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let client = oracle_client(world).await?;
    let (queryid, oracle_calls, oracle_rows) = pgss_row_by_like(&client, &pattern).await?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;

    if oracle_calls != expected_calls || oracle_rows != expected_rows {
        let (rows, _dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
        bail!(
            "{}",
            dump::section_dump(
                &format!("pg_stat_statements oracle disagrees with the scenario for {pattern:?}"),
                &rows,
                &failure_log,
                &[(
                    "oracle vs expected",
                    format!(
                        "calls: oracle {oracle_calls}, expected {expected_calls}; \
                         rows: oracle {oracle_rows}, expected {expected_rows}"
                    ),
                )],
            )
        );
    }

    let expected = vec![
        ExpectedColumn {
            name: "calls".to_owned(),
            value: ExpectedValue::Cell(Cell::I64(expected_calls)),
        },
        ExpectedColumn {
            name: "rows".to_owned(),
            value: ExpectedValue::Cell(Cell::I64(expected_rows)),
        },
    ];
    assert_row(
        &segment,
        section.type_id,
        &section.label,
        &RowSelector::ByKey {
            column: "queryid".to_owned(),
            cell: Cell::I64(queryid),
        },
        false,
        &expected,
        &failure_log,
    )
}

/// Assert the matched query's sealed text resolved through the Blob table of
/// the segment dictionary (text of 4096 bytes or more).
///
/// Fails when the row is missing, the text resolved as a String entry (too
/// short), or the id does not resolve at all.
#[then(
    regex = r"^section ([\w.+-]+) has a blob-path row for pg_stat_statements query like '([^']+)'$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn pgss_blob_path_row(world: &mut BddWorld, section: String, pattern: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let client = oracle_client(world).await?;
    let (queryid, _calls, _rows) = pgss_row_by_like(&client, &pattern).await?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;

    let row = rows
        .iter()
        .find(|r| r.get("queryid") == Some(&Cell::I64(queryid)))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                dump::section_dump(
                    &format!(
                        "section {}: no row with queryid={queryid} (pattern {pattern:?})",
                        section.label
                    ),
                    &rows,
                    &failure_log,
                    &[],
                )
            )
        })?;

    let query_cell = row.get("query").with_context(|| {
        format!(
            "section {}: row queryid={queryid} has no query column",
            section.label
        )
    })?;

    let Cell::StrId(str_id) = query_cell else {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {}: query cell for queryid={queryid} is not a StrId: {}",
                    section.label,
                    dump::render_cell(query_cell)
                ),
                &rows,
                &failure_log,
                &[],
            )
        )
    };

    match dict.resolve(*str_id) {
        Some(kronika_reader::Resolved::Blob { .. }) => Ok(()),
        Some(kronika_reader::Resolved::String(bytes)) => bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {}: queryid={queryid} query text ({} bytes) resolved as \
                     String, expected Blob (text must be 4096 bytes or more)",
                    section.label,
                    bytes.len()
                ),
                &rows,
                &failure_log,
                &[(
                    "query text head",
                    String::from_utf8_lossy(&bytes[..bytes.len().min(80)]).into_owned(),
                )],
            )
        ),
        None => bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {}: queryid={queryid} str_id={str_id} did not resolve \
                     through the dictionary",
                    section.label
                ),
                &rows,
                &failure_log,
                &[],
            )
        ),
    }
}
