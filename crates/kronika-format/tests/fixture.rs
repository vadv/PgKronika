//! Byte-exact fixture test.
//!
//! `fixtures/minimal.pgm` is a minimal PGM segment computed by hand from
//! `docs/segment-format.md` (a Python reference implementation, checked
//! against the canonical CRC32C test vector, produced the bytes). A
//! write-then-read roundtrip cannot catch the writer and the reader
//! drifting from the specification together; this fixture can.
//!
//! Layout of the 88-byte file:
//!
//! ```text
//!  0..4   magic "PGM1"
//!  4..8   section body 01 02 03 04 (opaque to the container)
//!  8..40  catalog entry: type_id 1_006_001, offset 4, len 4, rows 1
//! 40..80  catalog meta: ts 1_000_000..2_000_000, 1 entry, version 1
//! 80..88  tail index: catalog_len 72, magic "PGM1"
//! ```

use kronika_format::{Catalog, Entry, MAGIC, TAIL_INDEX_LEN, TailIndex, crc32c};
// Dev-dependencies of other test targets; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
use crc as _;
use proptest as _;

const SEGMENT: &[u8] = include_bytes!("fixtures/minimal.pgm");

#[test]
fn fixture_decodes_to_expected_catalog() {
    assert_eq!(&SEGMENT[..4], MAGIC, "file must start with the magic");

    let tail_bytes: [u8; TAIL_INDEX_LEN] = SEGMENT[SEGMENT.len() - TAIL_INDEX_LEN..]
        .try_into()
        .expect("fixed-size tail");
    let tail = TailIndex::decode(tail_bytes).expect("valid tail index");
    assert_eq!(tail.catalog_len, 72);

    let catalog_start = SEGMENT.len() - TAIL_INDEX_LEN - tail.catalog_len as usize;
    let catalog = Catalog::decode(&SEGMENT[catalog_start..SEGMENT.len() - TAIL_INDEX_LEN])
        .expect("valid catalog");

    let body = &SEGMENT[4..8];
    assert_eq!(
        catalog,
        Catalog {
            entries: vec![Entry {
                type_id: 1_006_001,
                flags: 0,
                offset: 4,
                len: 4,
                rows: 1,
                crc32c: crc32c(body),
            }],
            min_ts: 1_000_000,
            max_ts: 2_000_000,
            source_id: 0,
            format_version: 1,
        }
    );
}

#[test]
fn encode_reproduces_fixture_bytes_exactly() {
    let catalog_start = 8;
    let tail_bytes: [u8; TAIL_INDEX_LEN] = SEGMENT[SEGMENT.len() - TAIL_INDEX_LEN..]
        .try_into()
        .expect("fixed-size tail");
    TailIndex::decode(tail_bytes).expect("valid tail index");

    let catalog = Catalog::decode(&SEGMENT[catalog_start..SEGMENT.len() - TAIL_INDEX_LEN])
        .expect("valid catalog");

    // encode() returns catalog + tail index, i.e. everything after the
    // last section body — byte-identical to the fixture.
    assert_eq!(catalog.encode(), &SEGMENT[catalog_start..]);
}
