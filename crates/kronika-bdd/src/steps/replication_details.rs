//! Step definitions for `features/replication_details.feature`.
//!
//! Slots are created through SQL and dropped by harness cleanup (a logical
//! slot pins its database, so cleanup drops slots first). The walsender
//! scenario runs `pg_receivewal` from the cluster's own `bin` directory and
//! checks section `1_016_001` against that streaming connection.

use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use cucumber::gherkin::Step;
use cucumber::{given, then};
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::steps::common::parse_type_id;
use crate::steps::table;

/// Reject slot and application names that cannot be safely embedded.
fn ensure_safe_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "name {name:?} must be alphanumeric/underscore"
    );
    Ok(())
}

/// Open a client on the scenario database.
async fn scenario_client(world: &BddWorld) -> Result<tokio_postgres::Client> {
    let dsn = world.harness.database_dsn()?;
    let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect to the scenario database")?;
    tokio::spawn(async move { drop(conn.await) });
    Ok(client)
}

/// Create a replication slot of the requested kind on the scenario database.
///
/// `reserving WAL` makes a physical slot pin WAL immediately, so
/// `restart_lsn` and the columns derived from it are set without a consumer.
#[given(regex = r#"^an? (physical|logical) replication slot "([^"]+)"( reserving WAL)?$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn create_slot(
    world: &mut BddWorld,
    kind: String,
    slot_name: String,
    reserve: String,
) -> Result<()> {
    ensure_safe_name(&slot_name)?;
    let client = scenario_client(world).await?;
    match kind.as_str() {
        "physical" => {
            let reserve_wal = !reserve.is_empty();
            client
                .execute(
                    "SELECT pg_create_physical_replication_slot($1, $2)",
                    &[&slot_name, &reserve_wal],
                )
                .await
                .with_context(|| format!("create physical slot {slot_name:?}"))?;
        }
        _ => {
            client
                .execute(
                    "SELECT pg_create_logical_replication_slot($1, 'pgoutput')",
                    &[&slot_name],
                )
                .await
                .with_context(|| format!("create logical slot {slot_name:?}"))?;
        }
    }
    world.harness.add_slot_drop(slot_name);
    Ok(())
}

/// Start `pg_receivewal` and wait until its walsender shows up.
///
/// The tool comes from the scenario cluster's own `bin` directory, streams
/// into a fresh physical slot, and identifies itself through `PGAPPNAME`.
/// Cleanup kills the process and drops the slot.
#[given(regex = r#"^a WAL receiver streams as application "([^"]+)" using slot "([^"]+)"$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
async fn start_wal_receiver(
    world: &mut BddWorld,
    application: String,
    slot_name: String,
) -> Result<()> {
    ensure_safe_name(&application)?;
    ensure_safe_name(&slot_name)?;
    let client = scenario_client(world).await?;
    client
        .execute(
            "SELECT pg_create_physical_replication_slot($1, false)",
            &[&slot_name],
        )
        .await
        .with_context(|| format!("create slot {slot_name:?} for the receiver"))?;
    world.harness.add_slot_drop(slot_name.clone());

    let workdir = tempfile::TempDir::new().context("create a WAL receive directory")?;
    let cluster = world.harness.cluster()?;
    let child = tokio::process::Command::new(cluster.bindir().join("pg_receivewal"))
        .arg("-D")
        .arg(workdir.path())
        .arg("--slot")
        .arg(&slot_name)
        .arg("--no-password")
        .arg("-d")
        .arg(world.harness.database_dsn()?)
        .env("PGAPPNAME", &application)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn pg_receivewal")?;
    world
        .harness
        .add_background_tool(format!("pg_receivewal {slot_name}"), child, workdir);

    // The walsender registers asynchronously; poll until it is visible.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let count: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_stat_replication WHERE application_name = $1",
                &[&application],
            )
            .await
            .context("poll pg_stat_replication for the receiver")?
            .get(0);
        if count == 1 {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("pg_receivewal did not appear in pg_stat_replication within 15s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Assert columns of the slot row named in the step.
#[then(regex = r#"^section ([\d_]+) has a replication slot "([^"]+)" with:$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn slot_row_matches(
    world: &mut BddWorld,
    type_id: String,
    slot_name: String,
    step: &Step,
) -> Result<()> {
    assert_named_row(world, &type_id, "slot_name", &slot_name, step)
}

/// Assert columns of the walsender row named by its application.
#[then(regex = r#"^section ([\d_]+) has a replica row for application "([^"]+)" with:$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn replica_row_matches(
    world: &mut BddWorld,
    type_id: String,
    application: String,
    step: &Step,
) -> Result<()> {
    assert_named_row(world, &type_id, "application_name", &application, step)
}

/// Find the single row whose `key_column` resolves to `key`, then compare the
/// table: strings resolve through the dictionary, numbers and booleans render
/// as text, `null` expects a NULL cell, and `not null` expects any value.
fn assert_named_row(
    world: &BddWorld,
    type_id: &str,
    key_column: &str,
    key: &str,
    step: &Step,
) -> Result<()> {
    let type_id = parse_type_id(type_id)?;
    let segment = world.harness.segment()?.clone();
    let (rows, dict) = decode_section(&segment, type_id)?;

    let resolve = |cell: &Cell| -> Option<String> {
        let Cell::StrId(id) = cell else { return None };
        match dict.resolve(*id)? {
            kronika_reader::Resolved::String(bytes)
            | kronika_reader::Resolved::Blob { bytes, .. } => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        }
    };

    let row = rows
        .iter()
        .find(|row| {
            row.get(key_column)
                .and_then(&resolve)
                .is_some_and(|v| v == key)
        })
        .with_context(|| format!("section {type_id} has no row with {key_column} = {key:?}"))?;

    for expected in table(step)? {
        let [column, want] = expected.as_slice() else {
            bail!("the table needs | column | value | rows, got {expected:?}");
        };
        let cell = row
            .get(column.as_str())
            .with_context(|| format!("section {type_id} has no column {column:?}"))?;
        if want == "not null" {
            ensure!(
                cell != &Cell::Null,
                "section {type_id}: {column} of {key:?} is NULL, expected a value"
            );
            continue;
        }
        let rendered = match cell {
            Cell::Null => "null".to_owned(),
            Cell::Bool(b) => b.to_string(),
            Cell::I32(n) => n.to_string(),
            Cell::I64(n) => n.to_string(),
            Cell::StrId(_) => resolve(cell).with_context(|| {
                format!("{column} of {key:?} did not resolve through the dictionary")
            })?,
            other => bail!("{column} of {key:?} is {other:?}; the step compares text-like cells"),
        };
        ensure!(
            &rendered == want,
            "section {type_id}: {column} of {key:?} is {rendered:?}, expected {want:?}"
        );
    }
    Ok(())
}
