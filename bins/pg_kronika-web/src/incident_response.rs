//! JSON contract for incident clustering responses.

use std::collections::BTreeMap;

use kronika_reader::Gap;
use serde_json::{Value, json};

use crate::anomaly::ScanParams;
use crate::incident::{
    DormantLens, EngineOutcome, EngineSkip, EpisodeRefV1, Finding, IdentityValue, Incident,
    LimitAxis,
};
use crate::incident_input::{InputQuality, SectionSkip, SkipReason};

pub(crate) fn no_data_response(source: u64, scan: &ScanParams, data_age: Option<u64>) -> Value {
    let quality = InputQuality::default();
    json!({
        "source_id": source,
        "from": scan.from,
        "to": scan.to,
        "complete": false,
        "clustering_complete": false,
        "analysis_status": "no_data",
        "incidents": Value::Array(Vec::new()),
        "coverage_by_section": json!({}),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(),
        "data_quality": quality_to_json(&quality, "unknown"),
        "skipped": skipped_to_json(&[], &[], 0, &quality, Some("no_data")),
    })
}

pub(crate) fn identity_response(
    source: u64,
    scan: &ScanParams,
    data_age: Option<u64>,
    reason: &'static str,
) -> Value {
    let quality = InputQuality::default();
    json!({
        "source_id": source,
        "from": scan.from,
        "to": scan.to,
        "complete": false,
        "clustering_complete": false,
        "analysis_status": reason,
        "incidents": Value::Array(Vec::new()),
        "coverage_by_section": json!({}),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(),
        "data_quality": quality_to_json(&quality, reason),
        "skipped": skipped_to_json(&[], &[], 0, &quality, Some(reason)),
    })
}

pub(crate) fn build_response(
    source: u64,
    scan: &ScanParams,
    data_age: Option<u64>,
    outcome: &EngineOutcome,
    coverage: &BTreeMap<&'static str, Vec<Gap>>,
    quality: &InputQuality,
    input_skipped: &[SectionSkip],
) -> Value {
    let incidents: Vec<Value> = outcome.incidents.iter().map(incident_to_json).collect();
    let clustering_complete =
        outcome.complete && input_skipped.is_empty() && quality.episodes_truncated == 0;
    let analysis_status = if !clustering_complete {
        "partial"
    } else if quality.evaluated_positions == 0 {
        "insufficient_data"
    } else if incidents.is_empty() {
        "calm"
    } else {
        "incidents_detected"
    };
    json!({
        "source_id": source,
        "from": scan.from,
        "to": scan.to,
        "complete": false,
        "clustering_complete": clustering_complete,
        "analysis_status": analysis_status,
        "incidents": incidents,
        "coverage_by_section": coverage_to_json(coverage),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(),
        "data_quality": quality_to_json(quality, "available"),
        "skipped": skipped_to_json(
            input_skipped,
            &outcome.skipped,
            outcome.span_splits,
            quality,
            None,
        ),
    })
}

fn incident_to_json(incident: &Incident) -> Value {
    let members: Vec<Value> = incident.members.iter().map(member_to_json).collect();
    let findings: Vec<Value> = incident.findings.iter().map(finding_to_json).collect();
    json!({
        "interval": { "from": incident.start_us, "to": incident.end_us },
        "incident_key": hex(incident.key.canonical_bytes()),
        "members": members,
        "findings": findings,
        "evaluation_complete": incident.evaluation_complete,
        "finding_evaluation_status": if incident.evaluation_complete {
            "complete"
        } else {
            "partial"
        },
    })
}

fn finding_to_json(finding: &Finding) -> Value {
    let scope = finding.scope();
    let identity: Vec<Value> = scope.identity().iter().map(identity_to_json).collect();
    let evidence: Vec<Value> = finding
        .evidence()
        .iter()
        .map(|item| Value::from(item.label()))
        .collect();
    json!({
        "lens_id": finding.lens_id(),
        "role": finding.role().label(),
        "confidence": finding.confidence().label(),
        "scope": {
            "logical_section": scope.logical_section(),
            "column": scope.column(),
            "identity": identity,
        },
        "evidence": evidence,
    })
}

fn member_to_json(member: &EpisodeRefV1) -> Value {
    let identity: Vec<Value> = member.identity.iter().map(identity_to_json).collect();
    json!({
        "logical_section": member.logical_section,
        "column": member.column,
        "identity": identity,
        "from": member.start_us,
        "to": member.end_us,
    })
}

fn identity_to_json(value: &IdentityValue) -> Value {
    match value {
        IdentityValue::I64(v) => (*v).into(),
        IdentityValue::U64(v) => (*v).into(),
        IdentityValue::Bool(v) => (*v).into(),
        IdentityValue::Text(v) => Value::String(v.clone()),
    }
}

fn catalog_to_json() -> Value {
    let applied_ids = crate::incident::active_catalog_ids();
    let applied: Vec<Value> = applied_ids.iter().copied().map(Value::from).collect();
    json!({
        "status": "partial",
        "requirements_status": "incomplete",
        "diagnosis_available": !applied.is_empty(),
        "scope": "diagnostic_lenses",
        "applied": applied,
        "dormant": dormant_entries(crate::incident::dormant_catalog(), &applied_ids),
    })
}

fn dormant_entries(catalog: &'static [DormantLens], applied: &[&'static str]) -> Vec<Value> {
    catalog
        .iter()
        .filter(|lens| !applied.contains(&lens.lens_id()))
        .map(|lens| {
            let awaiting: Vec<_> = lens
                .missing()
                .iter()
                .map(|capability| capability.as_str())
                .collect();
            json!({
                "lens_id": lens.lens_id(),
                "slug": lens.slug(),
                "domain": lens.domain().as_str(),
                "title": lens.title(),
                "question": lens.detects(),
                "text_locale": "ru",
                "confidence_cap": lens.confidence().as_str(),
                "awaiting": awaiting,
                "requirements_status": "incomplete",
            })
        })
        .collect()
}

fn quality_to_json(quality: &InputQuality, node_identity: &str) -> Value {
    json!({
        "node_identity": node_identity,
        "non_canonical_identity": quality.non_canonical_identity,
        "non_finite_points": quality.non_finite_points,
        "first_points": quality.first_points,
        "resets": quality.resets,
        "gaps": quality.gaps,
        "not_collected": quality.not_collected,
        "anomalous_points": quality.anomalous_points,
        "invalid_gauge_points": quality.invalid_gauge_points,
        "duplicate_timestamps": quality.duplicate_timestamps,
        "evaluated_positions": quality.evaluated_positions,
        "unevaluated_positions": quality.unevaluated_positions,
        "episodes_truncated": quality.episodes_truncated,
    })
}

fn coverage_to_json(coverage: &BTreeMap<&'static str, Vec<Gap>>) -> Value {
    let object: serde_json::Map<String, Value> = coverage
        .iter()
        .map(|(&section, gaps)| {
            let gaps: Vec<Value> = gaps
                .iter()
                .map(|gap| json!({ "from": gap.from, "to": gap.to }))
                .collect();
            (section.to_owned(), json!({ "gaps": gaps }))
        })
        .collect();
    Value::Object(object)
}

fn skipped_to_json(
    input_skipped: &[SectionSkip],
    engine_skipped: &[EngineSkip],
    span_splits: u64,
    quality: &InputQuality,
    analysis_reason: Option<&str>,
) -> Value {
    let mut sections: Vec<Value> = input_skipped.iter().map(section_skip_to_json).collect();
    sections.sort_by(|left, right| left["section"].as_str().cmp(&right["section"].as_str()));
    let evaluations: Vec<Value> = engine_skipped.iter().map(engine_skip_to_json).collect();
    let mut analysis = Vec::new();
    if quality.episodes_truncated > 0 {
        analysis.push(json!({
            "scope": "episodes",
            "reason": {
                "kind": "retention_limit",
                "dropped": quality.episodes_truncated,
            },
        }));
    }
    if let Some(reason) = analysis_reason {
        analysis.push(json!({
            "scope": "request",
            "reason": { "kind": reason },
        }));
    }
    json!({
        "sections": sections,
        "evaluations": evaluations,
        "analysis": analysis,
        "span_splits": span_splits,
    })
}

fn section_skip_to_json(skip: &SectionSkip) -> Value {
    let reason = match skip.reason {
        SkipReason::MaterializationLimit { limit } => {
            json!({ "kind": "materialization_limit", "limit": limit })
        }
        SkipReason::IncompletePage => json!({ "kind": "incomplete_page" }),
        SkipReason::ScanBudget {
            required,
            available,
        } => json!({ "kind": "scan_budget", "required": required, "available": available }),
        SkipReason::ConflictingTimestamp { timestamp } => {
            json!({ "kind": "conflicting_timestamp", "timestamp": timestamp })
        }
        SkipReason::IdentityByteLimit { observed, limit } => {
            json!({ "kind": "identity_byte_limit", "observed": observed, "limit": limit })
        }
        SkipReason::SeriesPointLimit { observed, limit } => {
            json!({ "kind": "series_point_limit", "observed": observed, "limit": limit })
        }
    };
    json!({ "section": skip.section, "reason": reason })
}

fn engine_skip_to_json(skip: &EngineSkip) -> Value {
    json!({
        "lens_id": skip.lens_id.map_or(Value::Null, |id| Value::String(id.to_owned())),
        "axis": axis_name(skip.limit.axis),
        "observed": skip.limit.observed,
        "limit": skip.limit.limit,
    })
}

const fn axis_name(axis: LimitAxis) -> &'static str {
    match axis {
        LimitAxis::Work => "work",
        LimitAxis::LensEvaluations => "lens_evaluations",
        LimitAxis::Findings => "findings",
        LimitAxis::EvidenceRows => "evidence_rows",
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The active lens ids in catalog order, mirrored from [`active_catalog`].
    const APPLIED_IDS: [&str; 9] = [
        "PG-CACHE-010",
        "PG-WAL-009",
        "PG-TEMP-003",
        "PG-CHKPT-008",
        "PG-IO-011",
        "PG-HOT-007",
        "PG-ARCH-017",
        "OS-NET-028",
        "OS-CGRP-021",
    ];

    const MAX_ENTRY_JSON_BYTES: usize = 256
        + 2 * crate::incident::MAX_CATALOG_TOKEN_BYTES
        + 2 * crate::incident::MAX_CATALOG_TEXT_BYTES
        + crate::incident::MAX_MISSING_PER_LENS * (crate::incident::MAX_CATALOG_TOKEN_BYTES + 3);
    const MAX_CATALOG_JSON_BYTES: usize =
        256 + crate::incident::MAX_DORMANT_LENSES * MAX_ENTRY_JSON_BYTES;

    fn scan() -> ScanParams {
        ScanParams {
            from: 0,
            to: 10,
            window: 5,
            step: 1,
            threshold: 3.5,
            eps_rel: 0.05,
        }
    }

    #[test]
    fn hex_is_lowercase_and_fixed_width() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn identity_scalars_keep_their_json_types() {
        assert_eq!(identity_to_json(&IdentityValue::I64(-3)), json!(-3));
        assert_eq!(identity_to_json(&IdentityValue::U64(7)), json!(7));
        assert_eq!(identity_to_json(&IdentityValue::Bool(true)), json!(true));
        assert_eq!(
            identity_to_json(&IdentityValue::Text("db".to_owned())),
            json!("db"),
        );
    }

    #[test]
    fn axis_names_are_stable() {
        assert_eq!(axis_name(LimitAxis::Work), "work");
        assert_eq!(axis_name(LimitAxis::LensEvaluations), "lens_evaluations");
        assert_eq!(axis_name(LimitAxis::Findings), "findings");
        assert_eq!(axis_name(LimitAxis::EvidenceRows), "evidence_rows");
    }

    #[test]
    fn catalog_json_stays_within_its_static_budget() {
        let catalog = catalog_to_json();
        assert_eq!(catalog, catalog_to_json());
        let bytes = serde_json::to_vec(&catalog).expect("catalog JSON");
        assert_eq!(bytes.len(), 8_730);
        assert!(bytes.len() <= MAX_CATALOG_JSON_BYTES);
        assert!(catalog.get("log_dormant").is_none());
        let entry = &catalog["dormant"][11];
        let keys: std::collections::BTreeSet<_> = entry
            .as_object()
            .expect("catalog entry")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "awaiting",
                "confidence_cap",
                "domain",
                "lens_id",
                "question",
                "requirements_status",
                "slug",
                "text_locale",
                "title",
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn no_data_has_partial_envelope() {
        let body = no_data_response(7, &scan(), None);
        for field in [
            "source_id",
            "from",
            "to",
            "complete",
            "clustering_complete",
            "analysis_status",
            "incidents",
            "coverage_by_section",
            "data_age_seconds",
            "catalog",
            "data_quality",
            "skipped",
        ] {
            assert!(body.get(field).is_some(), "missing top-level field {field}");
        }
        assert_eq!(body["complete"], false);
        assert_eq!(body["analysis_status"], "no_data");
        assert_eq!(body["data_age_seconds"], Value::Null);
        assert!(body["skipped"].get("sections").is_some());
        assert_eq!(body["catalog"]["status"], "partial");
        assert_eq!(body["catalog"]["diagnosis_available"], true);
        assert_eq!(body["catalog"]["applied"], json!(APPLIED_IDS));
        assert!(
            body["catalog"]["dormant"]
                .as_array()
                .is_some_and(|entries| entries
                    .iter()
                    .all(|entry| entry["requirements_status"] == "incomplete"))
        );
    }

    #[test]
    fn global_catalog_readiness_is_not_a_request_skip() {
        let body = no_data_response(7, &scan(), None);
        assert!(
            body["skipped"]["analysis"]
                .as_array()
                .is_some_and(|entries| entries.iter().all(|entry| entry["scope"] != "catalog"))
        );
    }

    #[test]
    fn a_skipped_section_forces_partial_clustering() {
        let outcome = EngineOutcome {
            incidents: Vec::new(),
            span_splits: 0,
            complete: true,
            skipped: Vec::new(),
        };
        let body = build_response(
            7,
            &scan(),
            None,
            &outcome,
            &BTreeMap::new(),
            &InputQuality::default(),
            &[SectionSkip {
                section: "pg_stat_archiver",
                reason: SkipReason::IncompletePage,
            }],
        );
        assert_eq!(body["clustering_complete"], false);
        assert_eq!(body["complete"], false);
        assert_eq!(body["analysis_status"], "partial");
        assert_eq!(body["catalog"]["status"], "partial");
        assert_eq!(body["catalog"]["applied"], json!(APPLIED_IDS));
        assert_eq!(
            body["skipped"]["sections"][0]["reason"]["kind"],
            "incomplete_page",
        );
    }

    #[test]
    fn lock_episode_does_not_activate_catalog_without_edge_evidence() {
        use crate::incident::{
            ClockRelation, EnrichedEpisode, EpisodeRefV1, IdentityValue, IncidentConfig, SeriesSet,
            TypedInputs, analyze,
        };
        use kronika_analytics::{Direction, Episode, Evaluated};
        use std::sync::Arc;

        let peak = Evaluated {
            m: 0.0,
            dir: Direction::Flat,
            med_cur: 0.0,
            med_ref: 0.0,
            mad_ref: 1.0,
            sigma_used: 1.4826,
            n_cur: 0,
            n_ref: 0,
        };
        let episode = EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 0,
                peak_ts: 0,
                peak,
            },
            reference: EpisodeRefV1 {
                logical_section: "pg_locks",
                column: "depth",
                identity: Arc::from(vec![IdentityValue::I64(42)]),
                start_us: 0,
                end_us: 10,
            },
        };
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        let outcome = analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[],
            &config,
        )
        .expect("valid analysis");

        let body = build_response(
            7,
            &scan(),
            None,
            &outcome,
            &BTreeMap::new(),
            &InputQuality::default(),
            &[],
        );

        assert_eq!(body["incidents"][0]["findings"], json!([]));
        assert_eq!(body["incidents"][0]["evaluation_complete"], true);
        assert_eq!(body["catalog"]["status"], "partial");
        assert_eq!(body["catalog"]["diagnosis_available"], true);
        assert_eq!(body["catalog"]["applied"], json!(APPLIED_IDS));
        let dormant = body["catalog"]["dormant"]
            .as_array()
            .expect("catalog lists dormant lenses");
        assert_eq!(dormant.len(), 28 - APPLIED_IDS.len());
        let lock = dormant
            .iter()
            .find(|entry| entry["lens_id"] == "PG-LOCK-012")
            .expect("lock lens is dormant");
        assert_eq!(
            lock["awaiting"],
            json!(["sampled_blocked_by_edges", "lock_snapshot_coverage"])
        );
        assert_eq!(lock["domain"], "pg");
        assert_eq!(lock["slug"], "lock_wait_graph");
        assert_eq!(lock["confidence_cap"], "high");
        assert_eq!(lock["text_locale"], "ru");
        assert_eq!(lock["title"], "Граф ожидания блокировок");
        assert_eq!(
            lock["question"],
            "Кто блокировал ожидающего в момент снимка (`blocked_by` из `pg_locks`)."
        );
        assert!(
            body["catalog"]["dormant"]
                .as_array()
                .expect("catalog is a list")
                .iter()
                .all(|entry| entry["requirements_status"] == "incomplete")
        );
        assert!(
            body["skipped"]["analysis"]
                .as_array()
                .is_some_and(|entries| entries.iter().all(|entry| entry["scope"] != "catalog"))
        );
    }

    #[test]
    fn a_cache_miss_finding_renders_role_evidence_and_scope() {
        use crate::incident::{
            ClockRelation, EnrichedEpisode, EpisodeRefV1, IdentityValue, IncidentConfig, Lens,
            SeriesSet, TypedInputs, active_catalog, analyze,
        };
        use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};
        use std::sync::Arc;

        let identity: Arc<[IdentityValue]> = Arc::from(vec![IdentityValue::U64(5)]);
        // One-second intervals, so the recovered delta equals the rate.
        let point = |delta: f64| DiffPoint::Value {
            delta: Scalar::Int(0),
            rate: delta,
            dt_micros: 1_000_000,
        };
        let counter = |deltas: [f64; 3]| -> Vec<(i64, DiffPoint)> {
            deltas
                .iter()
                .zip(0_i64..)
                .map(|(&d, ts)| (ts, point(d)))
                .collect()
        };
        let mut typed = TypedInputs::new();
        // Cold cache: reads dominate hits over three valid intervals.
        typed.insert_counter(
            "pg_stat_database",
            "blks_read",
            Arc::clone(&identity),
            counter([30.0, 30.0, 20.0]),
        );
        typed.insert_counter(
            "pg_stat_database",
            "blks_hit",
            Arc::clone(&identity),
            counter([5.0, 5.0, 10.0]),
        );

        let episode = EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 0,
                peak_ts: 0,
                peak: Evaluated {
                    m: 0.0,
                    dir: Direction::Up,
                    med_cur: 0.0,
                    med_ref: 0.0,
                    mad_ref: 1.0,
                    sigma_used: 1.4826,
                    n_cur: 0,
                    n_ref: 0,
                },
            },
            reference: EpisodeRefV1 {
                logical_section: "pg_stat_database",
                column: "blks_read",
                identity: Arc::clone(&identity),
                start_us: 0,
                end_us: 10,
            },
        };
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        let catalog = active_catalog();
        let lenses: Vec<&dyn Lens> = catalog.iter().map(AsRef::as_ref).collect();
        let outcome = analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            &typed,
            &lenses,
            &config,
        )
        .expect("valid analysis");

        let body = build_response(
            7,
            &scan(),
            None,
            &outcome,
            &BTreeMap::new(),
            &InputQuality::default(),
            &[],
        );
        let finding = &body["incidents"][0]["findings"][0];
        assert_eq!(finding["lens_id"], "PG-CACHE-010");
        assert_eq!(finding["role"], "amplifier");
        assert_eq!(finding["confidence"], "medium");
        assert_eq!(finding["evidence"], json!(["ratio"]));
        assert_eq!(finding["scope"]["logical_section"], "pg_stat_database");
        assert_eq!(finding["scope"]["column"], "blks_read");
        assert_eq!(finding["scope"]["identity"], json!([5]));
        assert_eq!(body["incidents"][0]["evaluation_complete"], true);
        assert_eq!(
            body["incidents"][0]["finding_evaluation_status"],
            "complete"
        );
    }
}
