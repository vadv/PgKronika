use std::fs::File;
use std::path::Path;

use kronika_format::{
    FRAME_HEADER_LEN, FrameHeader, JOURNAL_HEADER_LEN, JournalHeader, JournalHeaderError,
    JournalState, PartRef, ReadAt, RecoveryDamageReason, RecoveryDamageRegion, RecoveryScanLimits,
    RecoveryScanReport, RecoveryScanStop, scan_journal_streaming_recovery_from,
};
use kronika_layout::{FileIdentity, SegmentId};
use kronika_reader::PgmUnit;

use crate::model::{
    JournalDamageOutput, JournalFrameOutput, JournalHeaderOutput, JournalOutput, RecoverableOutput,
};
use crate::pgm::{checked_add, inspect_unit, timestamp_range};
use crate::{DumpError, Options};

pub(crate) fn inspect_file(
    file: &File,
    path: &Path,
    options: Options,
) -> Result<JournalOutput, DumpError> {
    let source_identity =
        FileIdentity::from_file(file).map_err(|error| DumpError::input("stat journal", error))?;
    let physical_bytes = file
        .byte_len()
        .map_err(|error| DumpError::input("stat journal", error))?;
    let header = inspect_header(file, physical_bytes)?;
    let scan = if physical_bytes >= JOURNAL_HEADER_LEN as u64 {
        Some(
            scan_journal_streaming_recovery_from(
                file,
                JOURNAL_HEADER_LEN as u64,
                RecoveryScanLimits::default(),
            )
            .map_err(|error| DumpError::input("scan journal", error))?,
        )
    } else {
        None
    };

    let mut frames = Vec::new();
    let mut recoverable_windows = 0_u64;
    let mut first_us = None;
    let mut last_us = None;
    if let Some(scan) = &scan {
        for part in &scan.parts {
            let bytes = read_part(file, *part)?;
            let unit = PgmUnit::open(bytes)
                .map_err(|error| DumpError::input("open recovered journal part", error))?;
            let catalog = unit.catalog();
            let windows = u64::from(catalog.window_count.max(1));
            recoverable_windows =
                checked_add(recoverable_windows, windows, "recoverable window count")?;
            let (part_first, part_last) = timestamp_range(catalog.min_ts, catalog.max_ts);
            if let Some(value) = part_first {
                first_us = Some(first_us.map_or(value, |current: i64| current.min(value)));
            }
            if let Some(value) = part_last {
                last_us = Some(last_us.map_or(value, |current: i64| current.max(value)));
            }
            let detail = if options.rows {
                Some(inspect_unit(&unit, part.len as u64, options)?)
            } else {
                None
            };
            let frame_offset = part
                .offset
                .checked_sub(FRAME_HEADER_LEN)
                .ok_or_else(|| DumpError::message("journal part precedes its frame header"))?;
            frames.push(JournalFrameOutput {
                offset: u64::try_from(frame_offset)
                    .map_err(|_error| DumpError::message("journal frame offset overflow"))?,
                part_bytes: u64::try_from(part.len)
                    .map_err(|_error| DumpError::message("journal part length overflow"))?,
                windows,
                crc_ok: true,
                dictionary: detail.as_ref().map(|detail| detail.dictionary.clone()),
                sections: detail.map(|detail| detail.sections),
            });
        }
    }

    let valid_prefix_bytes = scan.as_ref().map_or(0, |scan| {
        valid_prefix(header.valid, JOURNAL_HEADER_LEN as u64, &scan.parts)
    });
    let damage = damage_output(file, physical_bytes, &header, scan.as_ref())?;
    let recovered_frames = u64::try_from(frames.len())
        .map_err(|_error| DumpError::message("journal frame count overflow"))?;
    let output = JournalOutput {
        kind: "journal",
        path: path.display().to_string(),
        header: header.output,
        physical_bytes,
        frames,
        valid_prefix_bytes,
        damage,
        recoverable: RecoverableOutput {
            frames: recovered_frames,
            windows: recoverable_windows,
            first_us,
            last_us,
        },
    };
    verify_identity(file, source_identity)?;
    Ok(output)
}

struct HeaderInspection {
    output: JournalHeaderOutput,
    decoded: Option<JournalHeader>,
    valid: bool,
    damage: Option<JournalDamageOutput>,
}

fn inspect_header(file: &File, physical_bytes: u64) -> Result<HeaderInspection, DumpError> {
    if physical_bytes == 0 {
        return Ok(HeaderInspection {
            output: JournalHeaderOutput {
                state: "invalid",
                segment_id: None,
                recorded_body_len: None,
                error: Some("zero_length"),
            },
            decoded: None,
            valid: false,
            damage: Some(JournalDamageOutput {
                offset: 0,
                kind: "zero_length",
                detail: "journal file is empty".to_owned(),
            }),
        });
    }
    if physical_bytes < JOURNAL_HEADER_LEN as u64 {
        return Ok(HeaderInspection {
            output: JournalHeaderOutput {
                state: "invalid",
                segment_id: None,
                recorded_body_len: None,
                error: Some("torn_header"),
            },
            decoded: None,
            valid: false,
            damage: Some(JournalDamageOutput {
                offset: 0,
                kind: "torn_header",
                detail: format!(
                    "journal header requires {JOURNAL_HEADER_LEN} bytes, {physical_bytes} remain"
                ),
            }),
        });
    }

    let mut bytes = [0_u8; JOURNAL_HEADER_LEN];
    file.read_exact_at(&mut bytes, 0)
        .map_err(|error| DumpError::input("read journal header", error))?;
    match JournalHeader::decode(bytes) {
        Ok(decoded) => {
            let (state, segment_id) = match decoded.state {
                JournalState::Empty => ("empty", None),
                JournalState::Active { segment_id } => ("active", Some(segment_id)),
            };
            if let Some(segment_id) = segment_id
                && SegmentId::new(segment_id).is_err()
            {
                return Ok(HeaderInspection {
                    output: JournalHeaderOutput {
                        state: "invalid",
                        segment_id: Some(segment_id),
                        recorded_body_len: Some(decoded.body_len),
                        error: Some("invalid_segment_id"),
                    },
                    decoded: Some(decoded),
                    valid: false,
                    damage: Some(JournalDamageOutput {
                        offset: 16,
                        kind: "invalid_segment_id",
                        detail: format!(
                            "journal segment identity {segment_id} is outside the supported UTC range"
                        ),
                    }),
                });
            }
            Ok(HeaderInspection {
                output: JournalHeaderOutput {
                    state,
                    segment_id,
                    recorded_body_len: Some(decoded.body_len),
                    error: None,
                },
                decoded: Some(decoded),
                valid: true,
                damage: None,
            })
        }
        Err(error) => {
            let (code, detail) = header_error(error);
            Ok(HeaderInspection {
                output: JournalHeaderOutput {
                    state: "invalid",
                    segment_id: None,
                    recorded_body_len: None,
                    error: Some(code),
                },
                decoded: None,
                valid: false,
                damage: Some(JournalDamageOutput {
                    offset: 0,
                    kind: "invalid_header",
                    detail: detail.to_owned(),
                }),
            })
        }
    }
}

const fn header_error(error: JournalHeaderError) -> (&'static str, &'static str) {
    match error {
        JournalHeaderError::UnsupportedMagic { .. } => {
            ("unsupported_magic", "journal header magic is unsupported")
        }
        JournalHeaderError::UnsupportedVersion { .. } => (
            "unsupported_version",
            "journal header version is unsupported",
        ),
        JournalHeaderError::BadChecksum { .. } => {
            ("bad_checksum", "journal header checksum does not match")
        }
        JournalHeaderError::InvalidState { .. } => {
            ("invalid_state", "journal header state is invalid")
        }
        JournalHeaderError::MissingIdentity => (
            "missing_identity",
            "active journal header has no segment identity",
        ),
        JournalHeaderError::UnexpectedIdentity => (
            "unexpected_identity",
            "empty journal header carries a segment identity",
        ),
        JournalHeaderError::NonZeroReserved => (
            "nonzero_reserved",
            "journal header reserved bytes are nonzero",
        ),
    }
}

fn read_part(file: &File, part: PartRef) -> Result<Vec<u8>, DumpError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(part.len)
        .map_err(|error| DumpError::input("allocate journal part", error))?;
    bytes.resize(part.len, 0);
    file.read_exact_at(
        &mut bytes,
        u64::try_from(part.offset)
            .map_err(|_error| DumpError::message("journal part offset overflow"))?,
    )
    .map_err(|error| DumpError::input("read recovered journal part", error))?;
    Ok(bytes)
}

fn valid_prefix(header_valid: bool, start: u64, parts: &[PartRef]) -> u64 {
    if !header_valid {
        return 0;
    }
    let mut valid = start;
    for part in parts {
        let Ok(part_offset) = u64::try_from(part.offset) else {
            break;
        };
        let Some(frame_at) = part_offset.checked_sub(FRAME_HEADER_LEN as u64) else {
            break;
        };
        if frame_at != valid {
            break;
        }
        let Ok(part_len) = u64::try_from(part.len) else {
            break;
        };
        let Some(end) = part_offset.checked_add(part_len) else {
            break;
        };
        valid = end;
    }
    valid
}

fn damage_output(
    file: &File,
    physical_bytes: u64,
    header: &HeaderInspection,
    scan: Option<&RecoveryScanReport>,
) -> Result<Option<JournalDamageOutput>, DumpError> {
    if let Some(damage) = &header.damage {
        return Ok(Some(JournalDamageOutput {
            offset: damage.offset,
            kind: damage.kind,
            detail: damage.detail.clone(),
        }));
    }
    if let Some(region) = scan.and_then(|scan| scan.damages.first()) {
        return recovery_damage(file, physical_bytes, *region).map(Some);
    }
    if let Some(scan) = scan
        && scan.stop != RecoveryScanStop::EndOfSource
    {
        return Ok(Some(JournalDamageOutput {
            offset: valid_prefix(header.valid, JOURNAL_HEADER_LEN as u64, &scan.parts),
            kind: "work_limit",
            detail: scan_stop_detail(scan.stop),
        }));
    }
    let Some(decoded) = header.decoded else {
        return Ok(None);
    };
    let physical_body = physical_bytes.saturating_sub(JOURNAL_HEADER_LEN as u64);
    if decoded.body_len != physical_body {
        return Ok(Some(JournalDamageOutput {
            offset: JOURNAL_HEADER_LEN as u64 + decoded.body_len.min(physical_body),
            kind: "body_length_mismatch",
            detail: format!(
                "header records {} body bytes, physical body has {physical_body}",
                decoded.body_len
            ),
        }));
    }
    let frame_count = scan.map_or(0, |scan| scan.parts.len());
    match decoded.state {
        JournalState::Empty if physical_body != 0 => Ok(Some(JournalDamageOutput {
            offset: JOURNAL_HEADER_LEN as u64,
            kind: "unexpected_frames",
            detail: format!("empty journal header is followed by {physical_body} bytes"),
        })),
        JournalState::Active { .. } if frame_count == 0 => Ok(Some(JournalDamageOutput {
            offset: JOURNAL_HEADER_LEN as u64,
            kind: "active_without_frames",
            detail: "active journal header has no complete frame".to_owned(),
        })),
        JournalState::Empty | JournalState::Active { .. } => Ok(None),
    }
}

fn recovery_damage(
    file: &File,
    physical_bytes: u64,
    region: RecoveryDamageRegion,
) -> Result<JournalDamageOutput, DumpError> {
    let (kind, detail) = match region.reason {
        RecoveryDamageReason::TornFrameHeader => (
            "torn_frame",
            format!(
                "frame header requires {FRAME_HEADER_LEN} bytes, {} remain",
                physical_bytes.saturating_sub(region.from)
            ),
        ),
        RecoveryDamageReason::TornFrameBody => {
            let detail = torn_body_detail(file, physical_bytes, region.from)?;
            ("torn_frame", detail)
        }
        RecoveryDamageReason::InvalidFrameHeader => (
            "invalid_frame",
            "frame magic or header checksum is invalid".to_owned(),
        ),
        RecoveryDamageReason::PartTooLarge => (
            "part_too_large",
            "frame body exceeds the journal part limit".to_owned(),
        ),
        RecoveryDamageReason::InvalidPart => (
            "invalid_part",
            "frame body is not a valid canonical PGM part".to_owned(),
        ),
        RecoveryDamageReason::WorkLimit => (
            "work_limit",
            "bounded recovery scan left this region unexamined".to_owned(),
        ),
    };
    Ok(JournalDamageOutput {
        offset: region.from,
        kind,
        detail,
    })
}

fn torn_body_detail(file: &File, physical_bytes: u64, at: u64) -> Result<String, DumpError> {
    let mut bytes = [0_u8; FRAME_HEADER_LEN];
    if at
        .checked_add(FRAME_HEADER_LEN as u64)
        .is_none_or(|end| end > physical_bytes)
    {
        return Ok("frame body is incomplete".to_owned());
    }
    file.read_exact_at(&mut bytes, at)
        .map_err(|error| DumpError::input("read torn frame header", error))?;
    let Ok(frame) = FrameHeader::decode(bytes) else {
        return Ok("frame body is incomplete".to_owned());
    };
    let remaining = physical_bytes.saturating_sub(at + FRAME_HEADER_LEN as u64);
    Ok(format!(
        "frame header declares {} bytes, {remaining} remain",
        frame.part_len
    ))
}

fn scan_stop_detail(stop: RecoveryScanStop) -> String {
    match stop {
        RecoveryScanStop::EndOfSource => String::new(),
        RecoveryScanStop::ScanByteLimit { .. } => {
            String::from("journal scan byte limit was reached")
        }
        RecoveryScanStop::PartLimit { .. } => {
            String::from("journal recovered-frame limit was reached")
        }
        RecoveryScanStop::CandidateLimit { .. } => {
            String::from("journal resynchronization candidate limit was reached")
        }
        RecoveryScanStop::CandidateByteLimit { .. } => {
            String::from("journal candidate-validation byte limit was reached")
        }
    }
}

fn verify_identity(file: &File, expected: FileIdentity) -> Result<(), DumpError> {
    let observed =
        FileIdentity::from_file(file).map_err(|error| DumpError::input("stat journal", error))?;
    if observed == expected {
        Ok(())
    } else {
        Err(DumpError::message(
            "journal changed while it was being inspected",
        ))
    }
}
