use std::sync::Arc;

use super::super::model::IdentityValue;
use super::*;

fn scope(id: i64) -> FindingScope {
    FindingScope {
        logical_section: "section",
        column: "column",
        identity: Arc::from(vec![IdentityValue::I64(id)]),
    }
}

#[test]
fn confidence_orders_low_medium_high() {
    assert!(Confidence::LOW < Confidence::MEDIUM);
    assert!(Confidence::MEDIUM < Confidence::HIGH);
}

#[test]
fn confidence_cap_strings_are_stable() {
    assert_eq!(ConfidenceCap::Low.as_str(), "low");
    assert_eq!(ConfidenceCap::Medium.as_str(), "medium");
    assert_eq!(ConfidenceCap::High.as_str(), "high");
}

#[test]
fn weak_evidence_cannot_reach_high() {
    for evidence in [
        Evidence::Ratio,
        Evidence::Gauge,
        Evidence::Counter,
        Evidence::Event,
    ] {
        let finding = Finding::from_draft(
            "L",
            ConfidenceCap::High,
            FindingDraft::new(Role::Amplifier, scope(1), vec![evidence]),
        );
        assert_eq!(finding.confidence(), Confidence::MEDIUM);
    }
}

#[test]
fn sampled_lock_edge_can_reach_high_and_prove_direction() {
    let finding = Finding::from_draft(
        "PG-LOCK-012",
        ConfidenceCap::High,
        FindingDraft::new(
            Role::Lead,
            scope(1),
            vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                10,
                20,
                30,
                LockParticipant::Blocker,
            ))],
        ),
    );
    assert_eq!(finding.confidence(), Confidence::HIGH);
    assert_eq!(finding.role(), Role::Lead);
}

#[test]
fn sampled_lock_edge_only_proves_the_role_of_its_participant() {
    let finding = Finding::from_draft(
        "PG-LOCK-012",
        ConfidenceCap::High,
        FindingDraft::new(
            Role::Downstream,
            scope(1),
            vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                10,
                20,
                30,
                LockParticipant::Blocker,
            ))],
        ),
    );
    assert_eq!(finding.role(), Role::Coincident);

    let finding = Finding::from_draft(
        "PG-LOCK-012",
        ConfidenceCap::High,
        FindingDraft::new(
            Role::Downstream,
            scope(1),
            vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                10,
                20,
                30,
                LockParticipant::Waiter,
            ))],
        ),
    );
    assert_eq!(finding.role(), Role::Downstream);
}

#[test]
fn resource_event_does_not_prove_direction() {
    let finding = Finding::from_draft(
        "OS-MEM-022",
        ConfidenceCap::High,
        FindingDraft::new(
            Role::Lead,
            scope(1),
            vec![Evidence::Direct(DirectEvidence::resource_limit_event())],
        ),
    );
    assert_eq!(finding.confidence(), Confidence::HIGH);
    assert_eq!(finding.role(), Role::Coincident);
}

#[test]
fn unproven_direction_is_always_coincident() {
    let finding = Finding::from_draft(
        "TEMPORAL",
        ConfidenceCap::Medium,
        FindingDraft::new(Role::Downstream, scope(1), vec![Evidence::Counter]),
    );
    assert_eq!(finding.role(), Role::Coincident);
}

#[test]
fn empty_evidence_forces_low() {
    let finding = Finding::from_draft(
        "L",
        ConfidenceCap::High,
        FindingDraft::new(Role::Coincident, scope(1), vec![]),
    );
    assert_eq!(finding.confidence(), Confidence::LOW);
}

#[test]
fn scope_order_is_total() {
    assert!(scope(1) < scope(2));
}

#[test]
fn gauge_evidence_rejects_non_finite_values_and_zero_denominators() {
    let entity = Arc::from(vec![IdentityValue::I64(1)]);
    assert!(
        GaugeEvidence::value(GaugeValueInput {
            operand: "bytes",
            value: f64::NAN,
            unit: GaugeUnit::Bytes,
            threshold: 1.0,
            threshold_kind: ThresholdKind::AtLeast,
            observed_at_us: 10,
            samples: 1,
            source_window: SourceWindow::from_bounds(0, 0, None, 0),
            entity: GaugeEntity::new("section", Arc::clone(&entity)),
        })
        .is_none()
    );
    assert!(
        GaugeEvidence::ratio(
            GaugeRatio::new("a", 1.0, "b", 0.0, GaugeUnit::Count),
            0.5,
            ThresholdKind::AtLeast,
            10,
            1,
            SourceWindow::from_bounds(0, 0, None, 0),
            GaugeEntity::new("section", entity),
        )
        .is_none()
    );
    assert!(
        GaugeEvidence::ratio(
            GaugeRatio::new("a", f64::MAX, "b", f64::MIN_POSITIVE, GaugeUnit::Bytes,),
            0.5,
            ThresholdKind::AtLeast,
            10,
            1,
            SourceWindow::from_bounds(0, 0, None, 0),
            GaugeEntity::new("section", Arc::from([])),
        )
        .is_none()
    );
    assert!(
        GaugeEvidence::value(GaugeValueInput {
            operand: "count",
            value: 1.0,
            unit: GaugeUnit::Count,
            threshold: 1.0,
            threshold_kind: ThresholdKind::AtLeast,
            observed_at_us: 10,
            samples: 0,
            source_window: SourceWindow::from_bounds(0, 0, None, 0),
            entity: GaugeEntity::new("section", Arc::from([])),
        })
        .is_none()
    );

    let per_call = GaugeEvidence::ratio(
        GaugeRatio::with_units(
            "total_exec_time",
            100.0,
            GaugeUnit::Milliseconds,
            "calls",
            2.0,
            GaugeUnit::Count,
            GaugeUnit::MillisecondsPerCall,
        ),
        50.0,
        ThresholdKind::AtLeast,
        10,
        1,
        SourceWindow::from_bounds(0, 0, None, 0),
        GaugeEntity::new("section", Arc::from([])),
    )
    .expect("finite mixed-unit ratio");
    assert_eq!(per_call.unit(), GaugeUnit::MillisecondsPerCall);
}

#[test]
fn counter_evidence_bounds_and_names_its_operands() {
    let operand = |name, purpose| {
        CounterOperand::new(name, 1.0, GaugeUnit::Count, purpose).expect("valid operand")
    };
    let build = |operands| {
        CounterEvidence::new(CounterEvidenceInput {
            kind: CounterMeasurementKind::Ratio,
            formula: "a / b",
            value: 1.0,
            unit: GaugeUnit::Ratio,
            threshold: 0.5,
            threshold_kind: ThresholdKind::AtLeast,
            operands,
            window: CounterEvidenceWindow::new(CounterEvidenceWindowInput {
                selection_from_us: 0,
                selection_to_us: 10,
                first_interval_start_us: 0,
                first_interval_end_us: 1,
                last_interval_end_us: 1,
                usable_intervals: 1,
                candidate_intervals: 1,
                unmatched_endpoint_intervals: 0,
                unusable_delta_intervals: 0,
                unaligned_duration_intervals: 0,
                numeric_limit_intervals: 0,
                elapsed_us: 1_000_000,
                observed_period_us: None,
            })
            .expect("valid window"),
            entity: GaugeEntity::new("section", Arc::from([])),
        })
    };

    assert!(
        build(vec![
            operand("a", CounterOperandPurpose::Formula),
            operand("b", CounterOperandPurpose::Formula),
        ])
        .is_some()
    );
    assert!(
        build(vec![
            operand("same", CounterOperandPurpose::Formula),
            operand("same", CounterOperandPurpose::Formula),
        ])
        .is_none()
    );
    assert!(
        build(vec![
            operand("a", CounterOperandPurpose::Formula),
            operand("b", CounterOperandPurpose::Formula),
            operand("c", CounterOperandPurpose::AlignedContext),
            operand("d", CounterOperandPurpose::AlignedContext),
        ])
        .is_none()
    );
}
