//! Steps for `features/pg_stat_io.feature` (types `1_009_001` / `1_009_002`).

use anyhow::Result;
use cucumber::{gherkin::Step, then};

use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row};
use crate::steps::common::{contract_for, parse_table_with_empty_list, parse_type_id};
use crate::steps::table;

/// Assert the row identified by `pg_stat_io`'s natural label key.
#[then(regex = r"^section ([\d_]+) has a pg_stat_io row for \(([^,]+), ([^,]+), ([^)]+)\):$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn section_row_for_io_key(
    world: &mut BddWorld,
    type_id: String,
    backend_type: String,
    object: String,
    context: String,
    step: &Step,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let contract = contract_for(type_id)?;
    let expected = parse_table_with_empty_list(contract, table(step)?, |name| {
        world.harness.placeholder_pid(name)
    })?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        type_id,
        &RowSelector::ByStrFields {
            fields: vec![
                ("backend_type".to_owned(), backend_type),
                ("object".to_owned(), object),
                ("context".to_owned(), context),
            ],
        },
        false,
        &expected,
        &failure_log,
    )
}
