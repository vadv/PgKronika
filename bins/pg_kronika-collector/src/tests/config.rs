use crate::config::{
    validate_cardinality, validate_heavy_cap, validate_max_lock_rows, validate_max_plans,
    validate_plan_text_limits, validate_replication_detail_bounds, validate_settings_row_count,
};
use crate::plans_source::{plans_reread_delay, truncate_to_boundary};
use kronika_registry::MAX_SECTION_ROWS;
use kronika_source_pg::replication_details::ReplicationDetailBounds;

#[test]
fn cardinality_validation_passes_at_defaults() {
    assert!(validate_cardinality(500, 500).is_ok());
}

#[test]
fn cardinality_validation_rejects_overflowing_max_indexes() {
    // 20 databases * 4 index axes * 820 = 65600 > 65536.
    let err = validate_cardinality(500, 820).expect_err("820 indexes must overflow");
    assert!(err.to_string().contains("KRONIKA_PG_MAX_INDEXES"));
}

#[test]
fn cardinality_validation_rejects_overflowing_max_tables() {
    // 20 databases * 6 table axes * 547 = 65640 > 65536.
    let err = validate_cardinality(547, 500).expect_err("547 tables must overflow");
    assert!(err.to_string().contains("KRONIKA_PG_MAX_TABLES"));
}

#[test]
fn heavy_cap_validation_rejects_zero() {
    let err = validate_heavy_cap(0).expect_err("a zero heavy cap must be rejected");
    assert!(err.to_string().contains("KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS"));
}

#[test]
fn heavy_cap_validation_accepts_positive() {
    assert!(validate_heavy_cap(60_000).is_ok());
}
#[test]
fn truncate_to_boundary_respects_utf8_and_short_inputs() {
    let mut short = "plan".to_owned();
    truncate_to_boundary(&mut short, 10);
    assert_eq!(short, "plan", "short inputs stay whole");

    let mut exact = "план".to_owned(); // 8 bytes, 4 chars
    truncate_to_boundary(&mut exact, 5);
    assert_eq!(exact, "пл", "the cut lands on a character boundary");
    assert!(exact.len() <= 5);

    let mut zero = "план".to_owned();
    truncate_to_boundary(&mut zero, 0);
    assert_eq!(zero, "", "a zero cap empties the text");
}

#[test]
fn plan_text_limits_guard_matches_dictionary_bounds() {
    assert!(validate_plan_text_limits(32_768, 8 * 1024 * 1024).is_ok());
    assert!(validate_plan_text_limits(64 * 1024, 0).is_ok());
    assert!(
        validate_plan_text_limits(0, 1).is_err(),
        "a zero per-text cap is rejected"
    );
    assert!(
        validate_plan_text_limits(64 * 1024 + 1, 1).is_err(),
        "a per-text cap past the 64 KiB dictionary truncation is rejected"
    );
    assert!(
        validate_plan_text_limits(1, 16 * 1024 * 1024 + 1).is_err(),
        "a budget past the 16 MiB dictionary cap is rejected"
    );
}

#[test]
fn max_plans_guard_accepts_range_and_rejects_extremes() {
    assert!(validate_max_plans(1).is_ok());
    assert!(validate_max_plans(500).is_ok());
    assert!(validate_max_plans(0).is_err(), "zero is rejected");
    let cap = i64::try_from(MAX_SECTION_ROWS).expect("cap fits i64");
    assert!(validate_max_plans(cap).is_ok());
    assert!(
        validate_max_plans(cap + 1).is_err(),
        "a value above MAX_SECTION_ROWS is rejected"
    );
}

#[test]
fn settings_row_guard_rejects_section_overflow() {
    assert!(validate_settings_row_count(MAX_SECTION_ROWS).is_ok());
    let err = validate_settings_row_count(MAX_SECTION_ROWS + 1)
        .expect_err("pg_settings must not exceed one section");
    assert!(err.to_string().contains("pg_settings"));
}

#[test]
fn plans_reread_delay_shortens_only_empty_reads() {
    use std::time::Duration;
    let interval = Duration::from_mins(5);
    assert_eq!(plans_reread_delay(false, interval), interval);
    assert_eq!(plans_reread_delay(true, interval), Duration::from_secs(30));
    let short = Duration::from_secs(10);
    assert_eq!(
        plans_reread_delay(true, short),
        short,
        "the retry delay never exceeds the interval"
    );
}
#[test]
fn max_lock_rows_within_section_cap() {
    assert!(1000 <= i64::try_from(MAX_SECTION_ROWS).unwrap());
}

#[test]
fn max_lock_rows_validation_rejects_overflow() {
    let cap = i64::try_from(MAX_SECTION_ROWS).unwrap();
    let err = validate_max_lock_rows(cap + 1).expect_err("value above cap must be rejected");
    assert!(err.to_string().contains("KRONIKA_PG_MAX_LOCK_ROWS"));
}

#[test]
fn max_lock_rows_validation_rejects_zero() {
    let err = validate_max_lock_rows(0).expect_err("zero disables the graph guard");
    assert!(err.to_string().contains("greater than 0"));
}

#[test]
fn replication_detail_bounds_accept_defaults() {
    let bounds = ReplicationDetailBounds {
        max_wal_senders: 10,
        max_replication_slots: 10,
    };
    assert!(validate_replication_detail_bounds(bounds).is_ok());
}

#[test]
fn replication_detail_bounds_reject_section_overflow() {
    let cap = i64::try_from(MAX_SECTION_ROWS).unwrap();
    let bounds = ReplicationDetailBounds {
        max_wal_senders: cap + 1,
        max_replication_slots: 10,
    };
    let err = validate_replication_detail_bounds(bounds)
        .expect_err("max_wal_senders above the section cap must be rejected");
    assert!(err.to_string().contains("max_wal_senders"));
}

#[test]
fn replication_detail_bounds_reject_negative_guc() {
    let bounds = ReplicationDetailBounds {
        max_wal_senders: 10,
        max_replication_slots: -1,
    };
    let err = validate_replication_detail_bounds(bounds)
        .expect_err("negative max_replication_slots must be rejected");
    assert!(err.to_string().contains("non-negative"));
}

#[test]
fn replication_detail_bounds_reject_dictionary_overflow() {
    let bounds = ReplicationDetailBounds {
        max_wal_senders: 60_000,
        max_replication_slots: 40_000,
    };
    let err = validate_replication_detail_bounds(bounds)
        .expect_err("combined replication labels must fit the dictionary cap");
    assert!(err.to_string().contains("dictionary bytes"));
}
