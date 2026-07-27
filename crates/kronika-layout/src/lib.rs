//! Typed access to the `PgKronika` data-directory layout.
//!
//! Finished segments live at `YYYY/MM/DD/N.pgm`, where the UTC bucket is
//! derived from [`SegmentId`]. Derived overview files use the sibling
//! `N.ovf` name. [`DataRoot`] validates the closed directory grammar and opens
//! every component relative to an already-open directory descriptor without
//! following symbolic links.

mod error;
mod root;
mod time;

pub use error::{LayoutError, LimitKind, OwnerKind};
pub use root::{
    ACTIVE_JOURNAL_NAME, DataRoot, EntryDiagnostic, EntryFileType, EntryScope, EvidenceFile,
    EvidenceLocation, FileIdentity, FileKind, FilesystemUsage, ForeignEntry, ForeignEntryReason,
    FreshJournalGeneration, JournalActivation, JournalRotation, JournalRotationOutcome,
    JournalSlot, JournalSlotKind, LayoutLimits, LayoutSnapshot, OVERVIEW_OWNER_LOCK_NAME,
    OverviewOwner, OvfTemp, PathIdentity, PendingRootEntry, PendingRootKind, PgmTemp,
    QUARANTINE_DIRECTORY_NAME, QuarantineDirectoryState, QuarantineEntry, QuarantineFailure,
    QuarantineFailureStage, QuarantineOutcome, QuarantineReason, QuarantineStatus,
    SegmentArtifacts, SegmentRemoval, TemporaryKind, TemporaryObject, WRITER_OWNER_LOCK_NAME,
    WriterLease, WriterOwner,
};
pub use time::{SegmentAddress, SegmentId, UtcDay};
