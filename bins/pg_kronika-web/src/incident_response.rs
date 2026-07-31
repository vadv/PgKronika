//! JSON contract for incident clustering responses.

use std::collections::{BTreeMap, BTreeSet};

use kronika_reader::Gap;
use serde_json::{Value, json};

use crate::anomaly::ScanParams;
use crate::incident::{
    CounterEvidence, EngineOutcome, EngineSkip, EpisodeRefV1, EventOutcome, Evidence, Finding,
    GaugeEvidence, GaugeMeasurement, IdentityValue, Incident, LimitAxis, LogCoverage,
    SampledLockEdge, SourceWindow, SourceWindowGapReason,
};
use crate::incident_input::{
    CapabilityInputState, InputQuality, MaterializationKind, SectionSkip, SkipReason,
};
use crate::reason::{ApiReason, MaterializationResource};

pub(crate) fn no_data_response(scan: &ScanParams, data_age: Option<u64>) -> Value {
    let quality = InputQuality::default();
    json!({
        "from": scan.from,
        "to": scan.to,
        "complete": false,
        "clustering_complete": false,
        "analysis_status": "no_data",
        "incidents": Value::Array(Vec::new()),
        "coverage_by_section": json!({}),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(None, &[], &BTreeMap::new(), &[], &[]),
        "log": empty_log_json(),
        "data_quality": quality_to_json(&quality),
        "skipped": skipped_to_json(&[], &[], 0, &quality, Some(ApiReason::no_data())),
    })
}

pub(crate) struct ResponseInput<'a> {
    pub coverage: &'a BTreeMap<&'static str, Vec<Gap>>,
    pub quality: &'a InputQuality,
    pub skipped: &'a [SectionSkip],
    pub capability_by_section: &'a BTreeMap<&'static str, CapabilityInputState>,
}

pub(crate) fn build_response(
    scan: &ScanParams,
    data_age: Option<u64>,
    outcome: &EngineOutcome,
    log: &EventOutcome,
    input: &ResponseInput<'_>,
) -> Value {
    let ResponseInput {
        coverage,
        quality,
        skipped: input_skipped,
        capability_by_section,
    } = input;
    let incidents: Vec<Value> = outcome.incidents.iter().map(incident_to_json).collect();
    let clustering_complete = input_skipped.is_empty() && quality.episodes_truncated == 0;
    let analysis_complete = clustering_complete && outcome.complete;
    let analysis_status = if !analysis_complete {
        "partial"
    } else if quality.evaluated_positions == 0 {
        "insufficient_data"
    } else if incidents.is_empty() {
        "calm"
    } else {
        "incidents_detected"
    };
    json!({
        "from": scan.from,
        "to": scan.to,
        "complete": false,
        "clustering_complete": clustering_complete,
        "analysis_status": analysis_status,
        "incidents": incidents,
        "coverage_by_section": coverage_to_json(coverage),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(
            Some(coverage),
            input_skipped,
            capability_by_section,
            &outcome.evaluated_lens_ids,
            &log.evaluated_lens_ids,
        ),
        "log": log_to_json(log),
        "data_quality": quality_to_json(quality),
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
    let relations = incident_relations_to_json(incident);
    let finding_count = incident.findings.len();
    let coincident_count = incident
        .findings
        .iter()
        .filter(|finding| finding.role().label() == "coincident")
        .count();
    let focus = incident_focus(incident);
    json!({
        "interval": { "from": incident.start_us, "to": incident.end_us },
        "incident_key": hex(incident.key.canonical_bytes()),
        "peak_ts_us": focus.peak_ts_us.to_string(),
        "level": focus.level,
        "level_policy_revision": INCIDENT_LEVEL_POLICY_REVISION,
        "category_code": focus.category_code,
        "summary_code": focus.summary_code,
        "finding_count": finding_count,
        "coincident_count": coincident_count,
        "members": members,
        "findings": findings,
        "relations": relations,
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
    let evidence: Vec<Value> = finding.evidence().iter().map(evidence_to_json).collect();
    let metadata = finding_metadata(finding.lens_id());
    json!({
        "lens_id": finding.lens_id(),
        "slug": metadata.slug,
        "role": finding.role().label(),
        "confidence": finding.confidence().label(),
        "confidence_cap": metadata.confidence_cap,
        "scope": {
            "logical_section": scope.logical_section(),
            "column": scope.column(),
            "identity": identity,
        },
        "evidence": evidence,
    })
}

const INCIDENT_LEVEL_POLICY_REVISION: u16 = 1;
const CRITICAL_INCIDENT_SLUGS: &[&str] = &[
    "cgroup_memory_limit",
    "filesystem_space",
    "xid_wraparound_risk",
];

struct FindingMetadata {
    slug: &'static str,
    confidence_cap: &'static str,
    category_code: &'static str,
}

struct IncidentFocus {
    peak_ts_us: i64,
    level: &'static str,
    category_code: &'static str,
    summary_code: String,
}

fn finding_metadata(lens_id: &str) -> FindingMetadata {
    if let Some(entry) = crate::incident::core_catalog()
        .iter()
        .find(|entry| entry.lens_id() == lens_id)
    {
        return FindingMetadata {
            slug: entry.slug(),
            confidence_cap: entry.confidence().as_str(),
            category_code: match entry.domain().as_str() {
                "os" => "host",
                _ => "postgres",
            },
        };
    }
    if let Some(entry) = crate::incident::event_catalog_metadata()
        .iter()
        .find(|entry| entry.lens_id == lens_id)
    {
        return FindingMetadata {
            slug: entry.slug,
            confidence_cap: entry.confidence_cap.as_str(),
            category_code: if entry.lens_id.starts_with("OS-") {
                "host"
            } else {
                "postgres"
            },
        };
    }
    FindingMetadata {
        slug: "unregistered_lens",
        confidence_cap: "low",
        category_code: "postgres",
    }
}

fn incident_focus(incident: &Incident) -> IncidentFocus {
    let metadata = incident
        .findings
        .first()
        .map(|finding| finding_metadata(finding.lens_id()));
    let level =
        if incident.findings.iter().any(|finding| {
            CRITICAL_INCIDENT_SLUGS.contains(&finding_metadata(finding.lens_id()).slug)
        }) {
            "critical"
        } else {
            "warning"
        };
    let category_code = metadata.as_ref().map_or_else(
        || member_category(incident),
        |metadata| metadata.category_code,
    );
    let summary_code = metadata.map_or_else(
        || {
            incident.members.first().map_or_else(
                || "anomaly_cluster".to_owned(),
                |member| format!("anomaly.{}.{}", member.logical_section, member.column),
            )
        },
        |metadata| metadata.slug.to_owned(),
    );
    let peak_ts_us = i64::try_from(i128::midpoint(
        i128::from(incident.start_us),
        i128::from(incident.end_us),
    ))
    .expect("the midpoint of two i64 timestamps fits i64");
    IncidentFocus {
        peak_ts_us,
        level,
        category_code,
        summary_code,
    }
}

fn member_category(incident: &Incident) -> &'static str {
    if incident
        .members
        .iter()
        .all(|member| member.logical_section.starts_with("os_"))
    {
        "host"
    } else {
        "postgres"
    }
}

fn evidence_to_json(evidence: &Evidence) -> Value {
    match evidence {
        Evidence::GaugeObservation(gauge) => gauge_evidence_to_json(gauge),
        Evidence::CounterAggregate(counter) => counter_evidence_to_json(counter),
        Evidence::EntityJoin(join) => json!({
            "schema_version": 1,
            "type": "entity_join",
            "claim": "stored_typed_identity_join",
            "contract": join.contract(),
            "fields": join.fields(),
            "target": {
                "logical_section": join.target_section(),
                "identity": join
                    .target_identity()
                    .iter()
                    .map(identity_to_json)
                    .collect::<Vec<_>>(),
            },
        }),
        Evidence::Direct(direct) => direct
            .lock_edge()
            .map_or_else(|| Value::from(evidence.label()), lock_edge_evidence_to_json),
        Evidence::Ratio | Evidence::Gauge | Evidence::Counter | Evidence::Event => {
            Value::from(evidence.label())
        }
    }
}

fn incident_relations_to_json(incident: &Incident) -> Vec<Value> {
    let mut emitted = BTreeSet::new();
    let mut relations = Vec::new();
    for (from_finding, finding) in incident.findings.iter().enumerate() {
        for evidence in finding.evidence() {
            let Evidence::EntityJoin(join) = evidence else {
                continue;
            };
            for (to_finding, target) in incident.findings.iter().enumerate() {
                if from_finding == to_finding
                    || !join.matches(target.scope())
                    || !emitted.insert((from_finding, to_finding, join.contract()))
                {
                    continue;
                }
                relations.push(json!({
                    "from_finding": from_finding,
                    "to_finding": to_finding,
                    "kind": "proven",
                    "provenance": {
                        "contract": join.contract(),
                        "fields": join.fields(),
                    },
                }));
            }
        }
    }
    relations
}

fn gauge_evidence_to_json(gauge: &GaugeEvidence) -> Value {
    let measurement = match gauge.measurement() {
        GaugeMeasurement::Value { operand, value } => json!({
            "kind": "value",
            "operand": operand,
            "value": value.get(),
        }),
        GaugeMeasurement::Ratio {
            numerator_name,
            numerator,
            numerator_unit,
            denominator_name,
            denominator,
            denominator_unit,
        } => json!({
            "kind": "ratio",
            "numerator_name": numerator_name,
            "numerator": numerator.get(),
            "numerator_unit": numerator_unit.label(),
            "denominator_name": denominator_name,
            "denominator": denominator.get(),
            "denominator_unit": denominator_unit.label(),
            "value": numerator.get() / denominator.get(),
            "operand_unit": if numerator_unit == denominator_unit {
                Value::from(numerator_unit.label())
            } else {
                Value::Null
            },
            "headroom": if numerator_unit == denominator_unit {
                Value::from(denominator.get() - numerator.get())
            } else {
                Value::Null
            },
        }),
        GaugeMeasurement::Trend {
            operand,
            first,
            last,
            elapsed_us,
            operand_unit,
        } => json!({
            "kind": "trend",
            "operand": operand,
            "first": first.get(),
            "last": last.get(),
            "change": last.get() - first.get(),
            "elapsed_us": elapsed_us,
            "value": (last.get() - first.get())
                / std::time::Duration::from_micros(*elapsed_us).as_secs_f64(),
            "operand_unit": operand_unit.label(),
        }),
    };
    let entity: Vec<Value> = gauge
        .entity()
        .identity()
        .iter()
        .map(identity_to_json)
        .collect();
    json!({
        "schema_version": 1,
        "type": "gauge",
        "claim": "observed_threshold_crossing",
        "numeric_representation": "ieee754_binary64",
        "measurement": measurement,
        "unit": gauge.unit().label(),
        "threshold": {
            "operator": gauge.threshold_kind().label(),
            "value": gauge.threshold().get(),
        },
        "observed_at_us": gauge.observed_at_us(),
        "sample_count": gauge.samples(),
        "coverage": {
            "source_period": source_window_json(&gauge.source_window()),
        },
        "entity": {
            "logical_section": gauge.entity().section(),
            "identity": entity,
        },
    })
}

/// The observed source-window coverage: the collection cadence derived from the
/// series' own sample spacing, the intervals the incident window should have
/// held at that cadence, and the fraction actually covered. `expected` is null
/// and completeness is `unknown` (with a reason) when the cadence is unproven.
/// Completeness is emitted raw; a value above one signals an underestimated
/// period or data wider than the incident, not an error to clamp away.
fn source_window_json(window: &SourceWindow) -> Value {
    let completeness = window
        .source_window_completeness()
        .map_or_else(|| Value::from("unknown"), Value::from);
    let reason = window
        .completeness_gap_reason()
        .map(source_window_gap_reason);
    json!({
        "basis": "observed_series_delta_median",
        "observed_source_period_us": window.observed_period_us(),
        "expected_interval_count": window.expected_interval_count(),
        "expected_interval_count_reason": reason,
        "source_window_completeness": completeness,
    })
}

const fn source_window_gap_reason(reason: SourceWindowGapReason) -> ApiReason {
    match reason {
        SourceWindowGapReason::EmptyIncidentWindow => ApiReason::empty_incident_window(),
        SourceWindowGapReason::InsufficientIntervalsForObservedPeriod => {
            ApiReason::insufficient_intervals_for_observed_period()
        }
        SourceWindowGapReason::IncidentWindowShorterThanObservedPeriod => {
            ApiReason::incident_window_shorter_than_observed_period()
        }
    }
}

fn counter_evidence_to_json(counter: &CounterEvidence) -> Value {
    let operands: Vec<Value> = counter
        .operands()
        .iter()
        .map(|operand| {
            json!({
                "name": operand.name(),
                "aggregation": "delta_sum",
                "value": operand.value().get(),
                "unit": operand.unit().label(),
                "purpose": operand.purpose().label(),
                "numeric_representation": "ieee754_binary64",
            })
        })
        .collect();
    let entity: Vec<Value> = counter
        .entity()
        .identity()
        .iter()
        .map(identity_to_json)
        .collect();
    let window = counter.window();
    json!({
        "schema_version": 1,
        "type": "counter_aggregate",
        "claim": "derived_counter_threshold_crossing",
        "numeric_representation": "ieee754_binary64",
        "measurement": {
            "kind": counter.kind().label(),
            "formula": counter.formula(),
            "operands": operands,
            "value": counter.value().get(),
        },
        "unit": counter.unit().label(),
        "threshold": {
            "operator": counter.threshold_kind().label(),
            "value": counter.threshold().get(),
        },
        "coverage": {
            "basis": "paired_observed_interval_endpoints",
            "selection_from_us": window.selection_from_us(),
            "selection_to_us": window.selection_to_us(),
            "interval_end_bounds": "inclusive",
            "first_usable_interval_start_us": window.first_interval_start_us(),
            "first_usable_interval_end_us": window.first_interval_end_us(),
            "last_usable_interval_end_us": window.last_interval_end_us(),
            "candidate_interval_count": window.candidate_intervals(),
            "usable_interval_count": window.usable_intervals(),
            "excluded_interval_count": window.excluded_intervals(),
            "excluded_by_reason": {
                "unmatched_endpoint": window.unmatched_endpoint_intervals(),
                "unusable_delta": window.unusable_delta_intervals(),
                "unaligned_or_invalid_duration": window.unaligned_duration_intervals(),
                "numeric_limit": window.numeric_limit_intervals(),
            },
            "summed_interval_duration_us": window.elapsed_us(),
            "observed_endpoint_pairing_complete": window.excluded_intervals() == 0,
            "source_period": source_window_json(&window.source_window()),
        },
        "entity": {
            "logical_section": counter.entity().section(),
            "identity": entity,
        },
    })
}

fn lock_edge_evidence_to_json(edge: &SampledLockEdge) -> Value {
    json!({
        "schema_version": 1,
        "type": "lock_edge",
        "claim": "sampled_blocking_edge",
        "source": "pg_blocking_pids",
        "observed_at_us": edge.observed_at_us(),
        "waiter_pid": edge.waiter_pid(),
        "blocker_pid": edge.blocker_pid(),
        "blocker_kind": if edge.blocker_pid() == 0 {
            "prepared_transaction"
        } else {
            "backend"
        },
        "participant": edge.participant().label(),
        "edge_semantics": "hard_or_soft_block",
        "duplicate_policy": "deduplicated_by_waiter_and_blocker_pid",
        "transitive_inference": false,
        "evidence_completeness": "edge_only",
        "lock_target_available": false,
        "lock_mode_available": false,
    })
}

/// The log-event branch: self-contained facts, per-section coverage, and any
/// bound the pass hit. The current stderr source cannot prove exhaustive log
/// coverage, so `complete` remains false even when evaluation finished.
fn log_to_json(log: &EventOutcome) -> Value {
    let findings: Vec<Value> = log.findings.iter().map(finding_to_json).collect();
    let skipped: Vec<Value> = log.skipped.iter().map(engine_skip_to_json).collect();
    json!({
        "schema_version": 1,
        "complete": log.complete,
        "registered_lens_ids": registered_event_ids(),
        "evaluated_lens_ids": log.evaluated_lens_ids,
        "catalog": event_catalog_to_json(),
        "findings": findings,
        "coverage": log_coverage_to_json(&log.coverage),
        "skipped": skipped,
    })
}

fn registered_event_ids() -> Vec<Value> {
    crate::incident::event_catalog_ids()
        .into_iter()
        .map(Value::from)
        .collect()
}

fn event_catalog_to_json() -> Vec<Value> {
    crate::incident::event_catalog_metadata()
        .iter()
        .map(|entry| {
            json!({
                "lens_id": entry.lens_id,
                "slug": entry.slug,
                "confidence_cap": entry.confidence_cap.as_str(),
                "source_format": "stderr",
                "evidence_quality": "heuristic_positive_observation",
            })
        })
        .collect()
}

fn log_coverage_to_json(coverage: &BTreeMap<&'static str, LogCoverage>) -> Value {
    let object: serde_json::Map<String, Value> = coverage
        .iter()
        .map(|(&section, state)| (section.to_owned(), Value::from(state.label())))
        .collect();
    Value::Object(object)
}

fn empty_log_json() -> Value {
    json!({
        "schema_version": 1,
        "complete": false,
        "registered_lens_ids": registered_event_ids(),
        "evaluated_lens_ids": Value::Array(Vec::new()),
        "catalog": event_catalog_to_json(),
        "findings": Value::Array(Vec::new()),
        "coverage": json!({}),
        "skipped": Value::Array(Vec::new()),
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

const CONTRACT_CAPABILITIES: &[(&str, &str)] = &[
    ("PG-QRY-001", "pg_stat_statements"),
    ("PG-PLAN-002", "pg_store_plans"),
    ("PG-HORIZON-013", "pg_stat_activity"),
    ("PG-SYNC-018", "pg_stat_activity"),
    ("PG-WAIT-019", "pg_stat_activity"),
    ("OS-CPU-020", "os_cpu"),
    ("OS-BLOCK-024", "os_diskstats"),
    ("OS-IOWHO-026", "os_process"),
    ("PG-LOCK-012", "pg_locks"),
    ("PG-VACUUM-005", "pg_vacuum_observation"),
    ("PG-FREEZE-006", "pg_freeze_horizon"),
    ("PG-REPL-015", "pg_replication_physical"),
    ("PG-SLOT-016", "pg_replication_slot_retention"),
    ("OS-CGMEM-023", "pg_process_cgroup_memory"),
    ("OS-FS-027", "pg_storage_mount"),
];

fn catalog_to_json(
    coverage: Option<&BTreeMap<&'static str, Vec<Gap>>>,
    input_skipped: &[SectionSkip],
    capability_by_section: &BTreeMap<&'static str, CapabilityInputState>,
    evaluated_core_lens_ids: &[&'static str],
    evaluated_event_lens_ids: &[&'static str],
) -> Value {
    let mut applied_ids = crate::incident::active_catalog_ids();
    for id in crate::incident::event_catalog_ids() {
        if !applied_ids.contains(&id) {
            applied_ids.push(id);
        }
    }
    let applied: Vec<Value> = applied_ids.iter().copied().map(Value::from).collect();
    let registered_ids = crate::incident::registered_lens_ids();
    let registered: Vec<Value> = registered_ids.iter().copied().map(Value::from).collect();
    let evaluated_core: BTreeSet<_> = evaluated_core_lens_ids.iter().copied().collect();
    let evaluated_event: BTreeSet<_> = evaluated_event_lens_ids.iter().copied().collect();
    let evaluated_ids: BTreeSet<_> = evaluated_core
        .iter()
        .chain(&evaluated_event)
        .copied()
        .collect();
    let evaluated: Vec<Value> = evaluated_ids.iter().copied().map(Value::from).collect();
    let counts = crate::incident::catalog_counts();
    let lens_projection = lens_catalog_to_json(&evaluated_core);
    let evaluators =
        evaluator_catalog_to_json(&evaluated_core, &evaluated_event, counts.evaluator_branches);
    let capabilities = capabilities_to_json(coverage, input_skipped, capability_by_section);
    json!({
        "schema_version": 1,
        "status": "partial",
        "requirements_status": if lens_projection.incomplete_requirements == 0 {
            "complete"
        } else {
            "incomplete"
        },
        "catalog_available": true,
        "diagnosis_available": !evaluated_ids.is_empty(),
        "scope": "diagnostic_lenses",
        "registered_lens_ids": registered,
        "applied": applied,
        "evaluated_lens_ids": evaluated,
        "inactive_lens_ids": crate::incident::inactive_catalog()
            .iter()
            .map(|lens| Value::from(lens.lens_id()))
            .collect::<Vec<_>>(),
        "counts": catalog_counts_to_json(counts),
        "evaluators": evaluators,
        "lenses": lens_projection.entries,
        "active_count": counts.active_lens_ids,
        "catalog_count": counts.unique_lens_ids,
        "capabilities": capabilities,
        "dormant": Value::Array(Vec::new()),
    })
}

fn capabilities_to_json(
    coverage: Option<&BTreeMap<&'static str, Vec<Gap>>>,
    input_skipped: &[SectionSkip],
    capability_by_section: &BTreeMap<&'static str, CapabilityInputState>,
) -> Vec<Value> {
    CONTRACT_CAPABILITIES
        .iter()
        .map(|&(lens_id, section)| {
            if let Some(skip) = input_skipped.iter().find(|skip| skip.section == section) {
                return json!({
                    "lens_id": lens_id,
                    "section": section,
                    "status": "partial",
                    "reason": section_skip_reason(skip.reason),
                });
            }
            let mut provenance_available = false;
            if let Some(state) = capability_by_section.get(section) {
                match state {
                    CapabilityInputState::NotCollected => {
                        return json!({
                            "lens_id": lens_id,
                            "section": section,
                            "status": "not_collected",
                            "reason": ApiReason::producer_unavailable(),
                        });
                    }
                    CapabilityInputState::Partial => {
                        return json!({
                            "lens_id": lens_id,
                            "section": section,
                            "status": "partial",
                            "reason": ApiReason::provenance_or_input_missing(),
                        });
                    }
                    CapabilityInputState::Available => provenance_available = true,
                }
            }
            let Some(gaps) = coverage.and_then(|sections| sections.get(section)) else {
                if provenance_available {
                    return json!({
                        "lens_id": lens_id,
                        "section": section,
                        "status": "available",
                        "reason": ApiReason::complete_provenance(0),
                    });
                }
                return json!({
                    "lens_id": lens_id,
                    "section": section,
                    "status": "not_collected",
                    "reason": ApiReason::section_absent(),
                });
            };
            json!({
                "lens_id": lens_id,
                "section": section,
                "status": if gaps.is_empty() { "available" } else { "partial" },
                "reason": if gaps.is_empty() {
                    ApiReason::complete_coverage(gaps.len())
                } else {
                    ApiReason::coverage_gap(gaps.len())
                },
            })
        })
        .collect()
}

struct LensCatalogProjection {
    entries: Vec<Value>,
    incomplete_requirements: usize,
}

fn lens_catalog_to_json(evaluated_core: &BTreeSet<&'static str>) -> LensCatalogProjection {
    let mut incomplete_requirements = 0_usize;
    let entries = crate::incident::core_catalog()
        .iter()
        .map(|lens| {
            let requirements = lens
                .entity_join_contract()
                .map_or_else(Vec::new, |contract| {
                    incomplete_requirements = incomplete_requirements.saturating_add(1);
                    let activation = contract.activation();
                    vec![json!({
                        "requirement_id": format!("entity_join.{}", contract.as_str()),
                        "kind": "entity_join",
                        "identity": {
                            "domain": lens.domain().as_str(),
                            "name": "incident_lens",
                            "value": [lens.lens_id()],
                        },
                        "contract": contract.as_str(),
                        "activation": activation.as_str(),
                        "status": "unavailable",
                        "conditions": [
                            requirement_condition(
                                "producer",
                                activation.producer(),
                                "producer_unavailable",
                            ),
                            requirement_condition(
                                "provenance",
                                activation.provenance(),
                                "provenance_unavailable",
                            ),
                            requirement_condition(
                                "coverage",
                                activation.coverage(),
                                "coverage_unavailable",
                            ),
                        ],
                    })]
                });
            json!({
                "lens_id": lens.lens_id(),
                "slug": lens.slug(),
                "domain": lens.domain().as_str(),
                "confidence_cap": lens.confidence().as_str(),
                "evaluator_status": if evaluated_core.contains(lens.lens_id()) {
                    "evaluated"
                } else {
                    "not_evaluated"
                },
                "requirements_status": if requirements.is_empty() {
                    "not_applicable"
                } else {
                    "incomplete"
                },
                "requirements": requirements,
            })
        })
        .collect();
    LensCatalogProjection {
        entries,
        incomplete_requirements,
    }
}

fn requirement_condition(kind: &str, condition_id: &str, reason: &str) -> Value {
    json!({
        "kind": kind,
        "condition_id": condition_id,
        "status": "unavailable",
        "reason": reason,
    })
}

fn evaluator_catalog_to_json(
    evaluated_core: &BTreeSet<&'static str>,
    evaluated_event: &BTreeSet<&'static str>,
    evaluator_count: usize,
) -> Vec<Value> {
    let mut evaluators = Vec::with_capacity(evaluator_count);
    evaluators.extend(crate::incident::core_catalog().iter().map(|lens| {
        evaluator_to_json(
            "core",
            lens.lens_id(),
            lens.slug(),
            evaluated_core.contains(lens.lens_id()),
        )
    }));
    evaluators.extend(
        crate::incident::event_catalog_metadata()
            .iter()
            .map(|branch| {
                evaluator_to_json(
                    "event",
                    branch.lens_id,
                    branch.slug,
                    evaluated_event.contains(branch.lens_id),
                )
            }),
    );
    evaluators
}

fn evaluator_to_json(family: &str, lens_id: &str, slug: &str, evaluated: bool) -> Value {
    json!({
        "family": family,
        "lens_id": lens_id,
        "slug": slug,
        "status": if evaluated { "evaluated" } else { "not_evaluated" },
    })
}

fn catalog_counts_to_json(counts: crate::incident::IncidentCatalogCounts) -> Value {
    json!({
        "core_lenses": counts.core_lenses,
        "event_branches": counts.event_branches,
        "evaluator_branches": counts.evaluator_branches,
        "unique_lens_ids": counts.unique_lens_ids,
        "active_lens_ids": counts.active_lens_ids,
        "inactive_lens_ids": counts.inactive_lens_ids,
        "entity_join_requirements": counts.entity_join_requirements,
    })
}

fn section_skip_reason(reason: SkipReason) -> ApiReason {
    match reason {
        SkipReason::MaterializationLimit { resource, limit } => {
            let resource = match resource {
                MaterializationKind::Cells => MaterializationResource::Cells,
                MaterializationKind::Bytes => MaterializationResource::Bytes,
            };
            ApiReason::materialization_limit(resource, limit)
        }
        SkipReason::IncompletePage => ApiReason::incomplete_page(),
        SkipReason::ScanBudget {
            required,
            available,
        } => ApiReason::scan_budget(required, available),
        SkipReason::ConflictingTimestamp { timestamp } => {
            ApiReason::conflicting_timestamp(timestamp)
        }
        SkipReason::IdentityByteLimit { observed, limit } => {
            ApiReason::identity_byte_limit(observed, limit)
        }
        SkipReason::SeriesPointLimit { observed, limit } => {
            ApiReason::series_point_limit(observed, limit)
        }
        SkipReason::TypedGaugePointLimit { observed, limit } => {
            ApiReason::typed_gauge_point_limit(observed, limit)
        }
        SkipReason::SnapshotRowLimit { observed, limit } => {
            ApiReason::snapshot_row_limit(observed, limit)
        }
        SkipReason::IncompleteSnapshot => ApiReason::incomplete_snapshot(),
    }
}

fn quality_to_json(quality: &InputQuality) -> Value {
    json!({
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
        "snapshot_rows_withheld": quality.snapshot_rows_withheld,
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
    analysis_reason: Option<ApiReason>,
) -> Value {
    let mut sections: Vec<Value> = input_skipped.iter().map(section_skip_to_json).collect();
    sections.sort_by(|left, right| left["section"].as_str().cmp(&right["section"].as_str()));
    let evaluations: Vec<Value> = engine_skipped.iter().map(engine_skip_to_json).collect();
    let mut analysis = Vec::new();
    if quality.episodes_truncated > 0 {
        analysis.push(json!({
            "scope": "episodes",
            "reason": ApiReason::retention_limit(quality.episodes_truncated),
        }));
    }
    if let Some(reason) = analysis_reason {
        analysis.push(json!({
            "scope": "request",
            "reason": reason,
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
    json!({ "section": skip.section, "reason": section_skip_reason(skip.reason) })
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
        LimitAxis::OutputBytes => "output_bytes",
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
    const APPLIED_IDS: &[&str] = &[
        "PG-CACHE-010",
        "PG-WAL-009",
        "PG-TEMP-003",
        "PG-CHKPT-008",
        "PG-IO-011",
        "PG-HOT-007",
        "PG-ARCH-017",
        "OS-NET-028",
        "OS-CGRP-021",
        "PG-ANALYZE-004",
        "PG-CONN-014",
        "OS-MEM-022",
        "OS-WB-025",
        "PG-VACUUM-005",
        "PG-FREEZE-006",
        "PG-REPL-015",
        "PG-SLOT-016",
        "OS-CGMEM-023",
        "OS-FS-027",
        "PG-QRY-001",
        "PG-PLAN-002",
        "OS-CPU-020",
        "OS-BLOCK-024",
        "OS-IOWHO-026",
        "PG-HORIZON-013",
        "PG-SYNC-018",
        "PG-WAIT-019",
        "PG-LOCK-012",
        "PG-EVT-001",
        "PG-EVT-002",
        "PG-EVT-003",
        "PG-EVT-005",
        "PG-EVT-007",
        "PG-EVT-008",
        "PG-EVT-009",
        "PG-EVT-010",
        "PG-EVT-011",
        "PG-EVT-012",
        "PG-EVT-013",
        "PG-EVT-014",
    ];

    const MAX_CATALOG_JSON_BYTES: usize = 64 * 1_024;

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

    fn empty_log() -> EventOutcome {
        EventOutcome {
            findings: Vec::new(),
            coverage: BTreeMap::new(),
            complete: true,
            skipped: Vec::new(),
            evaluated_lens_ids: Vec::new(),
        }
    }

    fn counter_json_with_source_period(observed_period_us: Option<u64>) -> Value {
        use crate::incident::{
            CounterEvidenceInput, CounterEvidenceWindow, CounterEvidenceWindowInput,
            CounterMeasurementKind, CounterOperand, CounterOperandPurpose, GaugeEntity, GaugeUnit,
            ThresholdKind,
        };
        use std::sync::Arc;

        let counter = CounterEvidence::new(CounterEvidenceInput {
            kind: CounterMeasurementKind::Sum,
            formula: "writes",
            value: 3.0,
            unit: GaugeUnit::Count,
            threshold: 1.0,
            threshold_kind: ThresholdKind::AtLeast,
            operands: vec![
                CounterOperand::new(
                    "writes",
                    3.0,
                    GaugeUnit::Count,
                    CounterOperandPurpose::Formula,
                )
                .expect("valid operand"),
            ],
            window: CounterEvidenceWindow::new(CounterEvidenceWindowInput {
                selection_from_us: 0,
                selection_to_us: 4_000_000,
                first_interval_start_us: 0,
                first_interval_end_us: 1_000_000,
                last_interval_end_us: 3_000_000,
                usable_intervals: 3,
                candidate_intervals: 3,
                unmatched_endpoint_intervals: 0,
                unusable_delta_intervals: 0,
                unaligned_duration_intervals: 0,
                numeric_limit_intervals: 0,
                elapsed_us: 3_000_000,
                observed_period_us,
            })
            .expect("valid counter window"),
            entity: GaugeEntity::new("section", Arc::from([])),
        })
        .expect("valid counter evidence");

        counter_evidence_to_json(&counter)
    }

    fn gauge_json_with_source_period(source_window: SourceWindow) -> Value {
        use crate::incident::{GaugeEntity, GaugeUnit, GaugeValueInput, ThresholdKind};
        use std::sync::Arc;

        let gauge = GaugeEvidence::value(GaugeValueInput {
            operand: "queue_depth",
            value: 9.0,
            unit: GaugeUnit::Count,
            threshold: 8.0,
            threshold_kind: ThresholdKind::AtLeast,
            observed_at_us: 3_000_000,
            samples: 4,
            source_window,
            entity: GaugeEntity::new("section", Arc::from([])),
        })
        .expect("valid gauge evidence");

        gauge_evidence_to_json(&gauge)
    }

    #[test]
    fn counter_json_renders_known_source_period_without_legacy_aliases() {
        let evidence = counter_json_with_source_period(Some(1_000_000));

        assert_eq!(
            evidence["coverage"]["source_period"],
            json!({
                "basis": "observed_series_delta_median",
                "observed_source_period_us": 1_000_000,
                "expected_interval_count": 4,
                "expected_interval_count_reason": null,
                "source_window_completeness": 0.75,
            })
        );
        for legacy_field in [
            "observed_source_period_us",
            "expected_interval_count",
            "expected_interval_count_reason",
            "source_window_completeness",
        ] {
            assert!(evidence["coverage"].get(legacy_field).is_none());
        }
    }

    #[test]
    fn counter_json_renders_unknown_source_period_with_machine_reason() {
        let evidence = counter_json_with_source_period(None);

        assert_eq!(
            evidence["coverage"]["source_period"],
            json!({
                "basis": "observed_series_delta_median",
                "observed_source_period_us": null,
                "expected_interval_count": null,
                "expected_interval_count_reason": {
                    "kind": "insufficient_intervals_for_observed_period",
                    "params": {},
                },
                "source_window_completeness": "unknown",
            })
        );
    }

    #[test]
    fn gauge_json_renders_known_source_window_coverage() {
        let evidence =
            gauge_json_with_source_period(SourceWindow::new(4_000_000, Some(1_000_000), 3));

        assert_eq!(
            evidence["coverage"],
            json!({
                "source_period": {
                    "basis": "observed_series_delta_median",
                    "observed_source_period_us": 1_000_000,
                    "expected_interval_count": 4,
                    "expected_interval_count_reason": null,
                    "source_window_completeness": 0.75,
                }
            })
        );
    }

    #[test]
    fn gauge_json_renders_unknown_source_window_coverage() {
        let evidence = gauge_json_with_source_period(SourceWindow::new(4_000_000, None, 3));

        assert_eq!(
            evidence["coverage"],
            json!({
                "source_period": {
                    "basis": "observed_series_delta_median",
                    "observed_source_period_us": null,
                    "expected_interval_count": null,
                    "expected_interval_count_reason": {
                        "kind": "insufficient_intervals_for_observed_period",
                        "params": {},
                    },
                    "source_window_completeness": "unknown",
                }
            })
        );
    }

    #[test]
    fn every_source_window_gap_uses_the_closed_reason_shape() {
        for (window, kind) in [
            (
                SourceWindow::new(0, Some(1_000_000), 1),
                "empty_incident_window",
            ),
            (
                SourceWindow::new(4_000_000, None, 3),
                "insufficient_intervals_for_observed_period",
            ),
            (
                SourceWindow::new(400_000, Some(1_000_000), 1),
                "incident_window_shorter_than_observed_period",
            ),
        ] {
            let evidence = gauge_json_with_source_period(window);
            assert_eq!(
                evidence["coverage"]["source_period"]["expected_interval_count_reason"],
                json!({ "kind": kind, "params": {} })
            );
        }
    }

    #[test]
    fn the_log_branch_renders_facts_coverage_and_applied() {
        use crate::incident::{
            EventConfig, EventInputLimits, EventLens, LogCoverage, LogErrorGroup, LogEventInputs,
            evaluate_events, event_catalog,
        };

        let mut events = LogEventInputs::new(EventInputLimits::production());
        assert!(events.push_error(LogErrorGroup::new(100, 0, Some("40P01".to_owned()), 3,)));
        events.set_coverage("pg_log_errors", LogCoverage::Unknown);
        events.set_coverage("pg_log_lifecycle", LogCoverage::NotCollected);
        let catalog = event_catalog();
        let lenses: Vec<&dyn EventLens> = catalog.iter().map(AsRef::as_ref).collect();
        let log = evaluate_events(&events, &lenses, &EventConfig::production()).expect("valid");

        let outcome = EngineOutcome {
            incidents: Vec::new(),
            span_splits: 0,
            complete: true,
            skipped: Vec::new(),
            evaluated_lens_ids: Vec::new(),
        };
        let body = build_response(
            &scan(),
            None,
            &outcome,
            &log,
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &[],
                capability_by_section: &BTreeMap::new(),
            },
        );

        let log_json = &body["log"];
        assert_eq!(log_json["complete"], false);
        assert_eq!(
            log_json["registered_lens_ids"].as_array().map(Vec::len),
            Some(14)
        );
        assert_eq!(
            log_json["evaluated_lens_ids"].as_array().map(Vec::len),
            Some(12),
            "only the error-source branches executed"
        );
        assert_eq!(body["catalog"]["diagnosis_available"], true);
        assert_eq!(
            body["catalog"]["evaluators"]
                .as_array()
                .expect("evaluator branches")
                .iter()
                .filter(|entry| entry["family"] == "event" && entry["status"] == "evaluated")
                .count(),
            12
        );
        let finding = &log_json["findings"][0];
        assert_eq!(finding["lens_id"], "PG-EVT-007");
        assert_eq!(finding["confidence"], "medium");
        assert_eq!(finding["role"], "coincident");
        assert_eq!(finding["evidence"], json!(["event"]));
        assert_eq!(finding["scope"]["logical_section"], "pg_log_errors");
        assert_eq!(finding["scope"]["column"], "sqlstate");
        assert_eq!(
            finding["scope"]["identity"],
            json!(["40P01", 100]),
            "only the code and observation time reach the response"
        );
        assert_eq!(log_json["coverage"]["pg_log_errors"], "unknown");
        assert_eq!(log_json["coverage"]["pg_log_lifecycle"], "not_collected");

        let limited = evaluate_events(
            &events,
            &lenses,
            &EventConfig::with(u64::MAX, 1, u64::MAX, u64::MAX, u64::MAX),
        )
        .expect("bounded partial event evaluation");
        let limited_json = log_to_json(&limited);
        assert_eq!(
            limited_json["registered_lens_ids"].as_array().map(Vec::len),
            Some(14)
        );
        assert_eq!(limited_json["evaluated_lens_ids"], json!(["PG-EVT-003"]));
        assert_eq!(limited_json["skipped"][0]["lens_id"], "OS-FS-027");
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
        assert_eq!(axis_name(LimitAxis::OutputBytes), "output_bytes");
    }

    #[test]
    fn catalog_json_stays_within_its_static_budget() {
        let catalog = catalog_to_json(None, &[], &BTreeMap::new(), &[], &[]);
        assert_eq!(
            catalog,
            catalog_to_json(None, &[], &BTreeMap::new(), &[], &[])
        );
        let bytes = serde_json::to_vec(&catalog).expect("catalog JSON");
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= MAX_CATALOG_JSON_BYTES);
        assert!(catalog.get("log_dormant").is_none());
        assert!(catalog["dormant"].as_array().is_some_and(Vec::is_empty));
        let registered: BTreeSet<_> = catalog["registered_lens_ids"]
            .as_array()
            .expect("registered IDs")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let applied: BTreeSet<_> = catalog["applied"]
            .as_array()
            .expect("legacy applied IDs")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(applied, registered);
    }

    fn assert_machine_only(value: &Value) {
        match value {
            Value::Object(object) => {
                for forbidden in ["title", "question", "text_locale"] {
                    assert!(!object.contains_key(forbidden), "{forbidden}");
                }
                for child in object.values() {
                    assert_machine_only(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    assert_machine_only(child);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn catalog_metadata_contains_no_server_presentation() {
        let event = event_catalog_to_json();
        assert!(!event.is_empty());
        for entry in &event {
            let object = entry.as_object().expect("event catalog object");
            let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "confidence_cap",
                    "evidence_quality",
                    "lens_id",
                    "slug",
                    "source_format",
                ]
            );
        }

        let catalog = catalog_to_json(None, &[], &BTreeMap::new(), &[], &[]);
        assert_machine_only(&catalog);
        assert_eq!(catalog["lenses"].as_array().map(Vec::len), Some(28));
        assert_eq!(catalog["evaluators"].as_array().map(Vec::len), Some(42));
    }

    #[test]
    fn active_entity_join_requirements_are_typed_and_request_specific() {
        let catalog = catalog_to_json(None, &[], &BTreeMap::new(), &["PG-WAIT-019"], &[]);
        let lenses = catalog["lenses"].as_array().expect("active lens metadata");
        assert_eq!(
            lenses
                .iter()
                .flat_map(|lens| lens["requirements"].as_array().expect("lens requirements"))
                .count(),
            24
        );

        let wait = lenses
            .iter()
            .find(|lens| lens["lens_id"] == "PG-WAIT-019")
            .expect("wait lens");
        assert_eq!(wait["evaluator_status"], "evaluated");
        assert_eq!(wait["requirements_status"], "incomplete");
        assert_eq!(
            wait["requirements"][0],
            json!({
                "requirement_id": "entity_join.activity_lock_waiter",
                "kind": "entity_join",
                "identity": {
                    "domain": "pg",
                    "name": "incident_lens",
                    "value": ["PG-WAIT-019"],
                },
                "contract": "activity_lock_waiter",
                "activation": "shared_snapshot",
                "status": "unavailable",
                "conditions": [
                    {
                        "kind": "producer",
                        "condition_id": "shared_snapshot_producer",
                        "status": "unavailable",
                        "reason": "producer_unavailable",
                    },
                    {
                        "kind": "provenance",
                        "condition_id": "shared_snapshot_token",
                        "status": "unavailable",
                        "reason": "provenance_unavailable",
                    },
                    {
                        "kind": "coverage",
                        "condition_id": "both_inputs_complete",
                        "status": "unavailable",
                        "reason": "coverage_unavailable",
                    },
                ],
            })
        );
        let lock = lenses
            .iter()
            .find(|lens| lens["lens_id"] == "PG-LOCK-012")
            .expect("lock lens");
        assert_eq!(lock["evaluator_status"], "not_evaluated");
        assert_eq!(lock["requirements_status"], "not_applicable");
        assert_eq!(lock["requirements"], json!([]));

        let encoded = serde_json::to_string(&catalog).expect("active catalog JSON");
        for generic_token in [
            "typed_counter_deltas",
            "typed_gauge_samples",
            "paired_interval_inputs",
            "source_period_provenance",
            "request_input_coverage",
            "track_planning_gate",
            "store_plans_bridge",
            "sampled_blocked_by_edges",
            "lock_snapshot_coverage",
            "sampled_activity_rows",
            "pid_cgroup_mapping",
            "incident_log_event_input",
            "cross_section_entity_join",
        ] {
            assert!(!encoded.contains(generic_token), "{generic_token}");
        }
    }

    #[test]
    fn active_contracts_report_typed_request_capability_absence() {
        let mut states = BTreeMap::new();
        states.insert("pg_freeze_horizon", CapabilityInputState::NotCollected);
        states.insert("pg_storage_mount", CapabilityInputState::Partial);
        let catalog = catalog_to_json(Some(&BTreeMap::new()), &[], &states, &[], &[]);
        let capabilities = catalog["capabilities"].as_array().expect("capability list");
        let capability_entry = |lens_id| {
            capabilities
                .iter()
                .find(|entry| entry["lens_id"] == lens_id)
                .expect("contract capability")
        };
        assert_eq!(capability_entry("PG-FREEZE-006")["status"], "not_collected");
        assert_eq!(
            capability_entry("PG-FREEZE-006")["reason"],
            json!({ "kind": "producer_unavailable", "params": {} })
        );
        assert_eq!(capability_entry("OS-FS-027")["status"], "partial");
        assert_eq!(
            capability_entry("OS-FS-027")["reason"],
            json!({ "kind": "provenance_or_input_missing", "params": {} })
        );
        assert!(
            catalog["applied"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry == "PG-FREEZE-006"))
        );
        assert!(catalog["dormant"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .all(|entry| entry["lens_id"] != "PG-FREEZE-006")
        }));
    }

    #[test]
    fn no_data_has_partial_envelope() {
        let body = no_data_response(&scan(), None);
        for field in [
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
        assert_eq!(body["catalog"]["catalog_available"], true);
        assert_eq!(body["catalog"]["diagnosis_available"], false);
        assert_eq!(body["catalog"]["evaluated_lens_ids"], json!([]));
        assert!(
            body["catalog"]["evaluators"]
                .as_array()
                .is_some_and(|entries| entries
                    .iter()
                    .all(|entry| entry["status"] == "not_evaluated"))
        );
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
        let body = no_data_response(&scan(), None);
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
            evaluated_lens_ids: Vec::new(),
        };
        let body = build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &[SectionSkip {
                    section: "pg_stat_archiver",
                    reason: SkipReason::IncompletePage,
                }],
                capability_by_section: &BTreeMap::new(),
            },
        );
        assert_eq!(body["clustering_complete"], false);
        assert_eq!(body["complete"], false);
        assert_eq!(body["analysis_status"], "partial");
        assert_eq!(body["catalog"]["status"], "partial");
        assert_eq!(body["catalog"]["applied"], json!(APPLIED_IDS));
        assert_eq!(
            body["skipped"]["sections"][0]["reason"],
            json!({ "kind": "incomplete_page", "params": {} }),
        );
    }

    #[test]
    fn skipped_reason_arguments_are_nested_under_params() {
        let outcome = EngineOutcome {
            incidents: Vec::new(),
            span_splits: 0,
            complete: true,
            skipped: Vec::new(),
            evaluated_lens_ids: Vec::new(),
        };
        let body = build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &[SectionSkip {
                    section: "pg_stat_archiver",
                    reason: SkipReason::ScanBudget {
                        required: 11,
                        available: 10,
                    },
                }],
                capability_by_section: &BTreeMap::new(),
            },
        );
        assert_eq!(
            body["skipped"]["sections"][0]["reason"],
            json!({
                "kind": "scan_budget",
                "params": { "required": 11, "available": 10 },
            })
        );
        assert!(
            body["skipped"]["sections"][0]["reason"]
                .as_object()
                .is_some_and(|reason| reason.len() == 2)
        );
    }

    #[test]
    fn successful_partial_response_preserves_materialization_resource() {
        let outcome = EngineOutcome {
            incidents: Vec::new(),
            span_splits: 0,
            complete: true,
            skipped: Vec::new(),
            evaluated_lens_ids: Vec::new(),
        };
        let skipped = [
            SectionSkip {
                section: "pg_stat_archiver",
                reason: SkipReason::MaterializationLimit {
                    resource: MaterializationKind::Cells,
                    limit: 10,
                },
            },
            SectionSkip {
                section: "snapshot_coverage",
                reason: SkipReason::MaterializationLimit {
                    resource: MaterializationKind::Bytes,
                    limit: 20,
                },
            },
        ];
        let body = build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &skipped,
                capability_by_section: &BTreeMap::new(),
            },
        );
        let sections = body["skipped"]["sections"]
            .as_array()
            .expect("section skips");
        assert_eq!(
            sections[0]["reason"],
            json!({ "kind": "materialization_limit", "params": { "resource": "cells", "limit": 10 } })
        );
        assert_eq!(
            sections[1]["reason"],
            json!({ "kind": "materialization_limit", "params": { "resource": "bytes", "limit": 20 } })
        );
    }

    #[test]
    fn incomplete_lens_evaluation_keeps_clustering_complete() {
        let outcome = EngineOutcome {
            incidents: Vec::new(),
            span_splits: 0,
            complete: false,
            skipped: Vec::new(),
            evaluated_lens_ids: Vec::new(),
        };
        let quality = InputQuality {
            evaluated_positions: 1,
            ..InputQuality::default()
        };

        let body = build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &quality,
                skipped: &[],
                capability_by_section: &BTreeMap::new(),
            },
        );

        assert_eq!(body["clustering_complete"], true);
        assert_eq!(body["analysis_status"], "partial");
    }

    #[test]
    fn lock_lens_is_active_but_silent_without_sampled_edges() {
        use crate::incident::{
            ClockRelation, EnrichedEpisode, IncidentConfig, Lens, SeriesSet, TypedInputs,
            active_catalog, analyze,
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
        let config = IncidentConfig::for_test(5, 1_000, ClockRelation::Unknown);
        let catalog = active_catalog();
        let lenses: Vec<&dyn Lens> = catalog.iter().map(AsRef::as_ref).collect();
        // A pg_locks episode with no sampled lock edges: the lock lens is active
        // and runs, but has no direct evidence, so it emits nothing.
        let outcome = analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &lenses,
            &config,
        )
        .expect("valid analysis");

        let body = build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &[],
                capability_by_section: &BTreeMap::new(),
            },
        );

        assert_eq!(body["incidents"][0]["findings"], json!([]));
        assert_eq!(body["incidents"][0]["evaluation_complete"], true);
        assert_eq!(body["catalog"]["status"], "partial");
        assert_eq!(body["catalog"]["diagnosis_available"], true);
        assert_eq!(body["catalog"]["applied"], json!(APPLIED_IDS));
        let dormant = body["catalog"]["dormant"]
            .as_array()
            .expect("catalog lists dormant lenses");
        assert_eq!(dormant.len(), 0);
        assert_eq!(body["catalog"]["active_count"], 40);
        assert_eq!(body["catalog"]["catalog_count"], 40);
        assert!(
            APPLIED_IDS.contains(&"PG-LOCK-012"),
            "the lock lens is now applied, not dormant"
        );
        assert!(
            dormant
                .iter()
                .all(|entry| entry["lens_id"] != "PG-LOCK-012"),
            "the lock lens no longer appears among dormant lenses"
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

    fn cache_miss_response() -> Value {
        use crate::incident::{
            ClockRelation, EnrichedEpisode, IncidentConfig, Lens, SeriesSet, TypedInputs,
            active_catalog, analyze,
        };
        use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};
        use std::sync::Arc;

        let identity: Arc<[IdentityValue]> = Arc::from(vec![IdentityValue::U64(5)]);
        // One-second intervals with the same delta and rate.
        let point = |delta: f64| DiffPoint::Value {
            delta: Scalar::Float(delta),
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
        let config = IncidentConfig::for_test(5, 1_000, ClockRelation::Unknown);
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

        build_response(
            &scan(),
            None,
            &outcome,
            &empty_log(),
            &ResponseInput {
                coverage: &BTreeMap::new(),
                quality: &InputQuality::default(),
                skipped: &[],
                capability_by_section: &BTreeMap::new(),
            },
        )
    }

    #[test]
    fn a_cache_miss_finding_renders_role_evidence_and_scope() {
        let body = cache_miss_response();
        let finding = &body["incidents"][0]["findings"][0];
        assert_eq!(finding["lens_id"], "PG-CACHE-010");
        assert_eq!(finding["role"], "amplifier");
        assert_eq!(finding["confidence"], "medium");
        let evidence = &finding["evidence"][0];
        assert_eq!(evidence["schema_version"], 1);
        assert_eq!(evidence["type"], "counter_aggregate");
        assert_eq!(evidence["unit"], "ratio");
        assert_eq!(evidence["measurement"]["kind"], "ratio");
        assert_eq!(
            evidence["measurement"]["formula"],
            "blks_read / (blks_read + blks_hit)"
        );
        assert_eq!(evidence["measurement"]["value"], json!(0.8));
        assert_eq!(
            evidence["measurement"]["operands"],
            json!([
                {
                    "name": "blks_read",
                    "aggregation": "delta_sum",
                    "value": 80.0,
                    "unit": "count",
                    "purpose": "formula",
                    "numeric_representation": "ieee754_binary64"
                },
                {
                    "name": "blks_hit",
                    "aggregation": "delta_sum",
                    "value": 20.0,
                    "unit": "count",
                    "purpose": "formula",
                    "numeric_representation": "ieee754_binary64"
                }
            ])
        );
        assert_eq!(evidence["threshold"]["value"], json!(0.2));
        assert_eq!(evidence["coverage"]["candidate_interval_count"], 3);
        assert_eq!(evidence["coverage"]["usable_interval_count"], 3);
        assert_eq!(evidence["coverage"]["excluded_interval_count"], 0);
        assert_eq!(
            evidence["coverage"]["excluded_by_reason"],
            json!({
                "unmatched_endpoint": 0,
                "unusable_delta": 0,
                "unaligned_or_invalid_duration": 0,
                "numeric_limit": 0,
            })
        );
        assert_eq!(
            evidence["coverage"]["first_usable_interval_start_us"],
            -1_000_000
        );
        assert_eq!(
            evidence["coverage"]["summed_interval_duration_us"],
            3_000_000
        );
        assert_eq!(
            evidence["coverage"]["observed_endpoint_pairing_complete"],
            true
        );
        assert_eq!(finding["scope"]["logical_section"], "pg_stat_database");
        assert_eq!(finding["scope"]["column"], "blks_read");
        assert_eq!(finding["scope"]["identity"], json!([5]));
        assert_eq!(body["incidents"][0]["evaluation_complete"], true);
        assert_eq!(
            body["incidents"][0]["finding_evaluation_status"],
            "complete"
        );
    }

    #[test]
    fn a_prepared_transaction_lock_edge_renders_exact_provenance() {
        use crate::incident::{DirectEvidence, LockParticipant};

        let direct = DirectEvidence::sampled_lock_edge(123, 42, 0, LockParticipant::Blocker);
        let edge = direct
            .lock_edge()
            .expect("the direct evidence is a lock edge");

        assert_eq!(
            lock_edge_evidence_to_json(edge),
            json!({
                "schema_version": 1,
                "type": "lock_edge",
                "claim": "sampled_blocking_edge",
                "source": "pg_blocking_pids",
                "observed_at_us": 123,
                "waiter_pid": 42,
                "blocker_pid": 0,
                "blocker_kind": "prepared_transaction",
                "participant": "blocker",
                "edge_semantics": "hard_or_soft_block",
                "duplicate_policy": "deduplicated_by_waiter_and_blocker_pid",
                "transitive_inference": false,
                "evidence_completeness": "edge_only",
                "lock_target_available": false,
                "lock_mode_available": false,
            })
        );
    }
}
