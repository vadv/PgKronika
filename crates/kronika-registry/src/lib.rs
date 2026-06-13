//! Section type ids, contracts, and codecs.
//!
//! The registry defines what each `type_id` means: class, schema, column
//! classes, sort key, and collection semantics. It also contains the section
//! codecs (manual until `kronika-derive` generates them) and a linter
//! that checks the contract invariants.
//!
//! See the crate README for the `type_id` scheme, the contract model, the
//! registry linter, and the snapshot-section format.

mod codec;
mod contract;
mod type_id;

pub use codec::bgwriter_checkpointer;
pub use codec::{CodecError, MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS, arrow_schema};
pub use contract::{Column, ColumnClass, ColumnType, LintError, Semantics, TypeContract, lint};
pub use type_id::{SectionClass, TypeId};

/// Every type id known to this build, in registry order.
///
/// Retired types stay here marked [`TypeContract::deprecated`]; ids are never
/// reused.
#[must_use]
pub const fn registry() -> &'static [TypeContract] {
    &[bgwriter_checkpointer::CONTRACT]
}

/// Run the registry linter over every known type.
///
/// # Errors
///
/// Returns the [`LintError`]s found; this is the check wired into CI
/// (README.md, "Registry Linter").
pub fn lint_registry() -> Result<(), Vec<LintError>> {
    lint(registry())
}

#[cfg(test)]
mod tests {
    use super::{lint_registry, registry};

    #[test]
    fn the_registry_is_clean() {
        assert_eq!(lint_registry(), Ok(()));
    }

    #[test]
    fn registry_is_not_empty() {
        assert!(!registry().is_empty());
    }
}
