use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kronika_analytics::overview::{CoverageSpan, NamingContractId, SegmentLocator};
use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
use kronika_reader::{
    FactBuildKey, FactKey, LIMIT, LiveBuilder, PgmUnit, SealedLocator, SegmentDescriptor,
    lineage_from_catalog, source_scope_id,
};
use kronika_registry::Section;
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use tower::ServiceExt as _;

use super::{assert_problem, capture_json, test_metrics_handle, write_bgwriter_segment};
use crate::overview::admission::ColdWorkWeight;
use crate::overview::selection::{
    ABSOLUTE_MAX_SELECTED_SEGMENTS, SelectedSealedPlan, SelectionError,
};
use crate::overview::view::{DescriptorEntry, DescriptorView};
use crate::{AppState, OverviewConfig, PublishedStoreView, StateBuildError, app};

const SYNTHETIC_NAMESPACE: &[u8] = b"overview-admission-test";

fn one_weight() -> ColdWorkWeight {
    ColdWorkWeight {
        workers: 1,
        pgm_bytes: 1,
        decoded_bytes: 1,
        cpu: 1,
        file_descriptors: 1,
        read_bytes: 1,
        write_bytes: 1,
        publications: 1,
    }
}

fn synthetic_entries(
    count: usize,
    source_id: u64,
    min_ts: i64,
    max_ts: i64,
    ordinal_base: usize,
) -> Vec<DescriptorEntry> {
    let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
    let bytes = build_part(
        &[SectionInput {
            type_id: 1_006_001,
            rows: 0,
            body: &body,
        }],
        PartMeta {
            min_ts,
            max_ts,
            source_id,
        },
    );
    let unit = PgmUnit::open(bytes.as_slice()).expect("open synthetic catalog");
    let source_scope = source_scope_id(SYNTHETIC_NAMESPACE, source_id);
    let fact_key = FactKey::for_current_segment(source_scope, unit.source_descriptor());
    (0..count)
        .map(|offset| {
            let ordinal = ordinal_base.checked_add(offset).expect("synthetic ordinal");
            let file_name = format!("synthetic-{source_id}-{ordinal}.pgm");
            let locator = SealedLocator::from_file_name_bytes(file_name.as_bytes());
            let descriptor = SegmentDescriptor::from_catalog(locator, unit.catalog());
            let lineage = lineage_from_catalog(
                unit.catalog(),
                source_scope,
                NamingContractId([1; 16]),
                SegmentLocator(*locator.as_bytes()),
            )
            .expect("catalog has one entry");
            DescriptorEntry::new(
                descriptor,
                FactBuildKey::new(fact_key, lineage),
                one_weight(),
                source_scope,
            )
        })
        .collect()
}

fn synthetic_view(
    entries: Vec<DescriptorEntry>,
    unavailable: Vec<SegmentDescriptor>,
) -> Arc<DescriptorView> {
    let live = LiveBuilder::new(SYNTHETIC_NAMESPACE.to_vec(), LIMIT)
        .expect("live builder")
        .publish();
    Arc::new(DescriptorView::new(
        1,
        entries,
        unavailable,
        Arc::new(live),
        None,
    ))
}

fn state_with_limit(dir: &std::path::Path, limit: usize) -> AppState {
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir).expect("open snapshot");
    let mut config = OverviewConfig::new(
        dir.join(".overview-cache"),
        dir.as_os_str().as_encoded_bytes().to_vec(),
    );
    config.max_selected_segments = limit;
    AppState::with_overview_config(snapshot, 0, std::time::Duration::from_secs(10), config)
        .expect("state")
}

#[test]
fn programmatic_policy_rejects_zero_and_values_above_the_absolute_ceiling() {
    for configured in [0, ABSOLUTE_MAX_SELECTED_SEGMENTS + 1] {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let mut config = OverviewConfig::new(
            dir.path().join(".overview-cache"),
            SYNTHETIC_NAMESPACE.to_vec(),
        );
        config.max_selected_segments = configured;
        let error =
            AppState::with_overview_config(snapshot, 0, std::time::Duration::from_secs(10), config)
                .expect_err("invalid programmatic limit must fail before state construction");
        assert!(matches!(
            error,
            StateBuildError::Overview(
                crate::OverviewBuildError::SelectedSegmentLimit {
                    configured: actual,
                    maximum: ABSOLUTE_MAX_SELECTED_SEGMENTS,
                }
            ) if actual == configured
        ));
    }
}

fn install_view(state: &AppState, view: Arc<DescriptorView>) {
    let (snapshot, _previous) = state.overview_request_view();
    state.published.store(Arc::new(PublishedStoreView {
        snapshot: Arc::clone(&snapshot),
        timeline_snapshot: snapshot,
        timeline: view,
    }));
}

#[test]
fn canonical_plan_enforces_zero_one_limit_and_limit_plus_one() {
    let range = CoverageSpan::new(0, 2).expect("range");
    let empty = SelectedSealedPlan::build(synthetic_view(Vec::new(), Vec::new()), &[7], range, 1)
        .expect("empty plan");
    assert_eq!(empty.selected_count(), 0);

    let one = SelectedSealedPlan::build(
        synthetic_view(synthetic_entries(1, 7, 0, 1, 0), Vec::new()),
        &[7],
        range,
        1,
    )
    .expect("one segment is admitted");
    assert_eq!(one.selected_count(), 1);

    let entries = synthetic_entries(ABSOLUTE_MAX_SELECTED_SEGMENTS + 1, 7, 0, 1, 0);
    let view = synthetic_view(entries, Vec::new());
    let admitted = SelectedSealedPlan::build(
        Arc::clone(&view),
        &[7],
        range,
        ABSOLUTE_MAX_SELECTED_SEGMENTS,
    );
    assert!(matches!(
        admitted,
        Err(SelectionError::LimitExceeded {
            limit: ABSOLUTE_MAX_SELECTED_SEGMENTS,
        })
    ));

    let exact = synthetic_view(
        synthetic_entries(ABSOLUTE_MAX_SELECTED_SEGMENTS, 7, 0, 1, 10_000),
        Vec::new(),
    );
    assert_eq!(
        SelectedSealedPlan::build(exact, &[7], range, ABSOLUTE_MAX_SELECTED_SEGMENTS,)
            .expect("absolute boundary is inclusive")
            .selected_count(),
        ABSOLUTE_MAX_SELECTED_SEGMENTS
    );
}

#[test]
fn selection_is_source_scoped_aggregate_and_requires_canonical_sources() {
    let mut entries = synthetic_entries(1, 7, 0, 1, 0);
    entries.extend(synthetic_entries(
        ABSOLUTE_MAX_SELECTED_SEGMENTS,
        8,
        0,
        1,
        10_000,
    ));
    let view = synthetic_view(entries, Vec::new());
    let range = CoverageSpan::new(0, 2).expect("range");
    assert_eq!(
        SelectedSealedPlan::build(Arc::clone(&view), &[7], range, 1)
            .expect("foreign source is excluded")
            .selected_count(),
        1
    );
    assert!(matches!(
        SelectedSealedPlan::build(
            Arc::clone(&view),
            &[7, 8],
            range,
            ABSOLUTE_MAX_SELECTED_SEGMENTS,
        ),
        Err(SelectionError::LimitExceeded {
            limit: ABSOLUTE_MAX_SELECTED_SEGMENTS,
        })
    ));
    assert!(matches!(
        SelectedSealedPlan::build(view, &[7, 7], range, ABSOLUTE_MAX_SELECTED_SEGMENTS),
        Err(SelectionError::SourcesNotCanonical)
    ));
}

#[test]
fn selection_uses_inclusive_segment_end_and_exclusive_request_end() {
    let mut entries = synthetic_entries(1, 7, -10, 0, 0);
    entries.extend(synthetic_entries(1, 7, 10, 20, 1));
    entries.extend(synthetic_entries(1, 7, i64::MIN, i64::MIN, 2));
    entries.extend(synthetic_entries(1, 7, i64::MAX, i64::MAX, 3));
    let view = synthetic_view(entries, Vec::new());

    assert_eq!(
        SelectedSealedPlan::build(
            Arc::clone(&view),
            &[7],
            CoverageSpan::new(0, 10).expect("range"),
            4,
        )
        .expect("boundary plan")
        .selected_count(),
        1,
        "max_ts == from is selected and min_ts == to is excluded"
    );
    assert_eq!(
        SelectedSealedPlan::build(
            Arc::clone(&view),
            &[7],
            CoverageSpan::new(i64::MIN, i64::MIN + 1).expect("pre-epoch range"),
            4,
        )
        .expect("minimum range")
        .selected_count(),
        1
    );
    assert_eq!(
        SelectedSealedPlan::build(
            view,
            &[7],
            CoverageSpan::new(i64::MAX - 1, i64::MAX).expect("maximum range"),
            4,
        )
        .expect("maximum range")
        .selected_count(),
        0,
        "a segment beginning at the exclusive request end is not selected"
    );
}

#[test]
fn unavailable_descriptors_mark_a_gap_without_consuming_the_limit() {
    let unavailable = synthetic_entries(1, 7, 0, 1, 0)
        .into_iter()
        .map(|entry| *entry.descriptor())
        .collect();
    let plan = SelectedSealedPlan::build(
        synthetic_view(Vec::new(), unavailable),
        &[7],
        CoverageSpan::new(0, 2).expect("range"),
        1,
    )
    .expect("unavailable descriptors are not counted");
    assert_eq!(plan.selected_count(), 0);
    assert!(plan.sealed_gap());
}

#[test]
fn live_parts_are_not_counted_as_sealed_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
    let part = build_part(
        &[SectionInput {
            type_id: 1_006_001,
            rows: 0,
            body: &body,
        }],
        PartMeta {
            min_ts: 0,
            max_ts: 1,
            source_id: 7,
        },
    );
    let mut framed = FrameHeader {
        part_len: u64::try_from(part.len()).expect("part length"),
    }
    .encode()
    .to_vec();
    framed.extend_from_slice(&part);
    std::fs::write(dir.path().join("active.parts"), framed).expect("write active journal");

    let state = state_with_limit(dir.path(), 1);
    let plan = state
        .select_overview(
            state.overview_view(),
            &[7],
            CoverageSpan::new(0, 2).expect("range"),
        )
        .expect("live-only plan");
    assert_eq!(plan.selected_count(), 0);
}

#[tokio::test]
async fn all_timeline_routes_reject_before_cache_flight_capacity_or_cursor_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_with_limit(dir.path(), ABSOLUTE_MAX_SELECTED_SEGMENTS);
    install_view(
        &state,
        synthetic_view(
            synthetic_entries(ABSOLUTE_MAX_SELECTED_SEGMENTS + 1, 7, 0, 1, 0),
            Vec::new(),
        ),
    );
    let _analytic_permit = state
        .try_acquire_analytic()
        .expect("occupy analytic capacity");

    for uri in [
        "/v1/timeline/overview?source=7&from=0&to=2",
        "/v1/timeline/health?source=7&from=0&to=2",
        "/v1/timeline/events?source=7&from=0&to=2",
    ] {
        let response = app(state.clone(), None, test_metrics_handle())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("route");
        let response = capture_json(response).await;
        assert_problem(
            &response.body,
            StatusCode::BAD_REQUEST,
            "query_limit_exceeded",
            serde_json::json!({
                "resource": "selected_segments",
                "limit": ABSOLUTE_MAX_SELECTED_SEGMENTS,
            }),
        );
        assert_eq!(response.media_type(), Some("application/problem+json"));
        assert_eq!(
            response
                .headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(state.response_cache.len(), 0);
        assert_eq!(state.cursor_registry().pinned_views(), 0);
    }
}

#[tokio::test]
async fn events_counts_the_deduplicated_source_union_against_one_effective_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_with_limit(dir.path(), 4);
    let mut entries = synthetic_entries(2, 7, 0, 1, 0);
    entries.extend(synthetic_entries(3, 8, 0, 1, 10));
    install_view(&state, synthetic_view(entries, Vec::new()));

    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/events?source=8&source=7&source=7&from=0&to=2")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route");
    let response = capture_json(response).await;
    assert_problem(
        &response.body,
        StatusCode::BAD_REQUEST,
        "query_limit_exceeded",
        serde_json::json!({
            "resource": "selected_segments",
            "limit": 4,
        }),
    );
}

#[tokio::test]
async fn a_cold_weight_above_capacity_is_a_typed_no_store_http_overload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_with_limit(dir.path(), 1);
    let entry = synthetic_entries(1, 7, 0, 1, 0)
        .pop()
        .expect("synthetic entry");
    let oversized = DescriptorEntry::new(
        *entry.descriptor(),
        entry.fact_build_key(),
        ColdWorkWeight {
            workers: u32::MAX,
            ..one_weight()
        },
        entry.source_scope_id(),
    );
    install_view(&state, synthetic_view(vec![oversized], Vec::new()));

    let response = app(state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route");
    let response = capture_json(response).await;
    assert_problem(
        &response.body,
        StatusCode::SERVICE_UNAVAILABLE,
        "cold_build_overloaded",
        serde_json::json!({ "retry_after_seconds": 1 }),
    );
    assert_eq!(
        response
            .headers
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    assert_eq!(
        response
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(state.response_cache.len(), 0);
}

#[tokio::test]
async fn foreign_synthetic_descriptors_do_not_block_or_load_a_selected_real_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "real-source-7.pgm", 7, 0, 1);
    let state = state_with_limit(dir.path(), 1);
    let (snapshot, original) = state.overview_request_view();
    let mut entries = original.entries().to_vec();
    assert_eq!(entries.len(), 1);
    entries.extend(synthetic_entries(
        ABSOLUTE_MAX_SELECTED_SEGMENTS,
        8,
        0,
        1,
        10_000,
    ));
    state.published.store(Arc::new(PublishedStoreView {
        snapshot: Arc::clone(&snapshot),
        timeline_snapshot: snapshot,
        timeline: Arc::new(DescriptorView::new(
            original.view_generation(),
            entries,
            Vec::new(),
            Arc::clone(original.live()),
            None,
        )),
    }));

    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::OK);
}
