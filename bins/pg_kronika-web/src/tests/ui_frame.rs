use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::{
    Boundary, Classified, Comparison, Evidence, Level, MetricInput, NotClassifiedReason, Verdict,
};
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot, OutRow, Value};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};

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
use crate::ui::thresholds::OperandKind;

fn catalog() -> ProjectionCatalog {
    ProjectionCatalog::for_type_ids(&BTreeSet::new())
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
fn frame_spark_uses_the_selected_view_ovf_series() {
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
    crate::test_layout::write_named_pgm(directory.path(), "frame-events.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
    let state = super::state_for_dir(directory.path());
    let request =
        FrameRequest::parse("events", Some("at=1600&span=1ms"), &catalog()).expect("frame request");
    let mut request_snapshot = (*state.snapshot()).clone();
    let mut frame = project_frame(
        &mut request_snapshot,
        &request,
        &catalog(),
        FrameLimits::default(),
    )
    .expect("frame projection");
    assert_eq!(frame.rows.len(), 1);
    assert!(frame.rows[0].spark.values.is_empty());

    let live = std::sync::Arc::clone(state.overview_view().live());
    attach_sparks(&request_snapshot, &live, &request, &mut frame).expect("spark merge");

    assert_eq!(frame.rows[0].spark.values.len(), 60);
    assert!(frame.rows[0].spark.values.iter().any(Option::is_some));
}

fn operand_row(operands: RowOperands) -> ProjectedRow {
    ProjectedRow {
        entity: vec![1],
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
