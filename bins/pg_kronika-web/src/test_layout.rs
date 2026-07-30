use std::path::{Path, PathBuf};

use kronika_format::{FrameHeader, JournalHeader, JournalState};
use kronika_layout::{FileKind, SegmentAddress, SegmentId};

const TEST_DAY_START_US: i64 = 1_721_865_600_000_000;
const TEST_DAY_SPAN_US: u64 = 86_400_000_000;

pub(crate) fn address(raw_id: i64) -> SegmentAddress {
    SegmentAddress::new(SegmentId::new(raw_id).expect("representable fixture segment id"))
        .expect("fixture segment address")
}

pub(crate) fn named_address(name: &str) -> SegmentAddress {
    let stem = name.strip_suffix(".pgm").unwrap_or(name);
    if let Ok(raw_id) = stem.parse::<i64>()
        && let Ok(segment_id) = SegmentId::new(raw_id)
    {
        return SegmentAddress::new(segment_id).expect("numeric fixture address");
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let offset = i64::try_from(hash % TEST_DAY_SPAN_US).expect("test-day offset fits i64");
    address(TEST_DAY_START_US + offset)
}

pub(crate) fn file_path(root: &Path, address: SegmentAddress, kind: FileKind) -> PathBuf {
    let day = root
        .join(address.day.year_component())
        .join(address.day.month_component())
        .join(address.day.day_component());
    std::fs::create_dir_all(&day).expect("create fixture UTC day");
    day.join(match kind {
        FileKind::Pgm => address.pgm_name(),
        FileKind::Ovf => address.ovf_name(),
    })
}

pub(crate) fn named_pgm_path(root: &Path, name: &str) -> PathBuf {
    file_path(root, named_address(name), FileKind::Pgm)
}

pub(crate) fn write_named_pgm(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = named_pgm_path(root, name);
    std::fs::write(&path, bytes).expect("write fixture PGM");
    path
}

#[allow(
    dead_code,
    reason = "used by the feature-gated structural qualification test"
)]
pub(crate) fn write_segment_pgm(root: &Path, segment_id: i64, bytes: &[u8]) -> PathBuf {
    let path = file_path(root, address(segment_id), FileKind::Pgm);
    std::fs::write(&path, bytes).expect("write fixture PGM");
    path
}

pub(crate) fn journal_bytes(segment_id: SegmentId, parts: &[&[u8]]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(
            &FrameHeader {
                part_len: part.len() as u64,
            }
            .encode(),
        );
        body.extend_from_slice(part);
    }
    let mut bytes = JournalHeader {
        state: JournalState::Active {
            segment_id: segment_id.get(),
        },
        body_len: body.len() as u64,
    }
    .encode()
    .to_vec();
    bytes.extend_from_slice(&body);
    bytes
}

pub(crate) fn write_journal(root: &Path, segment_id: SegmentId, parts: &[&[u8]]) -> PathBuf {
    let path = root.join("active.parts");
    std::fs::write(&path, journal_bytes(segment_id, parts)).expect("write fixture journal");
    path
}

pub(crate) fn write_empty_journal(root: &Path) -> PathBuf {
    let path = root.join("active.parts");
    std::fs::write(&path, JournalHeader::EMPTY.encode()).expect("write empty fixture journal");
    path
}
