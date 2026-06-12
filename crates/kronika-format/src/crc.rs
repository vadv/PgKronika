//! CRC32C checksums for PGM bytes.

use crc::{CRC_32_ISCSI, Crc};

/// Castagnoli CRC32C polynomial.
const CRC32C: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

/// Return the CRC32C checksum of `bytes`.
///
/// PGM uses CRC32C for section bodies, the end catalog, and `active.parts`
/// frame headers.
#[must_use]
pub const fn crc32c(bytes: &[u8]) -> u32 {
    CRC32C.checksum(bytes)
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    /// The canonical CRC32C check value: `crc32c(b"123456789")`.
    /// Catches accidentally swapping the polynomial (e.g. plain CRC32).
    #[test]
    fn known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn empty_input() {
        assert_eq!(crc32c(b""), 0);
    }
}
