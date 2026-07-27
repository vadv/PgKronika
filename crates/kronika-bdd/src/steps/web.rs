//! Web-API step glue: drive the in-process JSON router over the sealed store and
//! check its response against the `.feature` table.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};
use kronika_registry::section_name;

use crate::BddWorld;
use crate::harness::expected::parse_table;
use crate::harness::web;
use crate::steps::common::{
    contract_for, parse_key_spec, parse_section_ref, parse_table_with_empty_list, resolve_database,
};
use crate::steps::table;

/// Assert the web API serves exactly one row of a section, matching the table.
///
/// A passing row proves the sealed segment, the reader, and the HTTP layer agree.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^the web API serves section ([\w.+-]+) with one row:$")]
async fn web_section_single_row(world: &mut BddWorld, section: String, step: &Step) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let contract = contract_for(section.type_id)?;
    let rows = table(step)?;
    let expected = parse_table(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let name = section_name(section.type_id)
        .with_context(|| format!("section {} has no logical name", section.label))?;
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    let source = web::only_source(dir).await?;
    let page = web::section_page(dir, name, source).await?;
    web::assert_one_row(&page, &expected)
}

/// Assert the web API serves a row of a section identified by key columns.
///
/// HTTP mirror of the direct-decode `has a row with <keys>:` step: the same
/// `column = value` conjunction selects the row (string keys against the
/// resolved column).
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^the web API serves section ([\w.+-]+) with a row where (.+):$")]
async fn web_section_row_by_key(
    world: &mut BddWorld,
    section: String,
    key_spec: String,
    step: &Step,
) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let contract = contract_for(section.type_id)?;
    let keys = parse_key_spec(contract, &key_spec, |slot| resolve_database(world, slot))?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let name = section_name(section.type_id)
        .with_context(|| format!("section {} has no logical name", section.label))?;
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    let source = web::only_source(dir).await?;
    let page = web::section_page(dir, name, source).await?;
    web::assert_row_where(&page, &keys, &expected)
}

/// Assert the transport-level, locale-neutral Problem Details contract.
#[then("an invalid web API request returns locale-neutral Problem Details")]
async fn web_locale_neutral_problem(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    web::assert_locale_neutral_problem(dir).await
}

/// Assert `/v1/sources` reports successful default `PostgreSQL` log collection.
#[then("the web API reports PostgreSQL log state collecting for the only source")]
async fn web_pg_log_collecting(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    let status = web::only_pg_log_status(dir).await?;
    anyhow::ensure!(
        status["state"] == "collecting",
        "expected PostgreSQL log state `collecting`, got {status}"
    );
    anyhow::ensure!(
        status["reason"] == "none",
        "expected PostgreSQL log reason `none`, got {status}"
    );
    anyhow::ensure!(
        status["parser"] == "stderr",
        "expected PostgreSQL log parser `stderr`, got {status}"
    );
    anyhow::ensure!(
        status["observed_at"].as_i64().is_some(),
        "expected PostgreSQL log observation timestamp, got {status}"
    );
    anyhow::ensure!(
        status["source_path"].as_str().is_some(),
        "expected PostgreSQL log source path, got {status}"
    );
    Ok(())
}

/// Assert collected log data reaches the request-specific incident catalog.
#[then("the incident API publishes exact active observability")]
async fn incident_active_observability(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    web::assert_incident_observability(dir).await
}

/// Assert an early no-data response retains catalog visibility without diagnosis.
#[then("the incident API keeps no-data observability honest")]
async fn incident_no_data_observability(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment.data_root();
    web::assert_incident_no_data_observability(dir).await
}
