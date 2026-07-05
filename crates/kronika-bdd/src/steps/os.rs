//! Step definitions for `features/os_core.feature`.
//!
//! All assertions read the fixture `/proc` tree (via `KRONIKA_PROC_ROOT`) as
//! the oracle, so no host-specific values appear in assertions.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given, then};
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::steps::common::parse_type_id;

/// Seed the fixture `/proc` tree with minimal valid content for all OS sources.
///
/// Sets `KRONIKA_PROC_ROOT` in the collector env so the spawned collector reads
/// the fixture tree instead of the host `/proc`.
#[given("a fixture proc tree")]
fn fixture_proc_tree(world: &mut BddWorld) -> Result<()> {
    world.harness.seed_default_proc_fixture()
}

/// Write the step's docstring to `<fixture-root>/<rel>`.
///
/// A later `And the fixture proc file "stat" contains:` step overwrites the
/// seed written by `a fixture proc tree` with scenario-specific values.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[given(regex = r#"^the fixture proc file "([^"]+)" contains:$"#)]
fn fixture_proc_file(world: &mut BddWorld, rel: String, step: &Step) -> Result<()> {
    let content = crate::steps::docstring(step)?;
    // fixture_proc_root may not exist yet if the step order differs; create it.
    world.harness.fixture_proc_root()?;
    world.harness.write_proc_fixture(&rel, content)
}

/// Assert the total number of rows in a section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) has (\d+) rows?$")]
fn section_row_count(world: &mut BddWorld, type_id: String, count: usize) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    anyhow::ensure!(
        rows.len() == count,
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: expected {count} row(s), got {}",
                rows.len()
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Assert an integer column in the row whose `cpu_id` equals `cpu_id_val`.
///
/// `cpu_id_val` is `-1` for the aggregate line.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) cpu_id row (-?\d+) has (\w+) = (-?\d+)$")]
fn section_cpu_row_column(
    world: &mut BddWorld,
    type_id: String,
    cpu_id: i32,
    column: String,
    expected: i64,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    let row = rows
        .iter()
        .find(|r| r.get("cpu_id") == Some(&Cell::I32(cpu_id)))
        .with_context(|| {
            dump::section_dump(
                &format!("section {type_id}: no row with cpu_id = {cpu_id}"),
                &rows,
                &failure_log,
                &[],
            )
        })?;
    let actual = row
        .get(column.as_str())
        .with_context(|| format!("section {type_id}: column {column:?} absent"))?;
    anyhow::ensure!(
        int_cell_equals(actual, expected),
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: cpu_id={cpu_id} {column}: expected {expected}, got {}",
                dump::render_cell(actual)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Assert an integer singleton column in a single-row section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) (\w+) equals (-?\d+)$")]
fn section_column_equals(
    world: &mut BddWorld,
    type_id: String,
    column: String,
    expected: i64,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    anyhow::ensure!(
        rows.len() == 1,
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: expected exactly one row, got {}",
                rows.len()
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    let actual = rows[0]
        .get(column.as_str())
        .with_context(|| format!("section {type_id}: column {column:?} absent"))?;
    anyhow::ensure!(
        int_cell_equals(actual, expected),
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: {column}: expected {expected}, got {}",
                dump::render_cell(actual)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Assert a floating-point singleton column in a single-row section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) (\w+) equals (-?\d+\.\d+)$")]
fn section_float_column_equals(
    world: &mut BddWorld,
    type_id: String,
    column: String,
    expected: f64,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    anyhow::ensure!(
        rows.len() == 1,
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: expected exactly one row, got {}",
                rows.len()
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    let actual = rows[0]
        .get(column.as_str())
        .with_context(|| format!("section {type_id}: column {column:?} absent"))?;
    anyhow::ensure!(
        float_cell_equals(actual, expected),
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: {column}: expected {expected}, got {}",
                dump::render_cell(actual)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Assert an integer column in a PSI row selected by resource id.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) resource row (-?\d+) has (\w+) = (-?\d+)$")]
fn section_resource_row_column(
    world: &mut BddWorld,
    type_id: String,
    resource: i64,
    column: String,
    expected: i64,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    let row = rows
        .iter()
        .find(|r| {
            r.get("resource")
                .is_some_and(|cell| int_cell_equals(cell, resource))
        })
        .with_context(|| {
            dump::section_dump(
                &format!("section {type_id}: no row with resource = {resource}"),
                &rows,
                &failure_log,
                &[],
            )
        })?;
    let actual = row
        .get(column.as_str())
        .with_context(|| format!("section {type_id}: column {column:?} absent"))?;
    anyhow::ensure!(
        int_cell_equals(actual, expected),
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: resource={resource} {column}: expected {expected}, got {}",
                dump::render_cell(actual)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

/// Assert a `NULL` column in a PSI row selected by resource id.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) resource row (-?\d+) has (\w+) = null$")]
fn section_resource_row_column_null(
    world: &mut BddWorld,
    type_id: String,
    resource: i64,
    column: String,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) = decode_section(&segment, type_id)?;
    let row = rows
        .iter()
        .find(|r| {
            r.get("resource")
                .is_some_and(|cell| int_cell_equals(cell, resource))
        })
        .with_context(|| {
            dump::section_dump(
                &format!("section {type_id}: no row with resource = {resource}"),
                &rows,
                &failure_log,
                &[],
            )
        })?;
    let actual = row
        .get(column.as_str())
        .with_context(|| format!("section {type_id}: column {column:?} absent"))?;
    anyhow::ensure!(
        actual == &Cell::Null,
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: resource={resource} {column}: expected null, got {}",
                dump::render_cell(actual)
            ),
            &rows,
            &failure_log,
            &[],
        )
    );
    Ok(())
}

fn int_cell_equals(actual: &Cell, expected: i64) -> bool {
    match actual {
        Cell::I16(value) => i64::from(*value) == expected,
        Cell::I32(value) => i64::from(*value) == expected,
        Cell::I64(value) | Cell::Ts(value) => *value == expected,
        Cell::U32(value) => u32::try_from(expected) == Ok(*value),
        Cell::U64(value) => u64::try_from(expected) == Ok(*value),
        _ => false,
    }
}

fn float_cell_equals(actual: &Cell, expected: f64) -> bool {
    match actual {
        Cell::F64(value) => (*value - expected).abs() <= f64::EPSILON,
        _ => false,
    }
}
