//! Step definitions for `features/pg_settings.feature`.

use anyhow::{Context, Result, bail};
use cucumber::then;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::steps::common::parse_type_id;

/// Assert one column of the `pg_settings` row named in the step.
///
/// The row is selected by resolving the `name` column through the segment
/// dictionary. The expected value is written as text: strings compare
/// resolved, booleans as `true`/`false`, integers as decimal, and `null`
/// matches a `NULL` cell.
#[then(regex = r#"^section ([\d_]+) pg_settings entry "([^"]+)" has (\w+) = "([^"]*)"$"#)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn settings_entry_column(
    world: &mut BddWorld,
    type_id: String,
    name: String,
    column: String,
    expected: String,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
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
            row.get("name")
                .and_then(&resolve)
                .is_some_and(|n| n == name)
        })
        .with_context(|| format!("section {type_id} has no pg_settings entry {name:?}"))?;

    let cell = row
        .get(column.as_str())
        .with_context(|| format!("section {type_id} has no column {column:?}"))?;
    let rendered = match cell {
        Cell::Null => "null".to_owned(),
        Cell::Bool(b) => b.to_string(),
        Cell::I32(n) => n.to_string(),
        Cell::StrId(_) => resolve(cell).with_context(|| {
            format!("{column} of {name:?} did not resolve through the dictionary")
        })?,
        other => bail!("{column} of {name:?} is {other:?}; the step compares text, bool, or i32"),
    };
    anyhow::ensure!(
        rendered == expected,
        "section {type_id}: {column} of {name:?} is {rendered:?}, expected {expected:?}"
    );
    Ok(())
}
