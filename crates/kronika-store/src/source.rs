//! Storage unit types returned by the store scan.

use std::path::PathBuf;

use kronika_format::{Catalog, DamageRegion, PartRef};

/// A sealed `.pgm` segment file with its catalog already decoded.
///
/// The catalog was read from the file tail; section bodies are not loaded.
#[derive(Debug, Clone)]
pub struct SealedUnit {
    /// Absolute path to the `.pgm` file.
    pub path: PathBuf,
    /// Catalog decoded from the file tail.
    pub catalog: Catalog,
}

/// One valid part from the `active.parts` journal.
///
/// The catalog was decoded from the part bytes but the section bodies remain
/// unread.
#[derive(Debug, Clone)]
pub struct ActivePart {
    /// Location of the part body inside the journal file.
    pub part: PartRef,
    /// Catalog decoded from the part bytes.
    pub catalog: Catalog,
}

/// Result of scanning a [`super::LocalDir`].
#[derive(Debug, Clone)]
pub struct LocalScan {
    /// Sealed segments, sorted by file name.
    pub sealed: Vec<SealedUnit>,
    /// Valid parts from `active.parts`, in journal order.
    pub active: Vec<ActivePart>,
    /// Damaged journal regions from the `active.parts` scan.
    pub damages: Vec<DamageRegion>,
    /// Warnings emitted while scanning sealed files or active journal parts.
    pub warnings: Vec<StoreWarning>,
    /// Byte offset of the end of the last valid journal frame.
    ///
    /// This is the resumable offset for the next incremental scan. It may be
    /// less than the journal file size when the tail holds an unfinished frame.
    pub valid_len: u64,
}

/// A storage item or live journal state that could not be read and was skipped.
#[derive(Debug, Clone)]
pub struct StoreWarning {
    /// Path of the file that triggered the warning.
    pub path: PathBuf,
    /// Human-readable reason the file was skipped.
    pub reason: String,
}

/// Why a storage read failed.
#[derive(Debug)]
pub enum StoreError {
    /// An I/O error occurred while reading the file.
    Io(std::io::Error),
    /// A journal part declares a body larger than the reader accepts.
    ActivePartTooLarge {
        /// Claimed active part body size, bytes.
        len: usize,
        /// Maximum accepted active part body size, bytes.
        max: u64,
    },
    /// The source is too short to contain a tail index.
    TooSmall,
    /// The first four bytes are not the PGM magic.
    BadMagic,
    /// The catalog declares a format version this build does not support.
    UnsupportedFormat {
        /// The `format_version` found in the catalog.
        version: u32,
    },
    /// `catalog_len` does not fit between the magic and the tail index.
    BadCatalogLen,
    /// The catalog bytes failed to decode.
    Catalog(kronika_format::DecodeError),
    /// A catalog entry points outside the section area.
    OutOfBounds,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::ActivePartTooLarge { len, max } => {
                write!(
                    f,
                    "active part of {len} bytes exceeds the part limit of {max}"
                )
            }
            Self::TooSmall => write!(f, "source is too small for a PGM segment"),
            Self::BadMagic => write!(f, "source does not start with PGM1 magic"),
            Self::UnsupportedFormat { version } => {
                write!(f, "unsupported format version {version}")
            }
            Self::BadCatalogLen => write!(f, "catalog_len does not fit in the source"),
            Self::Catalog(err) => write!(f, "catalog decode failed: {err}"),
            Self::OutOfBounds => write!(f, "a catalog entry points outside the section area"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Catalog(err) => Some(err),
            Self::ActivePartTooLarge { .. }
            | Self::TooSmall
            | Self::BadMagic
            | Self::UnsupportedFormat { .. }
            | Self::BadCatalogLen
            | Self::OutOfBounds => None,
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}
