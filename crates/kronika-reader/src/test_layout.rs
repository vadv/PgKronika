use std::path::{Path, PathBuf};

use kronika_format::{FrameHeader, JOURNAL_HEADER_LEN, JournalHeader, JournalState};
use kronika_layout::{FileKind, SegmentAddress, SegmentId};

const TEST_DAY_START_US: i64 = 1_721_865_600_000_000;
const TEST_DAY_SPAN_US: u64 = 86_400_000_000;

pub(crate) fn address(raw_id: i64) -> SegmentAddress {
    SegmentAddress::new(SegmentId::new(raw_id).expect("representable test segment id"))
        .expect("test segment address")
}

pub(crate) fn named_address(name: &str) -> SegmentAddress {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let offset = i64::try_from(hash % TEST_DAY_SPAN_US).expect("test-day offset fits i64");
    address(TEST_DAY_START_US + offset)
}

pub(crate) fn day_path(root: &Path, address: SegmentAddress) -> PathBuf {
    root.join(address.day.year_component())
        .join(address.day.month_component())
        .join(address.day.day_component())
}

pub(crate) fn file_path(root: &Path, address: SegmentAddress, kind: FileKind) -> PathBuf {
    let day = day_path(root, address);
    std::fs::create_dir_all(&day).expect("create test UTC day");
    let name = match kind {
        FileKind::Pgm => address.pgm_name(),
        FileKind::Ovf => address.ovf_name(),
    };
    day.join(name)
}

pub(crate) fn write_pgm(root: &Path, address: SegmentAddress, bytes: &[u8]) -> PathBuf {
    let path = file_path(root, address, FileKind::Pgm);
    std::fs::write(&path, bytes).expect("write test PGM");
    path
}

pub(crate) fn journal_frame(part: &[u8]) -> Vec<u8> {
    let mut bytes = FrameHeader {
        part_len: part.len() as u64,
    }
    .encode()
    .to_vec();
    bytes.extend_from_slice(part);
    bytes
}

pub(crate) fn journal_bytes(segment_id: SegmentId, parts: &[&[u8]]) -> Vec<u8> {
    let body = parts
        .iter()
        .flat_map(|part| journal_frame(part))
        .collect::<Vec<_>>();
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
    std::fs::write(&path, journal_bytes(segment_id, parts)).expect("write test journal");
    path
}

pub(crate) fn append_journal_part(path: &Path, part: &[u8]) -> Vec<u8> {
    let mut bytes = std::fs::read(path).expect("read test journal");
    let header_bytes: [u8; JOURNAL_HEADER_LEN] = bytes[..JOURNAL_HEADER_LEN]
        .try_into()
        .expect("complete test journal header");
    let header = JournalHeader::decode(header_bytes).expect("valid test journal header");
    let JournalState::Active { segment_id } = header.state else {
        panic!("cannot append a frame to an empty test journal");
    };
    bytes.extend_from_slice(&journal_frame(part));
    let body_len = bytes.len() - JOURNAL_HEADER_LEN;
    bytes[..JOURNAL_HEADER_LEN].copy_from_slice(
        &JournalHeader {
            state: JournalState::Active { segment_id },
            body_len: body_len as u64,
        }
        .encode(),
    );
    std::fs::write(path, &bytes).expect("append test journal part");
    bytes
}

pub(crate) fn write_empty_journal(root: &Path) -> PathBuf {
    let path = root.join("active.parts");
    std::fs::write(&path, JournalHeader::EMPTY.encode()).expect("write empty test journal");
    path
}
