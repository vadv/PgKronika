use kronika_format::DictLimits;

/// Limits for interned activity strings.
///
/// Query text can dominate the dictionary. Long values spill to `dict.blobs`,
/// truncate after 64 KiB, and the dictionary is capped at 16 MiB.
pub(crate) fn activity_dict_limits() -> DictLimits {
    DictLimits::new(4096, 64 * 1024)
        .and_then(|limits| limits.with_max_total_bytes(16 * 1024 * 1024))
        .expect("static activity dictionary limits satisfy 0 < blob <= truncate <= total")
}

/// The `1_013` layout collected on this server major.
pub(crate) const fn user_tables_type_id(major: u32) -> u32 {
    match major {
        0..=12 => 1_013_001,
        13..=15 => 1_013_002,
        16..=17 => 1_013_003,
        _ => 1_013_004,
    }
}

/// The `1_014` layout collected on this server major.
pub(crate) const fn user_indexes_type_id(major: u32) -> u32 {
    if major >= 16 { 1_014_002 } else { 1_014_001 }
}
