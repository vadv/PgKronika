//! JSON contract for incident clustering responses.

use std::collections::BTreeMap;

use kronika_reader::Gap;
use serde_json::{Value, json};

use crate::anomaly::ScanParams;
use crate::incident::{
    EngineOutcome, EngineSkip, EpisodeRefV1, IdentityValue, Incident, LimitAxis,
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
    json!({
        "interval": { "from": incident.start_us, "to": incident.end_us },
        "incident_key": hex(incident.key.canonical_bytes()),
        "members": members,
        "findings": Value::Array(Vec::new()),
        "evaluation_complete": false,
        "finding_evaluation_status": "not_available",
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
    let dormant: Vec<Value> = crate::incident::dormant_catalog()
        .iter()
        .map(|lens| {
            json!({
                "lens_id": lens.lens_id(),
                "awaiting": lens.awaiting(),
            })
        })
        .collect();
    json!({
        "status": "dormant",
        "diagnosis_available": false,
        "scope": "anomaly_clustering_only",
        "applied": Value::Array(Vec::new()),
        "dormant": dormant,
    })
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
    analysis.push(json!({
        "scope": "catalog",
        "reason": {
            "kind": "dormant",
            "awaiting": ["sampled_blocked_by_edges", "lock_snapshot_coverage"],
        },
    }));
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
    fn no_data_has_the_full_partial_envelope() {
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
        assert_eq!(body["catalog"]["status"], "dormant");
        assert_eq!(body["catalog"]["diagnosis_available"], false);
        assert_eq!(body["catalog"]["applied"], Value::Array(Vec::new()));
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
        assert_eq!(body["catalog"]["status"], "dormant");
        assert_eq!(body["catalog"]["applied"], json!([]));
        assert_eq!(
            body["skipped"]["sections"][0]["reason"]["kind"],
            "incomplete_page",
        );
    }

    #[test]
    fn a_lock_episode_does_not_activate_a_catalog_without_edge_evidence() {
        use crate::incident::{
            ClockRelation, EnrichedEpisode, EpisodeRefV1, IdentityValue, IncidentConfig, SeriesSet,
            analyze,
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
        let outcome =
            analyze(vec![episode], &SeriesSet::for_test(0), &[], &config).expect("valid analysis");

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
        assert_eq!(body["incidents"][0]["evaluation_complete"], false);
        assert_eq!(body["catalog"]["status"], "dormant");
        assert_eq!(body["catalog"]["diagnosis_available"], false);
        assert_eq!(body["catalog"]["applied"], json!([]));
        assert_eq!(body["catalog"]["dormant"][0]["lens_id"], "PG-LOCK-012");
        assert_eq!(
            body["catalog"]["dormant"][0]["awaiting"],
            json!(["sampled_blocked_by_edges", "lock_snapshot_coverage"])
        );
        assert_eq!(body["skipped"]["analysis"][0]["reason"]["kind"], "dormant");
    }
}
