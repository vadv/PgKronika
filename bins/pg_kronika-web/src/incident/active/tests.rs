use super::super::evidence::Confidence;
use super::*;
use crate::incident::model::{EnrichedEpisode, EpisodeRefV1};
use crate::incident::{
    ClockRelation, EngineOutcome, IncidentConfig, LimitAxis, LockEdge, LockParticipant,
    LockSnapshot, analyze,
};
use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};

fn id() -> Arc<[IdentityValue]> {
    Arc::from(vec![IdentityValue::U64(5)])
}

fn episode_window(start_us: i64, end_us: i64) -> EnrichedEpisode {
    EnrichedEpisode {
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
            logical_section: PG_STAT_DATABASE,
            column: "blks_read",
            identity: id(),
            start_us,
            end_us,
        },
    }
}

// One-second interval with the same delta and rate.
fn point(delta: f64) -> DiffPoint {
    DiffPoint::Value {
        delta: Scalar::Float(delta),
        rate: delta,
        dt_micros: 1_000_000,
    }
}

fn typed(read: &[f64], hit: &[f64]) -> TypedInputs {
    let mut typed = TypedInputs::new();
    let read_points = read.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
    let hit_points = hit.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
    typed.insert_counter(PG_STAT_DATABASE, "blks_read", id(), read_points.collect());
    typed.insert_counter(PG_STAT_DATABASE, "blks_hit", id(), hit_points.collect());
    typed
}

fn run(typed: &TypedInputs) -> Vec<(Role, Confidence)> {
    run_window(typed, 0, 10)
}

fn run_window(typed: &TypedInputs, start_us: i64, end_us: i64) -> Vec<(Role, Confidence)> {
    let lens = SharedBufferMissesLens;
    let lenses: [&dyn Lens; 1] = [&lens];
    let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
    let outcome = analyze(
        vec![episode_window(start_us, end_us)],
        &SeriesSet::for_test(0),
        typed,
        &lenses,
        &config,
    )
    .expect("valid analysis");
    outcome.incidents[0]
        .findings
        .iter()
        .map(|finding| (finding.role(), finding.confidence()))
        .collect()
}

#[test]
fn a_cold_cache_over_enough_intervals_reports_a_medium_amplifier() {
    // miss ratio 80/(80+20) = 0.8, three valid intervals.
    let findings = run(&typed(&[30.0, 30.0, 20.0], &[5.0, 5.0, 10.0]));
    assert_eq!(findings, vec![(Role::Amplifier, Confidence::MEDIUM)]);
}

#[test]
fn counter_pair_scan_is_admitted_before_reading_the_tracks() {
    let typed = typed(&[30.0, 30.0, 20.0], &[5.0, 5.0, 10.0]);
    let lens = SharedBufferMissesLens;
    let lenses: [&dyn Lens; 1] = [&lens];
    let config =
        IncidentConfig::for_test_with_work_limit("node", 5, 1_000, ClockRelation::Unknown, 2);
    let outcome = analyze(
        vec![episode_window(0, 10)],
        &SeriesSet::for_test(0),
        &typed,
        &lenses,
        &config,
    )
    .expect("work exhaustion returns a partial result");

    assert!(!outcome.complete);
    assert!(outcome.incidents[0].findings.is_empty());
    assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Work);
}

#[test]
fn counter_evidence_outside_the_incident_window_does_not_report() {
    let typed = typed(
        &[80.0, 80.0, 80.0, 1.0, 1.0, 1.0],
        &[1.0, 1.0, 1.0, 99.0, 99.0, 99.0],
    );
    assert!(
        run_window(&typed, 3, 5).is_empty(),
        "the cold-cache intervals end before the incident window"
    );
}

#[test]
fn a_warm_cache_reports_nothing() {
    // miss ratio 3/(3+297) = 0.01, below the threshold.
    assert!(run(&typed(&[1.0, 1.0, 1.0], &[99.0, 99.0, 99.0])).is_empty());
}

#[test]
fn a_ratio_below_the_threshold_reports_nothing() {
    // miss ratio 57/(57+243) = 0.19, just under the 0.2 floor.
    assert!(run(&typed(&[19.0, 19.0, 19.0], &[81.0, 81.0, 81.0])).is_empty());
}

#[test]
fn too_few_valid_intervals_report_nothing() {
    // A cold cache but only two intervals: below the data-quality minimum.
    assert!(run(&typed(&[50.0, 50.0], &[1.0, 1.0])).is_empty());
}

#[test]
fn an_empty_input_reports_nothing() {
    assert!(run(&TypedInputs::new()).is_empty());
}

#[test]
fn the_active_catalog_lists_every_wired_lens_once() {
    let ids = active_catalog_ids();
    assert_eq!(
        ids,
        vec![
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
        ]
    );
    let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "active ids are unique");
    assert_eq!(active_catalog().len(), ids.len());
}

// One-second interval with the same delta and rate.
fn pair(
    section: &'static str,
    column_a: &'static str,
    a: &[f64],
    column_b: &'static str,
    b: &[f64],
) -> TypedInputs {
    let points = |deltas: &[f64]| -> Vec<(i64, DiffPoint)> {
        deltas
            .iter()
            .zip(0_i64..)
            .map(|(&d, ts)| (ts, point(d)))
            .collect()
    };
    let mut typed = TypedInputs::new();
    typed.insert_counter(section, column_a, id(), points(a));
    typed.insert_counter(section, column_b, id(), points(b));
    typed
}

fn window_episode(section: &'static str, column: &'static str) -> EnrichedEpisode {
    EnrichedEpisode {
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
            logical_section: section,
            column,
            identity: id(),
            start_us: 0,
            end_us: 10,
        },
    }
}

struct CounterReading {
    kind: CounterMeasurementKind,
    formula: &'static str,
    unit: GaugeUnit,
    value: f64,
    operands: Vec<(&'static str, f64, GaugeUnit, CounterOperandPurpose)>,
    usable_intervals: u64,
    excluded_intervals: u64,
}

fn first_reading(
    lens: &dyn Lens,
    section: &'static str,
    column: &'static str,
    typed: &TypedInputs,
) -> Option<CounterReading> {
    let lenses: [&dyn Lens; 1] = [lens];
    let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
    let outcome = analyze(
        vec![window_episode(section, column)],
        &SeriesSet::for_test(0),
        typed,
        &lenses,
        &config,
    )
    .expect("valid analysis");
    let finding = outcome.incidents[0].findings.first()?;
    let Evidence::CounterAggregate(counter) = finding.evidence().first()? else {
        return None;
    };
    Some(CounterReading {
        kind: counter.kind(),
        formula: counter.formula(),
        unit: counter.unit(),
        value: counter.value().get(),
        operands: counter
            .operands()
            .iter()
            .map(|operand| {
                (
                    operand.name(),
                    operand.value().get(),
                    operand.unit(),
                    operand.purpose(),
                )
            })
            .collect(),
        usable_intervals: counter.window().usable_intervals(),
        excluded_intervals: counter.window().excluded_intervals(),
    })
}

fn run_lens(
    lens: &dyn Lens,
    section: &'static str,
    column: &'static str,
    typed: &TypedInputs,
) -> Vec<(Role, Confidence)> {
    let episode = window_episode(section, column);
    let lenses: [&dyn Lens; 1] = [lens];
    let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
    let outcome = analyze(
        vec![episode],
        &SeriesSet::for_test(0),
        typed,
        &lenses,
        &config,
    )
    .expect("valid analysis");
    outcome.incidents[0]
        .findings
        .iter()
        .map(|finding| (finding.role(), finding.confidence()))
        .collect()
}

#[test]
fn cold_cache_publishes_the_miss_ratio_operands() {
    // 80 reads and 20 hits produce an 0.8 miss ratio.
    let reading = first_reading(
        &SharedBufferMissesLens,
        PG_STAT_DATABASE,
        "blks_read",
        &typed(&[30.0, 30.0, 20.0], &[5.0, 5.0, 10.0]),
    )
    .expect("a cold cache reports counter evidence");
    assert_eq!(reading.kind, CounterMeasurementKind::Ratio);
    assert_eq!(reading.formula, "blks_read / (blks_read + blks_hit)");
    assert_eq!(reading.unit, GaugeUnit::Ratio);
    assert!((reading.value - 0.8).abs() < 1e-9);
    assert_eq!(reading.operands[0].0, "blks_read");
    assert!((reading.operands[0].1 - 80.0).abs() < 1e-9);
    assert_eq!(reading.operands[1].0, "blks_hit");
    assert!((reading.operands[1].1 - 20.0).abs() < 1e-9);
    assert_eq!(
        (reading.usable_intervals, reading.excluded_intervals),
        (3, 0)
    );
}

#[test]
fn temp_spill_publishes_the_spilled_byte_volume() {
    let typed = pair(
        PG_STAT_DATABASE,
        "temp_bytes",
        &[8_192.0, 8_192.0, 8_192.0],
        "temp_files",
        &[1.0, 1.0, 1.0],
    );
    let reading = first_reading(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed)
        .expect("a spill reports counter evidence");
    assert_eq!(reading.kind, CounterMeasurementKind::Sum);
    assert_eq!(reading.unit, GaugeUnit::Bytes);
    assert!((reading.value - 24_576.0).abs() < 1e-9);
    assert_eq!(reading.operands[0].0, "temp_bytes");
    assert!((reading.operands[0].1 - 24_576.0).abs() < 1e-9);
    assert_eq!(reading.operands[1].0, "temp_files");
    assert!((reading.operands[1].1 - 3.0).abs() < 1e-9);
}

#[test]
fn backend_io_latency_publishes_milliseconds_per_read() {
    // 30 ms over 10 reads = 3 ms/read.
    let typed = pair(
        PG_STAT_IO,
        "read_time",
        &[10.0, 10.0, 10.0],
        "reads",
        &[4.0, 3.0, 3.0],
    );
    let reading = first_reading(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed)
        .expect("slow reads report counter evidence");
    assert_eq!(reading.formula, "read_time / reads");
    assert_eq!(reading.unit, GaugeUnit::MillisecondsPerRead);
    assert!((reading.value - 3.0).abs() < 1e-9);
    assert_eq!(reading.operands[0].0, "read_time");
    assert!((reading.operands[0].1 - 30.0).abs() < 1e-9);
    assert_eq!(reading.operands[1].0, "reads");
    assert!((reading.operands[1].1 - 10.0).abs() < 1e-9);
}

#[test]
fn hot_update_failure_publishes_the_non_hot_operands() {
    // 6 HOT of 15 updates: 9 non-HOT over 15, a 0.6 fraction.
    let typed = pair(
        PG_STAT_USER_TABLES,
        "n_tup_hot_upd",
        &[2.0, 2.0, 2.0],
        "n_tup_upd",
        &[5.0, 5.0, 5.0],
    );
    let reading = first_reading(
        &HotUpdateFailureLens,
        PG_STAT_USER_TABLES,
        "n_tup_upd",
        &typed,
    )
    .expect("hot-update failure reports counter evidence");
    assert_eq!(reading.unit, GaugeUnit::Ratio);
    assert!((reading.value - 0.6).abs() < 1e-9);
    assert_eq!(reading.operands[0].0, "n_tup_hot_upd");
    assert!((reading.operands[0].1 - 6.0).abs() < 1e-9);
    assert_eq!(reading.operands[1].0, "n_tup_upd");
    assert!((reading.operands[1].1 - 15.0).abs() < 1e-9);
}

#[test]
fn wal_archiving_failure_publishes_the_failure_count() {
    // Three then two failures over two intervals: five in total.
    let typed = pair(
        PG_STAT_ARCHIVER,
        "failed_count",
        &[3.0, 2.0],
        "archived_count",
        &[1.0, 1.0],
    );
    let reading = first_reading(
        &WalArchivingFailureLens,
        PG_STAT_ARCHIVER,
        "failed_count",
        &typed,
    )
    .expect("an archiving failure reports counter evidence");
    assert_eq!(reading.unit, GaugeUnit::Count);
    assert!((reading.value - 5.0).abs() < 1e-9);
    assert_eq!(reading.operands[0].0, "failed_count");
    assert!((reading.operands[0].1 - 5.0).abs() < 1e-9);
    assert_eq!(reading.operands[1].0, "archived_count");
    assert!((reading.operands[1].1 - 2.0).abs() < 1e-9);
}

#[test]
fn wal_amplification_reports_a_medium_amplifier_above_the_fpi_floor() {
    // 6 FPIs over 10 records = 0.6, three intervals.
    let typed = pair(
        PG_STAT_WAL,
        "wal_fpi",
        &[2.0, 2.0, 2.0],
        "wal_records",
        &[4.0, 3.0, 3.0],
    );
    assert_eq!(
        run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
    let reading = first_reading(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed)
        .expect("WAL amplification reports counter evidence");
    assert_eq!(reading.formula, "wal_fpi / wal_records");
    assert_eq!(reading.unit, GaugeUnit::Ratio);
    assert!((reading.value - 0.6).abs() < 1e-9);
    assert_eq!(
        reading.operands[0],
        (
            "wal_fpi",
            6.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
    assert_eq!(
        reading.operands[1],
        (
            "wal_records",
            10.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
}

#[test]
fn wal_amplification_below_the_floor_reports_nothing() {
    // 3 FPIs over 30 records = 0.1, below 0.5.
    let typed = pair(
        PG_STAT_WAL,
        "wal_fpi",
        &[1.0, 1.0, 1.0],
        "wal_records",
        &[10.0, 10.0, 10.0],
    );
    assert!(run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed).is_empty());
}

#[test]
fn wal_amplification_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        PG_STAT_WAL,
        "wal_fpi",
        &[9.0, 9.0],
        "wal_records",
        &[10.0, 10.0],
    );
    assert!(run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed).is_empty());
}

#[test]
fn wal_amplification_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &WalAmplificationLens,
            PG_STAT_WAL,
            "wal_fpi",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn temp_spill_reports_a_medium_amplifier_when_both_counters_advance() {
    let typed = pair(
        PG_STAT_DATABASE,
        "temp_bytes",
        &[8_192.0; 3],
        "temp_files",
        &[1.0, 1.0, 2.0],
    );
    assert_eq!(
        run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
}

#[test]
fn temp_spill_without_file_growth_reports_nothing() {
    // Bytes advanced but no temp file was created over the incident.
    let typed = pair(
        PG_STAT_DATABASE,
        "temp_bytes",
        &[8_192.0; 3],
        "temp_files",
        &[0.0, 0.0, 0.0],
    );
    assert!(run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed).is_empty());
}

#[test]
fn temp_spill_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        PG_STAT_DATABASE,
        "temp_bytes",
        &[8_192.0, 8_192.0],
        "temp_files",
        &[1.0, 1.0],
    );
    assert!(run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed).is_empty());
}

#[test]
fn temp_spill_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &TempSpillLens,
            PG_STAT_DATABASE,
            "temp_bytes",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn requested_checkpoints_reports_a_medium_amplifier_above_the_floor() {
    // 9 requested vs 3 timed = 0.75, three intervals.
    let typed = pair(
        CHECKPOINTER,
        "checkpoints_req",
        &[3.0, 3.0, 3.0],
        "checkpoints_timed",
        &[1.0, 1.0, 1.0],
    );
    assert_eq!(
        run_lens(
            &RequestedCheckpointsLens,
            CHECKPOINTER,
            "checkpoints_req",
            &typed
        ),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
    let reading = first_reading(
        &RequestedCheckpointsLens,
        CHECKPOINTER,
        "checkpoints_req",
        &typed,
    )
    .expect("requested checkpoints report counter evidence");
    assert_eq!(
        reading.formula,
        "checkpoints_req / (checkpoints_req + checkpoints_timed)"
    );
    assert!((reading.value - 0.75).abs() < 1e-9);
    assert_eq!(
        reading.operands[0],
        (
            "checkpoints_req",
            9.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
    assert_eq!(
        reading.operands[1],
        (
            "checkpoints_timed",
            3.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
}

#[test]
fn requested_checkpoints_below_the_floor_reports_nothing() {
    // 3 requested vs 12 timed = 0.2, below 0.5.
    let typed = pair(
        CHECKPOINTER,
        "checkpoints_req",
        &[1.0, 1.0, 1.0],
        "checkpoints_timed",
        &[4.0, 4.0, 4.0],
    );
    assert!(
        run_lens(
            &RequestedCheckpointsLens,
            CHECKPOINTER,
            "checkpoints_req",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn requested_checkpoints_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        CHECKPOINTER,
        "checkpoints_req",
        &[9.0, 9.0],
        "checkpoints_timed",
        &[1.0, 1.0],
    );
    assert!(
        run_lens(
            &RequestedCheckpointsLens,
            CHECKPOINTER,
            "checkpoints_req",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn requested_checkpoints_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &RequestedCheckpointsLens,
            CHECKPOINTER,
            "checkpoints_req",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn backend_io_latency_reports_a_medium_amplifier_above_the_floor() {
    // 30 ms over 10 reads = 3 ms/read, three intervals.
    let typed = pair(
        PG_STAT_IO,
        "read_time",
        &[10.0, 10.0, 10.0],
        "reads",
        &[3.0, 3.0, 4.0],
    );
    assert_eq!(
        run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
}

#[test]
fn backend_io_latency_below_the_floor_reports_nothing() {
    // 3 ms over 30 reads = 0.1 ms/read, below 1 ms.
    let typed = pair(
        PG_STAT_IO,
        "read_time",
        &[1.0, 1.0, 1.0],
        "reads",
        &[10.0, 10.0, 10.0],
    );
    assert!(run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed).is_empty());
}

#[test]
fn backend_io_latency_with_too_few_intervals_reports_nothing() {
    let typed = pair(PG_STAT_IO, "read_time", &[10.0, 10.0], "reads", &[1.0, 1.0]);
    assert!(run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed).is_empty());
}

#[test]
fn backend_io_latency_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &BackendIoLatencyLens,
            PG_STAT_IO,
            "read_time",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn hot_update_failure_reports_a_medium_amplifier_above_the_floor() {
    // 3 HOT of 12 updates = 75% non-HOT, three intervals.
    let typed = pair(
        PG_STAT_USER_TABLES,
        "n_tup_hot_upd",
        &[1.0, 1.0, 1.0],
        "n_tup_upd",
        &[4.0, 4.0, 4.0],
    );
    assert_eq!(
        run_lens(
            &HotUpdateFailureLens,
            PG_STAT_USER_TABLES,
            "n_tup_upd",
            &typed
        ),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
}

#[test]
fn hot_update_failure_when_hot_dominates_reports_nothing() {
    // 9 HOT of 10 updates = 10% non-HOT, below 50%.
    let typed = pair(
        PG_STAT_USER_TABLES,
        "n_tup_hot_upd",
        &[3.0, 3.0, 3.0],
        "n_tup_upd",
        &[3.0, 3.0, 4.0],
    );
    assert!(
        run_lens(
            &HotUpdateFailureLens,
            PG_STAT_USER_TABLES,
            "n_tup_upd",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn hot_update_failure_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        PG_STAT_USER_TABLES,
        "n_tup_hot_upd",
        &[0.0, 0.0],
        "n_tup_upd",
        &[5.0, 5.0],
    );
    assert!(
        run_lens(
            &HotUpdateFailureLens,
            PG_STAT_USER_TABLES,
            "n_tup_upd",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn hot_update_failure_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &HotUpdateFailureLens,
            PG_STAT_USER_TABLES,
            "n_tup_upd",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn wal_archiving_failure_reports_a_medium_coincident_on_any_failure() {
    // One failure recorded in a single usable interval.
    let typed = pair(
        PG_STAT_ARCHIVER,
        "failed_count",
        &[1.0],
        "archived_count",
        &[4.0],
    );
    assert_eq!(
        run_lens(
            &WalArchivingFailureLens,
            PG_STAT_ARCHIVER,
            "failed_count",
            &typed
        ),
        vec![(Role::Coincident, Confidence::MEDIUM)]
    );
}

#[test]
fn wal_archiving_failure_without_failures_reports_nothing() {
    let typed = pair(
        PG_STAT_ARCHIVER,
        "failed_count",
        &[0.0, 0.0],
        "archived_count",
        &[5.0, 5.0],
    );
    assert!(
        run_lens(
            &WalArchivingFailureLens,
            PG_STAT_ARCHIVER,
            "failed_count",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn wal_archiving_failure_with_no_usable_interval_reports_nothing() {
    let mut typed = TypedInputs::new();
    typed.insert_counter(PG_STAT_ARCHIVER, "failed_count", id(), Vec::new());
    typed.insert_counter(PG_STAT_ARCHIVER, "archived_count", id(), Vec::new());
    assert!(
        run_lens(
            &WalArchivingFailureLens,
            PG_STAT_ARCHIVER,
            "failed_count",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn wal_archiving_failure_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &WalArchivingFailureLens,
            PG_STAT_ARCHIVER,
            "failed_count",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn network_errors_reports_a_low_coincident_above_the_floor() {
    // 2 errors over 100 packets = 2%, three intervals; capped at low.
    let typed = pair(
        OS_NETDEV,
        "rx_errs",
        &[1.0, 1.0, 0.0],
        "rx_packets",
        &[30.0, 30.0, 40.0],
    );
    assert_eq!(
        run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed),
        vec![(Role::Coincident, Confidence::LOW)]
    );
    let reading = first_reading(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed)
        .expect("network errors report counter evidence");
    assert_eq!(reading.formula, "rx_errs / rx_packets");
    assert!((reading.value - 0.02).abs() < 1e-9);
    assert_eq!(
        reading.operands[0],
        (
            "rx_errs",
            2.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
    assert_eq!(
        reading.operands[1],
        (
            "rx_packets",
            100.0,
            GaugeUnit::Count,
            CounterOperandPurpose::Formula
        )
    );
}

#[test]
fn network_errors_below_the_floor_reports_nothing() {
    // 3 errors over 30000 packets = 0.01%, below 1%.
    let typed = pair(
        OS_NETDEV,
        "rx_errs",
        &[1.0, 1.0, 1.0],
        "rx_packets",
        &[10_000.0, 10_000.0, 10_000.0],
    );
    assert!(run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed).is_empty());
}

#[test]
fn network_errors_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        OS_NETDEV,
        "rx_errs",
        &[5.0, 5.0],
        "rx_packets",
        &[10.0, 10.0],
    );
    assert!(run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed).is_empty());
}

#[test]
fn network_errors_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &NetworkErrorsLens,
            OS_NETDEV,
            "rx_errs",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn cgroup_throttling_reports_a_medium_coincident_above_the_floor() {
    // 600,000 throttled microseconds over three seconds = 200,000 us/s.
    let typed = pair(
        OS_CGROUP_CPU,
        "throttled_usec",
        &[200_000.0, 200_000.0, 200_000.0],
        "usage_usec",
        &[400_000.0, 400_000.0, 400_000.0],
    );
    assert_eq!(
        run_lens(
            &CgroupCpuThrottlingLens,
            OS_CGROUP_CPU,
            "throttled_usec",
            &typed
        ),
        vec![(Role::Coincident, Confidence::MEDIUM)]
    );
    let reading = first_reading(
        &CgroupCpuThrottlingLens,
        OS_CGROUP_CPU,
        "throttled_usec",
        &typed,
    )
    .expect("cgroup throttling reports counter evidence");
    assert_eq!(reading.kind, CounterMeasurementKind::Rate);
    assert_eq!(
        reading.formula,
        "throttled_usec * 1000000 / summed_interval_duration_us"
    );
    assert_eq!(reading.unit, GaugeUnit::MicrosecondsPerSecond);
    assert!((reading.value - 200_000.0).abs() < 1e-9);
    assert_eq!(
        reading.operands[0],
        (
            "throttled_usec",
            600_000.0,
            GaugeUnit::Microseconds,
            CounterOperandPurpose::Formula
        )
    );
    assert_eq!(
        reading.operands[1],
        (
            "summed_interval_duration_us",
            3_000_000.0,
            GaugeUnit::Microseconds,
            CounterOperandPurpose::Formula
        )
    );
    assert_eq!(
        reading.operands[2],
        (
            "usage_usec",
            1_200_000.0,
            GaugeUnit::Microseconds,
            CounterOperandPurpose::AlignedContext
        )
    );
}

#[test]
fn cgroup_throttling_below_the_floor_reports_nothing() {
    // 30,000 throttled microseconds over three seconds = 10,000 us/s.
    let typed = pair(
        OS_CGROUP_CPU,
        "throttled_usec",
        &[10_000.0, 10_000.0, 10_000.0],
        "usage_usec",
        &[330_000.0, 330_000.0, 340_000.0],
    );
    assert!(
        run_lens(
            &CgroupCpuThrottlingLens,
            OS_CGROUP_CPU,
            "throttled_usec",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn cgroup_throttling_with_too_few_intervals_reports_nothing() {
    let typed = pair(
        OS_CGROUP_CPU,
        "throttled_usec",
        &[200_000.0, 200_000.0],
        "usage_usec",
        &[400_000.0, 400_000.0],
    );
    assert!(
        run_lens(
            &CgroupCpuThrottlingLens,
            OS_CGROUP_CPU,
            "throttled_usec",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn cgroup_throttling_on_empty_input_reports_nothing() {
    assert!(
        run_lens(
            &CgroupCpuThrottlingLens,
            OS_CGROUP_CPU,
            "throttled_usec",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

// Gauge readings at ts 0.. within the run_lens incident window `[0, 10]`.
fn gauges(section: &'static str, columns: &[(&'static str, &[f64])]) -> TypedInputs {
    let mut typed = TypedInputs::new();
    for &(name, values) in columns {
        let points = values
            .iter()
            .zip(0_i64..)
            .map(|(&value, ts)| (ts, value))
            .collect();
        typed.insert_gauge(section, name, id(), points);
    }
    typed
}

#[test]
fn stale_statistics_uses_reltuples_and_reports_only_the_observation() {
    let typed = gauges(
        PG_STAT_USER_TABLES,
        &[("n_mod_since_analyze", &[250.0]), ("reltuples", &[1_000.0])],
    );
    assert_eq!(
        run_lens(
            &StaleStatisticsLens,
            PG_STAT_USER_TABLES,
            "n_mod_since_analyze",
            &typed,
        ),
        vec![(Role::Coincident, Confidence::LOW)]
    );
}

#[test]
fn stale_statistics_below_the_ratio_reports_nothing() {
    let typed = gauges(
        PG_STAT_USER_TABLES,
        &[("n_mod_since_analyze", &[199.0]), ("reltuples", &[1_000.0])],
    );
    assert!(
        run_lens(
            &StaleStatisticsLens,
            PG_STAT_USER_TABLES,
            "n_mod_since_analyze",
            &typed,
        )
        .is_empty()
    );
}

#[test]
fn stale_statistics_uses_absolute_estimate_and_includes_equality() {
    let typed = gauges(
        PG_STAT_USER_TABLES,
        &[
            ("n_mod_since_analyze", &[200.0]),
            ("reltuples", &[-1_000.0]),
        ],
    );
    assert_eq!(
        run_lens(
            &StaleStatisticsLens,
            PG_STAT_USER_TABLES,
            "n_mod_since_analyze",
            &typed,
        ),
        vec![(Role::Coincident, Confidence::LOW)]
    );
}

#[test]
fn per_database_connection_limit_includes_the_threshold_boundary() {
    let typed = gauges(
        PG_STAT_DATABASE,
        &[("numbackends", &[80.0]), ("datconnlimit", &[100.0])],
    );
    assert_eq!(
        run_lens(
            &ConnectionSaturationLens,
            PG_STAT_DATABASE,
            "numbackends",
            &typed,
        ),
        vec![(Role::Coincident, Confidence::MEDIUM)]
    );
}

#[test]
fn nonpositive_database_connection_limits_are_not_denominators() {
    for limit in [-2.0, -1.0, 0.0] {
        let typed = gauges(
            PG_STAT_DATABASE,
            &[("numbackends", &[80.0]), ("datconnlimit", &[limit])],
        );
        assert!(
            run_lens(
                &ConnectionSaturationLens,
                PG_STAT_DATABASE,
                "numbackends",
                &typed,
            )
            .is_empty()
        );
    }
}

#[test]
fn low_host_available_memory_is_a_low_confidence_observation() {
    let typed = gauges(
        OS_MEMINFO,
        &[("mem_available", &[4.0]), ("mem_total", &[100.0])],
    );
    assert_eq!(
        run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,),
        vec![(Role::Coincident, Confidence::LOW)]
    );
}

#[test]
fn available_memory_equal_to_the_floor_does_not_cross_below_it() {
    let typed = gauges(
        OS_MEMINFO,
        &[("mem_available", &[5.0]), ("mem_total", &[100.0])],
    );
    assert!(run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,).is_empty());
}

#[test]
fn zero_host_memory_total_is_not_a_denominator() {
    let typed = gauges(
        OS_MEMINFO,
        &[("mem_available", &[0.0]), ("mem_total", &[0.0])],
    );
    assert!(run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,).is_empty());
}

#[test]
fn writeback_ratio_uses_dirty_plus_writeback_at_one_timestamp() {
    let typed = gauges(
        OS_MEMINFO,
        &[
            ("dirty", &[6.0]),
            ("writeback", &[4.0]),
            ("mem_total", &[100.0]),
        ],
    );
    assert_eq!(
        run_lens(&WritebackPressureLens, OS_MEMINFO, "dirty", &typed,),
        vec![(Role::Coincident, Confidence::LOW)]
    );
}

#[test]
fn active_and_dormant_lenses_are_accounted_once() {
    let active = active_catalog_ids();
    assert_eq!(active.len(), 28);
    assert_eq!(crate::incident::core_catalog().len(), active.len());
    assert!(crate::incident::dormant_catalog().is_empty());
    let unique: std::collections::BTreeSet<_> = active.iter().copied().collect();
    assert_eq!(unique.len(), active.len());
}

#[test]
fn gauge_window_work_is_admitted_before_reduction() {
    let typed = gauges(
        OS_MEMINFO,
        &[
            ("mem_available", &[4.0, 4.0, 4.0]),
            ("mem_total", &[100.0, 100.0, 100.0]),
        ],
    );
    let mut episode = episode_window(0, 10);
    episode.reference.logical_section = OS_MEMINFO;
    episode.reference.column = "mem_available";
    let lens = MemoryReclaimLens;
    let lenses: [&dyn Lens; 1] = [&lens];
    let config =
        IncidentConfig::for_test_with_work_limit("node", 5, 1_000, ClockRelation::Unknown, 5);
    let outcome = analyze(
        vec![episode],
        &SeriesSet::for_test(0),
        &typed,
        &lenses,
        &config,
    )
    .expect("valid analysis");
    assert!(!outcome.complete);
    assert!(outcome.incidents[0].findings.is_empty());
    assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Work);
    assert_eq!(outcome.skipped[0].limit.observed, 8);
    assert_eq!(outcome.skipped[0].limit.limit, 5);
}

fn base_backend() -> ActivityBackend {
    ActivityBackend {
        pid: 1,
        backend_start: 1,
        xid_age: None,
        xmin_age: None,
        state: None,
        wait_event_type: None,
        wait_event: None,
        xact_age_us: None,
    }
}

// A snapshot at ts=5 sits inside the run_lens episode window [0, 10].
fn activity_typed(backends: Vec<ActivityBackend>) -> TypedInputs {
    let mut typed = TypedInputs::new();
    typed.insert_activity_snapshot(ActivitySnapshot {
        ts: 5,
        backends,
        completeness: SnapshotCompleteness::Complete,
    });
    typed
}

fn repeated_activity_typed(backends: &[ActivityBackend]) -> TypedInputs {
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: backends.to_owned(),
            completeness: SnapshotCompleteness::Complete,
        });
    }
    typed
}

fn run_activity(lens: &dyn Lens, typed: &TypedInputs) -> Vec<(Role, Confidence)> {
    run_lens(lens, PG_STAT_ACTIVITY, "backend_xmin_age", typed)
}

#[test]
fn xmin_hold_reports_a_medium_amplifier_for_an_old_idle_transaction() {
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: Some(2_000_000),
        state: Some("idle in transaction".into()),
        ..base_backend()
    }]);
    assert_eq!(
        run_activity(&XminHorizonHoldLens, &typed),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
}

#[test]
fn xmin_hold_reports_for_an_old_long_running_transaction() {
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: Some(2_000_000),
        state: Some("active".into()),
        xact_age_us: Some(400_000_000),
        ..base_backend()
    }]);
    assert_eq!(
        run_activity(&XminHorizonHoldLens, &typed),
        vec![(Role::Amplifier, Confidence::MEDIUM)]
    );
}

#[test]
fn xmin_hold_ignores_a_fresh_horizon() {
    // Idle in transaction but a young xmin: ordinary churn, not a hold.
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: Some(100),
        state: Some("idle in transaction".into()),
        ..base_backend()
    }]);
    assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
}

#[test]
fn xmin_hold_ignores_an_active_short_transaction() {
    // Old xmin, but a running query that just started holds no horizon yet.
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: Some(2_000_000),
        state: Some("active".into()),
        xact_age_us: Some(1_000),
        ..base_backend()
    }]);
    assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
}

#[test]
fn xmin_hold_ignores_a_backend_without_an_assigned_xmin() {
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: None,
        state: Some("idle in transaction".into()),
        ..base_backend()
    }]);
    assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
}

#[test]
fn xmin_hold_on_empty_input_reports_nothing() {
    assert!(run_activity(&XminHorizonHoldLens, &TypedInputs::new()).is_empty());
}

#[test]
fn sync_replication_reports_a_medium_coincident_on_a_syncrep_wait() {
    let typed = repeated_activity_typed(&[ActivityBackend {
        wait_event: Some("SyncRep".into()),
        ..base_backend()
    }]);
    assert_eq!(
        run_lens(
            &SyncReplicationWaitLens,
            PG_STAT_ACTIVITY,
            "wait_event",
            &typed
        ),
        vec![(Role::Coincident, Confidence::MEDIUM)]
    );
}

#[test]
fn sync_replication_ignores_other_waits() {
    let typed = activity_typed(vec![ActivityBackend {
        wait_event: Some("ClientRead".into()),
        ..base_backend()
    }]);
    assert!(
        run_lens(
            &SyncReplicationWaitLens,
            PG_STAT_ACTIVITY,
            "wait_event",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn sync_replication_requires_the_same_session_in_three_consecutive_samples() {
    let mut typed = TypedInputs::new();
    for (ts, backend_start) in [(3, 1), (5, 2), (7, 1)] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: vec![ActivityBackend {
                pid: 1,
                backend_start,
                wait_event: Some("SyncRep".into()),
                ..base_backend()
            }],
            completeness: SnapshotCompleteness::Restricted,
        });
    }
    assert!(
        run_lens(
            &SyncReplicationWaitLens,
            PG_STAT_ACTIVITY,
            "wait_event",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn internal_wait_reports_a_low_coincident_when_active_backends_concentrate() {
    // Three active backends, two on LWLock: 2*2 >= 3.
    let lwlock = || ActivityBackend {
        state: Some("active".into()),
        wait_event_type: Some("LWLock".into()),
        ..base_backend()
    };
    let running = ActivityBackend {
        state: Some("active".into()),
        ..base_backend()
    };
    let typed = repeated_activity_typed(&[lwlock(), lwlock(), running]);
    assert_eq!(
        run_lens(
            &InternalWaitConcentrationLens,
            PG_STAT_ACTIVITY,
            "wait_event_type",
            &typed
        ),
        vec![(Role::Coincident, Confidence::LOW)]
    );
}

#[test]
fn internal_wait_needs_a_floor_of_active_backends() {
    // Two active backends both on LWLock is concentrated but below the floor.
    let lwlock = || ActivityBackend {
        state: Some("active".into()),
        wait_event_type: Some("LWLock".into()),
        ..base_backend()
    };
    let typed = activity_typed(vec![lwlock(), lwlock()]);
    assert!(
        run_lens(
            &InternalWaitConcentrationLens,
            PG_STAT_ACTIVITY,
            "wait_event_type",
            &typed
        )
        .is_empty()
    );
}

#[test]
fn internal_wait_ignores_a_low_fraction() {
    // Four active backends, one on LWLock: 2*1 < 4.
    let lwlock = ActivityBackend {
        state: Some("active".into()),
        wait_event_type: Some("LWLock".into()),
        ..base_backend()
    };
    let running = || ActivityBackend {
        state: Some("active".into()),
        ..base_backend()
    };
    let typed = activity_typed(vec![lwlock, running(), running(), running()]);
    assert!(
        run_lens(
            &InternalWaitConcentrationLens,
            PG_STAT_ACTIVITY,
            "wait_event_type",
            &typed
        )
        .is_empty()
    );
}

fn lock_waiting_backend(pid: i64) -> ActivityBackend {
    ActivityBackend {
        pid,
        state: Some("active".into()),
        wait_event_type: Some("Lock".into()),
        ..base_backend()
    }
}

fn lock_edge(waiter_pid: i64, blocker_pid: i64) -> LockEdge {
    LockEdge {
        waiter_pid,
        waiter_backend_start: Some(1),
        blocker_pid,
    }
}

fn lock_wait_snapshots(pids: &[i64]) -> TypedInputs {
    let backends: Vec<_> = pids.iter().copied().map(lock_waiting_backend).collect();
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: backends.clone(),
            completeness: SnapshotCompleteness::Complete,
        });
    }
    typed
}

fn insert_lock_edges(typed: &mut TypedInputs, timestamps: &[i64], pids: &[i64], blockers: &[i64]) {
    for &ts in timestamps {
        let edges = pids
            .iter()
            .flat_map(|&waiter| {
                blockers
                    .iter()
                    .map(move |&blocker| lock_edge(waiter, blocker))
            })
            .collect();
        typed.insert_lock_snapshot(LockSnapshot {
            ts,
            activity_snapshot_ts: Some(ts),
            edges,
        });
    }
}

fn internal_wait_outcome(typed: &TypedInputs, work_limit: u64) -> EngineOutcome {
    let lens = InternalWaitConcentrationLens;
    let lenses: [&dyn Lens; 1] = [&lens];
    let config = IncidentConfig::for_test_with_work_limit(
        "node",
        5,
        1_000,
        ClockRelation::Unknown,
        work_limit,
    );
    analyze(
        vec![window_episode(PG_STAT_ACTIVITY, "wait_event_type")],
        &SeriesSet::for_test(0),
        typed,
        &lenses,
        &config,
    )
    .expect("valid internal-wait analysis")
}

fn assert_no_internal_wait(typed: &TypedInputs) {
    assert!(
        run_lens(
            &InternalWaitConcentrationLens,
            PG_STAT_ACTIVITY,
            "wait_event_type",
            typed,
        )
        .is_empty()
    );
}

#[test]
fn internal_wait_credits_exact_same_snapshot_lock_edges() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11, 12], &[99]);
    let outcome = internal_wait_outcome(&typed, u64::MAX);
    let finding = &outcome.incidents[0].findings[0];
    assert_eq!(
        (finding.role(), finding.confidence()),
        (Role::Coincident, Confidence::LOW)
    );
    assert_eq!(finding.scope().logical_section(), PG_STAT_ACTIVITY);
    assert_eq!(finding.scope().column(), "wait_event_type");
    assert_eq!(
        finding.scope().identity(),
        &[IdentityValue::Text("Lock".to_owned())]
    );
    let direct: Vec<_> = finding
        .evidence()
        .iter()
        .filter_map(|evidence| match evidence {
            Evidence::Direct(direct) => direct.lock_edge(),
            _ => None,
        })
        .collect();
    assert_eq!(direct.len(), 9);
    assert!(
        direct
            .iter()
            .all(|edge| edge.participant() == LockParticipant::Waiter)
    );
}

#[test]
fn internal_wait_ignores_lock_wait_without_a_blocked_by_edge() {
    let typed = lock_wait_snapshots(&[10, 11, 12]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_does_not_join_the_same_pid_across_snapshot_times() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    insert_lock_edges(&mut typed, &[5], &[10, 11, 12], &[99]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_does_not_infer_a_shared_snapshot_from_equal_timestamps() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    for ts in [3, 5, 7] {
        typed.insert_lock_snapshot(LockSnapshot {
            ts,
            activity_snapshot_ts: None,
            edges: [10, 11, 12]
                .into_iter()
                .map(|pid| lock_edge(pid, 99))
                .collect(),
        });
    }
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_does_not_join_a_reused_pid() {
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: [10, 11, 12]
                .into_iter()
                .map(|pid| ActivityBackend {
                    backend_start: 2,
                    ..lock_waiting_backend(pid)
                })
                .collect(),
            completeness: SnapshotCompleteness::Complete,
        });
    }
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11, 12], &[99]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_ignores_edges_without_matching_activity() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    insert_lock_edges(&mut typed, &[3, 5, 7], &[20, 21, 22], &[99]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_deduplicates_multiple_blockers_and_accepts_prepared_holders() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11, 12], &[99, 0, 99]);
    let outcome = internal_wait_outcome(&typed, u64::MAX);
    let direct = outcome.incidents[0].findings[0]
        .evidence()
        .iter()
        .filter(|evidence| matches!(evidence, Evidence::Direct(_)))
        .count();
    assert_eq!(direct, 18, "two unique blockers for each waiter snapshot");
}

#[test]
fn internal_wait_bounds_direct_lock_edge_evidence() {
    let mut typed = lock_wait_snapshots(&[1_000, 1_001, 1_002]);
    let blockers: Vec<_> = (0..200).collect();
    insert_lock_edges(&mut typed, &[3, 5, 7], &[1_000, 1_001, 1_002], &blockers);
    let outcome = internal_wait_outcome(&typed, u64::MAX);
    let direct: Vec<_> = outcome.incidents[0].findings[0]
        .evidence()
        .iter()
        .filter_map(|evidence| match evidence {
            Evidence::Direct(direct) => direct.lock_edge(),
            _ => None,
        })
        .collect();
    assert_eq!(direct.len(), 128);
    assert_eq!(direct[0].observed_at_us(), 3);
    assert_eq!(direct[0].waiter_pid(), 1_000);
    assert_eq!(direct[0].blocker_pid(), 0);
    assert_eq!(direct[127].blocker_pid(), 127);
}

#[test]
fn internal_wait_rejects_malformed_lock_edges() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    for ts in [3, 5, 7] {
        typed.insert_lock_snapshot(LockSnapshot {
            ts,
            activity_snapshot_ts: Some(ts),
            edges: vec![
                lock_edge(0, 99),
                lock_edge(-1, 99),
                lock_edge(10, 10),
                lock_edge(11, -1),
                LockEdge {
                    waiter_pid: 12,
                    waiter_backend_start: None,
                    blocker_pid: 99,
                },
            ],
        });
    }
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_lock_join_is_charged_before_membership_checks() {
    let mut typed = lock_wait_snapshots(&[10, 11, 12]);
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11, 12], &[99]);
    let outcome = internal_wait_outcome(&typed, 20);
    assert!(!outcome.complete);
    assert!(outcome.incidents[0].findings.is_empty());
    assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Work);
    assert_eq!(outcome.skipped[0].limit.observed, 29);
    assert_eq!(outcome.skipped[0].limit.limit, 20);
}

#[test]
fn internal_wait_reports_each_class_at_an_exact_tie() {
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: vec![
                ActivityBackend {
                    pid: 1,
                    state: Some("active".into()),
                    wait_event_type: Some("LWLock".into()),
                    ..base_backend()
                },
                ActivityBackend {
                    pid: 2,
                    state: Some("active".into()),
                    wait_event_type: Some("LWLock".into()),
                    ..base_backend()
                },
                lock_waiting_backend(10),
                lock_waiting_backend(11),
            ],
            completeness: SnapshotCompleteness::Complete,
        });
    }
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11], &[99]);
    let outcome = internal_wait_outcome(&typed, u64::MAX);
    let classes: Vec<_> = outcome.incidents[0]
        .findings
        .iter()
        .map(|finding| match finding.scope().identity() {
            [IdentityValue::Text(class)] => class.as_str(),
            _ => panic!("wait-class scope"),
        })
        .collect();
    assert_eq!(classes, ["LWLock", "Lock"]);
}

#[test]
fn internal_wait_lock_edges_do_not_override_incomplete_activity() {
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: [10, 11, 12].into_iter().map(lock_waiting_backend).collect(),
            completeness: SnapshotCompleteness::Restricted,
        });
    }
    insert_lock_edges(&mut typed, &[3, 5, 7], &[10, 11, 12], &[99]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_excludes_the_incident_end_snapshot() {
    let mut typed = TypedInputs::new();
    for ts in [0, 5, 10] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: [10, 11, 12].into_iter().map(lock_waiting_backend).collect(),
            completeness: SnapshotCompleteness::Complete,
        });
    }
    insert_lock_edges(&mut typed, &[0, 5, 10], &[10, 11, 12], &[99]);
    assert_no_internal_wait(&typed);
}

#[test]
fn internal_wait_withholds_ratio_without_complete_snapshot_markers() {
    let backends = vec![
        ActivityBackend {
            state: Some("active".into()),
            wait_event_type: Some("LWLock".into()),
            ..base_backend()
        },
        ActivityBackend {
            state: Some("active".into()),
            wait_event_type: Some("LWLock".into()),
            ..base_backend()
        },
        ActivityBackend {
            state: Some("active".into()),
            ..base_backend()
        },
    ];
    let mut typed = TypedInputs::new();
    for ts in [3, 5, 7] {
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts,
            backends: backends.clone(),
            completeness: SnapshotCompleteness::Unknown,
        });
    }
    assert!(
        run_lens(
            &InternalWaitConcentrationLens,
            PG_STAT_ACTIVITY,
            "wait_event_type",
            &typed
        )
        .is_empty()
    );
}

fn lock_typed(edges: Vec<LockEdge>) -> TypedInputs {
    let mut typed = TypedInputs::new();
    typed.insert_lock_snapshot(LockSnapshot {
        ts: 5,
        activity_snapshot_ts: None,
        edges,
    });
    typed
}

#[test]
fn lock_wait_graph_pairs_a_lead_blocker_with_a_downstream_waiter() {
    let typed = lock_typed(vec![LockEdge {
        waiter_pid: 20,
        waiter_backend_start: Some(1),
        blocker_pid: 10,
    }]);
    let findings = run_lens(&LockWaitGraphLens, PG_LOCKS, "blocked_by", &typed);
    assert_eq!(findings.len(), 2, "the edge proves both sides");
    assert_eq!(findings[0].0, Role::Lead, "the blocker leads");
    assert_eq!(findings[1].0, Role::Downstream, "the waiter trails");
    assert!(
        findings
            .iter()
            .all(|(_, confidence)| confidence.label() == "medium"),
        "missing lock target and mode cap both sides at medium"
    );
}

#[test]
fn lock_wait_graph_without_edges_reports_nothing() {
    // A pg_locks episode with no sampled edge is the honest boundary: the
    // lens is active but has no direct evidence to stand on.
    assert!(
        run_lens(
            &LockWaitGraphLens,
            PG_LOCKS,
            "blocked_by",
            &lock_typed(vec![])
        )
        .is_empty()
    );
    assert!(
        run_lens(
            &LockWaitGraphLens,
            PG_LOCKS,
            "blocked_by",
            &TypedInputs::new()
        )
        .is_empty()
    );
}

#[test]
fn a_snapshot_lens_ignores_a_cluster_without_its_section() {
    let typed = activity_typed(vec![ActivityBackend {
        xmin_age: Some(2_000_000),
        state: Some("idle in transaction".into()),
        ..base_backend()
    }]);
    assert!(run_lens(&XminHorizonHoldLens, PG_STAT_DATABASE, "blks_read", &typed).is_empty());
}

#[test]
fn wait_classifiers_cover_their_event_sets() {
    assert!(is_syncrep(Some("SyncRep")));
    assert!(!is_syncrep(Some("ClientRead")));
    assert!(!is_syncrep(None));

    for internal in ["LWLock", "BufferPin", "IO"] {
        assert!(is_internal_wait(Some(internal)), "{internal} is internal");
    }
    assert!(!is_internal_wait(Some("Client")), "client wait is external");
    assert!(!is_internal_wait(None));

    for idle in ["idle in transaction", "idle in transaction (aborted)"] {
        assert!(idle_in_transaction(Some(idle)), "{idle} pins the horizon");
    }
    assert!(!idle_in_transaction(Some("active")));
    assert!(!idle_in_transaction(None));
}
