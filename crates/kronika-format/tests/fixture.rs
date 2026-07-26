//! Byte-exact fixture test for the container framing.
//!
//! The fixture catches drift that a write-then-read roundtrip can miss,
//! because an encoder and decoder can share the same bug.
//! The bytes were generated independently from the documented layout and
//! checked against the canonical CRC32C test vector.
//!
//! Layout of the 88-byte current-format file:
//!
//! ```text
//!  0..4   magic "PGMC"
//!  4..8   section body 01 02 03 04 (opaque to the container)
//!  8..40  catalog entry: type_id 1_006_001, offset 4, len 4, rows 1
//! 40..80  catalog meta: ts 1_000_000..2_000_000, 1 entry, version 1
//! 80..88  tail index: catalog_len 72, magic "PGMC"
//! ```

use kronika_format::{Catalog, Entry, MAGIC, TAIL_INDEX_LEN, TailIndex, crc32c};
// Dependencies of other targets of this crate; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
use crc as _;
use proptest as _;
use sha2 as _;
use tempfile as _;
use xxhash_rust as _;

// Kept as a literal so this test remains independent of the encoder. The
// tracked `minimal.pgm` file is the deliberately obsolete pre-clean-break
// fixture exercised below.
const SEGMENT: &[u8] = &[
    0x50, 0x47, 0x4d, 0x43, 0x01, 0x02, 0x03, 0x04, 0xb1, 0x59, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0xf4, 0x8c, 0x30, 0x29, 0x40, 0x42, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x80, 0x84, 0x1e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x82, 0x90, 0x91, 0x37, 0x00, 0x00, 0x00, 0x00,
    0x48, 0x00, 0x00, 0x00, 0x50, 0x47, 0x4d, 0x43,
];
const OBSOLETE_SEGMENT: &[u8] = include_bytes!("fixtures/minimal.pgm");

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

    // encode() returns catalog + tail index: everything after the last
    // section body.
    assert_eq!(catalog.encode(), &SEGMENT[catalog_start..]);
}

#[test]
fn obsolete_fixture_is_rejected_without_a_legacy_decoder() {
    assert_eq!(&OBSOLETE_SEGMENT[..4], b"PGM1");
    let tail_bytes: [u8; TAIL_INDEX_LEN] =
        OBSOLETE_SEGMENT[OBSOLETE_SEGMENT.len() - TAIL_INDEX_LEN..]
            .try_into()
            .expect("fixed-size tail");
    assert!(TailIndex::decode(tail_bytes).is_err());
}
