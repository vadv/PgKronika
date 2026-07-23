//! Correctness and denial-of-service bounds for the fact-file codec.
//!
//! Every value here is a hard cap checked before an allocation, not a
//! performance claim. Exceeding a cap turns a segment `Uncacheable`; it never
//! truncates canonical facts, and it never lets an untrusted length drive an
//! allocation.

/// The `PGKOVF` admission bounds from the format contract.
///
/// The fields mirror the specification table one for one so a reviewer can
/// compare them against it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// Largest accepted fact-file length, bytes.
    pub fact_file_len: u64,
    /// Largest accepted block-directory entry count.
    pub directory_entries: u32,
    /// Largest accepted block-directory size, bytes.
    pub directory_bytes: u64,
    /// Largest accepted stored (on-disk) block size, bytes.
    pub stored_block_len: u64,
    /// Largest accepted decoded block size, bytes.
    pub decoded_block_len: u64,
    /// Largest accepted item count in one block.
    pub items_per_block: u64,
    /// Largest accepted SQLSTATE-key count in one aggregate.
    pub sqlstate_keys: u64,
    /// Largest accepted signal-key count in one aggregate.
    pub signal_keys: u64,
    /// Largest accepted coverage-span count in one segment.
    pub coverage_spans: u64,
    /// Largest accepted single retained normalized pattern, bytes.
    pub pattern_bytes: u64,
    /// Largest accepted decoded string-table size, bytes.
    pub string_table_bytes: u64,
}

/// The version-1 admission bounds.
pub const LIMIT: Bounds = Bounds {
    fact_file_len: 512 * MIB,
    directory_entries: 4096,
    directory_bytes: 256 * KIB,
    stored_block_len: 64 * MIB,
    decoded_block_len: 128 * MIB,
    items_per_block: 1_048_576,
    sqlstate_keys: 65_536,
    signal_keys: 1_024,
    coverage_spans: 262_144,
    pattern_bytes: 64 * KIB,
    string_table_bytes: 64 * MIB,
};

const KIB: u64 = 1_024;
const MIB: u64 = 1_024 * 1_024;

#[cfg(test)]
mod tests {
    use super::LIMIT;

    #[test]
    fn published_bounds_match_the_format_contract() {
        assert_eq!(LIMIT.fact_file_len, 536_870_912);
        assert_eq!(LIMIT.directory_entries, 4_096);
        assert_eq!(LIMIT.directory_bytes, 262_144);
        assert_eq!(LIMIT.stored_block_len, 67_108_864);
        assert_eq!(LIMIT.decoded_block_len, 134_217_728);
        assert_eq!(LIMIT.items_per_block, 1_048_576);
        assert_eq!(LIMIT.sqlstate_keys, 65_536);
        assert_eq!(LIMIT.signal_keys, 1_024);
        assert_eq!(LIMIT.coverage_spans, 262_144);
        assert_eq!(LIMIT.pattern_bytes, 65_536);
        assert_eq!(LIMIT.string_table_bytes, 67_108_864);
    }
}
