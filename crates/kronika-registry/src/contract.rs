//! The type contract: schema, column classes, sort key, and collection
//! semantics attached to every `type_id` (README.md, "Type Contract").
//!
//! A section codec must match its contract; codec tests check that directly.
//! The registry linter checks rules that span contracts.

use std::error::Error;
use std::fmt;

use crate::TypeId;

/// The role a column plays for the diff and chart machinery
/// (README.md, "Column Classes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnClass {
    /// Cumulative: a monotonic counter; rates are deltas over time.
    Cumulative,
    /// Gauge: an instantaneous value.
    Gauge,
    /// Label: identity or an attribute of the entity.
    Label,
    /// Timestamp: `i64` unix microseconds.
    Timestamp,
}

/// The on-disk type of a column value.
///
/// The set is the base types of the registry: a column uses the narrowest
/// type that fits its source so the section stays small (a `pid` is `I32`,
/// not `I64`). `Ts` is an `i64` unix-microsecond timestamp; `str_id`
/// references use `U64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer, including `str_id` references.
    U64,
    /// 32-bit float.
    F32,
    /// 64-bit float.
    F64,
    /// Boolean.
    Bool,
    /// Timestamp, `i64` unix microseconds.
    Ts,
}

/// One column of a typed section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    /// Column name, matching the registry schema and the codec.
    pub name: &'static str,
    /// On-disk value type.
    pub ty: ColumnType,
    /// The column's role.
    pub class: ColumnClass,
    /// Whether the column may be `NULL`.
    pub nullable: bool,
}

/// How a source is collected (README.md, "Collection Semantics").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Semantics {
    /// Every regular collection writes all rows of the source.
    SnapshotFull,
    /// A full snapshot only when a collection condition holds.
    ConditionalFull,
    /// All events seen in the segment interval.
    EventStream,
    /// Only rows whose cumulative counters changed, plus a baseline.
    Changed,
    /// Written on change or after a periodic refresh interval.
    OnChange,
}

/// The full contract of one `type_id`.
#[derive(Debug, Clone, Copy)]
pub struct TypeContract {
    /// The type id this contract describes.
    pub type_id: TypeId,
    /// Human-readable source name, e.g. the originating `pg_stat_*` views.
    pub name: &'static str,
    /// Collection semantics.
    pub semantics: Semantics,
    /// Columns in schema order.
    pub columns: &'static [Column],
    /// Sort-key column names, in order. Every name must be a column.
    pub sort_key: &'static [&'static str],
    /// Whether the type is retired (kept in the registry, no longer written).
    pub deprecated: bool,
}

impl TypeContract {
    /// Find a column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }
}

/// Registry lint error (README.md, "Registry Linter").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintError {
    /// The type id has an unknown class digit or a zero version.
    InvalidTypeId {
        /// The raw id that failed validation.
        type_id: u32,
    },
    /// Two contracts share a type id.
    DuplicateTypeId {
        /// The repeated raw id.
        type_id: u32,
    },
    /// A sort-key name is not a column of the type.
    SortKeyColumnMissing {
        /// The raw id whose sort key failed validation.
        type_id: u32,
        /// The sort-key name with no matching column.
        column: &'static str,
    },
    /// A `Changed` type lacks the required `is_baseline` column.
    MissingBaseline {
        /// The raw id whose `Changed` contract lacks a baseline.
        type_id: u32,
    },
    /// A column is marked [`ColumnClass::Timestamp`] but is not the
    /// non-nullable `ts` required by readers.
    BadTimestampColumn {
        /// The raw id whose timestamp column failed validation.
        type_id: u32,
        /// The column that failed validation.
        column: &'static str,
    },
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTypeId { type_id } => {
                write!(
                    f,
                    "type_id {type_id} has an unknown class, zero source, or zero version"
                )
            }
            Self::DuplicateTypeId { type_id } => {
                write!(f, "type_id {type_id} is declared more than once")
            }
            Self::SortKeyColumnMissing { type_id, column } => {
                write!(
                    f,
                    "type_id {type_id} sorts by {column:?}, which is not a column"
                )
            }
            Self::MissingBaseline { type_id } => {
                write!(
                    f,
                    "type_id {type_id} is `changed` but has no `is_baseline` column"
                )
            }
            Self::BadTimestampColumn { type_id, column } => {
                write!(
                    f,
                    "type_id {type_id} column {column:?} is class Timestamp but not a non-nullable ts"
                )
            }
        }
    }
}

impl Error for LintError {}

/// Check the invariants of one contract, appending findings to `out`.
fn lint_contract(contract: &TypeContract, out: &mut Vec<LintError>) {
    let raw = contract.type_id.get();

    // `source`/`version` number from 001, so 000 is the invalid floor the
    // `< 1` checks reject (README.md, "Registry Linter").
    if contract.type_id.section_class().is_none()
        || contract.type_id.source() < 1
        || contract.type_id.version() < 1
    {
        out.push(LintError::InvalidTypeId { type_id: raw });
    }

    for &name in contract.sort_key {
        if contract.column(name).is_none() {
            out.push(LintError::SortKeyColumnMissing {
                type_id: raw,
                column: name,
            });
        }
    }

    if matches!(contract.semantics, Semantics::Changed) && contract.column("is_baseline").is_none()
    {
        out.push(LintError::MissingBaseline { type_id: raw });
    }

    for column in contract.columns {
        if matches!(column.class, ColumnClass::Timestamp)
            && (column.nullable || !matches!(column.ty, ColumnType::Ts))
        {
            out.push(LintError::BadTimestampColumn {
                type_id: raw,
                column: column.name,
            });
        }
    }
}

/// Check every contract and the cross-type uniqueness of ids.
///
/// # Errors
///
/// Returns every [`LintError`] found across `contracts`; an empty registry
/// and a fully valid one both return `Ok`.
pub fn lint(contracts: &[TypeContract]) -> Result<(), Vec<LintError>> {
    let mut out = Vec::new();

    for (i, contract) in contracts.iter().enumerate() {
        lint_contract(contract, &mut out);
        // Quadratic, but the registry is a small fixed table.
        if contracts[..i]
            .iter()
            .any(|earlier| earlier.type_id == contract.type_id)
        {
            out.push(LintError::DuplicateTypeId {
                type_id: contract.type_id.get(),
            });
        }
    }

    if out.is_empty() { Ok(()) } else { Err(out) }
}

#[cfg(test)]
mod tests {
    use super::{Column, ColumnClass, ColumnType, LintError, Semantics, TypeContract, lint};
    use crate::TypeId;

    const TS: Column = Column {
        name: "ts",
        ty: ColumnType::Ts,
        class: ColumnClass::Timestamp,
        nullable: false,
    };
    const VALUE: Column = Column {
        name: "value",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    };

    fn contract(
        type_id: u32,
        columns: &'static [Column],
        sort_key: &'static [&'static str],
    ) -> TypeContract {
        TypeContract {
            type_id: TypeId::declared(type_id),
            name: "test",
            semantics: Semantics::SnapshotFull,
            columns,
            sort_key,
            deprecated: false,
        }
    }

    #[test]
    fn accepts_a_valid_contract() {
        let c = contract(1_006_001, &[TS, VALUE], &["ts"]);
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn rejects_invalid_id() {
        let c = contract(4_000_001, &[TS], &["ts"]);
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::InvalidTypeId { type_id: 4_000_001 }])
        );
    }

    #[test]
    fn rejects_zero_source() {
        // Class 1, version 1, but source 000 — outside the class range.
        let c = contract(1_000_001, &[TS], &["ts"]);
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::InvalidTypeId { type_id: 1_000_001 }])
        );
    }

    #[test]
    fn rejects_duplicate_ids() {
        let a = contract(1_006_001, &[TS], &["ts"]);
        let b = contract(1_006_001, &[TS], &["ts"]);
        assert_eq!(
            lint(&[a, b]),
            Err(vec![LintError::DuplicateTypeId { type_id: 1_006_001 }])
        );
    }

    #[test]
    fn rejects_sort_key_that_is_not_a_column() {
        let c = contract(1_006_001, &[TS], &["pid"]);
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::SortKeyColumnMissing {
                type_id: 1_006_001,
                column: "pid"
            }])
        );
    }

    #[test]
    fn rejects_changed_type_without_baseline() {
        let mut c = contract(1_002_001, &[TS, VALUE], &["ts"]);
        c.semantics = Semantics::Changed;
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::MissingBaseline { type_id: 1_002_001 }])
        );
    }

    #[test]
    fn rejects_nullable_timestamp() {
        const BAD_TS: Column = Column {
            name: "ts",
            ty: ColumnType::Ts,
            class: ColumnClass::Timestamp,
            nullable: true,
        };
        let c = contract(1_006_001, &[BAD_TS], &["ts"]);
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::BadTimestampColumn {
                type_id: 1_006_001,
                column: "ts"
            }])
        );
    }
}
