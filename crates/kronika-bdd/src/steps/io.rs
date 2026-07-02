//! Steps for `features/pg_stat_io.feature` (types `1_009_001` / `1_009_002`).
//!
//! All assertion and oracle steps are shared phrases from [`super::common`].
//! This module adds only the pg_stat_io-specific step that checks a section
//! is absent from the sealed segment.

use anyhow::{Context, Result};
use cucumber::then;
use kronika_reader::Segment;

use crate::BddWorld;

/// Assert that the section identified by `type_id` is not in the segment catalog.
///
/// Fails if the catalog lists an entry with that type id, which would mean the
/// wrong layout was sealed for this `PostgreSQL` version.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) is absent from the segment$")]
fn section_absent(world: &mut BddWorld, type_id: String) -> Result<()> {
    let type_id = crate::steps::common::parse_type_id(&type_id)?;
    let path = world.harness.segment()?;
    let segment = Segment::open(path).context("open sealed segment")?;
    let present = segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == type_id);
    anyhow::ensure!(
        !present,
        "section {type_id} is present in the segment but must be absent for this \
         PostgreSQL version"
    );
    Ok(())
}
