use crate::coverage::{SourceCoverage, min_total_time};
use crate::pool_sources::{user_indexes_type_id, user_tables_type_id};
use crate::statements_source::statements_type_id;
use kronika_source_pg::statements::StatementsVersion;

#[test]
fn coverage_reason_ranks_timeouts_over_other_skips() {
    let top_n = SourceCoverage {
        total: 100,
        collected: 50,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    assert_eq!(top_n.reason(), 0);
    assert_eq!(
        SourceCoverage {
            other_skips: 2,
            ..top_n
        }
        .reason(),
        3
    );
    assert_eq!(
        SourceCoverage {
            unknown_total: true,
            ..top_n
        }
        .reason(),
        3
    );
    assert_eq!(
        SourceCoverage {
            permission_skips: 1,
            other_skips: 2,
            ..top_n
        }
        .reason(),
        2
    );
    assert_eq!(
        SourceCoverage {
            timeouts: 1,
            permission_skips: 1,
            other_skips: 2,
            ..top_n
        }
        .reason(),
        1
    );
}

#[test]
fn coverage_truncated_needs_missing_rows_or_skips() {
    let complete = SourceCoverage {
        total: 50,
        collected: 50,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    assert!(!complete.truncated());
    assert!(
        SourceCoverage {
            total: 51,
            ..complete
        }
        .truncated()
    );
    assert!(
        SourceCoverage {
            timeouts: 1,
            ..complete
        }
        .truncated()
    );
    assert!(
        SourceCoverage {
            unknown_total: true,
            ..complete
        }
        .truncated()
    );
    assert!(
        SourceCoverage {
            other_skips: 1,
            ..complete
        }
        .truncated()
    );
    assert!(
        !SourceCoverage {
            total: 40,
            ..complete
        }
        .truncated()
    );
}

#[test]
fn min_total_time_finds_the_boundary() {
    assert_eq!(min_total_time([3.0, 1.5, 2.0].into_iter()), Some(1.5));
    assert_eq!(min_total_time(std::iter::empty()), None);
}

#[test]
fn type_id_maps_cover_the_supported_majors() {
    assert_eq!(user_tables_type_id(12), 1_013_001);
    assert_eq!(user_tables_type_id(13), 1_013_002);
    assert_eq!(user_tables_type_id(16), 1_013_003);
    assert_eq!(user_tables_type_id(18), 1_013_004);
    assert_eq!(user_indexes_type_id(15), 1_014_001);
    assert_eq!(user_indexes_type_id(16), 1_014_002);
    assert_eq!(statements_type_id(StatementsVersion::V6), 1_002_006);
}
