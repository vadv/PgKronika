use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::{
    Boundary, Classified, Comparison, Evidence, Level, NotClassifiedReason, Verdict,
};

use crate::api_error::ErrorCode;
use crate::ui::catalog::ProjectionCatalog;
use crate::ui::frame::FrameRequest;
use crate::ui::frame::cursor::{FrameCursor, SortKey};
use crate::ui::frame::dto::ClassificationResultDto;

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
