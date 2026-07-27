//! Read-only inspection and bounded replay of a rotated journal generation.
//!
//! Layout owns evidence rotation and supplies an already-open regular-file
//! descriptor. This module never renames, truncates, or deletes evidence.

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::os::unix::fs::FileExt as _;

use kronika_format::{
    JOURNAL_HEADER_LEN, JournalHeader, JournalHeaderError, JournalState, PartError, PartRef,
    RecoveryDamageRegion, RecoveryScanError, RecoveryScanLimits, RecoveryScanStop,
    scan_journal_streaming_recovery_from, validate_part,
};
use kronika_layout::{FileIdentity, SegmentId};

use crate::journal::validate_config;
use crate::{Journal, JournalConfig, JournalError};

/// Primary payload-free classification of one rotated journal generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalRecoveryReason {
    /// The evidence is a zero-length file and contains no telemetry.
    ZeroLength,
    /// The evidence ends before a complete version-1 root header.
    TornHeader,
    /// The complete header names unsupported magic or a different version.
    UnsupportedHeader,
    /// The complete version-1 header failed checksum or state validation.
    InvalidHeader,
    /// An empty header has trailing bytes but no trusted segment identity.
    EmptyWithTrailingBytes,
    /// The active header carries an identity unsupported by the calendar layout.
    InvalidSegmentId,
    /// Recorded and physical body lengths differ.
    BodyLengthMismatch,
    /// One or more physical frame regions failed verification.
    FrameDamage,
    /// An active header has no complete verified frame.
    ActiveWithoutFrames,
    /// A hard byte, part, or candidate-work admission limit stopped inspection.
    AdmissionLimit,
    /// Header, physical length, and every frame are internally consistent.
    Clean,
}

/// Bounded read-only result from inspecting exact quarantined evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecoverySummary {
    /// Primary recovery classification.
    pub reason: JournalRecoveryReason,
    /// Trusted identity from a complete valid Active v1 header.
    pub trusted_segment_id: Option<SegmentId>,
    /// Complete evidence-file bytes.
    pub evidence_bytes: u64,
    /// Body length declared by a complete valid header.
    pub recorded_body_bytes: Option<u64>,
    /// Physical bytes after a complete header, or zero without one.
    pub physical_body_bytes: u64,
    /// Whether declared and physical body lengths differ.
    pub body_length_mismatch: bool,
    /// Complete frames whose header, PGM catalog, and all section CRCs verify.
    pub verified_frames: usize,
    /// Catalog row declarations across verified PGM parts.
    pub verified_rows: u64,
    /// PGM part-body bytes across verified frames.
    pub verified_part_bytes: u64,
    /// Physical body bytes not belonging to verified complete frames.
    pub discarded_bytes: u64,
    /// Physical bytes after the final verified complete frame.
    pub discarded_suffix_bytes: u64,
    /// Payload-free physical damage ranges.
    pub damages: Vec<RecoveryDamageRegion>,
    /// Bounded scanner outcome for a trusted active header.
    pub scan_stop: Option<RecoveryScanStop>,
}

/// Exact evidence plus the immutable inspection plan used for replay.
#[derive(Debug)]
pub struct JournalRecovery {
    source: File,
    source_identity: FileIdentity,
    parts: Vec<PartRef>,
    summary: JournalRecoverySummary,
}

/// Counts successfully replayed through the normal journal writer contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JournalReplaySummary {
    /// Complete frames appended to the fresh journal.
    pub frames: usize,
    /// Catalog row declarations in replayed parts.
    pub rows: u64,
    /// Replayed PGM part-body bytes.
    pub part_bytes: u64,
}

/// Failure to inspect stable evidence or replay verified parts.
#[derive(Debug)]
pub enum JournalRecoveryError {
    /// The existing journal configuration is outside version-1 hard bounds.
    Config(JournalError),
    /// Evidence descriptor I/O failed.
    Io(std::io::Error),
    /// The bounded recovery scanner failed.
    Scan(RecoveryScanError),
    /// Evidence identity changed during inspection or replay.
    SourceChanged,
    /// The fresh destination already contains a frame or segment identity.
    DestinationNotFresh,
    /// Verified frames exist but no supported identity is available.
    MissingTrustedSegmentId,
    /// A previously verified part no longer validates when reread.
    VerifiedPartChanged(PartError),
    /// Bounded part-buffer allocation failed.
    Allocation(std::collections::TryReserveError),
    /// Normal journal append rejected a replayed part.
    Append {
        /// Frames appended before this failure.
        recovered_frames: usize,
        /// Typed journal failure.
        source: JournalError,
    },
    /// Checked recovery accounting overflowed.
    AccountingOverflow {
        /// Counter that overflowed.
        quantity: &'static str,
    },
}

impl fmt::Display for JournalRecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "invalid journal recovery configuration: {error}"),
            Self::Io(error) => write!(f, "journal recovery I/O: {error}"),
            Self::Scan(error) => write!(f, "journal recovery scan: {error}"),
            Self::SourceChanged => f.write_str("journal recovery evidence identity changed"),
            Self::DestinationNotFresh => {
                f.write_str("journal recovery destination is not a fresh empty generation")
            }
            Self::MissingTrustedSegmentId => {
                f.write_str("verified journal frames have no trusted active segment identity")
            }
            Self::VerifiedPartChanged(error) => {
                write!(f, "verified journal part changed before replay: {error}")
            }
            Self::Allocation(error) => write!(f, "journal recovery buffer allocation: {error}"),
            Self::Append {
                recovered_frames,
                source,
            } => write!(
                f,
                "journal recovery append failed after {recovered_frames} frame(s): {source}"
            ),
            Self::AccountingOverflow { quantity } => {
                write!(f, "journal recovery {quantity} accounting overflow")
            }
        }
    }
}

impl Error for JournalRecoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::VerifiedPartChanged(error) => Some(error),
            Self::Allocation(error) => Some(error),
            Self::Append { source, .. } => Some(source),
            Self::SourceChanged
            | Self::DestinationNotFresh
            | Self::MissingTrustedSegmentId
            | Self::AccountingOverflow { .. } => None,
        }
    }
}

impl From<std::io::Error> for JournalRecoveryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<RecoveryScanError> for JournalRecoveryError {
    fn from(error: RecoveryScanError) -> Self {
        Self::Scan(error)
    }
}

impl JournalRecovery {
    /// Inspects an already-rotated evidence descriptor without mutating it.
    ///
    /// Only a complete valid Active v1 header supplies a segment identity. The
    /// scanner then examines the physical bytes after that header irrespective
    /// of its declared body length.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, I/O, scan, or source-instability error.
    #[expect(
        clippy::too_many_lines,
        reason = "one bounded inspection classifies the root header and physical frame scan atomically"
    )]
    pub fn inspect(source: &File, config: JournalConfig) -> Result<Self, JournalRecoveryError> {
        validate_config(config).map_err(JournalRecoveryError::Config)?;
        let source = source.try_clone()?;
        let source_identity = FileIdentity::from_file(&source)?;
        let evidence_bytes = source_identity.len;

        if evidence_bytes == 0 {
            return Self::without_scan(
                source,
                source_identity,
                JournalRecoveryReason::ZeroLength,
                0,
            );
        }
        if evidence_bytes < JOURNAL_HEADER_LEN as u64 {
            return Self::without_scan(
                source,
                source_identity,
                JournalRecoveryReason::TornHeader,
                evidence_bytes,
            );
        }

        let mut encoded = [0_u8; JOURNAL_HEADER_LEN];
        source.read_exact_at(&mut encoded, 0)?;
        let header = match JournalHeader::decode(encoded) {
            Ok(header) => header,
            Err(
                JournalHeaderError::UnsupportedMagic { .. }
                | JournalHeaderError::UnsupportedVersion { .. },
            ) => {
                return Self::without_scan(
                    source,
                    source_identity,
                    JournalRecoveryReason::UnsupportedHeader,
                    evidence_bytes,
                );
            }
            Err(_) => {
                return Self::without_scan(
                    source,
                    source_identity,
                    JournalRecoveryReason::InvalidHeader,
                    evidence_bytes,
                );
            }
        };
        let physical_body_bytes = evidence_bytes - JOURNAL_HEADER_LEN as u64;

        match header.state {
            JournalState::Empty => {
                let reason = if header.body_len != physical_body_bytes {
                    JournalRecoveryReason::BodyLengthMismatch
                } else if physical_body_bytes == 0 {
                    JournalRecoveryReason::Clean
                } else {
                    JournalRecoveryReason::EmptyWithTrailingBytes
                };
                Self::with_header_without_scan(
                    source,
                    source_identity,
                    reason,
                    header.body_len,
                    physical_body_bytes,
                )
            }
            JournalState::Active { segment_id } => {
                let Ok(segment_id) = SegmentId::new(segment_id) else {
                    return Self::with_header_without_scan(
                        source,
                        source_identity,
                        JournalRecoveryReason::InvalidSegmentId,
                        header.body_len,
                        physical_body_bytes,
                    );
                };
                let limits = recovery_scan_limits(config);
                let scan = scan_journal_streaming_recovery_from(
                    &source,
                    JOURNAL_HEADER_LEN as u64,
                    limits,
                )?;
                ensure_identity(&source, source_identity)?;
                let body_length_mismatch = header.body_len != physical_body_bytes;
                let reason = if scan.stop != RecoveryScanStop::EndOfSource {
                    JournalRecoveryReason::AdmissionLimit
                } else if body_length_mismatch {
                    JournalRecoveryReason::BodyLengthMismatch
                } else if !scan.damages.is_empty() {
                    JournalRecoveryReason::FrameDamage
                } else if scan.parts.is_empty() {
                    JournalRecoveryReason::ActiveWithoutFrames
                } else {
                    JournalRecoveryReason::Clean
                };
                let summary = JournalRecoverySummary {
                    reason,
                    trusted_segment_id: Some(segment_id),
                    evidence_bytes,
                    recorded_body_bytes: Some(header.body_len),
                    physical_body_bytes,
                    body_length_mismatch,
                    verified_frames: scan.parts.len(),
                    verified_rows: scan.recovered_rows,
                    verified_part_bytes: scan.recovered_part_bytes,
                    discarded_bytes: scan.discarded_bytes,
                    discarded_suffix_bytes: scan.discarded_suffix_bytes,
                    damages: scan.damages,
                    scan_stop: Some(scan.stop),
                };
                Ok(Self {
                    source,
                    source_identity,
                    parts: scan.parts,
                    summary,
                })
            }
        }
    }

    /// Returns the immutable, payload-free inspection summary.
    #[must_use]
    pub const fn summary(&self) -> &JournalRecoverySummary {
        &self.summary
    }

    /// Replays verified evidence through [`Journal::append`].
    ///
    /// The destination must be a fresh empty generation. Every part is reread
    /// and fully validated, with evidence identity checked around the read.
    /// Sealing remains a separate call to the normal `seal` API.
    ///
    /// # Errors
    ///
    /// Returns a typed source-instability, validation, allocation, destination,
    /// or normal journal-append failure.
    pub fn replay_into(
        &self,
        destination: &mut Journal,
    ) -> Result<JournalReplaySummary, JournalRecoveryError> {
        if !destination.is_empty() || destination.segment_id().is_some() {
            return Err(JournalRecoveryError::DestinationNotFresh);
        }
        if self.parts.is_empty() {
            return Ok(JournalReplaySummary::default());
        }
        let Some(segment_id) = self.summary.trusted_segment_id else {
            return Err(JournalRecoveryError::MissingTrustedSegmentId);
        };
        ensure_identity(&self.source, self.source_identity)?;

        let mut recovered = JournalReplaySummary::default();
        for part in &self.parts {
            ensure_identity(&self.source, self.source_identity)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(part.len)
                .map_err(JournalRecoveryError::Allocation)?;
            bytes.resize(part.len, 0);
            self.source.read_exact_at(&mut bytes, part.offset as u64)?;
            ensure_identity(&self.source, self.source_identity)?;
            let catalog =
                validate_part(&bytes).map_err(JournalRecoveryError::VerifiedPartChanged)?;
            let rows = catalog
                .entries
                .iter()
                .try_fold(0_u64, |rows, entry| rows.checked_add(u64::from(entry.rows)))
                .ok_or(JournalRecoveryError::AccountingOverflow { quantity: "row" })?;
            destination.append(segment_id, &bytes).map_err(|source| {
                JournalRecoveryError::Append {
                    recovered_frames: recovered.frames,
                    source,
                }
            })?;
            recovered.frames = recovered.frames.checked_add(1).ok_or(
                JournalRecoveryError::AccountingOverflow {
                    quantity: "frame count",
                },
            )?;
            recovered.rows = recovered
                .rows
                .checked_add(rows)
                .ok_or(JournalRecoveryError::AccountingOverflow { quantity: "row" })?;
            recovered.part_bytes = recovered
                .part_bytes
                .checked_add(u64::try_from(bytes.len()).map_err(|_overflow| {
                    JournalRecoveryError::AccountingOverflow {
                        quantity: "part byte",
                    }
                })?)
                .ok_or(JournalRecoveryError::AccountingOverflow {
                    quantity: "part byte",
                })?;
        }
        ensure_identity(&self.source, self.source_identity)?;
        Ok(recovered)
    }

    fn without_scan(
        source: File,
        source_identity: FileIdentity,
        reason: JournalRecoveryReason,
        discarded_bytes: u64,
    ) -> Result<Self, JournalRecoveryError> {
        ensure_identity(&source, source_identity)?;
        Ok(Self {
            source,
            source_identity,
            parts: Vec::new(),
            summary: JournalRecoverySummary {
                reason,
                trusted_segment_id: None,
                evidence_bytes: source_identity.len,
                recorded_body_bytes: None,
                physical_body_bytes: 0,
                body_length_mismatch: false,
                verified_frames: 0,
                verified_rows: 0,
                verified_part_bytes: 0,
                discarded_bytes,
                discarded_suffix_bytes: discarded_bytes,
                damages: Vec::new(),
                scan_stop: None,
            },
        })
    }

    fn with_header_without_scan(
        source: File,
        source_identity: FileIdentity,
        reason: JournalRecoveryReason,
        recorded_body_bytes: u64,
        physical_body_bytes: u64,
    ) -> Result<Self, JournalRecoveryError> {
        ensure_identity(&source, source_identity)?;
        Ok(Self {
            source,
            source_identity,
            parts: Vec::new(),
            summary: JournalRecoverySummary {
                reason,
                trusted_segment_id: None,
                evidence_bytes: source_identity.len,
                recorded_body_bytes: Some(recorded_body_bytes),
                physical_body_bytes,
                body_length_mismatch: recorded_body_bytes != physical_body_bytes,
                verified_frames: 0,
                verified_rows: 0,
                verified_part_bytes: 0,
                discarded_bytes: physical_body_bytes,
                discarded_suffix_bytes: physical_body_bytes,
                damages: Vec::new(),
                scan_stop: None,
            },
        })
    }
}

fn recovery_scan_limits(config: JournalConfig) -> RecoveryScanLimits {
    let admitted_file_bytes = u64::try_from(config.max_journal_len).unwrap_or(u64::MAX);
    RecoveryScanLimits {
        journal: config.limits,
        max_scan_bytes: admitted_file_bytes
            .saturating_sub(JOURNAL_HEADER_LEN as u64)
            .max(1),
        max_parts: config.max_parts,
        max_candidates: kronika_format::MAX_RECOVERY_CANDIDATES,
        max_candidate_bytes: admitted_file_bytes.max(1),
    }
}

fn ensure_identity(file: &File, expected: FileIdentity) -> Result<(), JournalRecoveryError> {
    if FileIdentity::from_file(file)? != expected {
        return Err(JournalRecoveryError::SourceChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Seek as _, SeekFrom, Write as _};

    use kronika_format::{FRAME_HEADER_LEN, FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_layout::{DataRoot, LayoutLimits, SegmentAddress};
    use kronika_registry::{StrId, Ts, instance_metadata::InstanceMetadata};

    use super::*;
    use crate::SectionBuffers;

    fn part(min_ts: i64) -> Vec<u8> {
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 1,
                body: b"data",
            }],
            PartMeta {
                min_ts,
                max_ts: min_ts,
                source_id: 0,
            },
        )
    }

    fn production_part(ts: i64) -> Vec<u8> {
        let mut buffers = SectionBuffers::new();
        buffers
            .push(InstanceMetadata {
                ts: Ts(ts),
                hostname: StrId(1),
                node_self_id: StrId(2),
                pg_version_num: 180_000,
                kernel_version: StrId(3),
                pg_system_identifier: Some(7),
                clock_ticks_per_sec: 100,
                page_size_bytes: 4_096,
                boot_id: StrId(4),
                btime: Ts(1),
            })
            .unwrap();
        buffers.flush(&[], 0).unwrap().unwrap()
    }

    fn frame(part: &[u8]) -> Vec<u8> {
        let mut bytes = FrameHeader {
            part_len: part.len() as u64,
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(part);
        bytes
    }

    fn active_bytes(segment_id: i64, recorded_body_len: u64, physical: &[u8]) -> Vec<u8> {
        let mut bytes = JournalHeader {
            state: JournalState::Active { segment_id },
            body_len: recorded_body_len,
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(physical);
        bytes
    }

    fn evidence(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file.as_file().sync_data().unwrap();
        file
    }

    fn fresh_journal(directory: &tempfile::TempDir) -> Journal {
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        Journal::open(&owner, JournalConfig::default()).unwrap()
    }

    #[test]
    fn truncated_header_has_no_trusted_identity_or_verified_frames() {
        let evidence = evidence(b"PGKJNL1");
        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        assert_eq!(recovery.summary().reason, JournalRecoveryReason::TornHeader);
        assert_eq!(recovery.summary().trusted_segment_id, None);
        assert_eq!(recovery.summary().verified_frames, 0);
        assert_eq!(recovery.summary().discarded_bytes, 7);
    }

    #[test]
    fn short_and_long_declared_body_lengths_scan_the_physical_tail() {
        let part = part(999);
        let physical = frame(&part);
        for recorded in [0, physical.len() as u64 + 100] {
            let evidence = evidence(&active_bytes(1_000, recorded, &physical));
            let recovery =
                JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
            assert_eq!(
                recovery.summary().reason,
                JournalRecoveryReason::BodyLengthMismatch
            );
            assert_eq!(
                recovery.summary().trusted_segment_id,
                SegmentId::new(1_000).ok()
            );
            assert_eq!(recovery.summary().verified_frames, 1);
            assert_eq!(recovery.summary().discarded_bytes, 0);
            assert!(recovery.summary().body_length_mismatch);
        }
    }

    #[test]
    fn torn_frame_recovers_only_the_complete_verified_prefix() {
        let part = part(999);
        let mut physical = frame(&part);
        let torn = frame(&part);
        physical.extend_from_slice(&torn[..torn.len() - 3]);
        let evidence = evidence(&active_bytes(1_000, physical.len() as u64, &physical));

        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        assert_eq!(
            recovery.summary().reason,
            JournalRecoveryReason::FrameDamage
        );
        assert_eq!(recovery.summary().verified_frames, 1);
        assert_eq!(
            recovery.summary().discarded_suffix_bytes,
            (torn.len() - 3) as u64
        );
    }

    #[test]
    fn a_later_frame_after_complete_corruption_is_verified() {
        let part = part(999);
        let one = frame(&part);
        let mut damaged = one.clone();
        damaged[FRAME_HEADER_LEN + 4] ^= 1;
        let mut physical = one.clone();
        physical.extend_from_slice(&damaged);
        physical.extend_from_slice(&one);
        let evidence = evidence(&active_bytes(1_000, physical.len() as u64, &physical));

        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        assert_eq!(recovery.summary().verified_frames, 2);
        assert_eq!(recovery.summary().discarded_bytes, one.len() as u64);
        assert_eq!(recovery.summary().discarded_suffix_bytes, 0);
    }

    #[test]
    fn replay_uses_header_identity_not_part_timestamps() {
        let part = part(999);
        let physical = frame(&part);
        let evidence = evidence(&active_bytes(1_000, physical.len() as u64, &physical));
        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut journal = fresh_journal(&directory);

        let replayed = recovery.replay_into(&mut journal).unwrap();
        assert_eq!(replayed.frames, 1);
        assert_eq!(replayed.rows, 1);
        assert_eq!(journal.segment_id(), SegmentId::new(1_000).ok());
        let replayed_part = journal.read_part(journal.parts()[0]).unwrap();
        assert_eq!(validate_part(&replayed_part).unwrap().min_ts, 999);
    }

    #[test]
    fn replay_rejects_changed_evidence_before_writing() {
        let part = part(999);
        let physical = frame(&part);
        let mut evidence = evidence(&active_bytes(1_000, physical.len() as u64, &physical));
        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        evidence.as_file_mut().seek(SeekFrom::End(0)).unwrap();
        evidence.as_file_mut().write_all(b"x").unwrap();
        evidence.as_file().sync_data().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut journal = fresh_journal(&directory);

        assert!(matches!(
            recovery.replay_into(&mut journal),
            Err(JournalRecoveryError::SourceChanged)
        ));
        assert!(journal.is_empty());
    }

    #[test]
    fn replayed_production_part_seals_through_the_normal_contract() {
        let part = production_part(999);
        let physical = frame(&part);
        let evidence = evidence(&active_bytes(1_000, physical.len() as u64, &physical));
        let recovery =
            JournalRecovery::inspect(evidence.as_file(), JournalConfig::default()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();

        recovery.replay_into(&mut journal).unwrap();
        let address = SegmentAddress::new(SegmentId::new(1_000).unwrap()).unwrap();
        let sealed = crate::seal(&journal, &owner, address).unwrap();

        assert_eq!(sealed.sections, 1);
        assert_eq!((sealed.min_ts, sealed.max_ts), (999, 999));
        assert!(
            owner
                .root()
                .open_pgm(address)
                .unwrap()
                .metadata()
                .unwrap()
                .len()
                > 0
        );
    }
}
