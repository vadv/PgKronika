//! Exact-head overview parity qualification measurements.
//!
//! This is an evidence runner, not a universal performance claim. It records
//! the host, fixture schema, source/fact cardinality, byte accounting, and all
//! nine specification modes in one JSON artifact. Storage-cold measurements
//! remain explicitly unmeasured because this process cannot evict the host
//! page cache honestly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use arrow_array as _;
use arrow_schema as _;
use criterion as _;
use kronika_analytics::overview::{
    CountLimits, CoverageSpan, NamingContractId, OracleLimits, RawOracle, SegmentLocator,
};
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_reader::{
    BlockKind, FactFile, FactOrigin, FactStore, LIMIT, PgmUnit, SegmentContext, SegmentFacts,
};
use kronika_registry::pg_stat_database::PgStatDatabaseV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, Ts};
use kronika_store as _;
use kronika_writer as _;
use mimalloc as _;
use parquet as _;
use proptest as _;
use rustix as _;
use serde::Serialize;
use sha2 as _;
use tempfile as _;

const FIXTURE_SCHEMA: &str = "overview-dense-hour-v1";
const SAMPLES: usize = 720;
const CADENCE_US: i64 = 5_000_000;
const ITERATIONS: usize = 20;
const CONCURRENT_WORKERS: usize = 16;

const ORACLE_LIMITS: OracleLimits = OracleLimits {
    max_observations: 65_536,
    max_coverage_spans: 65_536,
    count_limits: CountLimits {
        max_input_entries: 65_536,
        max_joint_keys: 65_536,
        max_signal_keys: 65_536,
    },
};

#[derive(Debug, Serialize)]
struct QualificationArtifact {
    schema: &'static str,
    git_head: String,
    git_dirty: bool,
    generated_unix_ms: u128,
    ci: CiProfile,
    host: HostProfile,
    fixture: FixtureProfile,
    accounting: Accounting,
    budgets: Budgets,
    modes: Vec<ModeResult>,
    acceptance: Vec<AcceptanceEvidence>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CiProfile {
    repository: Option<String>,
    run_id: Option<String>,
    run_attempt: Option<String>,
    job: Option<String>,
    artifact_name: &'static str,
}

#[derive(Debug, Serialize)]
struct HostProfile {
    os: &'static str,
    arch: &'static str,
    kernel: String,
    filesystem: String,
    process_cold: bool,
    storage_cold: bool,
}

#[derive(Debug, Serialize)]
struct FixtureProfile {
    schema_version: &'static str,
    cadence_us: i64,
    source_rows: usize,
    source_sections: usize,
    source_bytes: usize,
    counter_series: usize,
    counter_samples: usize,
    gauge_series: usize,
    gauge_samples: usize,
    reset_markers: usize,
    entity_states: usize,
    factor_coverage: usize,
    event_facts: usize,
}

#[derive(Debug, Serialize)]
struct Accounting {
    fact_file_bytes: usize,
    decoded_block_bytes: u64,
    resident_fact_bytes: usize,
    pinned_fact_bytes: usize,
    fixed_metric_stored_bytes: u64,
    variable_event_string_stored_bytes: u64,
    retained_metric_samples: usize,
    fixed_metric_bytes_per_sample: f64,
}

#[derive(Debug, Serialize)]
struct Budgets {
    disk_bytes: Option<u64>,
    resident_bytes: Option<u64>,
    disk_within_budget: Option<bool>,
    resident_within_budget: Option<bool>,
    deployment_budget_status: &'static str,
    qualification_blocked: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct Work {
    pgm_body_reads: u64,
    pgm_body_bytes: u64,
    fact_reads: u64,
    fact_stored_bytes: u64,
    fact_decoded_bytes: u64,
    cache_writes: u64,
    successful_responses: u64,
}

#[derive(Debug, Serialize)]
struct ModeResult {
    mode: &'static str,
    iterations: usize,
    p50_ns: u128,
    p95_ns: u128,
    p99_ns: u128,
    work_per_iteration: Work,
    semantics: &'static str,
}

#[derive(Debug, Serialize)]
struct AcceptanceEvidence {
    id: u8,
    requirement: &'static str,
    evidence: &'static [&'static str],
    result: &'static str,
}

#[allow(
    clippy::too_many_lines,
    reason = "the qualification runner records all nine named modes in one auditable artifact"
)]
fn main() {
    let output = output_path().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    let pgm: Arc<[u8]> = dense_hour_pgm().into();
    let context = context();
    let unit = PgmUnit::open(pgm.as_ref()).expect("open dense-hour PGM");
    let facts = SegmentFacts::extract(&unit, &context, &LIMIT).expect("extract dense-hour facts");
    let fact_bytes = facts.encode(&LIMIT).expect("encode dense-hour facts");
    let catalog = facts.catalog_descriptors();
    let admitted = FactFile::admit(&fact_bytes, facts.identity(), facts.lineage(), &LIMIT)
        .expect("admit dense-hour facts");
    let accounting = accounting(&facts, &admitted, fact_bytes.len());
    let budgets = budgets(&accounting);
    let fixture = fixture_profile(&unit, &facts, pgm.len());
    let runtime_root = runtime_root(output.as_deref());

    let raw = Arc::new(facts);
    let fact_bytes: Arc<[u8]> = fact_bytes.into();
    let catalog = Arc::new(catalog);
    let context = Arc::new(context);
    let full_range = CoverageSpan::new(1_000_000, dense_end()).expect("dense full range");
    let restart_root = runtime_root.join("restart-warm");
    FactStore::new(&restart_root)
        .publish(raw.as_ref(), &LIMIT)
        .expect("seed restart-warm fact file");

    let mut modes = Vec::new();
    let derived_counter = std::cell::Cell::new(0_usize);
    modes.push(measure("derived-cold", ITERATIONS, || {
        let iteration = derived_counter.get();
        derived_counter.set(iteration + 1);
        let cache_root = runtime_root.join(format!("derived-cold-{iteration:02}"));
        assert!(
            !cache_root.exists(),
            "derived-cold cache root must start absent"
        );
        let unit = PgmUnit::open(pgm.as_ref()).expect("open cold PGM");
        let loaded = FactStore::new(cache_root)
            .load_or_build(&unit, context.as_ref(), &LIMIT)
            .expect("cold build and durable publication");
        assert_eq!(
            loaded.origin(),
            FactOrigin::Rebuilt,
            "an absent derived-cold root must rebuild"
        );
        assert_eq!(
            loaded.persist_error(),
            None,
            "derived-cold qualification requires durable publication"
        );
        assert_eq!(
            loaded.fact_write_bytes(),
            fact_bytes.len() as u64,
            "derived-cold publication must write the canonical fact bytes"
        );
        assert_eq!(
            loaded.facts(),
            raw.as_ref(),
            "derived-cold extraction diverged from the admitted fixture"
        );
        let pgm = loaded.pgm_body_read_stats();
        Work {
            pgm_body_reads: pgm.read_calls,
            pgm_body_bytes: pgm.stored_bytes_read,
            cache_writes: 1,
            successful_responses: 1,
            ..Work::default()
        }
    }));
    modes.push(measure("restart-warm", ITERATIONS, || {
        let unit = PgmUnit::open(pgm.as_ref()).expect("open restart-warm PGM metadata");
        let loaded = FactStore::new(&restart_root)
            .load_or_build(&unit, context.as_ref(), &LIMIT)
            .expect("restart-warm");
        assert_eq!(
            loaded.origin(),
            FactOrigin::CacheHit,
            "restart-warm must use the seeded durable fact file"
        );
        assert_eq!(
            loaded.pgm_body_read_stats().read_calls,
            0,
            "restart-warm must not read a PGM body"
        );
        let stats = loaded
            .fact_read_stats()
            .expect("restart-warm has exact fact read counters");
        assert_eq!(
            loaded.facts(),
            raw.as_ref(),
            "restart-warm decoding diverged from the admitted fixture"
        );
        Work {
            fact_reads: stats.read_calls,
            fact_stored_bytes: stats.stored_bytes_read,
            fact_decoded_bytes: stats.decoded_bytes,
            successful_responses: 1,
            ..Work::default()
        }
    }));
    modes.push(measure("process-hot", ITERATIONS, || {
        let result = raw
            .query(full_range, ORACLE_LIMITS)
            .expect("process-hot query");
        std::hint::black_box(result);
        Work {
            successful_responses: 1,
            ..Work::default()
        }
    }));
    let range_counter = std::cell::Cell::new(0_usize);
    modes.push(measure("range-cold/facts-warm", ITERATIONS, || {
        let offset = i64::try_from(range_counter.get() % 60).expect("offset fits") * CADENCE_US;
        range_counter.set(range_counter.get() + 1);
        let range = CoverageSpan::new(1_000_000 + offset, 1_000_000 + offset + 300_000_000)
            .expect("range-warm interval");
        std::hint::black_box(raw.query(range, ORACLE_LIMITS).expect("range-warm query"));
        Work {
            successful_responses: 1,
            ..Work::default()
        }
    }));
    modes.push(measure("live", ITERATIONS, || {
        let unit = PgmUnit::open(pgm.as_ref()).expect("open live part");
        let live = SegmentFacts::fold_live(
            &unit,
            b"qualification-store",
            1,
            b"dense-hour-active-part",
            &LIMIT,
        )
        .expect("fold live part");
        assert_eq!(
            live.counter_samples().samples().len(),
            raw.counter_samples().samples().len(),
            "live folding retained a different counter-sample cardinality"
        );
        let stats = unit.body_read_stats();
        Work {
            pgm_body_reads: stats.read_calls,
            pgm_body_bytes: stats.stored_bytes_read,
            successful_responses: 1,
            ..Work::default()
        }
    }));
    modes.push(measure("concurrent-identical", 5, || {
        let mut workers = Vec::with_capacity(CONCURRENT_WORKERS);
        for _ in 0..CONCURRENT_WORKERS {
            let bytes = Arc::clone(&fact_bytes);
            let catalog = Arc::clone(&catalog);
            let facts = Arc::clone(&raw);
            workers.push(std::thread::spawn(move || {
                SegmentFacts::from_reader(
                    bytes.as_ref(),
                    facts.identity(),
                    facts.lineage(),
                    catalog.as_ref(),
                    &LIMIT,
                )
                .expect("identical warm worker")
            }));
        }
        for worker in workers {
            assert_eq!(
                worker.join().expect("identical worker"),
                *raw,
                "concurrent identical decoding diverged from the admitted fixture"
            );
        }
        Work {
            successful_responses: CONCURRENT_WORKERS as u64,
            ..Work::default()
        }
    }));
    modes.push(measure("concurrent-disjoint", 5, || {
        let mut workers = Vec::with_capacity(CONCURRENT_WORKERS);
        for worker in 0..CONCURRENT_WORKERS {
            let facts = Arc::clone(&raw);
            workers.push(std::thread::spawn(move || {
                let start = 1_000_000 + i64::try_from(worker).expect("worker fits") * 30_000_000;
                let range = CoverageSpan::new(start, start + 30_000_000).expect("disjoint range");
                facts.query(range, ORACLE_LIMITS).expect("disjoint query")
            }));
        }
        for worker in workers {
            std::hint::black_box(worker.join().expect("disjoint worker"));
        }
        Work {
            successful_responses: CONCURRENT_WORKERS as u64,
            ..Work::default()
        }
    }));
    modes.push(measure("memory-only", ITERATIONS, || {
        let before = unit.body_read_stats();
        std::hint::black_box(
            raw.query(full_range, ORACLE_LIMITS)
                .expect("memory-only query"),
        );
        let after = unit.body_read_stats();
        assert_eq!(after, before, "memory-only query reread the PGM");
        Work {
            successful_responses: 1,
            ..Work::default()
        }
    }));
    modes.push(measure("oracle-profile", ITERATIONS, || {
        let warm = SegmentFacts::from_reader(
            fact_bytes.as_ref(),
            raw.identity(),
            raw.lineage(),
            catalog.as_ref(),
            &LIMIT,
        )
        .expect("oracle warm read");
        assert_eq!(
            warm, *raw,
            "oracle-profile decoding diverged from the admitted fixture"
        );
        Work {
            successful_responses: 1,
            ..Work::default()
        }
    }));

    let artifact = QualificationArtifact {
        schema: "pgkronika-overview-qualification-v1",
        git_head: git_output(&["rev-parse", "HEAD"]),
        git_dirty: !git_output(&["status", "--porcelain"]).is_empty(),
        generated_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
        ci: ci_profile(),
        host: host_profile(),
        fixture,
        accounting,
        budgets,
        modes,
        acceptance: acceptance_evidence(),
        limitations: vec![
            "storage-cold/page-cache-cold is not measured by this process",
            "HTTP serialization is measured separately by the web qualification tests",
            "final PASS requires this artifact and every CI job on the same exact git head",
        ],
    };
    let json = serde_json::to_vec_pretty(&artifact).expect("serialize qualification artifact");
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).expect("create artifact directory");
        }
        fs::write(&output, &json).expect("write qualification artifact");
        println!("{}", output.display());
    } else {
        println!("{}", String::from_utf8(json).expect("JSON is UTF-8"));
    }
}

fn output_path() -> Result<Option<PathBuf>, &'static str> {
    let mut arguments = std::env::args_os().skip(1);
    match (arguments.next(), arguments.next(), arguments.next()) {
        (None, None, None) => Ok(None),
        (Some(flag), Some(path), None) if flag == "--output" => Ok(Some(path.into())),
        _ => Err("usage: overview_qualification [--output PATH]"),
    }
}

fn runtime_root(output: Option<&Path>) -> PathBuf {
    let parent = output
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("target/qualification"), Path::to_path_buf);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    parent.join(format!("overview-runtime-{}-{nonce}", std::process::id()))
}

fn dense_end() -> i64 {
    1_000_000 + i64::try_from(SAMPLES).expect("sample count fits") * CADENCE_US
}

fn context() -> SegmentContext {
    SegmentContext::new(
        b"qualification-store".to_vec(),
        NamingContractId([0x31; 16]),
        SegmentLocator([0x41; 32]),
    )
    .expect("qualification context")
}

fn dense_hour_pgm() -> Vec<u8> {
    let database: Vec<_> = (0..SAMPLES)
        .map(|index| {
            let index = i64::try_from(index).expect("sample index fits");
            let index_f64 = f64::from(i32::try_from(index).expect("dense fixture index fits i32"));
            PgStatDatabaseV1 {
                ts: Ts(1_000_000 + index * CADENCE_US),
                datid: 16_384,
                datname: None,
                numbackends: Some(10 + i32::try_from(index % 5).expect("modulo fits")),
                xact_commit: 10_000 + index * 10,
                xact_rollback: 100 + index,
                blks_read: 1_000 + index,
                blks_hit: 20_000 + index * 20,
                tup_returned: 30_000 + index * 30,
                tup_fetched: 15_000 + index * 15,
                tup_inserted: 2_000 + index * 2,
                tup_updated: 3_000 + index * 3,
                tup_deleted: 500 + index,
                conflicts: index / 200,
                temp_files: index / 100,
                temp_bytes: index * 4_096,
                deadlocks: index / 180,
                blk_read_time: index_f64 * 0.25,
                blk_write_time: index_f64 * 0.125,
                stats_reset: None,
                frozen_xid_age: Some(1_000 + index),
                min_mxid_age: Some(100 + index),
                datconnlimit: Some(100),
                datallowconn: Some(true),
                datistemplate: Some(false),
            }
        })
        .collect();
    let coverage: Vec<_> = database
        .iter()
        .map(|row| SnapshotCoverageV1 {
            ts: row.ts,
            source_type_id: 1_005_001,
            collector_pid: 99,
            collector_started_at: Ts(1),
            read_state: 0,
            visibility: 0,
            source_total: 1,
            collected: 1,
        })
        .collect();
    let reset = [ResetMetadata {
        ts: Ts(1_000_000),
        postmaster_start_time: Ts(1),
        pg_stat_database_reset_max_at: None,
        pg_stat_statements_reset_at: None,
        pg_store_plans_reset_at: None,
        pg_stat_bgwriter_reset_at: None,
        pg_stat_checkpointer_reset_at: None,
        pg_stat_wal_reset_at: None,
        pg_stat_archiver_reset_at: None,
        pg_stat_io_reset_at: None,
        ext_pg_stat_statements_version: None,
        ext_pg_store_plans_version: None,
        compute_query_id: None,
        track_io_timing: None,
        track_wal_io_timing: None,
    }];
    let database_body = PgStatDatabaseV1::encode(&database).expect("encode dense database");
    let reset_body = ResetMetadata::encode(&reset).expect("encode dense reset context");
    let coverage_body =
        SnapshotCoverageV1::encode(&coverage).expect("encode dense source coverage");
    build_part(
        &[
            SectionInput {
                type_id: 1_005_001,
                rows: u32::try_from(database.len()).expect("database rows fit"),
                body: &database_body,
            },
            SectionInput {
                type_id: 1_020_001,
                rows: 1,
                body: &reset_body,
            },
            SectionInput {
                type_id: 1_038_001,
                rows: u32::try_from(coverage.len()).expect("coverage rows fit"),
                body: &coverage_body,
            },
        ],
        PartMeta {
            min_ts: 1_000_000,
            max_ts: dense_end() - CADENCE_US,
            source_id: 7,
        },
    )
}

fn fixture_profile(
    unit: &PgmUnit<&[u8]>,
    facts: &SegmentFacts,
    source_bytes: usize,
) -> FixtureProfile {
    FixtureProfile {
        schema_version: FIXTURE_SCHEMA,
        cadence_us: CADENCE_US,
        source_rows: SAMPLES,
        source_sections: unit.catalog().entries.len(),
        source_bytes,
        counter_series: facts.counter_samples().series().len(),
        counter_samples: facts.counter_samples().samples().len(),
        gauge_series: facts.gauge_samples().series().len(),
        gauge_samples: facts.gauge_samples().samples().len(),
        reset_markers: facts.reset_markers().markers().len(),
        entity_states: facts.entity_states().records().len(),
        factor_coverage: facts.loss_coverage().factor_coverage().len(),
        event_facts: facts.event_facts().len(),
    }
}

fn accounting(facts: &SegmentFacts, file: &FactFile, fact_file_bytes: usize) -> Accounting {
    let decoded_block_bytes = file.directory().iter().map(|entry| entry.decoded_len).sum();
    let metric_kinds = [
        BlockKind::LossCoverage,
        BlockKind::GaugeSamples,
        BlockKind::CounterSamples,
        BlockKind::ResetMarkers,
        BlockKind::EntityStates,
    ];
    let fixed_metric_stored_bytes = file
        .directory()
        .iter()
        .filter(|entry| {
            metric_kinds
                .iter()
                .any(|kind| kind.code() == entry.block_kind)
        })
        .map(|entry| entry.stored_len)
        .sum();
    let variable_event_string_stored_bytes = file
        .directory()
        .iter()
        .filter(|entry| {
            [
                BlockKind::EventObservations,
                BlockKind::EventFacts,
                BlockKind::StringTable,
            ]
            .iter()
            .any(|kind| kind.code() == entry.block_kind)
        })
        .map(|entry| entry.stored_len)
        .sum();
    let retained_metric_samples =
        facts.counter_samples().samples().len() + facts.gauge_samples().samples().len();
    let resident_fact_bytes = facts.resident_bytes().expect("resident size fits");
    let fixed_metric_stored_bytes_f64 = f64::from(
        u32::try_from(fixed_metric_stored_bytes)
            .expect("the fixed dense qualification fixture fits u32 bytes"),
    );
    let retained_metric_samples_f64 = f64::from(
        u32::try_from(retained_metric_samples)
            .expect("the fixed dense qualification fixture fits u32 samples"),
    );
    Accounting {
        fact_file_bytes,
        decoded_block_bytes,
        resident_fact_bytes,
        pinned_fact_bytes: resident_fact_bytes + 2 * size_of::<usize>(),
        fixed_metric_stored_bytes,
        variable_event_string_stored_bytes,
        retained_metric_samples,
        fixed_metric_bytes_per_sample: fixed_metric_stored_bytes_f64 / retained_metric_samples_f64,
    }
}

fn budgets(accounting: &Accounting) -> Budgets {
    let disk_bytes = env_u64("OVERVIEW_DENSE_DISK_BUDGET_BYTES");
    let resident_bytes = env_u64("OVERVIEW_DENSE_RESIDENT_BUDGET_BYTES");
    let disk_within_budget = disk_bytes.map(|limit| accounting.fact_file_bytes as u64 <= limit);
    let resident_within_budget =
        resident_bytes.map(|limit| accounting.pinned_fact_bytes as u64 <= limit);
    let deployment_budget_status = match (
        disk_bytes,
        resident_bytes,
        disk_within_budget,
        resident_within_budget,
    ) {
        (None, None, None, None) => "owner_deferred",
        (Some(_), Some(_), Some(true), Some(true)) => "within_approved",
        (Some(_), Some(_), Some(_), Some(_)) => "exceeds_approved",
        _ => "incomplete_configuration",
    };
    Budgets {
        disk_within_budget,
        resident_within_budget,
        deployment_budget_status,
        qualification_blocked: matches!(
            deployment_budget_status,
            "exceeds_approved" | "incomplete_configuration"
        ),
        disk_bytes,
        resident_bytes,
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn measure(
    mode: &'static str,
    iterations: usize,
    mut operation: impl FnMut() -> Work,
) -> ModeResult {
    let mut latencies = Vec::with_capacity(iterations);
    let mut expected_work = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let work = operation();
        latencies.push(started.elapsed().as_nanos());
        assert!(
            expected_work.is_none_or(|expected| expected == work),
            "{mode} work counters changed between iterations"
        );
        expected_work = Some(work);
    }
    latencies.sort_unstable();
    ModeResult {
        mode,
        iterations,
        p50_ns: percentile(&latencies, 50),
        p95_ns: percentile(&latencies, 95),
        p99_ns: percentile(&latencies, 99),
        work_per_iteration: expected_work.expect("measurement has iterations"),
        semantics: "process-cold or process-warm as named; OS page cache warm; storage-cold false",
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[rank.min(sorted.len() - 1)]
}

fn host_profile() -> HostProfile {
    HostProfile {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        kernel: command_output("uname", &["-srv"]),
        filesystem: command_output("stat", &["-f", "-c", "%T", "."]),
        process_cold: true,
        storage_cold: false,
    }
}

fn ci_profile() -> CiProfile {
    CiProfile {
        repository: std::env::var("GITHUB_REPOSITORY").ok(),
        run_id: std::env::var("GITHUB_RUN_ID").ok(),
        run_attempt: std::env::var("GITHUB_RUN_ATTEMPT").ok(),
        job: std::env::var("GITHUB_JOB").ok(),
        artifact_name: "overview-qualification",
    }
}

fn git_output(arguments: &[&str]) -> String {
    command_output("git", arguments)
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map_or_else(
            || "unavailable".to_owned(),
            |output| String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )
}

fn acceptance_evidence() -> Vec<AcceptanceEvidence> {
    const ROWS: [(&str, &[&str]); 18] = [
        ("restart-warm zero PGM reads", &["restart-warm"]),
        (
            "raw/index equality",
            &["oracle-profile", "reader all-family oracle"],
        ),
        (
            "partition/seal invariance",
            &["reader all-family contiguous partitions"],
        ),
        ("cache fallback", &["reader publish fallback suite"]),
        (
            "source damage visible",
            &["reader all-family source CRC suite"],
        ),
        (
            "policy bump avoids rebuild",
            &["web overview response cache suite"],
        ),
        ("cursor exact scan", &["web overview cursor suite"]),
        (
            "live/seal stable identities",
            &["reader live promotion suite"],
        ),
        ("lossless live builder", &["reader live state suite"]),
        (
            "required gap stays unknown",
            &["web overview health fixtures"],
        ),
        (
            "trusted floor survives",
            &["analytics health/downsample properties"],
        ),
        (
            "per-factor loss and applicability",
            &["web all-family API fixtures"],
        ),
        (
            "counter reset/gap/range semantics",
            &["reader all-family halo fixture"],
        ),
        (
            "source taxonomy mapping",
            &["reader metric extraction suite"],
        ),
        (
            "hit bypass and cold admission",
            &["web overview admission suite"],
        ),
        (
            "bounded memory-only fallback",
            &["memory-only", "dense accounting"],
        ),
        ("retention-safe quota and GC", &["reader GC suite"]),
        ("nine reproducible modes", &["this artifact"]),
    ];
    ROWS.iter()
        .enumerate()
        .map(|(index, (requirement, evidence))| AcceptanceEvidence {
            id: u8::try_from(index + 1).expect("acceptance ID fits"),
            requirement,
            evidence,
            result: "candidate; final result comes from exact-head CI attempt",
        })
        .collect()
}
