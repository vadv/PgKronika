//! Steps for `features/pg_stat_statements.feature` (types `1_002_001`..`1_002_006`).
//!
//! Row selection is by `queryid`, obtained from an independent oracle query on
//! `pg_stat_statements` (`WHERE query LIKE ...`). Query text is never compared
//! byte-for-byte: the extension normalizes constants to `$n`, so the `.feature`
//! patterns anchor on identifiers and aliases instead.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use cucumber::{given, then, when};
use kronika_registry::{Cell, Row, section_name};

use crate::BddWorld;
use crate::collector::Collector;
use crate::harness::assert_row::{RowSelector, assert_row, decode_section, decode_section_labeled};
use crate::harness::dump;
use crate::harness::expected::{ExpectedColumn, ExpectedValue};
use crate::harness::web;
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
                    "pg_stat_statements oracle: queryid is privilege-masked for pattern \
                     {pattern:?}"
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
        ExpectedColumn {
            name: "query".to_owned(),
            value: ExpectedValue::Cell(Cell::Null),
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

/// Assert that a known source query keeps the required `query` field while
/// withholding its value in both storage and the raw HTTP projection.
#[then(
    regex = r"^section ([\w.+-]+) exposes a null query for pg_stat_statements query like '([^']+)' in PGM and the raw API$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn pgss_null_query_row(world: &mut BddWorld, section: String, pattern: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let client = oracle_client(world).await?;
    let (queryid, _calls, _rows) = pgss_row_by_like(&client, &pattern).await?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;

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
    ensure!(
        query_cell == &Cell::Null,
        "{}",
        dump::section_dump(
            &format!(
                "section {}: query cell for queryid={queryid} is {}, expected NULL",
                section.label,
                dump::render_cell(query_cell)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );

    let logical_name = section_name(section.type_id)
        .with_context(|| format!("section {} has no logical name", section.label))?;
    let page = web::section_page(segment.data_root(), logical_name).await?;
    let api_rows = page["rows"]
        .as_array()
        .context("raw statement API `rows` is not an array")?;
    let api_row = api_rows
        .iter()
        .find(|candidate| candidate["queryid"].as_i64() == Some(queryid))
        .with_context(|| {
            format!(
                "raw statement API has no row with queryid={queryid}: {}",
                page["rows"]
            )
        })?;
    let object = api_row
        .as_object()
        .context("raw statement API row is not an object")?;
    ensure!(
        object.contains_key("query") && object["query"].is_null(),
        "raw statement API row must contain `query: null`: {api_row}"
    );
    Ok(())
}

/// Verify exact coverage plus reset/version context for a complete numeric
/// statement snapshot.
#[then(regex = r"^section ([\w.+-]+) has complete numeric statement provenance$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn complete_statement_provenance(world: &mut BddWorld, section: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let segment = world.harness.segment()?.clone();
    let (rows, _dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
    ensure!(
        !rows.is_empty(),
        "section {} has no numeric statement rows",
        section.label
    );
    assert_statement_rows_are_text_free(&rows, &section.label)?;
    assert_complete_coverage(&segment, section.type_id, &rows, &section.label)?;
    let dsn = world.harness.database_dsn()?;
    assert_statement_reset_context(&segment, &dsn).await
}

/// Verify that one normalized query appears under both V3+ identity variants.
#[then(
    regex = r"^section ([\w.+-]+) keeps both toplevel identities for pg_stat_statements query like '([^']+)' with calls = (\d+) and rows = (\d+)$"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn duplicate_toplevel_identity(
    world: &mut BddWorld,
    section: String,
    pattern: String,
    expected_calls: i64,
    expected_rows: i64,
) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let client = oracle_client(world).await?;
    let oracle = client
        .query(
            "SELECT queryid, toplevel, calls, rows \
             FROM pg_stat_statements \
             WHERE query LIKE $1 \
               AND ltrim(query) ILIKE 'DELETE%' \
               AND dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
             ORDER BY toplevel",
            &[&pattern],
        )
        .await
        .with_context(|| format!("duplicate-toplevel oracle for pattern {pattern:?}"))?;
    ensure!(
        oracle.len() == 2,
        "pg_stat_statements oracle returned {} DELETE rows for {pattern:?}, expected two",
        oracle.len()
    );
    let queryids = oracle
        .iter()
        .map(|row| {
            row.get::<_, Option<i64>>("queryid")
                .context("duplicate-toplevel oracle returned NULL queryid")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let levels = oracle
        .iter()
        .map(|row| row.get::<_, bool>("toplevel"))
        .collect::<BTreeSet<_>>();
    ensure!(
        queryids.len() == 1 && levels == BTreeSet::from([false, true]),
        "oracle did not expose one queryid under both toplevel identities: {oracle:?}"
    );
    ensure!(
        oracle.iter().all(|row| {
            row.get::<_, i64>("calls") == expected_calls
                && row.get::<_, i64>("rows") == expected_rows
        }),
        "oracle counts differ from calls={expected_calls}, rows={expected_rows}: {oracle:?}"
    );
    let queryid = *queryids
        .first()
        .context("duplicate-toplevel queryid set is empty")?;

    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
    let matches = rows
        .iter()
        .filter(|row| row.get("queryid") == Some(&Cell::I64(queryid)))
        .collect::<Vec<_>>();
    let stored_levels = matches
        .iter()
        .filter_map(|row| match row.get("toplevel") {
            Some(Cell::Bool(value)) => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        matches.len() == 2
            && stored_levels == BTreeSet::from([false, true])
            && matches.iter().all(|row| {
                row.get("calls") == Some(&Cell::I64(expected_calls))
                    && row.get("rows") == Some(&Cell::I64(expected_rows))
                    && row.get("query") == Some(&Cell::Null)
            }),
        "{}",
        dump::section_dump(
            &format!(
                "section {} did not keep both toplevel identities for queryid={queryid}",
                section.label
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Route the collector through a role that has no `pg_read_all_stats`
/// membership, while keeping the dynamically-created scenario database.
#[given(
    regex = r#"^the collector connects to the scenario database as role "([A-Za-z_][A-Za-z0-9_]*)"$"#
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn collector_uses_restricted_role(world: &mut BddWorld, role: String) -> Result<()> {
    let dsn = world.harness.database_dsn()?;
    let restricted = dsn.replace("user=postgres", &format!("user={role}"));
    ensure!(
        restricted != dsn,
        "scenario DSN has no `user=postgres` field to replace"
    );
    world
        .harness
        .add_collector_env("KRONIKA_PG_DSN".to_owned(), restricted);
    Ok(())
}

/// Assert the N=1 two-axis selection returns exactly two rows despite all
/// source identities being masked to `queryid=NULL`.
#[then(regex = r"^section ([\w.+-]+) has exactly two bounded rows with masked query identifiers$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn bounded_masked_queryids(world: &mut BddWorld, section: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
    ensure!(
        rows.len() == 2
            && rows.iter().all(|row| {
                row.get("queryid") == Some(&Cell::Null) && row.get("query") == Some(&Cell::Null)
            }),
        "{}",
        dump::section_dump(
            &format!(
                "section {} must contain exactly two rows with masked queryid and query",
                section.label
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    let calls = rows
        .iter()
        .filter_map(|row| match row.get("calls") {
            Some(Cell::I64(value)) => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        calls.contains(&1) && calls.iter().any(|value| *value >= 12),
        "the two selected axes did not retain the one-call time probe and repeated call probe: \
         {calls:?}"
    );
    assert_bounded_coverage(&segment, section.type_id, &rows, &section.label)?;

    let logical_name = section_name(section.type_id)
        .with_context(|| format!("section {} has no logical name", section.label))?;
    let page = web::section_page(segment.data_root(), logical_name).await?;
    let api_rows = page["rows"]
        .as_array()
        .context("raw statement API `rows` is not an array")?;
    ensure!(
        api_rows.len() == 2
            && api_rows.iter().all(|row| {
                row.as_object().is_some_and(|object| {
                    object.contains_key("queryid")
                        && object["queryid"].is_null()
                        && object.contains_key("query")
                        && object["query"].is_null()
                })
            }),
        "raw statement API did not preserve both nullable fields: {}",
        page["rows"]
    );
    let dsn = world.harness.database_dsn()?;
    assert_statement_reset_context(&segment, &dsn).await
}

/// Exercise source-cache invalidation and rediscovery in two signal-driven
/// cycles of the same collector, without time-based scheduling or retries.
#[when("one collector replaces a stale pg_stat_statements source with the second database")]
async fn rediscover_after_stale_source(world: &mut BddWorld) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let extra_env = world.harness.collector_env().to_vec();
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;
    let first = collector
        .snapshot()
        .await
        .context("seal the snapshot that caches the first statements source")?;
    let (first_rows, _dict) = decode_section(&first, 1_002_005)?;
    ensure!(
        !first_rows.is_empty(),
        "the first collector cycle did not cache a usable PG17 statements source"
    );

    execute_sql(
        &world.harness.database_dsn()?,
        "ALTER FUNCTION pg_stat_statements(boolean) \
         RENAME TO pg_stat_statements_stale_source",
    )
    .await
    .context("break the cached pg_stat_statements(boolean) source")?;
    execute_sql(
        &world.harness.extra_database_dsn(0)?,
        "CREATE EXTENSION pg_stat_statements; \
         SELECT pg_stat_statements_reset(); \
         SELECT 1 AS kronika_stale_source_recovered; \
         SELECT 1 AS kronika_stale_source_recovered;",
    )
    .await
    .context("prepare the replacement pg_stat_statements source")?;

    let outcome = collector
        .snapshot()
        .await
        .context("seal the snapshot after statements source rediscovery");
    let stderr = collector.stderr_captured();
    world.harness.set_collector_log(stderr.clone());
    if let Some(out_dir) = collector.take_output_dir() {
        world.harness.retain_collector_output_dir(out_dir);
    }
    let segment = outcome.with_context(|| format!("collector stderr:\n{stderr}"))?;
    world.harness.set_segment(segment);
    Ok(())
}

/// Assert that the successful rediscovered read, rather than the stale failure,
/// owns the factual coverage marker for the second cycle.
#[then(regex = r"^section ([\w.+-]+) records complete success after the cached-source failure$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn success_after_stale_failure(world: &mut BddWorld, section: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let segment = world.harness.segment()?.clone();
    let (rows, dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
    ensure!(
        !rows.is_empty(),
        "rediscovered statements source returned no rows"
    );
    assert_statement_rows_are_text_free(&rows, &section.label)?;
    assert_complete_coverage(&segment, section.type_id, &rows, &section.label)?;
    let second_database = world.harness.extra_database_name(0)?;
    ensure!(
        rows.iter().any(|row| {
            resolved_text(row.get("datname"), &dict) == Some(second_database)
                && row.get("calls") == Some(&Cell::I64(2))
        }),
        "rediscovered snapshot has no two-call row from database {second_database}"
    );
    let log = world.harness.failure_log()?;
    ensure!(
        log.contains("action=collection_probe_failure")
            && log.contains("cached_source=true")
            && log.contains("reason=query_failed"),
        "collector log does not prove the cached source failed before rediscovery:\n{log}"
    );
    let dsn = world.harness.extra_database_dsn(0)?;
    assert_statement_reset_context(&segment, &dsn).await
}

async fn execute_sql(dsn: &str, sql: &str) -> Result<()> {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect for statement-source mutation")?;
    let driver = tokio::spawn(connection);
    let result = client
        .batch_execute(sql)
        .await
        .context("execute statement-source mutation");
    drop(client);
    driver.abort();
    result
}

fn assert_statement_rows_are_text_free(rows: &[Row], label: &str) -> Result<()> {
    ensure!(
        rows.iter().all(|row| row.get("query") == Some(&Cell::Null)),
        "section {label} materialized query text in the numeric core"
    );
    Ok(())
}

fn assert_complete_coverage(
    segment: &crate::collector::SealedSegment,
    type_id: u32,
    rows: &[Row],
    label: &str,
) -> Result<()> {
    let by_ts = statement_rows_by_ts(rows, label)?;
    let (coverage, _dict) = decode_section(segment, 1_038_001)?;
    for (ts, row_count) in by_ts {
        let marker = unique_coverage_marker(&coverage, type_id, ts)?;
        ensure!(
            cell_u64(marker.get("read_state")) == Some(0)
                && cell_u64(marker.get("visibility")) == Some(0)
                && cell_u64(marker.get("source_total")) == Some(row_count)
                && cell_u64(marker.get("collected")) == Some(row_count),
            "{label} coverage at {ts} is not exact complete/full: {marker:?}"
        );
    }
    Ok(())
}

fn assert_bounded_coverage(
    segment: &crate::collector::SealedSegment,
    type_id: u32,
    rows: &[Row],
    label: &str,
) -> Result<()> {
    let by_ts = statement_rows_by_ts(rows, label)?;
    ensure!(
        by_ts.len() == 1,
        "{label} bounded regression must contain one snapshot"
    );
    let (ts, row_count) = by_ts
        .first_key_value()
        .map(|(ts, count)| (*ts, *count))
        .context("bounded statement snapshot has no timestamp")?;
    let (coverage, _dict) = decode_section(segment, 1_038_001)?;
    let marker = unique_coverage_marker(&coverage, type_id, ts)?;
    let source_total =
        cell_u64(marker.get("source_total")).context("coverage source_total is malformed")?;
    ensure!(
        cell_u64(marker.get("read_state")) == Some(1)
            && cell_u64(marker.get("visibility")) == Some(0)
            && source_total >= 14
            && cell_u64(marker.get("collected")) == Some(row_count)
            && row_count == 2,
        "{label} bounded coverage must be source_limit/full with exact N=1 axes and M>=14: \
         {marker:?}"
    );
    Ok(())
}

fn statement_rows_by_ts(rows: &[Row], label: &str) -> Result<BTreeMap<i64, u64>> {
    let mut by_ts = BTreeMap::<i64, u64>::new();
    for row in rows {
        let Some(Cell::Ts(ts)) = row.get("ts") else {
            bail!("section {label} contains a row without a timestamp");
        };
        *by_ts.entry(*ts).or_default() += 1;
    }
    Ok(by_ts)
}

fn unique_coverage_marker(coverage: &[Row], type_id: u32, ts: i64) -> Result<&Row> {
    let mut matches = coverage.iter().filter(|row| {
        row.get("ts") == Some(&Cell::Ts(ts))
            && cell_u64(row.get("section_type_id")) == Some(u64::from(type_id))
    });
    let marker = matches
        .next()
        .with_context(|| format!("no snapshot_coverage marker for type {type_id} at {ts}"))?;
    ensure!(
        matches.next().is_none(),
        "multiple snapshot_coverage markers for type {type_id} at {ts}"
    );
    Ok(marker)
}

async fn assert_statement_reset_context(
    segment: &crate::collector::SealedSegment,
    dsn: &str,
) -> Result<()> {
    let (resets, dict) = decode_section(segment, 1_020_001)?;
    ensure!(
        resets.len() == 1,
        "reset_metadata contains {} rows, expected one",
        resets.len()
    );
    let reset = &resets[0];
    let stored_version = resolved_text(reset.get("ext_pg_stat_statements_version"), &dict)
        .context("reset_metadata has no pg_stat_statements extension version")?;
    let Some(Cell::Ts(stored_reset_at)) = reset.get("pg_stat_statements_reset_at") else {
        bail!("reset_metadata has no pg_stat_statements reset timestamp");
    };

    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect for pg_stat_statements reset/version oracle")?;
    let driver = tokio::spawn(connection);
    let oracle = client
        .query_one(
            "SELECT e.extversion, \
                    (extract(epoch from i.stats_reset) * 1e6)::int8 AS reset_us \
             FROM pg_extension e \
             CROSS JOIN pg_stat_statements_info i \
             WHERE e.extname = 'pg_stat_statements'",
            &[],
        )
        .await
        .context("read pg_stat_statements reset/version oracle")?;
    driver.abort();
    let oracle_version: String = oracle.get("extversion");
    let oracle_reset_at: i64 = oracle.get("reset_us");
    ensure!(
        stored_version == oracle_version && *stored_reset_at == oracle_reset_at,
        "reset_metadata differs from pg_stat_statements oracle: \
         stored version={stored_version:?}, reset={stored_reset_at}; \
         oracle version={oracle_version:?}, reset={oracle_reset_at}"
    );
    Ok(())
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
    std::str::from_utf8(bytes).ok()
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
