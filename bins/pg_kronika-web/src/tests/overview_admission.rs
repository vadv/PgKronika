use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kronika_analytics::overview::CoverageSpan;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_layout::{FileIdentity, SegmentId};
use kronika_reader::{
    FactBuildKey, FactKey, LIMIT, LiveBuilder, PgmUnit, SealedLocator, SegmentDescriptor,
    lineage_from_catalog,
};
use kronika_registry::Section;
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_store::CatalogSummary;
use tower::ServiceExt as _;

use super::{assert_problem, capture_json, test_metrics_handle, write_bgwriter_segment};
use crate::overview::admission::ColdWorkWeight;
use crate::overview::selection::{
    ABSOLUTE_MAX_SELECTED_SEGMENTS, SelectedSealedPlan, SelectionError,
};
use crate::overview::view::{DescriptorEntry, DescriptorView};
use crate::{AppState, OverviewConfig, PublishedStoreView, StateBuildError, app};

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
    let fact_key = FactKey::for_current_segment(source_id, unit.source_descriptor());
    (0..count)
        .map(|offset| {
            let ordinal = ordinal_base.checked_add(offset).expect("synthetic ordinal");
            let file_name = format!("synthetic-{source_id}-{ordinal}.pgm");
            let locator =
                SealedLocator::from_segment_id(crate::test_layout::named_address(&file_name).id);
            let summary = CatalogSummary::from_catalog(
                unit.catalog(),
                u32::try_from(unit.catalog().encoded_len()).expect("catalog length"),
            );
            let descriptor = SegmentDescriptor::from_summary(
                locator,
                FileIdentity {
                    device: 1,
                    inode: u64::try_from(ordinal).unwrap_or(u64::MAX),
                    len: unit.source_file_len(),
                    mtime_seconds: 0,
                    mtime_nanoseconds: 0,
                    ctime_seconds: 0,
                    ctime_nanoseconds: 0,
                },
                &summary,
            );
            let lineage = lineage_from_catalog(unit.catalog(), unit.source_descriptor())
                .expect("catalog has one entry");
            DescriptorEntry::new(
                descriptor,
                FactBuildKey::new(fact_key, lineage),
                one_weight(),
            )
        })
        .collect()
}

fn synthetic_view(
    entries: Vec<DescriptorEntry>,
    unavailable: Vec<SegmentDescriptor>,
) -> Arc<DescriptorView> {
    let live = LiveBuilder::new(LIMIT).expect("live builder").publish();
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
    let mut config = OverviewConfig::new();
    config.max_selected_segments = limit;
    AppState::with_overview_config(snapshot, 0, std::time::Duration::from_secs(10), &config)
        .expect("state")
}

#[test]
fn programmatic_policy_rejects_zero_and_values_above_the_absolute_ceiling() {
    for configured in [0, ABSOLUTE_MAX_SELECTED_SEGMENTS + 1] {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let mut config = OverviewConfig::new();
        config.max_selected_segments = configured;
        let error = AppState::with_overview_config(
            snapshot,
            0,
            std::time::Duration::from_secs(10),
            &config,
        )
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

fn install_oversized_real_entry(state: &AppState) {
    let (snapshot, original) = state.overview_request_view();
    let entry = original
        .entries()
        .first()
        .expect("fixture has one real sealed descriptor");
    assert_eq!(original.entries().len(), 1);
    let oversized = DescriptorEntry::new(
        *entry.descriptor(),
        entry.fact_build_key(),
        ColdWorkWeight {
            workers: u32::MAX,
            ..entry.cold_weight()
        },
    );
    state.published.store(Arc::new(PublishedStoreView {
        snapshot: Arc::clone(&snapshot),
        timeline_snapshot: snapshot,
        timeline: Arc::new(DescriptorView::new(
            original.view_generation(),
            vec![oversized],
            Vec::new(),
            Arc::clone(original.live()),
            None,
        )),
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
fn selection_keeps_half_open_intersection_with_boundary_halos() {
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
        .entries()
        .map(|entry| (entry.descriptor().min_ts, entry.descriptor().max_ts))
        .collect::<Vec<_>>(),
        vec![(i64::MIN, i64::MIN), (-10, 0), (10, 20)],
        "the intersecting segment is surrounded by one left and right halo"
    );
    assert_eq!(
        SelectedSealedPlan::build(
            Arc::clone(&view),
            &[7],
            CoverageSpan::new(i64::MIN, i64::MIN + 1).expect("pre-epoch range"),
            4,
        )
        .expect("minimum range")
        .entries()
        .map(|entry| (entry.descriptor().min_ts, entry.descriptor().max_ts))
        .collect::<Vec<_>>(),
        vec![(i64::MIN, i64::MIN), (-10, 0)]
    );
    assert_eq!(
        SelectedSealedPlan::build(
            view,
            &[7],
            CoverageSpan::new(i64::MAX - 1, i64::MAX).expect("maximum range"),
            4,
        )
        .expect("maximum range")
        .entries()
        .map(|entry| (entry.descriptor().min_ts, entry.descriptor().max_ts))
        .collect::<Vec<_>>(),
        vec![(10, 20), (i64::MAX, i64::MAX)],
        "the exclusive-end segment is retained only as the right halo"
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
    crate::test_layout::write_journal(
        dir.path(),
        SegmentId::new(0).expect("fixture segment id"),
        &[&part],
    );

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
async fn a_cold_weight_above_capacity_uses_the_configured_retry_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
    let mut config = OverviewConfig::new();
    config.max_selected_segments = 1;
    config.cold.retry_after_seconds = 7;
    let state =
        AppState::with_overview_config(snapshot, 0, std::time::Duration::from_secs(10), &config)
            .expect("state");
    install_oversized_real_entry(&state);

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
        serde_json::json!({ "retry_after_seconds": 7 }),
    );
    assert_eq!(
        response
            .headers
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("7")
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
async fn an_exact_decoded_hit_bypasses_cold_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);
    let state = state_with_limit(dir.path(), 1);

    let cold = app(state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("cold request"),
        )
        .await
        .expect("cold route");
    assert_eq!(cold.status(), StatusCode::OK);

    install_oversized_real_entry(&state);
    let decoded = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/events?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("decoded request"),
        )
        .await
        .expect("decoded route");
    assert_eq!(
        decoded.status(),
        StatusCode::OK,
        "an exact L2 fact must be returned before the oversized cold charge is considered"
    );
}

#[tokio::test]
async fn an_exact_durable_hit_bypasses_cold_admission_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);

    let first_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("first snapshot");
    let first_state = AppState::with_overview_config(
        first_snapshot,
        0,
        std::time::Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("first state");
    let cold = app(first_state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("cold request"),
        )
        .await
        .expect("cold route");
    assert_eq!(cold.status(), StatusCode::OK);
    drop(first_state);

    let restarted_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("restart snapshot");
    let restarted = AppState::with_overview_config(
        restarted_snapshot,
        0,
        std::time::Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("restarted state");
    install_oversized_real_entry(&restarted);
    let durable = app(restarted, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/events?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("durable request"),
        )
        .await
        .expect("durable route");
    assert_eq!(
        durable.status(),
        StatusCode::OK,
        "an exact durable fact must be returned before the oversized cold charge is considered"
    );
}

#[tokio::test]
async fn unselected_synthetic_descriptors_do_not_block_or_load_a_real_source() {
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
