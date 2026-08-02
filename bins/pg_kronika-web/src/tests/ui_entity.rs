use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_format::DictLimits;
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_stat_statements::PgStatStatementsV2;
use kronika_registry::pg_store_plans::{PgStorePlansOsscV1, PgStorePlansVadvV1};
use kronika_writer::{Interner, dict};

use crate::api_error::ErrorCode;
use crate::ui::catalog::ProjectionCatalog;
use crate::ui::entity::{EntityMode, EntityRequest};

use super::*;

fn catalog() -> ProjectionCatalog {
    ProjectionCatalog::for_type_ids(&BTreeSet::new())
}

fn token(revision: u16) -> String {
    let mut bytes = revision.to_le_bytes().to_vec();
    bytes.push(1);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn statement_row(ts: i64, calls: i64, query: StrId) -> PgStatStatementsV2 {
    let calls_f64 = f64::from(i32::try_from(calls).expect("fixture calls fit i32"));
    PgStatStatementsV2 {
        ts: Ts(ts),
        queryid: Some(7),
        userid: 10,
        dbid: 20,
        datname: None,
        usename: None,
        query: Some(query),
        calls,
        rows: calls * 2,
        plans: calls,
        total_exec_time: calls_f64 * 10.0,
        total_plan_time: calls_f64,
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

fn plan_row(ts: i64, calls: i64, plan: StrId) -> PgStorePlansOsscV1 {
    let calls_f64 = f64::from(i32::try_from(calls).expect("fixture calls fit i32"));
    PgStorePlansOsscV1 {
        ts: Ts(ts),
        queryid: 7,
        planid: 99,
        userid: 10,
        dbid: 20,
        datname: None,
        usename: None,
        plan: Some(plan),
        calls,
        total_time: calls_f64 * 5.0,
        min_time: 0.0,
        max_time: 0.0,
        mean_time: 0.0,
        stddev_time: 0.0,
        rows: calls,
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
        shared_blk_read_time: 0.0,
        shared_blk_write_time: 0.0,
        local_blk_read_time: 0.0,
        local_blk_write_time: 0.0,
        temp_blk_read_time: 0.0,
        temp_blk_write_time: 0.0,
        first_call: Ts(1_000),
        last_call: Ts(ts),
    }
}

fn vadv_plan_row(ts: i64, calls: i64, plan: StrId) -> PgStorePlansVadvV1 {
    let calls_f64 = f64::from(i32::try_from(calls).expect("fixture calls fit i32"));
    PgStorePlansVadvV1 {
        ts: Ts(ts),
        queryid_stat_statements: 7,
        planid: 101,
        userid: 10,
        dbid: 20,
        datname: None,
        usename: None,
        plan: Some(plan),
        calls,
        slow_log_calls: 0,
        total_time: calls_f64 * 5.0,
        min_time: 0.0,
        max_time: 0.0,
        mean_time: 0.0,
        stddev_time: 0.0,
        rows: calls,
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
        first_call: Ts(1_000),
        last_call: Ts(ts),
        total_plan_time: 0.0,
        min_plan_time: 0.0,
        max_plan_time: 0.0,
        mean_plan_time: 0.0,
    }
}

#[derive(Clone, Copy)]
enum PlanFork {
    Ossc,
    Vadv,
}

fn entity_fixture(plan_fork: PlanFork) -> tempfile::TempDir {
    let mut interner = Interner::new(DictLimits::new(16, 4_096).expect("dictionary limits"));
    let query = interner
        .intern(b"select * from orders")
        .map(|id| StrId(id.get()))
        .expect("intern query");
    let plan = interner
        .intern(b"Seq Scan on orders")
        .map(|id| StrId(id.get()))
        .expect("intern plan");
    let rows = [
        statement_row(1_000, 10, query),
        statement_row(2_000, 25, query),
    ];
    let mut coincident_plan = plan_row(2_000, 5, plan);
    coincident_plan.queryid = 8;
    let plans = [
        plan_row(1_500, 2, plan),
        plan_row(2_000, 3, plan),
        coincident_plan,
    ];
    let statements = PgStatStatementsV2::encode(&rows).expect("encode statements");
    let plans_body = PgStorePlansOsscV1::encode(&plans).expect("encode plans");
    let vadv_plans_body =
        PgStorePlansVadvV1::encode(&[vadv_plan_row(2_000, 4, plan)]).expect("encode vadv plans");
    let dictionary = dict::encode(interner.window()).expect("encode dictionary");
    let mut sections = vec![SectionInput {
        type_id: 1_002_002,
        rows: u32::try_from(rows.len()).expect("statement rows"),
        body: &statements,
    }];
    let (type_id, rows, body) = match plan_fork {
        PlanFork::Ossc => (
            1_003_001,
            u32::try_from(plans.len()).expect("plan rows"),
            &plans_body,
        ),
        PlanFork::Vadv => (1_004_001, 1, &vadv_plans_body),
    };
    sections.push(SectionInput {
        type_id,
        rows,
        body,
    });
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
            max_ts: 2_000,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "entity-statements.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish entity web index");
    }
    directory
}

fn event_entity_fixture() -> tempfile::TempDir {
    let rows = [PgLogLifecycleV1 {
        ts: Ts(2_000),
        kind: 0,
        pid: Some(42),
        signal: Some(15),
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode event");
    let pgm = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: 2_000,
            max_ts: 2_000,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "entity-event.pgm", &pgm);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish event web index");
    }
    directory
}

async fn frame_entity_token(directory: &std::path::Path) -> String {
    let (status, body) = serve(
        directory,
        "/v1/frame/statements?at=2000&preset=time&limit=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["rows"][0]["entity"]
        .as_str()
        .expect("frame entity token")
        .to_owned()
}

#[test]
fn entity_request_requires_exactly_one_point_or_history_mode() {
    let catalog = catalog();
    let activity = token(1);
    let statements = token(1);

    let point =
        EntityRequest::parse("activity", &activity, Some("at=1"), &catalog).expect("point request");
    assert!(matches!(point.mode, EntityMode::Point { .. }));

    let history = EntityRequest::parse(
        "statements",
        &statements,
        Some("from=1&to=2&columns=queryid"),
        &catalog,
    )
    .expect("history request");
    assert!(matches!(history.mode, EntityMode::History { .. }));

    for raw in [
        "",
        "at=1&from=1&to=2&columns=queryid",
        "from=1&to=2",
        "at=1&columns=queryid",
    ] {
        let error =
            EntityRequest::parse("statements", &statements, Some(raw), &catalog).expect_err(raw);
        assert_eq!(error.code(), ErrorCode::InvalidQueryConstraint);
    }
}

#[test]
fn entity_request_rejects_bad_revision_and_ephemeral_history_before_io() {
    let catalog = catalog();
    let bad = EntityRequest::parse("statements", &token(7), Some("at=1"), &catalog)
        .expect_err("identity revision mismatch");
    assert_eq!(bad.code(), ErrorCode::InvalidQueryParameter);

    let events = EntityRequest::parse(
        "events",
        &token(1),
        Some("from=1&to=2&columns=time"),
        &catalog,
    )
    .expect_err("events history");
    assert_eq!(events.code(), ErrorCode::InvalidQueryConstraint);
}

#[tokio::test]
async fn entity_point_returns_lazy_fields_and_only_proven_related_links() {
    let directory = entity_fixture(PlanFork::Ossc);
    let entity = frame_entity_token(directory.path()).await;
    let uri = format!("/v1/entity/statements/{entity}?at=2000&include=related");

    let (status, body) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    let query = body["fields"]
        .as_array()
        .expect("point fields")
        .iter()
        .find(|field| field["code"] == "query")
        .expect("lazy query field");
    assert_eq!(query["value"], "select * from orders");
    assert!(query.get("status").is_none());
    assert!(query.get("reason").is_none());
    let related = body["related"].as_array().expect("related links");
    // The time-coincident OSSC plan with a different queryid must not link.
    assert_eq!(related.len(), 1);
    assert!(
        related
            .iter()
            .all(|relation| relation["relation"] == "statement_plan")
    );
    assert!(related.iter().all(|relation| relation["view"] == "plans"));
    assert!(related.iter().any(|relation| {
        relation["provenance"]
            == serde_json::json!({
                "kind": "best_effort",
                "method": "ossc_queryid_dbid_userid_attribution",
                "fields": ["queryid", "dbid", "userid"]
            })
    }));
    let directory = entity_fixture(PlanFork::Vadv);
    let entity = frame_entity_token(directory.path()).await;
    let uri = format!("/v1/entity/statements/{entity}?at=2000&include=related");
    let (status, body) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    let related = body["related"].as_array().expect("vadv related links");
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["relation"], "statement_plan");
    assert_eq!(related[0]["view"], "plans");
    assert_eq!(
        related[0]["provenance"],
        serde_json::json!({
            "kind": "best_effort",
            "method": "vadv_queryid_stat_statements_dbid_userid_attribution",
            "fields": ["queryid_stat_statements", "dbid", "userid"]
        })
    );
}

#[test]
fn entity_request_rejects_unknown_and_duplicate_parameters_before_io() {
    let catalog = catalog();
    let token = token(1);

    let unknown = EntityRequest::parse("statements", &token, Some("at=1&bogus=1"), &catalog)
        .expect_err("unknown parameter");
    assert_eq!(unknown.code(), ErrorCode::UnknownQueryParameter);

    let duplicate = EntityRequest::parse("statements", &token, Some("at=1&at=2"), &catalog)
        .expect_err("duplicate parameter");
    assert_eq!(duplicate.code(), ErrorCode::DuplicateQueryParameter);
}

#[tokio::test]
async fn entity_point_for_absent_identity_returns_entity_not_found() {
    let directory = entity_fixture(PlanFork::Ossc);
    let uri = format!("/v1/entity/statements/{}?at=2000", token(1));

    let (status, body) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "entity_not_found");
}

#[tokio::test]
async fn entity_history_tiles_view_snapshots_without_duplicates() {
    let directory = entity_fixture(PlanFork::Ossc);
    let entity = frame_entity_token(directory.path()).await;
    let uri =
        format!("/v1/entity/statements/{entity}?from=1000&to=2001&columns=queryid,calls&limit=1");

    let (status, first) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first["snapshots"]
            .as_array()
            .expect("first snapshots")
            .iter()
            .map(|snapshot| snapshot["ts_us"].as_str().expect("timestamp"))
            .collect::<Vec<_>>(),
        ["1000"]
    );
    let cursor = first["page"]["next"].as_str().expect("history cursor");

    let second_uri = format!("{uri}&cursor={cursor}");
    let (status, second) = serve(directory.path(), &second_uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["snapshots"][0]["ts_us"], "2000");
    assert_eq!(second["page"]["next"], serde_json::Value::Null);

    let mismatch = format!(
        "/v1/entity/statements/{entity}?from=1000&to=2001&columns=queryid&limit=1&cursor={cursor}"
    );
    let (status, body) = serve(directory.path(), &mismatch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "cursor_query_mismatch");
}

#[tokio::test]
async fn entity_history_marks_absent_entity_as_null_without_status_fields() {
    let directory = entity_fixture(PlanFork::Ossc);
    let uri = format!(
        "/v1/entity/statements/{}?from=1000&to=2001&columns=queryid&limit=10",
        token(1)
    );
    let (status, body) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["snapshots"]
            .as_array()
            .expect("snapshots")
            .iter()
            .map(|snapshot| snapshot["ts_us"].as_str().expect("timestamp"))
            .collect::<Vec<_>>(),
        ["1000", "2000"]
    );
    assert_eq!(body["snapshots"][0]["values"][0], serde_json::Value::Null);
    assert!(body["snapshots"][0].get("statuses").is_none());
    assert!(body["snapshots"][0].get("reasons").is_none());
    assert_eq!(body["quality"]["gaps"], serde_json::json!([]));
}

#[tokio::test]
async fn snapshot_bound_event_token_resolves_only_its_frame_snapshot() {
    let directory = event_entity_fixture();
    let (status, frame) = serve(directory.path(), "/v1/frame/events?at=2000").await;
    assert_eq!(status, StatusCode::OK);
    let entity = frame["rows"][0]["entity"].as_str().expect("event token");

    let uri = format!("/v1/entity/events/{entity}?at=2000");
    let (status, body) = serve(directory.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "point");
    assert_eq!(body["snapshot_ts_us"], "2000");

    let malformed = "/v1/entity/events/not-base64!?at=2000";
    let (status, body) = serve(directory.path(), malformed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["params"]["parameter"], "entity");
}
