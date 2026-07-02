//! Step definitions for `features/pg_store_plans.feature`.
//!
//! The oracle identifies rows by `(queryid_stat_statements, planid)` after
//! joining `pg_store_plans(false)` to `pg_stat_statements` with a LIKE pattern
//! on statement text. The sealed `plan` value must resolve through the segment
//! dictionary to a non-empty string.

use anyhow::{Context, Result, bail};
use cucumber::then;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::steps::common::parse_type_id;

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
    regex = r"^section ([\d_]+) has a pg_store_plans row for query like '([^']+)' with calls = (\d+) and a resolvable plan$"
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
    regex = r"^section ([\d_]+) has an ossc pg_store_plans row for query like '([^']+)' with calls = (\d+) and a (resolvable|NULL) plan$"
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
