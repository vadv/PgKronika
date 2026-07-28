//! Correctness and denial-of-service bounds for the fact-file codec.
//!
//! Admission checks these hard caps before allocating from untrusted lengths.
//! Encoders and decoders return a typed limit error instead of truncating data.

/// The `PGKOVF` admission bounds from the format contract.
///
/// Callers may provide tighter values. Values above [`LIMIT`] are rejected.
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
    /// Largest accepted sum of decoded block lengths in one file.
    pub decoded_file_bytes: u64,
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
    /// Largest accepted view count in one web summary.
    pub web_summary_views: u64,
    /// Largest accepted union timestamp count in one web summary.
    pub web_summary_timestamps: u64,
    /// Largest accepted metric count in one entity-series view.
    pub web_metrics_per_view: u64,
    /// Largest accepted retained series count for one metric.
    pub web_series_per_metric: u64,
    /// Largest accepted bucket count in one web-index grid.
    pub web_buckets: u64,
    /// Largest accepted typed entity identity, bytes.
    pub web_identity_bytes: u64,
    /// Largest accepted entity display label, bytes.
    pub web_label_bytes: u64,
    /// Largest accepted entity dictionary entry count in one view.
    pub web_dictionary_entries: u64,
    /// Largest accepted decoded `UiSummary` body, bytes.
    pub web_summary_decoded_bytes: u64,
    /// Largest accepted decoded `EntitySeries` body, bytes.
    pub web_series_decoded_bytes: u64,
    /// Largest accepted stored `EntitySeries` body, bytes.
    pub web_series_stored_bytes: u64,
}

impl Bounds {
    pub(crate) const fn is_within_absolute_limits(self) -> bool {
        self.fact_file_len <= LIMIT.fact_file_len
            && self.directory_entries <= LIMIT.directory_entries
            && self.directory_bytes <= LIMIT.directory_bytes
            && self.stored_block_len <= LIMIT.stored_block_len
            && self.decoded_block_len <= LIMIT.decoded_block_len
            && self.decoded_file_bytes <= LIMIT.decoded_file_bytes
            && self.items_per_block <= LIMIT.items_per_block
            && self.sqlstate_keys <= LIMIT.sqlstate_keys
            && self.signal_keys <= LIMIT.signal_keys
            && self.coverage_spans <= LIMIT.coverage_spans
            && self.pattern_bytes <= LIMIT.pattern_bytes
            && self.string_table_bytes <= LIMIT.string_table_bytes
            && self.web_summary_views <= LIMIT.web_summary_views
            && self.web_summary_timestamps <= LIMIT.web_summary_timestamps
            && self.web_metrics_per_view <= LIMIT.web_metrics_per_view
            && self.web_series_per_metric <= LIMIT.web_series_per_metric
            && self.web_buckets <= LIMIT.web_buckets
            && self.web_identity_bytes <= LIMIT.web_identity_bytes
            && self.web_label_bytes <= LIMIT.web_label_bytes
            && self.web_dictionary_entries <= LIMIT.web_dictionary_entries
            && self.web_summary_decoded_bytes <= LIMIT.web_summary_decoded_bytes
            && self.web_series_decoded_bytes <= LIMIT.web_series_decoded_bytes
            && self.web_series_stored_bytes <= LIMIT.web_series_stored_bytes
    }

    pub(crate) const fn admits_profile(self, admitted: Self) -> bool {
        self.is_within_absolute_limits()
            && admitted.is_within_absolute_limits()
            && self.fact_file_len >= admitted.fact_file_len
            && self.directory_entries >= admitted.directory_entries
            && self.directory_bytes >= admitted.directory_bytes
            && self.stored_block_len >= admitted.stored_block_len
            && self.decoded_block_len >= admitted.decoded_block_len
            && self.decoded_file_bytes >= admitted.decoded_file_bytes
            && self.items_per_block >= admitted.items_per_block
            && self.sqlstate_keys >= admitted.sqlstate_keys
            && self.signal_keys >= admitted.signal_keys
            && self.coverage_spans >= admitted.coverage_spans
            && self.pattern_bytes >= admitted.pattern_bytes
            && self.string_table_bytes >= admitted.string_table_bytes
            && self.web_summary_views >= admitted.web_summary_views
            && self.web_summary_timestamps >= admitted.web_summary_timestamps
            && self.web_metrics_per_view >= admitted.web_metrics_per_view
            && self.web_series_per_metric >= admitted.web_series_per_metric
            && self.web_buckets >= admitted.web_buckets
            && self.web_identity_bytes >= admitted.web_identity_bytes
            && self.web_label_bytes >= admitted.web_label_bytes
            && self.web_dictionary_entries >= admitted.web_dictionary_entries
            && self.web_summary_decoded_bytes >= admitted.web_summary_decoded_bytes
            && self.web_series_decoded_bytes >= admitted.web_series_decoded_bytes
            && self.web_series_stored_bytes >= admitted.web_series_stored_bytes
    }
}

/// The format admission bounds.
pub const LIMIT: Bounds = Bounds {
    fact_file_len: 512 * MIB,
    directory_entries: 4096,
    directory_bytes: 256 * KIB,
    stored_block_len: 64 * MIB,
    decoded_block_len: 128 * MIB,
    decoded_file_bytes: GIB,
    items_per_block: 1_048_576,
    sqlstate_keys: 65_536,
    signal_keys: 1_024,
    coverage_spans: 262_144,
    pattern_bytes: 64 * KIB,
    string_table_bytes: 64 * MIB,
    web_summary_views: 32,
    web_summary_timestamps: 4_096,
    web_metrics_per_view: 16,
    web_series_per_metric: 64,
    web_buckets: 256,
    web_identity_bytes: 256,
    web_label_bytes: 160,
    web_dictionary_entries: 1_024,
    web_summary_decoded_bytes: 64 * KIB,
    web_series_decoded_bytes: 256 * KIB,
    web_series_stored_bytes: 128 * KIB,
};

const KIB: u64 = 1_024;
const MIB: u64 = 1_024 * 1_024;
const GIB: u64 = 1_024 * MIB;

#[cfg(test)]
mod tests {
    use super::LIMIT;

    #[test]
    fn absolute_bounds_match_the_format_contract() {
        assert_eq!(LIMIT.fact_file_len, 536_870_912);
        assert_eq!(LIMIT.directory_entries, 4_096);
        assert_eq!(LIMIT.directory_bytes, 262_144);
        assert_eq!(LIMIT.stored_block_len, 67_108_864);
        assert_eq!(LIMIT.decoded_block_len, 134_217_728);
        assert_eq!(LIMIT.decoded_file_bytes, 1_073_741_824);
        assert_eq!(LIMIT.items_per_block, 1_048_576);
        assert_eq!(LIMIT.sqlstate_keys, 65_536);
        assert_eq!(LIMIT.signal_keys, 1_024);
        assert_eq!(LIMIT.coverage_spans, 262_144);
        assert_eq!(LIMIT.pattern_bytes, 65_536);
        assert_eq!(LIMIT.string_table_bytes, 67_108_864);
        assert_eq!(LIMIT.web_summary_views, 32);
        assert_eq!(LIMIT.web_summary_timestamps, 4_096);
        assert_eq!(LIMIT.web_metrics_per_view, 16);
        assert_eq!(LIMIT.web_series_per_metric, 64);
        assert_eq!(LIMIT.web_buckets, 256);
        assert_eq!(LIMIT.web_identity_bytes, 256);
        assert_eq!(LIMIT.web_label_bytes, 160);
        assert_eq!(LIMIT.web_dictionary_entries, 1_024);
        assert_eq!(LIMIT.web_summary_decoded_bytes, 65_536);
        assert_eq!(LIMIT.web_series_decoded_bytes, 262_144);
        assert_eq!(LIMIT.web_series_stored_bytes, 131_072);
    }
}
