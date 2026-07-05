//! Shared helpers for decoding `tokio_postgres::Row` values into source rows.

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;

use tokio_postgres::Row;
use tokio_postgres::types::FromSqlOwned;

/// Error while building a `PostgreSQL` row mapper or decoding one row.
#[derive(Debug)]
#[allow(
    clippy::enum_variant_names,
    reason = "variants name the column-related failure without relying on enum context"
)]
pub enum PgRowError {
    /// The query result does not contain the column required for a field.
    MissingColumn {
        /// Source row type being decoded.
        row: &'static str,
        /// Rust field being filled.
        field: &'static str,
        /// `PostgreSQL` column alias expected by the field.
        column: &'static str,
    },
    /// The query result contains the same selected column name more than once.
    DuplicateColumn {
        /// Source row type being decoded.
        row: &'static str,
        /// Rust field being filled.
        field: &'static str,
        /// `PostgreSQL` column alias expected by the field.
        column: &'static str,
    },
    /// `tokio-postgres` could not decode the selected column into the field type.
    DecodeColumn {
        /// Source row type being decoded.
        row: &'static str,
        /// Rust field being filled.
        field: &'static str,
        /// `PostgreSQL` column alias read by the field.
        column: &'static str,
        /// Decode failure returned by `tokio-postgres`.
        source: tokio_postgres::Error,
    },
}

impl fmt::Display for PgRowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColumn { row, field, column } => {
                write!(f, "{row}.{field}: missing PostgreSQL column `{column}`")
            }
            Self::DuplicateColumn { row, field, column } => {
                write!(f, "{row}.{field}: duplicate PostgreSQL column `{column}`")
            }
            Self::DecodeColumn {
                row,
                field,
                column,
                source,
            } => write!(
                f,
                "{row}.{field}: cannot decode PostgreSQL column `{column}`: {source}"
            ),
        }
    }
}

impl Error for PgRowError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DecodeColumn { source, .. } => Some(source),
            Self::MissingColumn { .. } | Self::DuplicateColumn { .. } => None,
        }
    }
}

/// Error returned by converted `PostgreSQL` collectors.
#[derive(Debug)]
pub enum PgCollectError {
    /// Query preparation or execution failed.
    Query(tokio_postgres::Error),
    /// Query succeeded, but the returned row shape did not match the mapper.
    Row(PgRowError),
}

impl PgCollectError {
    /// Return the underlying query error when this is a database failure.
    #[must_use]
    pub const fn as_query_error(&self) -> Option<&tokio_postgres::Error> {
        match self {
            Self::Query(err) => Some(err),
            Self::Row(_) => None,
        }
    }
}

impl fmt::Display for PgCollectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Query(err) => fmt::Display::fmt(err, f),
            Self::Row(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for PgCollectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Query(err) => Some(err),
            Self::Row(err) => Some(err),
        }
    }
}

impl From<tokio_postgres::Error> for PgCollectError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self::Query(source)
    }
}

impl From<PgRowError> for PgCollectError {
    fn from(source: PgRowError) -> Self {
        Self::Row(source)
    }
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "used by pg_row_mapper-generated collector mappings"
)]
pub(crate) struct PgCol<T> {
    index: usize,
    row: &'static str,
    field: &'static str,
    column: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> PgCol<T> {
    pub(crate) fn required<I, S>(
        row: &'static str,
        field: &'static str,
        column: &'static str,
        columns: I,
    ) -> Result<Self, PgRowError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut matches = columns
            .into_iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.as_ref() == column)
            .map(|(index, _)| index);
        let Some(index) = matches.next() else {
            return Err(PgRowError::MissingColumn { row, field, column });
        };
        if matches.next().is_some() {
            return Err(PgRowError::DuplicateColumn { row, field, column });
        }
        Ok(Self {
            index,
            row,
            field,
            column,
            _type: PhantomData,
        })
    }

    #[cfg(test)]
    pub(crate) const fn index(&self) -> usize {
        self.index
    }
}

impl<T> PgCol<T>
where
    T: FromSqlOwned,
{
    #[allow(
        dead_code,
        reason = "used by pg_row_mapper-generated collector mappings"
    )]
    pub(crate) fn get(&self, row: &Row) -> Result<T, PgRowError> {
        row.try_get(self.index)
            .map_err(|source| PgRowError::DecodeColumn {
                row: self.row,
                field: self.field,
                column: self.column,
                source,
            })
    }
}

pub(crate) trait PgGatedDecode<T>: Sized {
    fn decode_gated(column: Option<&PgCol<T>>, row: &Row) -> Result<Self, PgRowError>;
}

// A gated non-null column (`plans: i64 if ...`) becomes `Option<i64>`, while a
// gated nullable column (`query_id: Option<i64> if ...`) stays `Option<i64>`.
impl<T> PgGatedDecode<T> for Option<T>
where
    T: FromSqlOwned,
{
    fn decode_gated(column: Option<&PgCol<T>>, row: &Row) -> Result<Self, PgRowError> {
        column.map_or_else(|| Ok(None), |column| column.get(row).map(Some))
    }
}

impl<T> PgGatedDecode<Self> for Option<T>
where
    Self: FromSqlOwned,
{
    fn decode_gated(column: Option<&PgCol<Self>>, row: &Row) -> Result<Self, PgRowError> {
        column.map_or_else(|| Ok(None), |column| column.get(row))
    }
}

pub(crate) fn read_gated<T, O>(column: Option<&PgCol<T>>, row: &Row) -> Result<O, PgRowError>
where
    T: FromSqlOwned,
    O: PgGatedDecode<T>,
{
    O::decode_gated(column, row)
}

#[cfg(test)]
mod tests {
    use super::{PgCol, PgRowError};

    #[test]
    fn required_column_finds_index() {
        let col = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid", "ts_us"])
            .expect("column should exist");

        assert_eq!(col.index(), 1);
    }

    #[test]
    fn required_column_reports_missing_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid"])
            .expect_err("missing column should be an error");

        assert!(matches!(
            err,
            PgRowError::MissingColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(
            err.to_string(),
            "ActivityRow.ts: missing PostgreSQL column `ts_us`"
        );
    }

    #[test]
    fn required_column_reports_duplicate_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["ts_us", "ts_us"])
            .expect_err("duplicate column should be an error");

        assert!(matches!(
            err,
            PgRowError::DuplicateColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(
            err.to_string(),
            "ActivityRow.ts: duplicate PostgreSQL column `ts_us`"
        );
    }
}

#[cfg(test)]
mod macro_tests {
    #[allow(dead_code, reason = "macro tests inspect the generated column map")]
    #[derive(Debug, PartialEq)]
    struct DemoRow {
        ts: i64,
        optional: Option<i64>,
        gated: Option<i64>,
        renamed: i64,
    }

    #[derive(Debug, Clone, Copy)]
    enum DemoVersion {
        V1,
        V2,
    }

    pg_row_mapper! {
        DemoCols(version: DemoVersion) => DemoRow {
            ts: i64 = "ts_us",
            optional: Option<i64> = "optional_value",
            gated: Option<i64> = "gated_value"
                if matches!(version, DemoVersion::V2),
            renamed: i64 = {
                match version {
                    DemoVersion::V1 => "old_name",
                    DemoVersion::V2 => "new_name",
                }
            },
        }
    }

    #[test]
    fn macro_builds_versioned_column_map() {
        let cols =
            DemoCols::new_from_names(DemoVersion::V1, ["ts_us", "optional_value", "old_name"])
                .expect("V1 columns should resolve");

        assert!(cols.gated.is_none());
    }

    #[test]
    fn macro_requires_gated_column_when_version_enables_it() {
        let err =
            DemoCols::new_from_names(DemoVersion::V2, ["ts_us", "optional_value", "new_name"])
                .expect_err("V2 should require gated_value");

        assert_eq!(
            err.to_string(),
            "DemoRow.gated: missing PostgreSQL column `gated_value`"
        );
    }

    #[test]
    fn macro_uses_versioned_column_alias() {
        let err = DemoCols::new_from_names(
            DemoVersion::V2,
            ["ts_us", "optional_value", "gated_value", "old_name"],
        )
        .expect_err("V2 should require the new alias");

        assert_eq!(
            err.to_string(),
            "DemoRow.renamed: missing PostgreSQL column `new_name`"
        );
    }
}
