//! Deterministic producer-to-storage-to-real-HTTP plan evidence.
//!
//! Live optimizer plan selection varies across `PostgreSQL` majors and host
//! statistics. This fixture writes registry rows with fixed cumulative
//! counters, while the fork-specific features continue to prove that the real
//! collectors produce those row contracts from `PostgreSQL` 15-18.

use std::io::Write as _;

use anyhow::{Context, Result, ensure};
use cucumber::{given, then};
use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
use kronika_layout::{DataRoot, LayoutLimits, SegmentAddress, SegmentId};
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::pg_store_plans::PgStorePlansOsscV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, StrId, Ts};

use crate::BddWorld;
use crate::collector::SealedSegment;
use crate::harness::web_process::WebCase;

const MINUTE_US: i64 = 60 * 1_000_000;
const SNAPSHOTS: i64 = 60;
const SOURCE_ID: u64 = 7;

#[given("a deterministic OSSC plan-evidence segment")]
fn deterministic_plan_segment(world: &mut BddWorld) -> Result<()> {
    let directory = tempfile::tempdir().context("create plan-evidence fixture directory")?;
    let address = SegmentAddress::new(
        SegmentId::new(0).context("construct plan-evidence fixture SegmentId")?,
    )
    .context("construct plan-evidence fixture address")?;
    let root = DataRoot::open(directory.path()).context("open plan-evidence data root")?;
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .context("acquire plan-evidence fixture writer")?;
    let mut temporary = owner
        .create_pgm_temp(address)
        .context("create plan-evidence PGM temporary")?;
    temporary
        .file_mut()
        .write_all(&plan_evidence_pgm()?)
        .context("write plan-evidence PGM")?;
    temporary
        .file_mut()
        .sync_all()
        .context("sync plan-evidence PGM")?;
    temporary.publish().context("publish plan-evidence PGM")?;
    drop(temporary);
    drop(owner);
    let segment = SealedSegment::from_address(directory.path(), address)?;
    world.harness.set_segment(segment);
    world.harness.retain_collector_output_dir(directory);
    Ok(())
}

#[then("a real web process reports the plan-mixture and per-call buffer signals")]
async fn real_web_reports_plan_evidence(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let case = WebCase::from_segment(&segment, "plan-evidence")?;
    let process = case.spawn(&[]).await?;
    let target = format!(
        "/v1/anomalies?source={}&from={}&to={}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc",
        case.source_id(),
        case.range_start_us(),
        case.to_us() - 1,
    );
    let body_result = async {
        let body = process.client().get_json(&target).await?;
        assert_plan_evidence_response(&body)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let stop_result = process.stop().await;
    case.assert_sources_preserved()?;
    body_result?;
    stop_result
}

fn assert_plan_evidence_response(body: &serde_json::Value) -> Result<()> {
    ensure!(
        body["status"] == "signals_detected",
        "unexpected body: {body}"
    );
    ensure!(
        body["complete"] == true,
        "plan evidence must be complete: {body}"
    );
    let signals = body["plan_signals"]
        .as_array()
        .context("plan_signals is not an array")?;
    let distribution = signals
        .iter()
        .find(|signal| signal["signal_id"] == "pg.query.plan_distribution_shift.v1")
        .context("missing plan distribution signal")?;
    ensure!(
        distribution["scope"]["queryid"] == 7_777
            && distribution["scope"]["query_identity"] == "dbid_userid_core_queryid"
            && distribution["parameters"]["count_basis"] == "calls_delta",
        "distribution identity or normalization is wrong: {distribution}"
    );
    ensure!(
        distribution["evidence"]["current_newly_observed_planids"] == serde_json::json!([202]),
        "new plan membership is not explicit: {distribution}"
    );
    ensure!(
        distribution["evidence"]["total_variation"]
            .as_f64()
            .is_some_and(|distance| distance >= 0.20),
        "distribution effect gate is not evidenced: {distribution}"
    );

    let buffer = signals
        .iter()
        .find(|signal| {
            signal["signal_id"] == "pg.plan.buffer_work_per_call_increase.v1"
                && signal["scope"]["planid"] == 101
                && signal["dimension"]["column"] == "shared_blks_read"
        })
        .context("missing same-plan shared-read signal")?;
    let reference = buffer["evidence"]["reference_blocks_per_call"]
        .as_f64()
        .context("reference_blocks_per_call is not numeric")?;
    let current = buffer["evidence"]["current_blocks_per_call"]
        .as_f64()
        .context("current_blocks_per_call is not numeric")?;
    ensure!(
        current > reference,
        "per-call work did not increase: {buffer}"
    );
    ensure!(
        buffer["interpretation"] == "observed_same_plan_association_not_causation",
        "buffer evidence overclaims causality: {buffer}"
    );

    let analysis = &body["plan_analysis"]["pg_store_plans_ossc"];
    ensure!(
        analysis["status"] == "complete"
            && analysis["applicability"]["plan_distribution"] == "exact_queryid_identity"
            && analysis["quality"]["plan_set_additions"] == 1,
        "plan analysis provenance is incomplete: {analysis}"
    );
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the deterministic fixture keeps plan, coverage, reset, instance, and dictionary rows visibly co-located"
)]
fn plan_evidence_pgm() -> Result<Vec<u8>> {
    let mut interner = kronika_writer::Interner::new(
        DictLimits::new(64, 4096).context("plan fixture dictionary limits")?,
    );
    let (extension_version, compute_query_id, hostname, node_self_id, kernel_version, boot_id) = {
        let mut intern = |value: &str| -> Result<StrId> {
            interner
                .intern(value.as_bytes())
                .map(|id| StrId(id.get()))
                .context("intern plan fixture string")
        };
        (
            intern("1.10")?,
            intern("auto")?,
            intern("plan-host")?,
            intern("plan-node")?,
            intern("test-kernel")?,
            intern("test-boot")?,
        )
    };

    let mut calls = [100_i64, 0];
    let mut shared_reads = [100_i64, 0];
    let mut first_calls = [0_i64, 0];
    let mut plans = Vec::with_capacity(80);
    let mut coverage = Vec::with_capacity(usize::try_from(SNAPSHOTS)?);
    for minute in 0..SNAPSHOTS {
        let ts = minute * MINUTE_US;
        if minute != 0 {
            let shifted = (40..=50).contains(&minute);
            let call_deltas = if shifted {
                [4, 6]
            } else if minute < 40 {
                [10, 0]
            } else {
                [9, 1]
            };
            if minute == 40 {
                first_calls[1] = (minute - 1) * MINUTE_US + 1;
            }
            calls[0] += call_deltas[0];
            calls[1] += call_deltas[1];
            shared_reads[0] += if shifted {
                call_deltas[0] * 10
            } else {
                call_deltas[0]
            };
            shared_reads[1] += call_deltas[1];
        }
        plans.push(plan_row(ts, 101, calls[0], shared_reads[0], first_calls[0]));
        if minute >= 40 {
            plans.push(plan_row(ts, 202, calls[1], shared_reads[1], first_calls[1]));
        }
        let row_count = if minute < 40 { 1 } else { 2 };
        coverage.push(SnapshotCoverageV1 {
            ts: Ts(ts),
            source_type_id: 1_003_001,
            collector_pid: 42,
            collector_started_at: Ts(0),
            read_state: 0,
            visibility: 0,
            source_total: row_count,
            collected: row_count,
        });
    }

    let resets = (0..SNAPSHOTS)
        .map(|minute| ResetMetadata {
            ts: Ts(minute * MINUTE_US),
            postmaster_start_time: Ts(1),
            pg_stat_database_reset_max_at: None,
            pg_stat_statements_reset_at: None,
            pg_store_plans_reset_at: Some(Ts(1)),
            pg_stat_bgwriter_reset_at: None,
            pg_stat_checkpointer_reset_at: None,
            pg_stat_wal_reset_at: None,
            pg_stat_archiver_reset_at: None,
            pg_stat_io_reset_at: None,
            ext_pg_stat_statements_version: None,
            ext_pg_store_plans_version: Some(extension_version),
            compute_query_id: Some(compute_query_id),
            track_io_timing: Some(true),
            track_wal_io_timing: Some(false),
        })
        .collect::<Vec<_>>();
    let instance = InstanceMetadata {
        ts: Ts(0),
        hostname,
        node_self_id,
        pg_version_num: 150_000,
        kernel_version,
        pg_system_identifier: Some(99),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4096,
        boot_id,
        btime: Ts(0),
    };

    let dictionary =
        kronika_writer::dict::encode(interner.window()).context("encode fixture dictionary")?;
    let plans_body = PgStorePlansOsscV1::encode(&plans).context("encode plan evidence")?;
    let coverage_body = SnapshotCoverageV1::encode(&coverage).context("encode plan coverage")?;
    let reset_body = ResetMetadata::encode(&resets).context("encode plan reset metadata")?;
    let instance_body =
        InstanceMetadata::encode(&[instance]).context("encode plan instance metadata")?;
    let mut sections: Vec<SectionInput<'_>> = dictionary
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: &section.body,
        })
        .collect();
    sections.extend([
        SectionInput {
            type_id: 1_003_001,
            rows: u32::try_from(plans.len())?,
            body: &plans_body,
        },
        SectionInput {
            type_id: 1_038_001,
            rows: u32::try_from(coverage.len())?,
            body: &coverage_body,
        },
        SectionInput {
            type_id: 1_020_001,
            rows: u32::try_from(resets.len())?,
            body: &reset_body,
        },
        SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &instance_body,
        },
    ]);
    Ok(build_part(
        &sections,
        PartMeta {
            min_ts: 0,
            max_ts: (SNAPSHOTS - 1) * MINUTE_US,
            source_id: SOURCE_ID,
        },
    ))
}

const fn plan_row(
    ts: i64,
    planid: i64,
    calls: i64,
    shared_blks_read: i64,
    first_call: i64,
) -> PgStorePlansOsscV1 {
    PgStorePlansOsscV1 {
        ts: Ts(ts),
        queryid: 7_777,
        planid,
        userid: 10,
        dbid: 5,
        datname: None,
        usename: None,
        plan: None,
        calls,
        total_time: 0.0,
        min_time: 1.0,
        max_time: 1.0,
        mean_time: 1.0,
        stddev_time: 0.0,
        rows: calls,
        shared_blks_hit: 0,
        shared_blks_read,
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
        first_call: Ts(first_call),
        last_call: Ts(ts),
    }
}
