//! CRC32C (Castagnoli) checksum used throughout the PGM container.

use crc::{CRC_32_ISCSI, Crc};

/// `CRC_32_ISCSI` is the Castagnoli polynomial, i.e. CRC32C — the same
/// algorithm hardware-accelerated by SSE4.2 and used by S3 checksums.
const CRC32C: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);

/// CRC32C of `bytes`.
///
/// Every checksum in the PGM container — section bodies, the end catalog,
/// `active.parts` frame headers — is CRC32C (README.md, "CRC32C").
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
