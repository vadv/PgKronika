//! Section codecs and the pieces shared between them.
//!
//! A snapshot section body is a self-contained Parquet file
//! (README.md, "Snapshot Sections"). Until `kronika-derive` generates these
//! codecs, they are implemented manually, one module per type.
//!
//! The Arrow schema of a section is built from its [`TypeContract`], so a
//! codec uses the registry's column order, Arrow types, and nullability.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, SchemaRef};

use crate::contract::{ColumnType, TypeContract};

pub mod bgwriter_checkpointer;

/// Maximum rows in one snapshot section.
///
/// Enforced memory bound (README.md, "Memory Bounds"): encode rejects larger
/// input slices, and decode rejects sections whose metadata claims more rows
/// before allocating arrays for them. The current single-row sources produce
/// far fewer rows in one segment; this limit catches writer bugs and malformed
/// sections.
pub const MAX_SECTION_ROWS: usize = 65_536;

/// Maximum accepted section byte length on decode.
///
/// This guard runs before Parquet reads the file metadata, so it bounds the
/// amount of input Parquet can inspect before the registry checks row counts
/// and row groups (README.md, "Memory Bounds"). The value is far above the
/// size of current snapshot sections.
pub const MAX_SECTION_BYTES: usize = 8 * 1024 * 1024;

/// Maximum row groups in one section. The writer emits one row group per
/// section; this rejects an abnormal section after metadata is read and before
/// any column chunk is read.
pub const MAX_ROW_GROUPS: usize = 16;

/// Why a section failed to encode or decode.
#[derive(Debug)]
pub enum CodecError {
    /// An Arrow operation failed (building the record batch).
    Arrow(arrow_schema::ArrowError),
    /// A Parquet operation failed (writing or reading the file).
    Parquet(parquet::errors::ParquetError),
    /// More rows than [`MAX_SECTION_ROWS`] were given to encode, or a
    /// section claims or holds more on decode.
    TooManyRows {
        /// The row count that exceeded the cap.
        rows: usize,
        /// The enforced cap.
        max: usize,
    },
    /// The section byte length is above [`MAX_SECTION_BYTES`].
    SectionTooLarge {
        /// The byte length that exceeded the cap.
        len: usize,
        /// The enforced cap.
        max: usize,
    },
    /// The section has more than [`MAX_ROW_GROUPS`] row groups.
    TooManyRowGroups {
        /// The row-group count that exceeded the cap.
        groups: usize,
        /// The enforced cap.
        max: usize,
    },
    /// A column required by the contract is absent from the decoded file.
    MissingColumn {
        /// The missing column name.
        name: &'static str,
    },
    /// A decoded column has a different Arrow type than the contract.
    ColumnType {
        /// The column name.
        name: &'static str,
    },
    /// A `NULL` appeared in a column the contract declares non-nullable.
    ///
    /// `NULL` must stay distinct from a real value, so a non-nullable column
    /// that decodes a null is a malformed section, not a zero.
    NullInRequiredColumn {
        /// The column name.
        name: &'static str,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arrow(err) => write!(f, "arrow: {err}"),
            Self::Parquet(err) => write!(f, "parquet: {err}"),
            Self::TooManyRows { rows, max } => {
                write!(f, "section has {rows} rows, above the cap of {max}")
            }
            Self::SectionTooLarge { len, max } => {
                write!(f, "section is {len} bytes, above the cap of {max}")
            }
            Self::TooManyRowGroups { groups, max } => {
                write!(f, "section has {groups} row groups, above the cap of {max}")
            }
            Self::MissingColumn { name } => write!(f, "decoded section lacks column {name:?}"),
            Self::ColumnType { name } => write!(f, "decoded column {name:?} has the wrong type"),
            Self::NullInRequiredColumn { name } => {
                write!(
                    f,
                    "decoded column {name:?} has a NULL but the contract forbids it"
                )
            }
        }
    }
}

impl Error for CodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Arrow(err) => Some(err),
            Self::Parquet(err) => Some(err),
            Self::TooManyRows { .. }
            | Self::SectionTooLarge { .. }
            | Self::TooManyRowGroups { .. }
            | Self::MissingColumn { .. }
            | Self::ColumnType { .. }
            | Self::NullInRequiredColumn { .. } => None,
        }
    }
}

impl From<arrow_schema::ArrowError> for CodecError {
    fn from(err: arrow_schema::ArrowError) -> Self {
        Self::Arrow(err)
    }
}

impl From<parquet::errors::ParquetError> for CodecError {
    fn from(err: parquet::errors::ParquetError) -> Self {
        Self::Parquet(err)
    }
}

/// Build the Arrow schema of a section from its contract.
///
/// The returned schema uses the contract's column order, Arrow types, and
/// nullability.
#[must_use]
pub fn arrow_schema(contract: &TypeContract) -> SchemaRef {
    let fields: Vec<Field> = contract
        .columns
        .iter()
        .map(|column| {
            let data_type = match column.ty {
                ColumnType::I64 | ColumnType::Ts => DataType::Int64,
                ColumnType::F64 => DataType::Float64,
                ColumnType::U64 => DataType::UInt64,
                ColumnType::Bool => DataType::Boolean,
            };
            Field::new(column.name, data_type, column.nullable)
        })
        .collect();
    Arc::new(Schema::new(fields))
}
