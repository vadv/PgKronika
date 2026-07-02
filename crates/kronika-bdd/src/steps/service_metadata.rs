//! Step definitions for the host-fact columns of `instance_metadata`
//! (`1_021_001`).
//!
//! `PostgreSQL` cannot vouch for `/proc` values, so these oracles read the
//! host independently: the file steps re-read and re-parse `/proc` with their
//! own code, and the sysconf steps call `rustix` directly. Both pin that the
//! collector delivered the live host value through interning, sealing, and
//! decoding unchanged.

use anyhow::{Context, Result};
use cucumber::then;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::steps::common::{parse_type_id, resolve_str_column, single_row};

/// The section's string column equals the trimmed content of a host file.
#[then(regex = r#"^section ([\d_]+) (\w+) equals the trimmed content of "([^"]+)"$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn column_equals_file(
    world: &mut BddWorld,
    type_id: String,
    column: String,
    path: String,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let expected = std::fs::read_to_string(&path)
        .with_context(|| format!("read the oracle file {path}"))?
        .trim()
        .to_owned();
    let (row, dict) = single_row(world, type_id)?;
    let actual = resolve_str_column(type_id, &row, &dict, &column)?;
    anyhow::ensure!(
        actual == expected,
        "section {type_id}: {column} is {actual:?}, but {path} holds {expected:?}"
    );
    Ok(())
}

/// `btime` equals an independent parse of the `/proc/stat` btime line.
#[then(regex = r"^section ([\d_]+) btime equals the /proc/stat btime in microseconds$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn btime_equals_proc_stat(world: &mut BddWorld, type_id: String) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let stat = std::fs::read_to_string("/proc/stat").context("read /proc/stat")?;
    let seconds: i64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .context("/proc/stat has no btime line")?
        .trim()
        .parse()
        .context("parse the btime seconds")?;
    let expected = Cell::Ts(seconds * 1_000_000);
    let (row, _dict) = single_row(world, type_id)?;
    let actual = row
        .get("btime")
        .with_context(|| format!("section {type_id} has no btime column"))?;
    anyhow::ensure!(
        actual == &expected,
        "section {type_id}: btime is {actual:?}, /proc/stat says {expected:?}"
    );
    Ok(())
}

/// A numeric column equals a live sysconf value read through `rustix`.
///
/// `getconf` does not exist in the scratch test image, so this is a transport
/// oracle: the value source is the same syscall, while sealing and decoding
/// are exercised independently.
#[then(regex = r"^section ([\d_]+) (\w+) equals the local sysconf (clock ticks|page size)$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn column_equals_sysconf(
    world: &mut BddWorld,
    type_id: String,
    column: String,
    which: String,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let expected = match which.as_str() {
        "clock ticks" => i64::try_from(rustix::param::clock_ticks_per_second())
            .context("clock ticks exceed i64")?,
        _ => i64::try_from(rustix::param::page_size()).context("page size exceeds i64")?,
    };
    let (row, _dict) = single_row(world, type_id)?;
    let actual = row
        .get(column.as_str())
        .with_context(|| format!("section {type_id} has no column {column:?}"))?;
    anyhow::ensure!(
        actual == &Cell::I64(expected),
        "section {type_id}: {column} is {actual:?}, sysconf says {expected}"
    );
    Ok(())
}
