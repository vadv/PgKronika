use crate::coverage::query_failure_attempt;
use crate::statements_source::{
    CachedStatementsSource, MissingStatementsSource, StatementsCollection, StatementsSource,
    StatementsSourceCache,
};
use kronika_source_pg::statements::{StatementsRow, StatementsVersion};

fn statement_row(ts: i64) -> StatementsRow {
    StatementsRow {
        ts,
        queryid: Some(777),
        userid: 10,
        dbid: 5,
        toplevel: Some(true),
        datname: Some("appdb".to_owned()),
        usename: Some("alice".to_owned()),
        query: None,
        calls: 100,
        rows: 5_000,
        plans: Some(90),
        total_time: 1_234.5,
        total_plan_time: Some(12.5),
        min_time: 0.5,
        max_time: 40.0,
        mean_time: 12.3,
        stddev_time: 3.1,
        min_plan_time: Some(0.1),
        max_plan_time: Some(1.0),
        mean_plan_time: Some(0.2),
        stddev_plan_time: Some(0.05),
        shared_blks_hit: 90_000,
        shared_blks_read: 4_000,
        shared_blks_dirtied: 50,
        shared_blks_written: 30,
        local_blks_hit: 0,
        local_blks_read: 0,
        local_blks_dirtied: 0,
        local_blks_written: 0,
        temp_blks_read: 0,
        temp_blks_written: 0,
        blk_read_time: 12.5,
        blk_write_time: 3.0,
        local_blk_read_time: Some(1.0),
        local_blk_write_time: Some(0.5),
        temp_blk_read_time: Some(2.0),
        temp_blk_write_time: Some(1.5),
        wal_records: Some(42),
        wal_fpi: Some(3),
        wal_bytes: Some(8_192),
        wal_buffers_full: Some(1),
        jit_functions: Some(0),
        jit_generation_time: Some(0.0),
        jit_inlining_count: Some(0),
        jit_inlining_time: Some(0.0),
        jit_optimization_count: Some(0),
        jit_optimization_time: Some(0.0),
        jit_emission_count: Some(0),
        jit_emission_time: Some(0.0),
        jit_deform_count: Some(0),
        jit_deform_time: Some(0.0),
        parallel_workers_to_launch: Some(4),
        parallel_workers_launched: Some(3),
        stats_since: Some(1_500),
        minmax_stats_since: Some(1_800),
    }
}

#[test]
fn successful_statements_read_outranks_stale_failure() {
    let mut collected = StatementsCollection::successful(
        StatementsVersion::V6,
        vec![statement_row(200), statement_row(200)],
        7,
        999,
    );

    collected.retain_attempt(Some(query_failure_attempt(100, 1_002_005, Some("42501"))));

    let (version, rows, source_total) = collected.read.as_ref().expect("successful read retained");
    assert_eq!(*version, StatementsVersion::V6);
    assert_eq!(rows.len(), 2);
    assert_eq!(*source_total, 7);
    let attempt = collected.attempt.expect("successful attempt retained");
    assert_eq!(attempt.ts, 200);
    assert_eq!(attempt.section_type_id, 1_002_006);
    assert_eq!(attempt.coverage.total, 7);
    assert_eq!(attempt.coverage.collected, 2);
    assert_eq!(attempt.coverage.exact_total(), Some(7));
    assert_eq!(attempt.coverage.read_state(), (1, 0));
}

#[test]
fn successful_empty_statements_read_outranks_stale_failure() {
    let mut collected = StatementsCollection::successful(StatementsVersion::V6, Vec::new(), 0, 300);

    collected.retain_attempt(Some(query_failure_attempt(250, 1_002_005, None)));

    let (version, rows, source_total) = collected.read.as_ref().expect("empty read retained");
    assert_eq!(*version, StatementsVersion::V6);
    assert!(rows.is_empty());
    assert_eq!(*source_total, 0);
    let attempt = collected.attempt.expect("empty success attempt retained");
    assert_eq!(attempt.ts, 300);
    assert_eq!(attempt.section_type_id, 1_002_006);
    assert_eq!(attempt.coverage.total, 0);
    assert_eq!(attempt.coverage.collected, 0);
    assert_eq!(attempt.coverage.exact_total(), Some(0));
    assert_eq!(attempt.coverage.read_state(), (0, 0));
}

#[test]
fn failure_only_statements_attempts_keep_preference_order() {
    let mut collected = StatementsCollection::default();
    collected.retain_attempt(Some(query_failure_attempt(10, 1_002_005, Some("42501"))));
    collected.retain_attempt(Some(query_failure_attempt(20, 1_002_006, Some("57014"))));
    collected.retain_attempt(Some(query_failure_attempt(30, 1_002_005, Some("42501"))));

    assert!(collected.read.is_none());
    let attempt = collected.attempt.expect("failure attempt retained");
    assert_eq!(attempt.ts, 20);
    assert_eq!(attempt.section_type_id, 1_002_006);
    assert_eq!(attempt.coverage.exact_total(), None);
    assert_eq!(attempt.coverage.read_state(), (3, 2));
}

#[test]
fn known_layout_query_failures_keep_a_typed_attempt() {
    let permission = query_failure_attempt(10, 1_002_006, Some("42501"));
    assert_eq!(permission.coverage.read_state(), (2, 1));
    assert_eq!(permission.coverage.exact_total(), None);

    let timeout = query_failure_attempt(20, 1_002_006, Some("57014"));
    assert_eq!(timeout.coverage.read_state(), (3, 2));

    let other = query_failure_attempt(30, 1_002_006, None);
    assert_eq!(other.coverage.read_state(), (3, 2));
    assert_eq!(other.section_type_id, 1_002_006);
}

#[test]
fn cached_statements_source_tracks_extversion_and_layout() {
    let cached = CachedStatementsSource::new(
        StatementsSource::Database("metrics".to_owned()),
        "1.11".to_owned(),
    );
    assert_eq!(cached.version, StatementsVersion::V5);
    assert!(cached.matches_extversion("1.11"));
    assert!(!cached.matches_extversion("1.12"));
    assert!(!cached.matches_extversion("1.10"));
}

#[test]
fn missing_statements_source_rotates_per_db_probes() {
    let mut missing = MissingStatementsSource::new(vec!["app".to_owned(), "metrics".to_owned()]);
    assert!(missing.matches_covered(&["app".to_owned(), "metrics".to_owned()]));
    assert!(!missing.matches_covered(&["app".to_owned()]));
    assert_eq!(missing.next_per_db_probe(2), Some(0));
    assert_eq!(missing.next_per_db_probe(2), Some(1));
    assert_eq!(missing.next_per_db_probe(2), Some(0));
    assert_eq!(missing.next_per_db_probe(0), None);
}

#[test]
fn statements_source_cache_replaces_missing_with_selected_source() {
    let mut cache = StatementsSourceCache::default();
    cache.mark_missing(vec!["app".to_owned()]);
    assert!(cache.selected.is_none());
    assert!(cache.missing.is_some());

    let version = cache.store(StatementsSource::Main, "1.12".to_owned());
    assert_eq!(version, StatementsVersion::V6);
    assert!(cache.selected.is_some());
    assert!(cache.missing.is_none());

    cache.invalidate();
    assert!(cache.selected.is_none());
    assert!(cache.missing.is_none());
}
