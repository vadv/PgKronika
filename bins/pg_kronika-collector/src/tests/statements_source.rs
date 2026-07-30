use crate::coverage::query_failure_attempt;
use crate::statements_source::{
    CachedStatementsSource, MissingStatementsSource, StatementsSource, StatementsSourceCache,
};
use kronika_source_pg::statements::StatementsVersion;

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
