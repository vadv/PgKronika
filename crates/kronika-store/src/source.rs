//! Storage unit types returned by the store scan.

use std::path::PathBuf;
use std::sync::Arc;

use kronika_format::{Catalog, DamageRegion, PartRef};
use kronika_layout::{FileIdentity, LayoutError, SegmentAddress, SegmentId};

use crate::{CatalogDigest, CatalogSummary};

/// A sealed `.pgm` segment pinned to one filesystem identity.
///
/// Discovery retains only a compact catalog summary. Consumers open the full
/// catalog lazily after [`super::LocalDir::open_sealed`] verifies that the
/// PGM at the verified [`SegmentAddress`] still has this identity.
#[derive(Debug, Clone)]
pub struct SealedUnit {
    /// Verified logical and physical address.
    pub address: SegmentAddress,
    /// Exact filesystem identity observed by the strict layout traversal and
    /// revalidated around the catalog read.
    pub identity: FileIdentity,
    /// Fixed-size validated catalog metadata shared across cached scans.
    pub summary: Arc<CatalogSummary>,
}

/// One valid part from the `active.parts` journal.
///
/// The catalog was decoded from the part bytes but the section bodies remain
/// unread.
#[derive(Debug, Clone)]
pub struct ActivePart {
    /// Identity of the active segment persisted in journal v1.
    pub segment_id: SegmentId,
    /// Location of the part body inside the journal file.
    pub part: PartRef,
    /// Catalog decoded from the part bytes.
    pub catalog: Catalog,
    /// Offset-independent identity derived when the catalog was validated.
    pub catalog_digest: CatalogDigest,
}

/// A complete, internally consistent scan of `active.parts`.
///
/// This value can be captured under a journal identity handshake and completed
/// with a sealed-tree scan later via [`super::LocalDir::complete_scan`].
#[derive(Debug, Clone)]
pub struct JournalScan {
    /// Valid parts from `active.parts`, in journal order.
    #[expect(
        clippy::rc_buffer,
        reason = "incremental readers must share an unchanged active baseline without copying catalogs"
    )]
    pub active: Arc<Vec<ActivePart>>,
    /// Journal damage diagnostics. A successful strict v1 scan leaves this
    /// empty.
    pub damages: Vec<DamageRegion>,
    /// Byte offset of the end of the complete physical journal state.
    pub valid_len: u64,
    /// Whether the journal contains a committed reset marker.
    ///
    /// All marker publication phases are logically empty even though the old
    /// frames remain present and are validated by the scan.
    pub committed_reset: bool,
    /// Accounted retained memory for active parts and catalog entries.
    pub(crate) metadata_bytes: usize,
}

impl JournalScan {
    /// Accounted retained memory for active parts and catalog entries.
    #[must_use]
    pub const fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
}

/// Result of scanning a [`super::LocalDir`].
#[derive(Debug, Clone)]
pub struct LocalScan {
    /// Sealed segments, sorted by numeric [`SegmentId`].
    ///
    /// The collection is shared so cloning a snapshot does not copy one entry
    /// per retained segment.
    #[expect(
        clippy::rc_buffer,
        reason = "Arc<Vec<_>> preserves the completed Vec allocation; Vec-to-Arc-slice would copy it"
    )]
    pub sealed: Arc<Vec<SealedUnit>>,
    /// Valid parts from `active.parts`, in journal order.
    #[expect(
        clippy::rc_buffer,
        reason = "snapshot clones and unchanged refreshes share the validated active baseline"
    )]
    pub active: Arc<Vec<ActivePart>>,
    /// Journal damage diagnostics. A successful strict v1 scan leaves this
    /// empty.
    pub damages: Vec<DamageRegion>,
    /// Non-fatal scan diagnostics. A successful strict owned-tree scan leaves
    /// this empty.
    pub warnings: Vec<StoreWarning>,
    /// Byte offset of the end of the last valid journal frame.
    ///
    /// This is the resumable offset for the next incremental scan and equals
    /// the complete journal length after a successful strict v1 scan.
    pub valid_len: u64,
    /// Whether the captured journal state is a committed reset marker phase.
    pub committed_reset: bool,
}

/// A non-fatal storage diagnostic retained in the scan API.
#[derive(Debug, Clone)]
pub struct StoreWarning {
    /// Path of the file that triggered the warning.
    pub path: PathBuf,
    /// Human-readable diagnostic reason.
    pub reason: String,
}

/// Why a storage read failed.
#[derive(Debug)]
pub enum StoreError {
    /// An I/O error occurred while reading the file.
    Io(std::io::Error),
    /// The typed data layout rejected an unsafe or malformed tree.
    Layout(LayoutError),
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
            Self::Layout(err) => write!(f, "data layout: {err}"),
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
            Self::Layout(err) => Some(err),
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

impl From<LayoutError> for StoreError {
    fn from(err: LayoutError) -> Self {
        Self::Layout(err)
    }
}
