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

impl ColumnClass {
    /// Every column class, for exhaustive registry lints.
    pub const ALL: [Self; 4] = [Self::Cumulative, Self::Gauge, Self::Label, Self::Timestamp];

    /// Absolute floor for the anomaly-score scale: `Some` only for scorable
    /// classes (Cumulative in rate units/s, Gauge in raw units). Calibrated,
    /// not literature: below any real one-event-per-day rate (~1.2e-5/s),
    /// above f64 noise of integer-derived series.
    #[must_use]
    pub const fn eps_abs(self) -> Option<f64> {
        match self {
            Self::Cumulative | Self::Gauge => Some(1e-6),
            Self::Label | Self::Timestamp => None,
        }
    }
}

/// The on-disk type of a column value.
///
/// The set is the base types of the registry: a column uses the narrowest
/// type that fits its source so the section stays small (a `pid` is `I32`,
/// not `I64`). `Ts` is an `i64` unix-microsecond timestamp; `StrId` is a `u64`
/// reference into the segment string dictionary.
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
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit float.
    F32,
    /// 64-bit float.
    F64,
    /// Boolean.
    Bool,
    /// Timestamp, `i64` unix microseconds.
    Ts,
    /// A `u64` reference into the segment string dictionary (the bytes live in
    /// the dictionary, not the section).
    StrId,
    /// A list of `i32` (Arrow `List<Int32>`); an empty list is not NULL.
    ListI32,
}

/// A unix-microseconds timestamp, as carried in a `Ts` column.
///
/// A distinct type so a timestamp column is `Ts`, not a bare `i64`, whatever
/// its [`ColumnClass`] — the collection time is class [`ColumnClass::Timestamp`],
/// but `postmaster_start_time` and friends are `Ts` gauges.
///
/// `#[repr(transparent)]` guarantees the same ABI as `i64`, so a future column
/// build can `cast` a `&[Ts]` to `&[i64]` instead of mapping `.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Ts(pub i64);

/// A reference into the segment string dictionary, as carried in a `StrId`
/// column. The dictionary (`kronika-format`) holds the bytes; the section
/// stores only this id.
///
/// `#[repr(transparent)]` guarantees the same ABI as `u64` (see [`Ts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct StrId(pub u64);

/// A column of another section, referenced by logical names.
///
/// Contracts are consts, so the type system cannot resolve the reference;
/// [`lint`] checks that the section and column exist and fit the declared use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionColumnRef {
    /// Logical section name, e.g. `"reset_metadata"`.
    pub section: &'static str,
    /// Column name inside that section.
    pub column: &'static str,
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
    /// The boolean gate this column's values depend on: while the referenced
    /// GUC column reads `false`, the source does not measure this value, so a
    /// zero means "not collected", never "measured zero".
    pub gated_by: Option<SectionColumnRef>,
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
///
/// A plain public record with `pub` fields — a caller can build one directly
/// (for a test, or to feed [`lint`]). Only the [`TypeId`] is valid by
/// construction (its constructor is crate-private); the rest of a contract
/// (sort key names match columns, a `Changed` type has `is_baseline`, …) is
/// checked by [`lint`], not the type system, so a hand-built contract can be
/// internally inconsistent until it passes the linter. The registry table is
/// [`registry`](crate::registry).
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
    /// Identity columns for the diff layer: the tuple that identifies one series
    /// across snapshots. Every name must be a `Label` column. Empty until the
    /// section is wired for diffing.
    pub identity: &'static [&'static str],
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
    /// An identity name is not a column of the type.
    IdentityColumnMissing {
        /// The raw id whose identity failed validation.
        type_id: u32,
        /// The identity name with no matching column.
        column: &'static str,
    },
    /// An identity name is a column but not of class [`ColumnClass::Label`].
    IdentityColumnNotLabel {
        /// The raw id whose identity failed validation.
        type_id: u32,
        /// The identity name whose column is not a `Label`.
        column: &'static str,
    },
    /// A column class declares an `eps_abs` that is not positive and finite.
    /// Zero or negative collapses the score scale; `NaN` poisons every score.
    BadEpsAbs {
        /// The class whose declaration failed validation.
        class: ColumnClass,
    },
    /// A `gated_by` reference does not resolve to a `Bool` column of a known
    /// section.
    BadGatedBy {
        /// The raw id whose column declares the reference.
        type_id: u32,
        /// The column carrying the `gated_by` declaration.
        column: &'static str,
    },
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::IdentityColumnMissing { type_id, column } => {
                write!(
                    f,
                    "type_id {type_id} identity uses {column:?}, which is not a column"
                )
            }
            Self::IdentityColumnNotLabel { type_id, column } => {
                write!(
                    f,
                    "type_id {type_id} identity uses {column:?}, which is not a Label column"
                )
            }
            Self::BadEpsAbs { class } => {
                write!(
                    f,
                    "column class {class:?} declares an eps_abs that is not positive and finite"
                )
            }
            Self::BadGatedBy { type_id, column } => {
                write!(
                    f,
                    "type_id {type_id} column {column:?} declares a gated_by that does not \
                     resolve to a Bool column of a known section"
                )
            }
        }
    }
}

impl Error for LintError {}

/// Check the invariants of one contract, appending findings to `out`.
fn lint_contract(contract: &TypeContract, out: &mut Vec<LintError>) {
    let raw = contract.type_id.get();

    for &name in contract.sort_key {
        if contract.column(name).is_none() {
            out.push(LintError::SortKeyColumnMissing {
                type_id: raw,
                column: name,
            });
        }
    }

    for &name in contract.identity {
        match contract.column(name) {
            None => out.push(LintError::IdentityColumnMissing {
                type_id: raw,
                column: name,
            }),
            Some(column) if !matches!(column.class, ColumnClass::Label) => {
                out.push(LintError::IdentityColumnNotLabel {
                    type_id: raw,
                    column: name,
                });
            }
            Some(_) => {}
        }
    }

    if matches!(contract.semantics, Semantics::Changed) && contract.column("is_baseline").is_none()
    {
        out.push(LintError::MissingBaseline { type_id: raw });
    }

    for column in contract.columns {
        // Readers key the time axis on a column literally named `ts`, so a
        // timestamp column must be a non-nullable `Ts` *and* carry that name.
        if matches!(column.class, ColumnClass::Timestamp)
            && (column.nullable || !matches!(column.ty, ColumnType::Ts) || column.name != "ts")
        {
            out.push(LintError::BadTimestampColumn {
                type_id: raw,
                column: column.name,
            });
        }
    }
}

/// Check one class's `eps_abs` declaration, appending findings to `out`.
fn lint_eps_abs(class: ColumnClass, eps_abs: Option<f64>, out: &mut Vec<LintError>) {
    if let Some(value) = eps_abs
        && !(value.is_finite() && value > 0.0)
    {
        out.push(LintError::BadEpsAbs { class });
    }
}

/// Check cross-section references of every contract: a `gated_by` must name
/// a `Bool` column of a section present in `contracts`.
///
/// Separate from [`lint`], which holds for any subset (codec tests lint one
/// contract): references only resolve against the whole registry table.
///
/// # Errors
///
/// Returns every unresolved reference found across `contracts`.
pub fn lint_references(contracts: &[TypeContract]) -> Result<(), Vec<LintError>> {
    let mut out = Vec::new();
    for contract in contracts {
        for column in contract.columns {
            let Some(gate) = column.gated_by else {
                continue;
            };
            if !resolves_to_bool(contracts, gate) {
                out.push(LintError::BadGatedBy {
                    type_id: contract.type_id.get(),
                    column: column.name,
                });
            }
        }
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

/// Whether `gate` names a `Bool` column of any contract version sharing the
/// referenced section name.
fn resolves_to_bool(contracts: &[TypeContract], gate: SectionColumnRef) -> bool {
    contracts
        .iter()
        .filter(|contract| contract.name == gate.section)
        .any(|contract| {
            contract
                .column(gate.column)
                .is_some_and(|column| matches!(column.ty, ColumnType::Bool))
        })
}

/// Check every contract, the cross-type uniqueness of ids, and the per-class
/// `eps_abs` declarations.
///
/// # Errors
///
/// Returns every [`LintError`] found across `contracts`; an empty registry
/// and a fully valid one both return `Ok`.
pub fn lint(contracts: &[TypeContract]) -> Result<(), Vec<LintError>> {
    let mut out = Vec::new();

    for class in ColumnClass::ALL {
        lint_eps_abs(class, class.eps_abs(), &mut out);
    }

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
    use super::{
        Column, ColumnClass, ColumnType, LintError, Semantics, TypeContract, lint, lint_eps_abs,
        lint_references,
    };
    use crate::TypeId;

    const TS: Column = Column {
        name: "ts",
        ty: ColumnType::Ts,
        class: ColumnClass::Timestamp,
        nullable: false,
        gated_by: None,
    };
    const VALUE: Column = Column {
        name: "value",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
        gated_by: None,
    };

    fn contract(
        type_id: u32,
        columns: &'static [Column],
        sort_key: &'static [&'static str],
    ) -> TypeContract {
        TypeContract {
            type_id: TypeId::new(type_id).expect("test type_id must be valid"),
            name: "test",
            semantics: Semantics::SnapshotFull,
            columns,
            sort_key,
            identity: &[],
            deprecated: false,
        }
    }

    #[test]
    fn accepts_a_valid_contract() {
        let c = contract(1_006_001, &[TS, VALUE], &["ts"]);
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn all_enumerates_every_column_class() {
        // Compile-time tripwire: a new `ColumnClass` variant fails this match,
        // pointing whoever adds it at `ALL`, which must list the variant for
        // `lint` to check its `eps_abs` declaration.
        for class in ColumnClass::ALL {
            match class {
                ColumnClass::Cumulative
                | ColumnClass::Gauge
                | ColumnClass::Label
                | ColumnClass::Timestamp => {}
            }
        }
    }

    #[test]
    fn scorable_classes_declare_a_positive_finite_eps_abs() {
        for class in [ColumnClass::Cumulative, ColumnClass::Gauge] {
            let eps = class.eps_abs().expect("scorable class declares eps_abs");
            assert!(eps.is_finite() && eps > 0.0, "{class:?}: {eps}");
        }
        assert_eq!(ColumnClass::Label.eps_abs(), None);
        assert_eq!(ColumnClass::Timestamp.eps_abs(), None);
    }

    #[test]
    fn lint_rejects_a_degenerate_eps_abs() {
        // The registry constants are valid by construction, so the lint arm
        // is exercised through the helper with injected values.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1e-6] {
            let mut out = Vec::new();
            lint_eps_abs(ColumnClass::Gauge, Some(bad), &mut out);
            assert_eq!(
                out,
                vec![LintError::BadEpsAbs {
                    class: ColumnClass::Gauge
                }],
                "eps_abs {bad} must be rejected"
            );
        }
        let mut out = Vec::new();
        lint_eps_abs(ColumnClass::Cumulative, Some(1e-6), &mut out);
        lint_eps_abs(ColumnClass::Label, None, &mut out);
        assert!(out.is_empty(), "valid and absent eps_abs pass");
    }

    #[test]
    fn gated_by_resolves_against_a_bool_column_of_a_named_section() {
        const BOOL_GATE: Column = Column {
            name: "track_io_timing",
            ty: ColumnType::Bool,
            class: ColumnClass::Label,
            nullable: true,
            gated_by: None,
        };
        const TEXT_GATE: Column = Column {
            name: "track_io_timing",
            ty: ColumnType::StrId,
            class: ColumnClass::Label,
            nullable: true,
            gated_by: None,
        };
        const TIMED: Column = Column {
            name: "blk_read_time",
            ty: ColumnType::F64,
            class: ColumnClass::Cumulative,
            nullable: false,
            gated_by: Some(super::SectionColumnRef {
                section: "meta",
                column: "track_io_timing",
            }),
        };
        let timed_section = TypeContract {
            name: "timed",
            ..contract(1_006_001, &[TS, TIMED], &["ts"])
        };
        let meta_section = TypeContract {
            name: "meta",
            ..contract(1_006_002, &[TS, BOOL_GATE], &["ts"])
        };
        assert_eq!(
            lint_references(&[timed_section, meta_section]),
            Ok(()),
            "resolvable gate lints clean"
        );

        // The reference points at a section no contract carries.
        assert_eq!(
            lint_references(&[timed_section]),
            Err(vec![LintError::BadGatedBy {
                type_id: 1_006_001,
                column: "blk_read_time"
            }]),
            "an unknown gate section is rejected"
        );

        // The section exists but the gate column is not Bool there.
        let text_meta_section = TypeContract {
            name: "meta",
            ..contract(1_006_002, &[TS, TEXT_GATE], &["ts"])
        };
        assert_eq!(
            lint_references(&[timed_section, text_meta_section]),
            Err(vec![LintError::BadGatedBy {
                type_id: 1_006_001,
                column: "blk_read_time"
            }]),
            "a non-Bool gate column is rejected"
        );
    }

    #[test]
    fn an_empty_registry_lints_clean() {
        // Also pins the built-in per-class eps_abs declarations, which are
        // linted regardless of the contract list.
        assert_eq!(lint(&[]), Ok(()));
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
    fn rejects_identity_that_is_not_a_column() {
        let c = TypeContract {
            identity: &["pid"],
            ..contract(1_006_001, &[TS, VALUE], &["ts"])
        };
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::IdentityColumnMissing {
                type_id: 1_006_001,
                column: "pid"
            }])
        );
    }

    #[test]
    fn rejects_identity_column_that_is_not_a_label() {
        // `value` is a Cumulative column, not a Label.
        let c = TypeContract {
            identity: &["value"],
            ..contract(1_006_001, &[TS, VALUE], &["ts"])
        };
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::IdentityColumnNotLabel {
                type_id: 1_006_001,
                column: "value"
            }])
        );
    }

    #[test]
    fn accepts_identity_of_label_columns() {
        const QUERYID: Column = Column {
            name: "queryid",
            ty: ColumnType::I64,
            class: ColumnClass::Label,
            nullable: true,
            gated_by: None,
        };
        let c = TypeContract {
            identity: &["queryid"],
            ..contract(1_002_001, &[TS, QUERYID, VALUE], &["ts"])
        };
        assert_eq!(lint(&[c]), Ok(()));
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
            gated_by: None,
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

    #[test]
    fn rejects_a_misnamed_timestamp_column() {
        // A non-nullable `Ts` of class Timestamp, but not named `ts`: readers
        // key the time axis on the literal name, so the linter must reject it.
        const COLLECTED_AT: Column = Column {
            name: "collected_at",
            ty: ColumnType::Ts,
            class: ColumnClass::Timestamp,
            nullable: false,
            gated_by: None,
        };
        let c = contract(1_006_001, &[COLLECTED_AT], &["collected_at"]);
        assert_eq!(
            lint(&[c]),
            Err(vec![LintError::BadTimestampColumn {
                type_id: 1_006_001,
                column: "collected_at"
            }])
        );
    }
}
