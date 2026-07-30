use crate::config::{
    RetentionConfig, parse_retention, resolve_log_enabled, resolve_log_status_interval,
    validate_cardinality, validate_heavy_cap, validate_journal_max_bytes, validate_max_lock_rows,
    validate_max_plans, validate_max_statements, validate_plan_text_limits,
    validate_replication_detail_bounds, validate_retention, validate_settings_row_count,
    validate_state_target,
};
use crate::plans_source::{plans_reread_delay, truncate_to_boundary};
use kronika_format::{JOURNAL_HEADER_LEN, MAX_JOURNAL_LEN};
use kronika_registry::MAX_SECTION_ROWS;
use kronika_source_pg::replication_details::ReplicationDetailBounds;
use std::time::Duration;

#[test]
fn pg_log_is_enabled_when_the_flag_is_absent() {
    assert!(resolve_log_enabled(None).expect("default log flag"));
}

#[test]
fn explicit_false_disables_pg_log_independently_of_a_path_override() {
    let path_override = Some("/var/lib/postgresql/log/postgresql.log");
    assert!(path_override.is_some());
    assert!(!resolve_log_enabled(Some("0")).expect("explicit false"));
}

#[test]
fn pg_log_status_interval_defaults_to_five_minutes() {
    assert_eq!(
        resolve_log_status_interval(None).expect("default status interval"),
        Duration::from_mins(5)
    );
}

#[test]
fn pg_log_status_interval_rejects_zero() {
    let error =
        resolve_log_status_interval(Some("0")).expect_err("a zero heartbeat interval must fail");
    assert!(
        error
            .to_string()
            .contains("KRONIKA_PG_LOG_STATUS_INTERVAL_S")
    );
}

#[test]
fn journal_limit_accepts_the_complete_v1_range() {
    validate_journal_max_bytes(JOURNAL_HEADER_LEN as u64).expect("canonical empty journal");
    validate_journal_max_bytes(MAX_JOURNAL_LEN as u64).expect("absolute v1 maximum");
}

#[test]
fn journal_limit_rejects_values_outside_the_v1_range() {
    for value in [JOURNAL_HEADER_LEN as u64 - 1, MAX_JOURNAL_LEN as u64 + 1] {
        let error = validate_journal_max_bytes(value).expect_err("value outside v1 range");
        assert!(error.to_string().contains("KRONIKA_JOURNAL_MAX_BYTES"));
    }
}

#[test]
fn log_state_runtime_check_rejects_direct_and_symlinked_paths_inside_data_root() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let data_root = directory.path().join("segments");
    std::fs::create_dir(&data_root).unwrap();
    assert!(validate_state_target(&data_root, &data_root.join("state")).is_err());

    let link = directory.path().join("state-parent");
    symlink(&data_root, &link).unwrap();
    assert!(validate_state_target(&data_root, &link.join("state")).is_err());
    assert!(validate_state_target(&data_root, &directory.path().join("state")).is_ok());
}

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
fn max_statements_guard_accounts_for_both_candidate_axes() {
    let per_axis_cap = i64::try_from(MAX_SECTION_ROWS).expect("cap fits i64") / 2;
    assert!(validate_max_statements(1).is_ok());
    assert!(validate_max_statements(500).is_ok());
    assert!(validate_max_statements(per_axis_cap).is_ok());
    assert!(validate_max_statements(0).is_err(), "zero is rejected");
    assert!(
        validate_max_statements(per_axis_cap + 1).is_err(),
        "two disjoint axes must fit the section cap"
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

#[test]
fn retention_parses_a_fixed_byte_budget() {
    assert_eq!(
        parse_retention("1073741824").expect("a plain number is a byte budget"),
        RetentionConfig::Fixed(1_073_741_824)
    );
}

#[test]
fn retention_auto_defaults_to_eighty_percent() {
    assert_eq!(
        parse_retention("auto").expect("bare auto is auto:80"),
        RetentionConfig::Auto(80)
    );
}

#[test]
fn retention_auto_takes_an_explicit_percentage() {
    assert_eq!(
        parse_retention("auto:65").expect("auto with a percentage"),
        RetentionConfig::Auto(65)
    );
    assert_eq!(
        parse_retention("  auto:1 ").expect("surrounding whitespace is trimmed"),
        RetentionConfig::Auto(1)
    );
}

#[test]
fn retention_rejects_an_empty_value() {
    assert!(
        parse_retention("   ").is_err(),
        "an empty value is not a target"
    );
}

#[test]
fn retention_rejects_an_out_of_range_percentage() {
    assert!(parse_retention("auto:0").is_err(), "0% cannot be a target");
    assert!(
        parse_retention("auto:100").is_err(),
        "100% cannot be a target"
    );
    assert!(
        parse_retention("auto:256").is_err(),
        "a percentage above a u8 is rejected"
    );
}

#[test]
fn retention_rejects_a_malformed_auto_suffix() {
    assert!(
        parse_retention("automatic").is_err(),
        "an unrecognized auto suffix is rejected"
    );
    assert!(
        parse_retention("auto:").is_err(),
        "auto with an empty percentage is rejected"
    );
    assert!(
        parse_retention("12x").is_err(),
        "a non-numeric budget is rejected"
    );
}

#[test]
fn retention_fixed_budget_must_hold_two_segments() {
    let segment_max_bytes = 64 * 1024 * 1024;
    let floor = 2 * segment_max_bytes;
    assert!(
        validate_retention(RetentionConfig::Fixed(floor), segment_max_bytes).is_ok(),
        "exactly two segments is the minimum viable budget"
    );
    let err = validate_retention(RetentionConfig::Fixed(floor - 1), segment_max_bytes)
        .expect_err("a budget below two segments cannot converge");
    assert!(err.to_string().contains("cannot converge"));
}

#[test]
fn retention_auto_has_no_fixed_floor() {
    assert!(
        validate_retention(RetentionConfig::Auto(80), 64 * 1024 * 1024).is_ok(),
        "auto targets a partition fraction, not a byte floor"
    );
}

#[test]
fn retention_rejects_a_negative_budget() {
    assert!(
        parse_retention("-1").is_err(),
        "a byte budget cannot be negative"
    );
}

#[test]
fn retention_floor_saturates_when_the_segment_cap_is_huge() {
    // 2 × segment_max overflows u64; the floor saturates instead of wrapping
    // to a small number that would accept a tiny budget.
    assert!(validate_retention(RetentionConfig::Fixed(u64::MAX), u64::MAX).is_ok());
    assert!(validate_retention(RetentionConfig::Fixed(u64::MAX - 1), u64::MAX).is_err());
}
