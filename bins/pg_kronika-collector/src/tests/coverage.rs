use crate::coverage::{SourceCoverage, min_total_time};
use crate::plans_source::plan_source_coverage;
use crate::source_contracts::{user_indexes_type_id, user_tables_type_id};
use crate::statements_source::statements_type_id;
use kronika_source_pg::statements::StatementsVersion;

fn successful(source_total: u64, collected: usize) -> SourceCoverage {
    SourceCoverage::successful(source_total, collected)
}

#[test]
fn coverage_state_machine_keeps_exact_facts_and_error_priority() {
    let complete = successful(50, 50);
    assert_eq!(complete.read_state(), (0, 0));
    assert_eq!(complete.exact_total(), Some(50));

    let top_n = successful(100, 50);
    assert_eq!(top_n.read_state(), (1, 0));
    assert_eq!(top_n.exact_total(), Some(100));

    let mut permission = SourceCoverage::new_attempt();
    permission.record_permission_failure();
    assert_eq!(permission.read_state(), (2, 1));
    assert_eq!(permission.exact_total(), None);

    let mut restricted = successful(100, 50);
    restricted.record_permission_restriction();
    assert_eq!(restricted.read_state(), (2, 1));
    assert_eq!(restricted.exact_total(), Some(100));

    let mut timeout = permission;
    timeout.record_timeout();
    assert_eq!(timeout.read_state(), (3, 2));

    let mut other_failure = permission;
    other_failure.record_other_failure();
    assert_eq!(other_failure.read_state(), (3, 2));

    let overflow = successful(u64::from(u32::MAX) + 1, 1);
    assert_eq!(overflow.read_state(), (4, 2));
    assert_eq!(overflow.exact_total(), None);

    let mut collector_loss = successful(50, 50);
    collector_loss.record_collector_loss();
    assert_eq!(collector_loss.read_state(), (4, 2));
    assert_eq!(collector_loss.exact_total(), None);
}

#[test]
fn attempted_empty_source_is_distinct_from_not_attempted() {
    let empty = successful(0, 0);
    let marker = empty.snapshot_marker(123, 1_013_004);
    assert!(empty.attempted);
    assert_eq!(empty.exact_total(), Some(0));
    assert_eq!((marker.source_total, marker.collected), (0, 0));
    assert_eq!((marker.read_state, marker.visibility), (0, 0));

    let not_attempted = SourceCoverage::default();
    assert!(!not_attempted.attempted);
    assert_eq!(not_attempted.exact_total(), None);
}

#[test]
fn ossc_masked_rows_keep_exact_total_with_restricted_visibility() {
    let coverage = plan_source_coverage(100, 40, 10);
    assert_eq!(coverage.collected, 40);
    assert_eq!(coverage.exact_total(), Some(100));
    assert_eq!(coverage.read_state(), (2, 1));
}

#[test]
fn coverage_reason_ranks_timeouts_over_other_skips() {
    let top_n = SourceCoverage {
        total: 100,
        collected: 50,
        ..SourceCoverage::new_attempt()
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
        ..SourceCoverage::new_attempt()
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
    let inconsistent = SourceCoverage {
        total: 40,
        ..complete
    };
    assert!(inconsistent.truncated());
    assert_eq!(inconsistent.read_state(), (4, 2));
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
