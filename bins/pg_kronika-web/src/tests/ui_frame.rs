use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::threshold::{Policy, catalog_entry, classify};
use kronika_analytics::web_projection::web_view_by_name;
use kronika_analytics::{
    Boundary, Classified, Comparison, Evidence, Level, MetricId, MetricInput, NotClassifiedReason,
    Verdict,
};
use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot, OutRow, Value};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_settings::PgSettingsV1;
use kronika_registry::pg_stat_statements::PgStatStatementsV2;
use kronika_registry::{Section, StrId, Ts};

use crate::api_error::ErrorCode;
use crate::ui::catalog::ProjectionCatalog;
use crate::ui::frame::FrameRequest;
use crate::ui::frame::cursor::{FrameCursor, SortKey};
use crate::ui::frame::dto::ClassificationResultDto;
use crate::ui::frame::projection::{
    DeltaOperand, FrameLimits, ProjectedRow, RowOperands, StatementOperands, TableOperands,
    project_frame,
};
use crate::ui::frame::projection::{ProjectionInput, project_input};
use crate::ui::frame::spark::attach_sparks;
use crate::ui::frame::threshold::{FrameThresholdContext, prepare_input};
use crate::ui::snapshot::{resolve_snapshot_at, resolve_view_snapshot};
use crate::ui::thresholds::OperandKind;

fn catalog() -> ProjectionCatalog {
    ProjectionCatalog::for_type_ids(&BTreeSet::new())
}

#[test]
fn snapshot_resolvers_select_the_latest_snapshot_at_or_before_at() {
    let directory = frame_event_fixture();
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let events = web_view_by_name("events").expect("events view");

    let resolved = resolve_view_snapshot(&snapshot, events, 1_550).expect("view snapshot");
    assert_eq!(resolved.neighbors.expect("view neighbors").current, 1_500);

    let resolved = resolve_snapshot_at(&snapshot, 1_550)
        .expect("snapshot lookup")
        .expect("snapshot");
    assert_eq!(resolved.timestamp_us, 1_500);
}

#[test]
fn frame_query_defaults_are_bounded_and_come_from_the_first_preset() {
    let request = FrameRequest::parse("activity", Some("at=123"), &catalog()).expect("request");

    assert_eq!(request.at_us, 123);
    assert_eq!(request.span_us, 3_600_000_000);
    assert_eq!(request.preset, "sessions");
    assert_eq!(request.sort, "query_duration_us");
    assert!(request.descending);
    assert_eq!(request.limit, 100);
    assert!(request.database.is_none());
    assert!(request.filter.is_none());
    assert!(request.cursor.is_none());
}

#[test]
fn frame_query_rejects_invalid_shapes_before_storage_access() {
    let catalog = catalog();
    for (view, raw, code) in [
        ("activity", "", ErrorCode::MissingQueryParameter),
        ("activity", "at=1&span=25h", ErrorCode::QueryLimitExceeded),
        ("activity", "at=1&limit=0", ErrorCode::InvalidQueryParameter),
        ("activity", "at=1&limit=201", ErrorCode::QueryLimitExceeded),
        (
            "activity",
            "at=1&preset=missing",
            ErrorCode::InvalidQueryParameter,
        ),
        (
            "activity",
            "at=1&sort=missing",
            ErrorCode::InvalidQueryParameter,
        ),
        (
            "activity",
            "at=1&sort=query",
            ErrorCode::InvalidQueryParameter,
        ),
        (
            "activity",
            "at=1&order=sideways",
            ErrorCode::InvalidQueryParameter,
        ),
        (
            "activity",
            "at=1&source=legacy",
            ErrorCode::UnknownQueryParameter,
        ),
        ("activity", "at=1&at=2", ErrorCode::DuplicateQueryParameter),
        ("missing", "at=1", ErrorCode::InvalidQueryParameter),
    ] {
        let error = FrameRequest::parse(view, Some(raw), &catalog).expect_err(raw);
        assert_eq!(error.code(), code, "{view}?{raw}");
    }
}

#[test]
fn frame_query_applies_decoded_filter_and_cursor_byte_limits() {
    let catalog = catalog();
    let filter = "я".repeat(129);
    let raw = format!("at=1&q={filter}");
    let error = FrameRequest::parse("activity", Some(&raw), &catalog).expect_err("257 bytes");
    assert_eq!(error.code(), ErrorCode::QueryLimitExceeded);

    let cursor = "a".repeat(513);
    let raw = format!("at=1&cursor={cursor}");
    let error = FrameRequest::parse("activity", Some(&raw), &catalog).expect_err("513 bytes");
    assert_eq!(error.code(), ErrorCode::QueryLimitExceeded);
}

#[test]
fn frame_cursor_round_trips_every_sort_key_and_rejects_bad_payloads() {
    let keys = [
        SortKey::Null,
        SortKey::Signed(-7),
        SortKey::Unsigned(u64::MAX),
        SortKey::Float(28.4),
        SortKey::Boolean(true),
        SortKey::Timestamp(i64::MAX),
        SortKey::text_prefix("текст longer than one scalar"),
    ];
    for key in keys {
        let cursor =
            FrameCursor::new(2, 7, 123, [9; 32], key, vec![1, 2, 3]).expect("bounded cursor");
        let encoded = cursor.encode().expect("encode");
        assert!(encoded.len() <= 512);
        assert_eq!(FrameCursor::decode(&encoded), Ok(cursor));
    }

    assert!(FrameCursor::new(1, 1, 1, [0; 32], SortKey::Float(f64::NAN), vec![]).is_err());
    assert!(FrameCursor::new(1, 1, 1, [0; 32], SortKey::Null, vec![0; 257]).is_err());

    for len in 0..96 {
        let bytes = (0..len)
            .map(|index: u8| index.wrapping_mul(37).wrapping_add(11))
            .collect::<Vec<_>>();
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        assert!(
            FrameCursor::decode(&encoded).is_err(),
            "random length {len}"
        );
    }

    let cursor = FrameCursor::new(1, 1, 1, [0; 32], SortKey::Null, vec![]).expect("bounded cursor");
    let mut bytes = URL_SAFE_NO_PAD
        .decode(cursor.encode().expect("encode"))
        .expect("payload");
    bytes.push(0);
    assert!(FrameCursor::decode(&URL_SAFE_NO_PAD.encode(bytes)).is_err());
}

#[test]
fn frame_cursor_is_bound_to_the_normalized_query() {
    let catalog = catalog();
    let request = FrameRequest::parse(
        "activity",
        Some("at=123&span=2h&preset=sessions&q=active&sort=pid&order=asc&limit=7"),
        &catalog,
    )
    .expect("request");
    let cursor = FrameCursor::new(
        request.view.code,
        request.view.revision,
        120,
        request.query_fingerprint(),
        SortKey::Signed(42),
        vec![1],
    )
    .expect("cursor")
    .encode()
    .expect("encode");

    let raw = format!(
        "at=123&span=2h&preset=sessions&q=active&sort=pid&order=asc&limit=7&cursor={cursor}"
    );
    FrameRequest::parse("activity", Some(&raw), &catalog).expect("matching cursor");

    let mismatch = format!(
        "at=123&span=2h&preset=sessions&q=other&sort=pid&order=asc&limit=7&cursor={cursor}"
    );
    let error = FrameRequest::parse("activity", Some(&mismatch), &catalog)
        .expect_err("query fingerprint mismatch");
    assert_eq!(error.code(), ErrorCode::CursorQueryMismatch);
}

#[test]
fn classified_dto_preserves_all_evidence_variants() {
    let boundary = Boundary {
        operator: Comparison::AtLeast,
        value: 10.0,
    };
    let cases = [
        (
            Evidence::Scalar { observed: 28.4 },
            serde_json::json!({"kind":"scalar","observed":28.4}),
        ),
        (
            Evidence::Fraction {
                numerator: 7.0,
                denominator: 10.0,
                value: 0.7,
            },
            serde_json::json!({
                "kind":"fraction","numerator":7.0,"denominator":10.0,"value":0.7
            }),
        ),
        (
            Evidence::Limit {
                observed: 11.0,
                limit: 10.0,
            },
            serde_json::json!({"kind":"limit","observed":11.0,"limit":10.0}),
        ),
        (
            Evidence::RatioWithFloor {
                ratio: 0.2,
                count: 20_000.0,
                floor: boundary,
            },
            serde_json::json!({
                "kind":"ratio_with_floor",
                "ratio":0.2,
                "count":20000.0,
                "floor":{"operator":"at_least","value":10.0}
            }),
        ),
        (
            Evidence::Age {
                epoch_seconds: 10.0,
                now_seconds: 30.0,
                age_seconds: 20.0,
            },
            serde_json::json!({
                "kind":"age","epoch_seconds":10.0,"now_seconds":30.0,"age_seconds":20.0
            }),
        ),
        (
            Evidence::FreeCapacity {
                available_bytes: 10.0,
                total_bytes: 100.0,
                available_fraction: 0.1,
                absolute_ceiling_bytes: boundary,
            },
            serde_json::json!({
                "kind":"free_capacity",
                "available_bytes":10.0,
                "total_bytes":100.0,
                "available_fraction":0.1,
                "absolute_ceiling_bytes":{"operator":"at_least","value":10.0}
            }),
        ),
    ];

    for (evidence, expected) in cases {
        let dto = ClassificationResultDto::from(Classified::Verdict(Verdict {
            level: Level::Warning,
            boundary: Some(boundary),
            evidence,
        }));
        let value = serde_json::to_value(dto).expect("serialize");
        assert_eq!(value["status"], "classified");
        assert_eq!(value["level"], "warning");
        assert_eq!(
            value["boundary"],
            serde_json::json!({"operator":"at_least","value":10.0})
        );
        assert_eq!(value["evidence"], expected);
    }
}

#[test]
fn classified_dto_preserves_inactive_and_every_not_classified_reason() {
    let inactive = ClassificationResultDto::from(Classified::Verdict(Verdict {
        level: Level::Inactive,
        boundary: None,
        evidence: Evidence::Scalar { observed: 0.0 },
    }));
    assert_eq!(
        serde_json::to_value(inactive).expect("inactive"),
        serde_json::json!({
            "status":"classified",
            "level":"inactive",
            "evidence":{"kind":"scalar","observed":0.0}
        })
    );

    for (reason, spelling) in [
        (NotClassifiedReason::Missing, "missing"),
        (NotClassifiedReason::NonFinite, "non_finite"),
        (NotClassifiedReason::OutOfDomain, "out_of_domain"),
        (
            NotClassifiedReason::InvalidDenominator,
            "invalid_denominator",
        ),
        (NotClassifiedReason::NotApplicable, "not_applicable"),
        (
            NotClassifiedReason::InputShapeMismatch,
            "input_shape_mismatch",
        ),
    ] {
        let dto = ClassificationResultDto::from(Classified::NotClassified(reason));
        assert_eq!(
            serde_json::to_value(dto).expect("not classified"),
            serde_json::json!({"status":"not_classified","reason":spelling})
        );
    }
}

#[test]
fn classified_dto_preserves_every_level_and_comparison_spelling() {
    for (level, spelling) in [(Level::Ok, "ok"), (Level::Critical, "critical")] {
        let dto = ClassificationResultDto::from(Classified::Verdict(Verdict {
            level,
            boundary: None,
            evidence: Evidence::Scalar { observed: 1.0 },
        }));
        assert_eq!(serde_json::to_value(dto).expect("level")["level"], spelling);
    }

    for (operator, spelling) in [
        (Comparison::Above, "above"),
        (Comparison::AtLeast, "at_least"),
        (Comparison::Below, "below"),
        (Comparison::AtMost, "at_most"),
    ] {
        let dto = ClassificationResultDto::from(Classified::Verdict(Verdict {
            level: Level::Critical,
            boundary: Some(Boundary {
                operator,
                value: 1.0,
            }),
            evidence: Evidence::Scalar { observed: 2.0 },
        }));
        assert_eq!(
            serde_json::to_value(dto).expect("operator")["boundary"]["operator"],
            spelling
        );
    }
}

fn out_row(values: &[(&str, Value)]) -> OutRow {
    values
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the golden keeps all nine projection fixtures adjacent for completeness review"
)]
fn frame_projection_covers_all_nine_views_and_omits_lazy_cells() {
    let fixtures = [
        (
            "activity",
            "pg_stat_activity",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("pid", Value::I64(7)),
                ("backend_start", Value::Ts(1)),
                ("usename", Value::Str("alice".to_owned())),
                ("datname", Value::Str("app".to_owned())),
                ("application_name", Value::Str("psql".to_owned())),
                ("state", Value::Str("active".to_owned())),
                ("query", Value::Str("secret".to_owned())),
                ("query_start", Value::Ts(10)),
            ]),
            6,
        ),
        (
            "statements",
            "pg_stat_statements",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("queryid", Value::I64(8)),
                ("userid", Value::U64(2)),
                ("dbid", Value::U64(3)),
                ("toplevel", Value::Bool(true)),
                ("query", Value::Str("secret".to_owned())),
                ("calls", Value::U64(10)),
                ("rows", Value::U64(20)),
                ("total_exec_time", Value::F64(30.0)),
            ]),
            8,
        ),
        (
            "plans",
            "pg_store_plans_vadv",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("dbid", Value::U64(3)),
                ("userid", Value::U64(2)),
                ("planid", Value::I64(9)),
                ("queryid", Value::I64(8)),
                ("plan", Value::Str("secret".to_owned())),
                ("calls", Value::U64(10)),
                ("total_time", Value::F64(30.0)),
                ("rows", Value::U64(20)),
            ]),
            5,
        ),
        (
            "tables",
            "pg_stat_user_tables",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("datid", Value::U64(3)),
                ("relid", Value::U64(4)),
                ("schemaname", Value::Str("public".to_owned())),
                ("relname", Value::Str("orders".to_owned())),
                ("seq_scan", Value::U64(10)),
                ("idx_scan", Value::U64(20)),
                ("n_live_tup", Value::I64(90)),
                ("n_dead_tup", Value::I64(10)),
            ]),
            6,
        ),
        (
            "indexes",
            "pg_stat_user_indexes",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("datid", Value::U64(3)),
                ("indexrelid", Value::U64(5)),
                ("schemaname", Value::Str("public".to_owned())),
                ("relname", Value::Str("orders".to_owned())),
                ("indexrelname", Value::Str("orders_pkey".to_owned())),
                ("idx_scan", Value::U64(10)),
                ("idx_tup_read", Value::U64(20)),
            ]),
            4,
        ),
        (
            "vacuum",
            "pg_stat_progress_vacuum",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("pid", Value::I64(11)),
                ("datid", Value::U64(3)),
                ("relid", Value::U64(4)),
                ("phase", Value::Str("scanning heap".to_owned())),
                ("heap_blks_total", Value::U64(100)),
                ("heap_blks_scanned", Value::U64(25)),
                ("num_dead_tuples", Value::U64(5)),
            ]),
            5,
        ),
        (
            "processes",
            "os_process",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("pid", Value::I64(12)),
                ("starttime", Value::Ts(1)),
                ("comm", Value::Str("postgres".to_owned())),
                ("rmem_kb", Value::U64(1_024)),
                ("cmdline", Value::Str("secret".to_owned())),
            ]),
            4,
        ),
        (
            "locks",
            "pg_locks",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("pid", Value::I64(13)),
                ("backend_start", Value::Ts(1)),
                ("usename", Value::Str("alice".to_owned())),
                ("application_name", Value::Str("psql".to_owned())),
                ("lock_relname", Value::Str("orders".to_owned())),
                ("query", Value::Str("secret".to_owned())),
            ]),
            5,
        ),
        (
            "events",
            "pg_log_errors",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("severity", Value::U64(3)),
                ("category", Value::U64(2)),
                ("sample", Value::Str("secret".to_owned())),
            ]),
            3,
        ),
    ];

    for (view, section, row, expected_cells) in fixtures {
        let request = FrameRequest::parse(view, Some("at=20"), &catalog()).expect("request");
        let frame = project_input(
            &request,
            &catalog(),
            ProjectionInput::single(20, section, row),
        )
        .unwrap_or_else(|error| panic!("{view}: {error:?}"));
        assert_eq!(frame.rows.len(), 1, "{view}");
        assert_eq!(frame.rows[0].cells.len(), expected_cells, "{view}");
        assert!(frame.rows[0].spark.values.is_empty(), "{view}");
        assert!(
            frame.rows[0]
                .cells
                .iter()
                .all(|cell| cell != &crate::ui::frame::dto::FrameValue::String("secret".into())),
            "{view} leaked a lazy field"
        );
    }
}

#[test]
fn frame_pagination_filters_then_sorts_by_value_and_entity() {
    let request = FrameRequest::parse(
        "processes",
        Some("at=20&preset=memory&q=post&sort=rss&order=desc&limit=1"),
        &catalog(),
    )
    .expect("request");
    let mut input = ProjectionInput::empty(20);
    input.push(
        "os_process",
        out_row(&[
            ("ts", Value::Ts(20)),
            ("pid", Value::I64(2)),
            ("starttime", Value::Ts(1)),
            ("comm", Value::Str("postgres".to_owned())),
            ("rmem_kb", Value::U64(10)),
        ]),
    );
    input.push(
        "os_process",
        out_row(&[
            ("ts", Value::Ts(20)),
            ("pid", Value::I64(1)),
            ("starttime", Value::Ts(1)),
            ("comm", Value::Str("postgres".to_owned())),
            ("rmem_kb", Value::U64(20)),
        ]),
    );

    let frame = project_input(&request, &catalog(), input).expect("projection");
    assert_eq!(frame.matched, 2);
    assert_eq!(frame.rows.len(), 1);
    assert_eq!(
        frame.rows[0].cells[0],
        crate::ui::frame::dto::FrameValue::Number(1.0)
    );
    assert!(frame.next.is_some());
}

#[test]
fn frame_text_sort_pagination_uses_the_bounded_cursor_key_consistently() {
    let prefix = "x".repeat(64);
    let raw = "at=20&preset=phase&sort=phase&order=asc&limit=1";
    let request = FrameRequest::parse("vacuum", Some(raw), &catalog()).expect("first request");
    let mut input = ProjectionInput::empty(20);
    for (pid, suffix) in [(2, "a"), (1, "z")] {
        input.push(
            "pg_stat_progress_vacuum",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("pid", Value::I64(pid)),
                ("datid", Value::U64(3)),
                (
                    "relid",
                    Value::U64(u64::try_from(pid).expect("positive fixture pid")),
                ),
                ("phase", Value::Str(format!("{prefix}{suffix}"))),
                ("heap_blks_total", Value::U64(100)),
                ("heap_blks_scanned", Value::U64(25)),
            ]),
        );
    }

    let first = project_input(&request, &catalog(), input.clone()).expect("first page");
    assert_eq!(first.rows.len(), 1);
    assert_eq!(
        first.rows[0].cells[0],
        crate::ui::frame::dto::FrameValue::Number(2.0),
        "full text order must compare the suffix after the bounded cursor prefix"
    );
    let first_pid = first.rows[0].cells[0].clone();
    let cursor = first.next.expect("next cursor");
    let raw = format!("{raw}&cursor={cursor}");
    let request = FrameRequest::parse("vacuum", Some(&raw), &catalog()).expect("second request");
    let second = project_input(&request, &catalog(), input).expect("second page");
    assert_eq!(second.rows.len(), 1);
    assert_ne!(second.rows[0].cells[0], first_pid);
}

#[test]
fn frame_filter_does_not_search_hidden_lazy_values() {
    let request = FrameRequest::parse(
        "processes",
        Some("at=20&preset=memory&q=hidden-command"),
        &catalog(),
    )
    .expect("request");
    let input = ProjectionInput::single(
        20,
        "os_process",
        out_row(&[
            ("ts", Value::Ts(20)),
            ("pid", Value::I64(1)),
            ("starttime", Value::Ts(1)),
            ("comm", Value::Str("postgres".to_owned())),
            ("rmem_kb", Value::U64(10)),
            ("cmdline", Value::Str("hidden-command".to_owned())),
        ]),
    );

    let frame = project_input(&request, &catalog(), input).expect("projection");
    assert_eq!(frame.matched, 0);
    assert!(frame.rows.is_empty());
}

#[test]
fn statement_time_percent_rejects_a_partial_reset_denominator() {
    let request = FrameRequest::parse("statements", Some("at=20"), &catalog()).expect("request");
    let mut input = ProjectionInput::empty(20);
    for (queryid, exec) in [(1, 20.0), (2, 40.0)] {
        input.push(
            "pg_stat_statements",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("queryid", Value::I64(queryid)),
                ("userid", Value::U64(10)),
                ("dbid", Value::U64(20)),
                ("calls", Value::I64(2)),
                ("rows", Value::I64(2)),
                ("total_exec_time", Value::F64(exec)),
            ]),
        );
    }
    input.push_previous(
        10,
        "pg_stat_statements",
        out_row(&[
            ("ts", Value::Ts(10)),
            ("queryid", Value::I64(1)),
            ("userid", Value::U64(10)),
            ("dbid", Value::U64(20)),
            ("calls", Value::I64(1)),
            ("rows", Value::I64(1)),
            ("total_exec_time", Value::F64(10.0)),
        ]),
    );

    let frame = project_input(&request, &catalog(), input).expect("projection");
    assert_eq!(frame.rows.len(), 2);
    assert!(frame.rows.iter().all(|row| {
        row.operands
            .statements
            .as_ref()
            .is_some_and(|operands| operands.snapshot_exec_ms_delta_sum.is_none())
    }));
}

#[test]
fn statement_filter_uses_the_final_returned_time_percent() {
    let request =
        FrameRequest::parse("statements", Some("at=20&q=25"), &catalog()).expect("request");
    let mut input = ProjectionInput::empty(20);
    for (queryid, current_exec, previous_exec) in [(1, 30.0, 10.0), (2, 90.0, 30.0)] {
        input.push(
            "pg_stat_statements",
            out_row(&[
                ("ts", Value::Ts(20)),
                ("queryid", Value::I64(queryid)),
                ("userid", Value::U64(10)),
                ("dbid", Value::U64(20)),
                ("calls", Value::I64(2)),
                ("rows", Value::I64(2)),
                ("total_exec_time", Value::F64(current_exec)),
            ]),
        );
        input.push_previous(
            10,
            "pg_stat_statements",
            out_row(&[
                ("ts", Value::Ts(10)),
                ("queryid", Value::I64(queryid)),
                ("userid", Value::U64(10)),
                ("dbid", Value::U64(20)),
                ("calls", Value::I64(1)),
                ("rows", Value::I64(1)),
                ("total_exec_time", Value::F64(previous_exec)),
            ]),
        );
    }

    let frame = project_input(&request, &catalog(), input).expect("projection");
    assert_eq!(frame.matched, 1);
    assert_eq!(frame.rows[0].label, "1");
}

fn frame_event_fixture() -> tempfile::TempDir {
    frame_event_fixture_with_historical_gated_segment(false)
}

fn frame_event_fixture_with_historical_gated_segment(historical_gated: bool) -> tempfile::TempDir {
    let rows = [
        PgLogLifecycleV1 {
            ts: Ts(1_500),
            kind: 0,
            pid: Some(42),
            signal: Some(9),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
        PgLogLifecycleV1 {
            ts: Ts(1_600),
            kind: 0,
            pid: Some(43),
            signal: Some(15),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
    ];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle fixture");
    let pgm = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: u32::try_from(rows.len()).expect("fixture rows"),
            body: &body,
        }],
        PartMeta {
            min_ts: 1_500,
            max_ts: 1_600,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    if historical_gated {
        let body = kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer::encode(&[
            super::bgwriter_row(1_000),
        ])
        .expect("encode historical gated fixture");
        let historical = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 1_000,
            },
        );
        crate::test_layout::write_named_pgm(
            directory.path(),
            "frame-events-history.pgm",
            &historical,
        );
    }
    crate::test_layout::write_named_pgm(directory.path(), "frame-events.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
    directory
}

#[test]
fn frame_quality_ignores_an_unselected_historical_summary_status() {
    let directory = frame_event_fixture_with_historical_gated_segment(true);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let request =
        FrameRequest::parse("events", Some("at=1600&span=1ms"), &catalog()).expect("request");

    let frame = project_frame(&snapshot, &request, &catalog(), FrameLimits::default())
        .expect("frame projection");

    assert_eq!(frame.snapshot_ts_us, 1_600);
    assert!(frame.quality.gated.is_empty());
    assert!(frame.quality.unavailable_revision.is_empty());
    assert!(frame.quality.resource_limited.is_empty());

    let before_current =
        FrameRequest::parse("events", Some("at=1000&span=1ms"), &catalog()).expect("request");
    let frame = project_frame(
        &snapshot,
        &before_current,
        &catalog(),
        FrameLimits::default(),
    )
    .expect("empty frame");
    assert_eq!(frame.quality.gated, ["events"]);
}

fn frame_many_event_fixture() -> tempfile::TempDir {
    let rows = (0..201)
        .map(|index| PgLogLifecycleV1 {
            ts: Ts(2_000),
            kind: 0,
            pid: Some(index),
            signal: Some(9),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        })
        .collect::<Vec<_>>();
    let body = PgLogLifecycleV1::encode(&rows).expect("encode bounded lifecycle fixture");
    let pgm = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: u32::try_from(rows.len()).expect("fixture rows"),
            body: &body,
        }],
        PartMeta {
            min_ts: 2_000,
            max_ts: 2_000,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "frame-many-events.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
    directory
}

fn statement_row(ts: i64, calls: i64, rows: i64, exec_ms: f64, plan_ms: f64) -> PgStatStatementsV2 {
    PgStatStatementsV2 {
        ts: Ts(ts),
        queryid: Some(7),
        userid: 10,
        dbid: 20,
        datname: None,
        usename: None,
        query: None,
        calls,
        rows,
        plans: calls,
        total_exec_time: exec_ms,
        total_plan_time: plan_ms,
        min_exec_time: 0.0,
        max_exec_time: 0.0,
        mean_exec_time: 0.0,
        stddev_exec_time: 0.0,
        min_plan_time: 0.0,
        max_plan_time: 0.0,
        mean_plan_time: 0.0,
        stddev_plan_time: 0.0,
        shared_blks_hit: 0,
        shared_blks_read: 0,
        shared_blks_dirtied: 0,
        shared_blks_written: 0,
        local_blks_hit: 0,
        local_blks_read: 0,
        local_blks_dirtied: 0,
        local_blks_written: 0,
        temp_blks_read: 0,
        temp_blks_written: 0,
        blk_read_time: 0.0,
        blk_write_time: 0.0,
        wal_records: 0,
        wal_fpi: 0,
        wal_bytes: 0,
    }
}

fn frame_statement_planning_fixture() -> tempfile::TempDir {
    frame_statement_planning_fixture_with_extra_current(false)
}

fn frame_statement_planning_fixture_with_extra_current(extra_current: bool) -> tempfile::TempDir {
    let mut interner =
        kronika_writer::Interner::new(DictLimits::new(32, 4_096).expect("dictionary limits"));
    let mut intern = |value: &str| {
        interner
            .intern(value.as_bytes())
            .map(|id| StrId(id.get()))
            .expect("intern settings fixture")
    };
    let settings = [PgSettingsV1 {
        ts: Ts(1_000),
        name: intern("pg_stat_statements.track_planning"),
        setting: intern("on"),
        unit: None,
        source: intern("configuration file"),
        sourcefile: None,
        sourceline: None,
        pending_restart: false,
        context: intern("sighup"),
        vartype: intern("bool"),
        boot_val: Some(intern("off")),
        reset_val: Some(intern("on")),
    }];
    let mut statements = vec![
        statement_row(1_500, 10, 20, 100.0, 25.0),
        statement_row(1_600, 12, 24, 120.0, 30.0),
    ];
    if extra_current {
        let mut extra = statement_row(1_600, 4, 8, 40.0, 10.0);
        extra.queryid = Some(8);
        statements.push(extra);
    }
    let settings_body = PgSettingsV1::encode(&settings).expect("encode settings fixture");
    let statements_body =
        PgStatStatementsV2::encode(&statements).expect("encode statements fixture");
    let dictionary =
        kronika_writer::dict::encode(interner.window()).expect("encode fixture dictionary");
    let mut sections = vec![
        SectionInput {
            type_id: 1_002_002,
            rows: u32::try_from(statements.len()).expect("statement rows"),
            body: &statements_body,
        },
        SectionInput {
            type_id: 1_019_001,
            rows: u32::try_from(settings.len()).expect("settings rows"),
            body: &settings_body,
        },
    ];
    sections.extend(dictionary.iter().map(|section| SectionInput {
        type_id: section.type_id,
        rows: section.rows,
        body: &section.body,
    }));
    sections.sort_unstable_by_key(|section| section.type_id);
    let pgm = build_part(
        &sections,
        PartMeta {
            min_ts: 1_000,
            max_ts: 1_600,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "frame-statements.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
    directory
}

fn frame_statement_two_segment_fixture(gap_us: i64) -> (tempfile::TempDir, i64) {
    let current_ts = 1_000 + gap_us;
    let directory = tempfile::tempdir().expect("tempdir");
    for (file, row) in [
        (
            "frame-statements-prev.pgm",
            statement_row(1_000, 10, 20, 100.0, 0.0),
        ),
        (
            "frame-statements-current.pgm",
            statement_row(current_ts, 20, 40, 200.0, 0.0),
        ),
    ] {
        let body = PgStatStatementsV2::encode(&[row]).expect("encode statement gap fixture");
        let pgm = build_part(
            &[SectionInput {
                type_id: 1_002_002,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: row.ts.0,
                max_ts: row.ts.0,
            },
        );
        crate::test_layout::write_named_pgm(directory.path(), file, &pgm);
    }
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
    (directory, current_ts)
}

#[test]
fn frame_uses_last_known_track_planning_from_the_same_exact_pgm() {
    let directory = frame_statement_planning_fixture();
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let request =
        FrameRequest::parse("statements", Some("at=1600"), &catalog()).expect("frame request");
    kronika_reader::qualification_reset_open_unit_calls();

    let frame = project_frame(
        &snapshot,
        &request,
        &catalog(),
        FrameLimits {
            rows: 1,
            ..FrameLimits::default()
        },
    )
    .expect("frame projection");

    assert_eq!(kronika_reader::qualification_open_unit_calls(), 1);
    let row = frame.rows.first().expect("statement row");
    let plan_column = row
        .values
        .iter()
        .position(|(column, _)| *column == "plan_time_pct")
        .expect("plan_time_pct column");
    assert_eq!(
        row.cells[plan_column],
        crate::ui::frame::dto::FrameValue::Number(20.0)
    );
    let classification = row
        .classifications
        .iter()
        .find(|classification| classification.column == "plan_time_pct")
        .expect("plan_time_pct classification");
    assert!(matches!(classification.result, Classified::Verdict(_)));
}

#[test]
fn frame_rejects_an_internally_paginated_exact_snapshot() {
    let directory = frame_statement_planning_fixture_with_extra_current(true);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let request =
        FrameRequest::parse("statements", Some("at=1600"), &catalog()).expect("frame request");

    let error = project_frame(
        &snapshot,
        &request,
        &catalog(),
        FrameLimits {
            rows: 1,
            cells: 2_000_000,
            bytes: 32 * 1024 * 1024,
        },
    )
    .expect_err("a partial exact snapshot must not be projected");
    assert!(matches!(
        error,
        crate::ui::frame::projection::FrameError::Query(kronika_reader::QueryError::RowsTooLarge {
            max_rows: 1
        })
    ));
}

#[test]
fn frame_does_not_open_or_use_a_predecessor_beyond_the_rate_gap() {
    let (directory, current_ts) = frame_statement_two_segment_fixture(16 * 60 * 1_000_000);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let raw = format!("at={current_ts}");
    let request = FrameRequest::parse("statements", Some(&raw), &catalog()).expect("request");
    kronika_reader::qualification_reset_open_unit_calls();

    let frame = project_frame(&snapshot, &request, &catalog(), FrameLimits::default())
        .expect("frame projection");

    assert_eq!(kronika_reader::qualification_open_unit_calls(), 1);
    assert_eq!(frame.predecessor_ts_us, None);
    assert_eq!(frame.neighbors.previous, Some(1_000));
    assert!(!frame.quality.gaps.is_empty());
    let row = frame.rows.first().expect("statement row");
    assert!(matches!(
        row.operands.statements,
        Some(StatementOperands {
            exec_ms: DeltaOperand::Gap,
            ..
        })
    ));
}

#[test]
fn frame_materialization_budget_is_shared_by_two_pgm_reads() {
    let (directory, current_ts) = frame_statement_two_segment_fixture(60 * 1_000_000);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let raw = format!("at={current_ts}");
    let request = FrameRequest::parse("statements", Some(&raw), &catalog()).expect("request");
    let one_statement_row_cells = kronika_reader::logical_section("pg_stat_statements")
        .expect("logical statements")
        .columns
        .len();

    let error = project_frame(
        &snapshot,
        &request,
        &catalog(),
        FrameLimits {
            rows: 2,
            cells: one_statement_row_cells,
            bytes: 32 * 1024 * 1024,
        },
    )
    .expect_err("two PGM reads exceed one request-wide materialization budget");

    assert!(matches!(
        error,
        crate::ui::frame::projection::FrameError::Query(
            kronika_reader::QueryError::ResultTooLarge { .. }
        )
    ));
}

#[test]
fn frame_spark_uses_the_selected_view_ovf_series() {
    let directory = frame_event_fixture();
    let state = super::state_for_dir(directory.path());
    let request =
        FrameRequest::parse("events", Some("at=1600&span=1ms"), &catalog()).expect("frame request");
    let request_snapshot = (*state.snapshot()).clone();
    kronika_reader::qualification_reset_open_unit_calls();
    let mut frame = project_frame(
        &request_snapshot,
        &request,
        &catalog(),
        FrameLimits::default(),
    )
    .expect("frame projection");
    assert_eq!(kronika_reader::qualification_open_unit_calls(), 1);
    assert_eq!(frame.rows.len(), 1);
    assert!(frame.rows[0].spark.values.is_empty());

    let live = std::sync::Arc::clone(state.overview_view().live());
    attach_sparks(&request_snapshot, &live, &request, &mut frame).expect("spark merge");

    assert_eq!(kronika_reader::qualification_open_unit_calls(), 1);
    assert_eq!(frame.rows[0].spark.values.len(), 60);
    assert!(frame.rows[0].spark.values.iter().any(Option::is_some));
}

#[tokio::test]
async fn frame_http_returns_bounded_classified_shape_and_rejects_before_io() {
    let directory = frame_event_fixture();
    let (status, body) = super::serve(directory.path(), "/v1/frame/events?at=1600&span=1ms").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["view"], "events");
    assert_eq!(body["snapshot_ts_us"], "1600");
    assert_eq!(body["columns"].as_array().map(Vec::len), Some(3));
    assert_eq!(body["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["rows"][0]["cells"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        body["rows"][0]["classifications"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        body["rows"][0]["spark"]["values"].as_array().map(Vec::len),
        Some(60)
    );
    assert!(serde_json::to_vec(&body).expect("serialize").len() <= 1_048_576);

    for uri in [
        "/v1/frame/missing?at=1",
        "/v1/frame/events?at=1&at=2",
        "/v1/frame/events?at=1&order=sideways",
    ] {
        let (_directory, status, body) = super::fixture_response(uri).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            matches!(
                body["code"].as_str(),
                Some(
                    "invalid_query_parameter"
                        | "duplicate_query_parameter"
                        | "unknown_query_parameter"
                )
            ),
            "{uri}: {body}"
        );
    }
}

#[tokio::test]
async fn frame_qualification_caps_rows_and_serialized_response() {
    let directory = frame_many_event_fixture();
    let (status, body) = super::serve(
        directory.path(),
        "/v1/frame/events?at=2000&span=1ms&limit=200",
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["page"]["matched"], 201);
    assert_eq!(body["page"]["returned"], 200);
    assert!(body["page"]["next"].is_string());
    assert!(serde_json::to_vec(&body).expect("serialize").len() <= 1_048_576);
}

#[tokio::test]
async fn frame_event_cursor_tiles_every_matching_row() {
    let directory = frame_many_event_fixture();
    let mut cursor: Option<String> = None;
    let mut returned = 0_usize;
    for _page in 0..3 {
        let mut uri = "/v1/frame/events?at=2000&span=1ms&limit=200".to_owned();
        if let Some(value) = &cursor {
            uri.push_str("&cursor=");
            uri.push_str(value);
        }
        let (status, body) = super::serve(directory.path(), &uri).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        returned += body["page"]["returned"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .expect("returned count");
        cursor = body["page"]["next"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(returned, 201);
    assert!(cursor.is_none());
}

#[tokio::test]
async fn frame_rejects_a_missing_cursor_anchor_as_invalid_cursor() {
    let directory = frame_many_event_fixture();
    let uri = "/v1/frame/events?at=2000&span=1ms&limit=200";
    let (status, body) = super::serve(directory.path(), uri).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let cursor = FrameCursor::decode(body["page"]["next"].as_str().expect("first page cursor"))
        .expect("decode first page cursor");
    let changed = FrameCursor::new(
        cursor.view_code(),
        cursor.view_revision(),
        cursor.snapshot_ts_us(),
        cursor.query_fingerprint(),
        cursor.sort_key().clone(),
        vec![u8::MAX; cursor.entity().len()],
    )
    .expect("changed bounded anchor")
    .encode()
    .expect("encode changed anchor");

    let uri = format!("{uri}&cursor={changed}");
    let (status, body) = super::serve(directory.path(), &uri).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");
}

fn operand_row(operands: RowOperands) -> ProjectedRow {
    ProjectedRow {
        entity: vec![1],
        spark_entity: vec![1],
        label: "row".to_owned(),
        cells: Vec::new(),
        operands,
        classifications: Vec::new(),
        spark: crate::ui::frame::dto::SparkDto {
            values: Vec::new(),
            complete: false,
        },
        values: Vec::new(),
        database: None,
        searchable: String::new(),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive table keeps all fourteen adapters reviewable together"
)]
fn threshold_inputs_cover_all_fourteen_typed_adapters() {
    let mut operands = RowOperands {
        snapshot_ts_us: 30_000_000,
        activity_state: Some("active".to_owned()),
        query_start_us: Some(20_000_000),
        transaction_start_us: Some(10_000_000),
        statements: Some(StatementOperands {
            calls: DeltaOperand::Value(2.0),
            rows: DeltaOperand::Value(4.0),
            exec_ms: DeltaOperand::Value(20.0),
            plan_ms: DeltaOperand::Value(5.0),
            planning_fields: true,
            track_planning: true,
            total_exec_ms: Some(100.0),
            snapshot_exec_ms_delta_sum: Some(80.0),
        }),
        table: Some(TableOperands {
            live: Some(90.0),
            dead: Some(10.0),
            seq_scan: DeltaOperand::Value(3.0),
            idx_scan: DeltaOperand::Value(1.0),
            modified: Some(12_000.0),
            inserted: Some(Some(4_000.0)),
            last_autovacuum_us: Some(20_000_000),
            last_autoanalyze_us: Some(15_000_000),
        }),
        process_rss_kib: Some(1_024.0),
    };
    let context = FrameThresholdContext;
    let row = operand_row(operands.clone());
    let cases = [
        (
            OperandKind::ActivityQueryDuration,
            MetricInput::Scalar(10.0),
        ),
        (
            OperandKind::ActivityTransactionDuration,
            MetricInput::Scalar(20.0),
        ),
        (
            OperandKind::StatementMillisecondsPerRow,
            MetricInput::Scalar(5.0),
        ),
        (
            OperandKind::StatementMeanMilliseconds,
            MetricInput::Scalar(10.0),
        ),
        (OperandKind::StatementTimePercent, MetricInput::Scalar(25.0)),
        (
            OperandKind::StatementPlanTimePercent,
            MetricInput::Scalar(20.0),
        ),
        (
            OperandKind::TableDeadTupleRatio,
            MetricInput::RatioWithFloor {
                ratio: 0.1,
                count: 10.0,
            },
        ),
        (OperandKind::TableDeadTuples, MetricInput::Scalar(10.0)),
        (
            OperandKind::TableSequentialScanPercent,
            MetricInput::Scalar(75.0),
        ),
        (
            OperandKind::TableModifiedSinceAnalyze,
            MetricInput::Scalar(12_000.0),
        ),
        (
            OperandKind::TableInsertedSinceVacuum,
            MetricInput::Scalar(4_000.0),
        ),
        (
            OperandKind::TableAutovacuumAge,
            MetricInput::Age {
                epoch_seconds: 20.0,
                now_seconds: 30.0,
                gate: true,
            },
        ),
        (
            OperandKind::TableAutoanalyzeAge,
            MetricInput::Age {
                epoch_seconds: 15.0,
                now_seconds: 30.0,
                gate: true,
            },
        ),
        (OperandKind::ProcessRssKib, MetricInput::Scalar(1_024.0)),
    ];
    for (kind, expected) in cases {
        assert_eq!(prepare_input(kind, &row, &context), expected, "{kind:?}");
    }

    operands.activity_state = Some("idle".to_owned());
    assert_eq!(
        prepare_input(
            OperandKind::ActivityQueryDuration,
            &operand_row(operands.clone()),
            &context,
        ),
        MetricInput::NotApplicable
    );
    operands.statements.as_mut().expect("statements").calls = DeltaOperand::Reset;
    assert_eq!(
        prepare_input(
            OperandKind::StatementMeanMilliseconds,
            &operand_row(operands.clone()),
            &context,
        ),
        MetricInput::Missing
    );
    operands.table.as_mut().expect("table").inserted = None;
    assert_eq!(
        prepare_input(
            OperandKind::TableInsertedSinceVacuum,
            &operand_row(operands),
            &context,
        ),
        MetricInput::NotApplicable
    );
}

fn warning_boundary(metric_id: MetricId) -> Boundary {
    let scalar = match catalog_entry(metric_id).policy {
        Policy::Scalar(policy) => policy,
        Policy::RatioWithFloor(policy) => policy.ratio(),
        Policy::AgeGated(policy) => policy.scalar(),
        policy => panic!("unexpected bound frame policy: {policy:?}"),
    };
    scalar.warning().expect("bound frame warning boundary")
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "bounded catalog seconds fit i64 microseconds; each adapter needs distinct operands"
)]
fn boundary_row(kind: OperandKind, observed: f64, metric_id: MetricId) -> ProjectedRow {
    let now_us = 2_000_000_000_000_i64;
    let duration_start = || now_us - (observed * 1_000_000.0).round() as i64;
    let mut operands = RowOperands {
        snapshot_ts_us: now_us,
        ..RowOperands::default()
    };

    match kind {
        OperandKind::ActivityQueryDuration => {
            operands.activity_state = Some("active".to_owned());
            operands.query_start_us = Some(duration_start());
        }
        OperandKind::ActivityTransactionDuration => {
            operands.transaction_start_us = Some(duration_start());
        }
        OperandKind::StatementMillisecondsPerRow => {
            operands.statements = Some(StatementOperands {
                rows: DeltaOperand::Value(1.0),
                exec_ms: DeltaOperand::Value(observed),
                ..StatementOperands::default()
            });
        }
        OperandKind::StatementMeanMilliseconds => {
            operands.statements = Some(StatementOperands {
                calls: DeltaOperand::Value(1.0),
                exec_ms: DeltaOperand::Value(observed),
                ..StatementOperands::default()
            });
        }
        OperandKind::StatementTimePercent => {
            operands.statements = Some(StatementOperands {
                exec_ms: DeltaOperand::Value(observed),
                snapshot_exec_ms_delta_sum: Some(100.0),
                ..StatementOperands::default()
            });
        }
        OperandKind::StatementPlanTimePercent => {
            operands.statements = Some(StatementOperands {
                exec_ms: DeltaOperand::Value(100.0 - observed),
                plan_ms: DeltaOperand::Value(observed),
                planning_fields: true,
                track_planning: true,
                ..StatementOperands::default()
            });
        }
        OperandKind::TableDeadTupleRatio => {
            let floor = match catalog_entry(metric_id).policy {
                Policy::RatioWithFloor(policy) => policy.floor().value,
                policy => panic!("unexpected dead tuple policy: {policy:?}"),
            };
            let dead = floor + floor.max(1.0);
            operands.table = Some(TableOperands {
                live: Some(dead * (1.0 - observed) / observed),
                dead: Some(dead),
                ..TableOperands::default()
            });
        }
        OperandKind::TableDeadTuples => {
            operands.table = Some(TableOperands {
                dead: Some(observed),
                ..TableOperands::default()
            });
        }
        OperandKind::TableSequentialScanPercent => {
            operands.table = Some(TableOperands {
                seq_scan: DeltaOperand::Value(observed),
                idx_scan: DeltaOperand::Value(100.0 - observed),
                ..TableOperands::default()
            });
        }
        OperandKind::TableModifiedSinceAnalyze => {
            operands.table = Some(TableOperands {
                modified: Some(observed),
                ..TableOperands::default()
            });
        }
        OperandKind::TableInsertedSinceVacuum => {
            operands.table = Some(TableOperands {
                inserted: Some(Some(observed)),
                ..TableOperands::default()
            });
        }
        OperandKind::TableAutovacuumAge => {
            operands.table = Some(TableOperands {
                dead: Some(1.0),
                last_autovacuum_us: Some(duration_start()),
                ..TableOperands::default()
            });
        }
        OperandKind::TableAutoanalyzeAge => {
            operands.table = Some(TableOperands {
                modified: Some(10_000.0),
                last_autoanalyze_us: Some(duration_start()),
                ..TableOperands::default()
            });
        }
        OperandKind::ProcessRssKib => operands.process_rss_kib = Some(observed),
    }

    operand_row(operands)
}

fn prepared_observed(input: MetricInput) -> f64 {
    match input {
        MetricInput::Scalar(value) => value,
        MetricInput::RatioWithFloor { ratio, .. } => ratio,
        MetricInput::Age {
            epoch_seconds,
            now_seconds,
            gate: true,
        } => now_seconds - epoch_seconds,
        input => panic!("unexpected prepared boundary input: {input:?}"),
    }
}

#[test]
fn every_bound_adapter_preserves_below_on_and_above_boundary_semantics() {
    let cases = [
        (
            OperandKind::ActivityQueryDuration,
            MetricId::PgActivityQueryDurationSeconds,
        ),
        (
            OperandKind::ActivityTransactionDuration,
            MetricId::PgActivityTransactionDurationSeconds,
        ),
        (
            OperandKind::StatementMillisecondsPerRow,
            MetricId::PgStatementsMillisecondsPerRow,
        ),
        (
            OperandKind::StatementMeanMilliseconds,
            MetricId::PgStatementsMeanTimeMilliseconds,
        ),
        (
            OperandKind::StatementTimePercent,
            MetricId::PgStatementsTimePercent,
        ),
        (
            OperandKind::StatementPlanTimePercent,
            MetricId::PgStatementsPlanTimePercent,
        ),
        (
            OperandKind::TableDeadTupleRatio,
            MetricId::PgTablesDeadTuplePercent,
        ),
        (OperandKind::TableDeadTuples, MetricId::PgTablesDeadTuples),
        (
            OperandKind::TableSequentialScanPercent,
            MetricId::PgTablesSequentialScanPercent,
        ),
        (
            OperandKind::TableModifiedSinceAnalyze,
            MetricId::PgTablesModifiedSinceAnalyze,
        ),
        (
            OperandKind::TableInsertedSinceVacuum,
            MetricId::PgTablesInsertedSinceVacuum,
        ),
        (
            OperandKind::TableAutovacuumAge,
            MetricId::PgTablesAutovacuumAgeSeconds,
        ),
        (
            OperandKind::TableAutoanalyzeAge,
            MetricId::PgTablesAutoanalyzeAgeSeconds,
        ),
        (OperandKind::ProcessRssKib, MetricId::OsProcessRssKib),
    ];
    let context = FrameThresholdContext;

    for (kind, metric_id) in cases {
        let boundary = warning_boundary(metric_id);
        let step = (boundary.value * 1e-6).max(0.001);
        for (position, observed) in [
            ("below", boundary.value - step),
            ("on", boundary.value),
            ("above", boundary.value + step),
        ] {
            let input = prepare_input(kind, &boundary_row(kind, observed, metric_id), &context);
            let actual = prepared_observed(input);
            assert!(
                (actual - observed).abs() <= step * 1e-3,
                "{kind:?} {position}: expected {observed}, got {actual}"
            );
            let Classified::Verdict(verdict) = classify(metric_id, input) else {
                panic!("{kind:?} {position}: adapter did not classify");
            };
            let expected = match (position, boundary.operator) {
                ("below", _) | ("on", Comparison::Above) => Level::Ok,
                ("on", Comparison::AtLeast) | ("above", _) => Level::Warning,
                (_, Comparison::Below | Comparison::AtMost) => {
                    panic!("unexpected lower-is-worse frame boundary")
                }
                _ => unreachable!(),
            };
            assert_eq!(verdict.level, expected, "{kind:?} {position}");
        }
    }
}
