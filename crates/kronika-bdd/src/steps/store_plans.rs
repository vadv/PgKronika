//! Step definitions for `features/pg_store_plans.feature`.
//!
//! The oracle identifies rows by `(queryid_stat_statements, planid)` after
//! joining `pg_store_plans(false)` to `pg_stat_statements` with a LIKE pattern
//! on statement text. The sealed `plan` value must resolve through the segment
//! dictionary to a non-empty string.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use cucumber::then;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::steps::common::parse_type_id;

const BUFFER_COLUMNS: [&str; 10] = [
    "shared_blks_hit",
    "shared_blks_read",
    "shared_blks_dirtied",
    "shared_blks_written",
    "local_blks_hit",
    "local_blks_read",
    "local_blks_dirtied",
    "local_blks_written",
    "temp_blks_read",
    "temp_blks_written",
];

/// Use a separate scenario-database connection for live extension oracles.
async fn oracle_client(world: &BddWorld) -> Result<tokio_postgres::Client> {
    let dsn = world.harness.database_dsn()?;
    let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the pg_store_plans oracle")?;
    tokio::spawn(async move { drop(conn.await) });
    Ok(client)
}

/// Look up `(queryid_stat_statements, planid, calls)` by statement-text pattern.
///
/// The oracle expects exactly one live plan row in the scenario database.
/// Ambiguous patterns fail instead of selecting an arbitrary plan.
async fn plans_row_by_like(
    client: &tokio_postgres::Client,
    pattern: &str,
) -> Result<(i64, i64, i64)> {
    let sql = "SELECT p.queryid_stat_statements, p.planid, p.calls \
               FROM pg_store_plans(false) p \
               JOIN pg_stat_statements s \
                 ON s.queryid = p.queryid_stat_statements \
                AND s.dbid = p.dbid \
                AND s.userid = p.userid \
               WHERE s.query LIKE $1 \
                 AND p.dbid = (SELECT oid FROM pg_database WHERE datname = current_database())";
    let pg_rows = client
        .query(sql, &[&pattern])
        .await
        .with_context(|| format!("pg_store_plans oracle for pattern {pattern:?}"))?;
    match pg_rows.len() {
        0 => bail!("pg_store_plans oracle: no plan row matches pattern {pattern:?}"),
        1 => {
            let r = &pg_rows[0];
            Ok((r.get(0), r.get(1), r.get(2)))
        }
        n => bail!(
            "pg_store_plans oracle: {n} plan rows match pattern {pattern:?}; \
             use a more specific pattern"
        ),
    }
}

/// Assert the sealed section contains the oracle-matched row, exact `calls`,
/// and a non-empty dictionary-backed plan text.
#[then(
    regex = r"^section ([\w.+-]+) has a pg_store_plans row for query like '([^']+)' with calls = (\d+) and a resolvable plan$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn psp_row_with_plan(
    world: &mut BddWorld,
    type_id: String,
    pattern: String,
    expected_calls: i64,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let client = oracle_client(world).await?;
    let (qss, planid, oracle_calls) = plans_row_by_like(&client, &pattern).await?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, dict) = decode_section(&segment, type_id)?;

    if oracle_calls != expected_calls {
        bail!(
            "{}",
            dump::section_dump(
                &format!("pg_store_plans oracle disagrees with the scenario for {pattern:?}"),
                &rows,
                &failure_log,
                &[(
                    "oracle vs expected",
                    format!("calls: oracle {oracle_calls}, expected {expected_calls}"),
                )],
            )
        );
    }

    let row = rows
        .iter()
        .find(|r| {
            r.get("queryid_stat_statements") == Some(&Cell::I64(qss))
                && r.get("planid") == Some(&Cell::I64(planid))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                dump::section_dump(
                    &format!(
                        "section {type_id}: no row with queryid_stat_statements={qss} \
                         planid={planid} (pattern {pattern:?})"
                    ),
                    &rows,
                    &failure_log,
                    &[],
                )
            )
        })?;

    match row.get("calls") {
        Some(&Cell::I64(calls)) if calls == expected_calls => {}
        other => bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {type_id}: calls for qss={qss} planid={planid} is {other:?}, \
                     expected {expected_calls}"
                ),
                &rows,
                &failure_log,
                &[],
            )
        ),
    }

    let plan_cell = row.get("plan").with_context(|| {
        format!("section {type_id}: row qss={qss} planid={planid} has no plan column")
    })?;
    let Cell::StrId(str_id) = plan_cell else {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {type_id}: plan for qss={qss} planid={planid} is {}, \
                     expected an interned text (the text fetch must have run)",
                    dump::render_cell(plan_cell)
                ),
                &rows,
                &failure_log,
                &[],
            )
        )
    };
    match dict.resolve(*str_id) {
        Some(
            kronika_reader::Resolved::String(bytes) | kronika_reader::Resolved::Blob { bytes, .. },
        ) if !bytes.is_empty() => Ok(()),
        Some(_) => bail!("section {type_id}: plan text for qss={qss} planid={planid} is empty"),
        None => bail!(
            "section {type_id}: plan str_id={str_id} for qss={qss} planid={planid} \
             did not resolve through the dictionary"
        ),
    }
}

/// Look up `(queryid, planid, calls)` in the ossc view by statement-text
/// pattern; the upstream keys entries by the real core query id.
async fn ossc_row_by_like(
    client: &tokio_postgres::Client,
    pattern: &str,
) -> Result<(i64, i64, i64)> {
    let sql = "SELECT p.queryid, p.planid, p.calls \
               FROM pg_store_plans p \
               JOIN pg_stat_statements s \
                 ON s.queryid = p.queryid \
                AND s.dbid = p.dbid \
                AND s.userid = p.userid \
               WHERE s.query LIKE $1 \
                 AND p.dbid = (SELECT oid FROM pg_database WHERE datname = current_database())";
    let pg_rows = client
        .query(sql, &[&pattern])
        .await
        .with_context(|| format!("ossc pg_store_plans oracle for pattern {pattern:?}"))?;
    match pg_rows.len() {
        0 => bail!("ossc pg_store_plans oracle: no plan row matches pattern {pattern:?}"),
        1 => {
            let r = &pg_rows[0];
            Ok((r.get(0), r.get(1), r.get(2)))
        }
        n => bail!(
            "ossc pg_store_plans oracle: {n} plan rows match pattern {pattern:?}; \
             use a more specific pattern"
        ),
    }
}

/// Assert the sealed `1_003_001` section carries the oracle-matched row with
/// the exact `calls` count and a dictionary-backed plan text.
#[then(
    regex = r"^section ([\w.+-]+) has an ossc pg_store_plans row for query like '([^']+)' with calls = (\d+) and a (resolvable|NULL) plan$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn ossc_row_with_plan(
    world: &mut BddWorld,
    type_id: String,
    pattern: String,
    expected_calls: i64,
    plan_expectation: String,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let client = oracle_client(world).await?;
    let (queryid, planid, oracle_calls) = ossc_row_by_like(&client, &pattern).await?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, dict) = decode_section(&segment, type_id)?;

    if oracle_calls != expected_calls {
        bail!(
            "{}",
            dump::section_dump(
                &format!("ossc pg_store_plans oracle disagrees with the scenario for {pattern:?}"),
                &rows,
                &failure_log,
                &[(
                    "oracle vs expected",
                    format!("calls: oracle {oracle_calls}, expected {expected_calls}"),
                )],
            )
        );
    }

    let row = rows
        .iter()
        .find(|r| {
            r.get("queryid") == Some(&Cell::I64(queryid))
                && r.get("planid") == Some(&Cell::I64(planid))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                dump::section_dump(
                    &format!(
                        "section {type_id}: no row with queryid={queryid} planid={planid} \
                         (pattern {pattern:?})"
                    ),
                    &rows,
                    &failure_log,
                    &[],
                )
            )
        })?;

    match row.get("calls") {
        Some(&Cell::I64(calls)) if calls == expected_calls => {}
        other => bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {type_id}: calls for queryid={queryid} planid={planid} is \
                     {other:?}, expected {expected_calls}"
                ),
                &rows,
                &failure_log,
                &[],
            )
        ),
    }

    if plan_expectation == "NULL" {
        let plan_cell = row
            .get("plan")
            .with_context(|| format!("section {type_id}: row has no plan column"))?;
        anyhow::ensure!(
            plan_cell == &Cell::Null,
            "section {type_id}: plan is {}, expected NULL under a zero text budget",
            dump::render_cell(plan_cell)
        );
        return Ok(());
    }
    assert_plan_resolves(type_id, row, &dict, &rows, &failure_log)
}

/// The row's `plan` must be an interned id resolving to a non-empty text.
fn assert_plan_resolves(
    type_id: u32,
    row: &kronika_registry::Row,
    dict: &kronika_reader::Dictionary,
    rows: &[kronika_registry::Row],
    failure_log: &str,
) -> Result<()> {
    let plan_cell = row
        .get("plan")
        .with_context(|| format!("section {type_id}: row has no plan column"))?;
    let Cell::StrId(str_id) = plan_cell else {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {type_id}: plan is {}, expected an interned text",
                    dump::render_cell(plan_cell)
                ),
                rows,
                failure_log,
                &[],
            )
        )
    };
    match dict.resolve(*str_id) {
        Some(
            kronika_reader::Resolved::String(bytes) | kronika_reader::Resolved::Blob { bytes, .. },
        ) if !bytes.is_empty() => Ok(()),
        Some(_) => bail!("section {type_id}: plan text is empty"),
        None => {
            bail!("section {type_id}: plan str_id={str_id} did not resolve through the dictionary")
        }
    }
}

#[derive(Debug)]
struct LiveBufferRow {
    queryid: i64,
    planid: i64,
    buffers: [i64; BUFFER_COLUMNS.len()],
}

async fn live_buffer_row(
    client: &tokio_postgres::Client,
    pattern: &str,
    ossc: bool,
) -> Result<LiveBufferRow> {
    let source = if ossc {
        "pg_store_plans p"
    } else {
        "pg_store_plans(false) p"
    };
    let queryid = if ossc {
        "p.queryid"
    } else {
        "p.queryid_stat_statements"
    };
    let sql = format!(
        "SELECT {queryid}, p.planid, \
                p.shared_blks_hit, p.shared_blks_read, \
                p.shared_blks_dirtied, p.shared_blks_written, \
                p.local_blks_hit, p.local_blks_read, \
                p.local_blks_dirtied, p.local_blks_written, \
                p.temp_blks_read, p.temp_blks_written \
         FROM {source} \
         JOIN pg_stat_statements s \
           ON s.queryid = {queryid} \
          AND s.dbid = p.dbid \
          AND s.userid = p.userid \
         WHERE s.query LIKE $1 \
           AND p.dbid = (SELECT oid FROM pg_database WHERE datname = current_database())"
    );
    let rows = client
        .query(&sql, &[&pattern])
        .await
        .with_context(|| format!("live buffer oracle for pattern {pattern:?}"))?;
    ensure!(
        rows.len() == 1,
        "live buffer oracle expected one row for {pattern:?}, got {}",
        rows.len()
    );
    let row = &rows[0];
    Ok(LiveBufferRow {
        queryid: row.get(0),
        planid: row.get(1),
        buffers: std::array::from_fn(|index| row.get(index + 2)),
    })
}

#[then(
    regex = r"^section pg_store_plans\.vadv matches every buffer counter for query like '([^']+)'$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn vadv_buffer_counters(world: &mut BddWorld, pattern: String) -> Result<()> {
    assert_live_buffer_counters(world, &pattern, false).await
}

#[then(
    regex = r"^section pg_store_plans\.ossc matches every buffer counter for query like '([^']+)'$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn ossc_buffer_counters(world: &mut BddWorld, pattern: String) -> Result<()> {
    assert_live_buffer_counters(world, &pattern, true).await
}

async fn assert_live_buffer_counters(world: &BddWorld, pattern: &str, ossc: bool) -> Result<()> {
    let type_id = if ossc { 1_003_001 } else { 1_004_001 };
    let query_column = if ossc {
        "queryid"
    } else {
        "queryid_stat_statements"
    };
    let client = oracle_client(world).await?;
    let expected = live_buffer_row(&client, pattern, ossc).await?;
    let segment = world.harness.segment()?;
    let failure_log = world.harness.failure_log()?;
    let (rows, _) = decode_section(segment, type_id)?;
    let stored = rows
        .iter()
        .find(|row| {
            row.get(query_column) == Some(&Cell::I64(expected.queryid))
                && row.get("planid") == Some(&Cell::I64(expected.planid))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                dump::section_dump(
                    &format!(
                        "section {type_id}: no stored buffer row for {query_column}={} planid={}",
                        expected.queryid, expected.planid
                    ),
                    &rows,
                    &failure_log,
                    &[],
                )
            )
        })?;
    let mut mismatches = Vec::new();
    for (&column, value) in BUFFER_COLUMNS.iter().zip(expected.buffers) {
        if stored.get(column) != Some(&Cell::I64(value)) {
            mismatches.push(format!(
                "{column}: stored {}, live {value}",
                stored
                    .get(column)
                    .map_or_else(|| "<absent>".to_owned(), dump::render_cell)
            ));
        }
    }
    ensure!(
        mismatches.is_empty(),
        "{}",
        dump::section_dump(
            &format!("section {type_id}: split buffer counters differ from the live extension"),
            &rows,
            &failure_log,
            &[("counter differences", mismatches.join("\n"))],
        )
    );
    Ok(())
}

#[then(regex = r"^section (pg_store_plans\.(?:ossc|vadv)) has complete analyzer provenance$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn plan_analyzer_provenance(world: &mut BddWorld, section: String) -> Result<()> {
    let type_id = parse_type_id(&section)?;
    let segment = world.harness.segment()?;
    let (plans, _) = decode_section(segment, type_id)?;
    let (coverage, _) = decode_section(segment, 1_038_001)?;
    let (resets, reset_dict) = decode_section(segment, 1_020_001)?;
    let mut rows_per_timestamp = BTreeMap::<i64, u64>::new();
    for row in &plans {
        let Some(Cell::Ts(ts)) = row.get("ts") else {
            bail!("section {section} contains a row without a timestamp");
        };
        *rows_per_timestamp.entry(*ts).or_default() += 1;
    }
    ensure!(
        !rows_per_timestamp.is_empty(),
        "section {section} has no plan snapshots"
    );
    for (ts, plan_rows) in rows_per_timestamp {
        let marker = coverage
            .iter()
            .find(|row| {
                row.get("ts") == Some(&Cell::Ts(ts))
                    && cell_u64(row.get("section_type_id")) == Some(u64::from(type_id))
            })
            .with_context(|| format!("no snapshot_coverage marker for {section} at {ts}"))?;
        ensure!(
            cell_u64(marker.get("read_state")) == Some(0)
                && cell_u64(marker.get("visibility")) == Some(0)
                && cell_u64(marker.get("source_total")) == Some(plan_rows)
                && cell_u64(marker.get("collected")) == Some(plan_rows),
            "{section} coverage at {ts} is not exact: {marker:?}"
        );
        let reset = exact_row_at(&resets, ts)
            .with_context(|| format!("invalid reset_metadata for {section} timestamp {ts}"))?;
        let extension_version = resolved_text(reset.get("ext_pg_store_plans_version"), &reset_dict)
            .context("pg_store_plans extension version is absent or malformed")?;
        let compute_query_id = resolved_text(reset.get("compute_query_id"), &reset_dict)
            .context("compute_query_id is absent or malformed")?;
        ensure!(
            if type_id == 1_003_001 {
                extension_version.starts_with("1.")
                    && matches!(reset.get("pg_store_plans_reset_at"), Some(Cell::Ts(_)))
            } else {
                extension_version.starts_with("2.")
                    && reset.get("pg_store_plans_reset_at") == Some(&Cell::Null)
            } && matches!(compute_query_id, "auto" | "on" | "regress"),
            "{section} reset metadata does not carry extension/query-id context: {reset:?}"
        );
    }
    Ok(())
}

fn exact_row_at(rows: &[kronika_registry::Row], ts: i64) -> Result<&kronika_registry::Row> {
    let mut matches = rows
        .iter()
        .filter(|row| row.get("ts") == Some(&Cell::Ts(ts)));
    let row = matches
        .next()
        .with_context(|| format!("no coordinated row at timestamp {ts}"))?;
    ensure!(
        matches.next().is_none(),
        "conflicting coordinated rows at timestamp {ts}"
    );
    Ok(row)
}

fn resolved_text<'a>(
    cell: Option<&Cell>,
    dictionary: &'a kronika_reader::Dictionary,
) -> Option<&'a str> {
    let Some(Cell::StrId(id)) = cell else {
        return None;
    };
    let bytes = match dictionary.resolve(*id)? {
        kronika_reader::Resolved::String(bytes) | kronika_reader::Resolved::Blob { bytes, .. } => {
            bytes
        }
    };
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.is_empty()).then_some(text)
}

fn cell_u64(cell: Option<&Cell>) -> Option<u64> {
    match cell {
        Some(Cell::I16(value)) => u64::try_from(*value).ok(),
        Some(Cell::I32(value)) => u64::try_from(*value).ok(),
        Some(Cell::I64(value)) => u64::try_from(*value).ok(),
        Some(Cell::U32(value)) => Some(u64::from(*value)),
        Some(Cell::U64(value)) => Some(*value),
        _ => None,
    }
}
