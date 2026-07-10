//! Web-API step glue: drive the in-process JSON router over the sealed store and
//! check its response against the `.feature` table.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};
use kronika_registry::section_name;

use crate::BddWorld;
use crate::harness::expected::parse_table;
use crate::harness::web;
use crate::steps::common::{contract_for, parse_section_ref};
use crate::steps::table;

/// Assert the web API serves exactly one row of a section, matching the table.
///
/// The row travels the whole read path — the sealed segment, the reader query
/// layer, and the HTTP serialization — so a passing row proves they agree, not
/// only that the on-disk segment is correct.
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
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    let source = web::only_source(dir).await?;
    let page = web::section_page(dir, name, source).await?;
    web::assert_one_row(&page, &expected)
}
