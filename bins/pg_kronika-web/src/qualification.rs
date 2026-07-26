//! Exact-head Overview M6 qualification runner.
//!
//! The public entry point exists only behind the `qualification` feature. Each
//! timing sample runs in a fresh child process over a fresh owned data
//! directory. A separate `strace` pass records file-operation counts so syscall
//! tracing cannot distort the reported latency samples.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use kronika_analytics::overview::{CountLimits, CoverageSpan, OracleLimits, RawOracle};
use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
use kronika_reader::{
    BlockKind, FactFile, FactOrigin, FactStore, FallbackConfig, LIMIT, LocalDirSnapshot,
    PersistError, PersistenceProbeOutcome, PgmUnit, SegmentContext, SegmentFacts,
};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_stat_database::PgStatDatabaseV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, Ts};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde::{Deserialize, Serialize};
use tower::ServiceExt as _;

use crate::overview::live::LiveFoldStats;
use crate::overview::loader::{LoaderIoSnapshot, LoaderQualificationSnapshot};
use crate::{AppState, OverviewConfig, app};

const ARTIFACT_SCHEMA: &str = "pgkronika-overview-qualification-v2";
const FIXTURE_SCHEMA: &str = "overview-dense-hour-v2";
const SAMPLES: usize = 720;
const CADENCE_US: i64 = 5_000_000;
const ITERATIONS: usize = 20;
const CONCURRENT_WORKERS: usize = 16;
const SOURCE_ID: u64 = 7;

/// Prefix emitted by a qualification-enabled real server after its listener
/// has bound successfully.
#[doc(hidden)]
pub const PROCESS_READY_PREFIX: &str = "PGKRONIKA_QUALIFICATION_WEB_READY ";

/// Announces the exact ephemeral listener address to the process BDD harness.
///
/// Production builds do not contain this function or write to stdout.
#[doc(hidden)]
pub fn announce_process_ready(address: std::net::SocketAddr) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{PROCESS_READY_PREFIX}{address}")?;
    stdout.flush()
}

const ORACLE_LIMITS: OracleLimits = OracleLimits {
    max_observations: 65_536,
    max_coverage_spans: 65_536,
    count_limits: CountLimits {
        max_input_entries: 65_536,
        max_joint_keys: 65_536,
        max_signal_keys: 65_536,
    },
};

const MODES: [&str; 9] = [
    "derived-cold",
    "restart-warm",
    "process-hot",
    "range-cold/facts-warm",
    "live",
    "concurrent-identical",
    "concurrent-disjoint",
    "memory-only",
    "oracle-profile",
];

/// Runs the qualification coordinator or one private worker invocation.
#[allow(
    clippy::exit,
    reason = "invalid standalone CLI usage must return a nonzero process status"
)]
pub fn run_cli() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [flag, mode, root] if flag == "--worker" => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build qualification runtime");
            let outcome = runtime.block_on(run_worker(
                mode.to_str().expect("UTF-8 mode"),
                Path::new(root),
            ));
            println!(
                "{}",
                serde_json::to_string(&outcome).expect("serialize worker result")
            );
        }
        [flag, output] if flag == "--output" => run_coordinator(Path::new(output)),
        _ => {
            eprintln!(
                "usage: overview_m6_qualification --output PATH\n\
                 private: overview_m6_qualification --worker MODE ROOT"
            );
            std::process::exit(2);
        }
    }
}

#[derive(Debug, Serialize)]
struct QualificationArtifact {
    schema: &'static str,
    git_head: String,
    git_dirty: bool,
    generated_unix_ms: u128,
    ci: CiProfile,
    host: HostProfile,
    storage: StorageProfile,
    fixture: FixtureProfile,
    accounting: Accounting,
    budgets: Budgets,
    modes: Vec<ModeResult>,
    compact_performance: CompactPerformanceProfile,
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
    filesystem_device: u64,
    process_samples_are_fresh_children: bool,
    syscall_trace_scope: &'static str,
    storage_cold: bool,
}

#[derive(Debug, Serialize)]
struct StorageProfile {
    model: &'static str,
    active_journal_name: &'static str,
    pgm_file_name: &'static str,
    sidecar_file_name: &'static str,
    same_stem: bool,
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
    auxiliary_datasets: [&'static str; 10],
}

#[derive(Debug, Serialize)]
struct Accounting {
    fact_file_logical_bytes: usize,
    fact_file_allocated_bytes: u64,
    header_and_directory_bytes: u64,
    stored_block_bytes: u64,
    decoded_block_bytes: u64,
    resident_fact_bytes: usize,
    pinned_fact_bytes: usize,
    fixed_metric_stored_bytes: u64,
    variable_event_string_stored_bytes: u64,
    retained_metric_samples: usize,
    fixed_metric_bytes_per_sample_numerator: u64,
    fixed_metric_bytes_per_sample_denominator: usize,
    identity_holds: bool,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Work {
    pgm_body_reads: u64,
    pgm_body_bytes: u64,
    pgm_sections_decoded: u64,
    pgm_rows_decoded: u64,
    fact_reads: u64,
    fact_stored_bytes: u64,
    fact_decoded_bytes: u64,
    sidecar_writes: u64,
    sidecar_write_bytes: u64,
    source_builds: u64,
    singleflight_builds: u64,
    singleflight_waiters: u64,
    persistence_failures: u64,
    publication_attempts: u64,
    retry_probes: u64,
    max_inflight_builds: u32,
    max_inflight_file_descriptors: u32,
    max_queue_depth: usize,
    decoded_cache_entries: usize,
    decoded_cache_bytes: usize,
    fallback_hits: u64,
    fallback_request_pgm_body_reads: u64,
    recovered_restart_pgm_body_reads: u64,
    fallback_resident_entries: u64,
    fallback_resident_segment_hours: u64,
    fallback_resident_bytes: u64,
    completed_active_parts: u64,
    visibility_lag_us: u64,
    tail_pending_from_offset_bytes: u64,
    tail_pending_to_offset_bytes: u64,
    successful_responses: u64,
    serialized_response_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ProcIo {
    rchar: u64,
    wchar: u64,
    syscr: u64,
    syscw: u64,
    read_bytes: u64,
    write_bytes: u64,
    cancelled_write_bytes: u64,
}

impl ProcIo {
    const fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            rchar: self.rchar.saturating_sub(earlier.rchar),
            wchar: self.wchar.saturating_sub(earlier.wchar),
            syscr: self.syscr.saturating_sub(earlier.syscr),
            syscw: self.syscw.saturating_sub(earlier.syscw),
            read_bytes: self.read_bytes.saturating_sub(earlier.read_bytes),
            write_bytes: self.write_bytes.saturating_sub(earlier.write_bytes),
            cancelled_write_bytes: self
                .cancelled_write_bytes
                .saturating_sub(earlier.cancelled_write_bytes),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerOutcome {
    wall_ns: u128,
    cpu_ns: u128,
    process_peak_rss_bytes: u64,
    fd_start: usize,
    fd_peak: usize,
    fd_end: usize,
    proc_io: ProcIo,
    work: Work,
}

#[derive(Debug, Serialize)]
struct ModeResult {
    mode: &'static str,
    semantics: &'static str,
    iterations: usize,
    wall_p50_ns: u128,
    wall_p95_ns: u128,
    wall_p99_ns: u128,
    cpu_p50_ns: u128,
    cpu_p95_ns: u128,
    cpu_p99_ns: u128,
    peak_rss_bytes: u64,
    peak_open_file_descriptors: usize,
    samples: Vec<WorkerOutcome>,
    syscalls: SyscallCounts,
}

#[derive(Debug, Serialize)]
struct CompactPerformanceProfile {
    semantics: &'static str,
    modes: Vec<CompactModeResult>,
}

#[derive(Debug, Serialize)]
struct CompactModeResult {
    mode: &'static str,
    iterations: usize,
    wall_p50_ns: u128,
    wall_p95_ns: u128,
    wall_p99_ns: u128,
    samples_ns: Vec<u128>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct SyscallCounts {
    process_scope: bool,
    opens: u64,
    reads: u64,
    writes: u64,
    syncs: u64,
    renames: u64,
    unlinks: u64,
    total_traced: u64,
}

#[derive(Debug, Serialize)]
struct EvidenceRef {
    kind: &'static str,
    binary: &'static str,
    path: &'static str,
    name: &'static str,
}

#[derive(Debug, Serialize)]
struct AcceptanceEvidence {
    id: u8,
    requirement: &'static str,
    implementation_status: &'static str,
    evidence: Vec<EvidenceRef>,
    decision: &'static str,
}

fn run_coordinator(output: &Path) {
    let iterations = iterations();
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("target/qualification"));
    fs::create_dir_all(parent).expect("create qualification output directory");
    let runtime_root = parent.join(format!(
        "overview-runtime-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    fs::create_dir(&runtime_root).expect("create qualification runtime root");

    let executable = std::env::current_exe().expect("qualification executable");
    let mut modes = Vec::with_capacity(MODES.len());
    for mode in MODES {
        let mut samples = Vec::with_capacity(iterations);
        for iteration in 0..iterations {
            let root = runtime_root.join(format!("{}-{iteration:02}", mode_slug(mode)));
            prepare_mode(mode, &root);
            samples.push(spawn_worker(&executable, mode, &root));
        }
        let trace_root = runtime_root.join(format!("{}-trace", mode_slug(mode)));
        prepare_mode(mode, &trace_root);
        let syscalls = trace_worker(&executable, mode, &trace_root);
        modes.push(mode_result(mode, samples, syscalls));
    }

    let dense = dense_hour_pgm(SOURCE_ID, 1_000_000, SAMPLES);
    let unit = PgmUnit::open(dense.as_slice()).expect("open dense fixture");
    let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract dense fixture");
    let encoded = facts.encode(&LIMIT).expect("encode dense facts");
    let file = FactFile::admit(&encoded, facts.identity(), facts.lineage(), &LIMIT)
        .expect("admit dense facts");
    let profile_root = runtime_root.join("accounting-profile");
    fs::create_dir(&profile_root).expect("create accounting profile directory");
    fs::write(profile_root.join("dense-hour.pgm"), &dense).expect("write profile PGM");
    FactStore::new(&profile_root)
        .publish(
            &facts,
            &SegmentContext::new("dense-hour.pgm").expect("profile context"),
            &LIMIT,
        )
        .expect("publish profile OVF");
    let sidecar_meta =
        fs::metadata(profile_root.join("dense-hour.ovf")).expect("stat profile sidecar");

    let accounting = accounting(&facts, &file, encoded.len(), &sidecar_meta);
    let compact_performance = compact_performance(&runtime_root, &dense, &facts);
    let artifact = QualificationArtifact {
        schema: ARTIFACT_SCHEMA,
        git_head: command_output("git", &["rev-parse", "HEAD"]),
        git_dirty: !command_output("git", &["status", "--porcelain"]).is_empty(),
        generated_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
        ci: ci_profile(),
        host: host_profile(&runtime_root),
        storage: StorageProfile {
            model: "owned-data-directory-sibling-sidecars-v1",
            active_journal_name: "active.parts",
            pgm_file_name: "dense-hour.pgm",
            sidecar_file_name: "dense-hour.ovf",
            same_stem: true,
        },
        fixture: fixture_profile(&unit, &facts, dense.len()),
        budgets: budgets(&accounting),
        accounting,
        modes,
        compact_performance,
        acceptance: acceptance_evidence(),
        limitations: vec![
            "storage-cold/page-cache-cold is not measured or claimed",
            "deployment size budgets remain owner-deferred unless both approved values are configured",
            "charts remain owner-deferred and are absent from the qualification datasets",
            "the final PASS is assigned only by the same-head same-attempt CI acceptance job",
        ],
    };
    let json = serde_json::to_vec_pretty(&artifact).expect("serialize artifact");
    fs::write(output, json).expect("write qualification artifact");
    println!("{}", output.display());
}

fn prepare_mode(mode: &str, root: &Path) {
    assert!(!root.exists(), "qualification root must start absent");
    fs::create_dir(root).expect("create mode data directory");
    match mode {
        "concurrent-disjoint" => {
            for worker in 0..CONCURRENT_WORKERS {
                let source = 100 + u64::try_from(worker).expect("worker source");
                let start =
                    1_000_000 + i64::try_from(worker).expect("worker range") * 1_000_000_000;
                let bytes = dense_hour_pgm(source, start, 32);
                fs::write(root.join(format!("segment-{worker:02}.pgm")), bytes)
                    .expect("write disjoint PGM");
            }
        }
        "live" => {
            fs::write(
                root.join("dense-hour.pgm"),
                dense_hour_pgm(SOURCE_ID, 1_000_000, SAMPLES),
            )
            .expect("write live sealed PGM");
            let first = lifecycle_part(dense_end() + 10, 41);
            fs::write(root.join("active.parts"), framed(&first)).expect("write first active frame");
        }
        _ => {
            let bytes = dense_hour_pgm(SOURCE_ID, 1_000_000, SAMPLES);
            fs::write(root.join("dense-hour.pgm"), &bytes).expect("write dense PGM");
            if matches!(mode, "restart-warm" | "oracle-profile") {
                let facts = SegmentFacts::extract(
                    &PgmUnit::open(bytes.as_slice()).expect("open seed PGM"),
                    &LIMIT,
                )
                .expect("extract seed facts");
                FactStore::new(root)
                    .publish(
                        &facts,
                        &SegmentContext::new("dense-hour.pgm").expect("seed context"),
                        &LIMIT,
                    )
                    .expect("seed durable facts");
            }
        }
    }
}

fn spawn_worker(executable: &Path, mode: &str, root: &Path) -> WorkerOutcome {
    let output = Command::new(executable)
        .arg("--worker")
        .arg(mode)
        .arg(root)
        .output()
        .expect("spawn qualification worker");
    require_success(&output, mode);
    serde_json::from_slice(&output.stdout).expect("decode qualification worker")
}

fn trace_worker(executable: &Path, mode: &str, root: &Path) -> SyscallCounts {
    let trace = root.with_extension("strace-summary");
    let output = Command::new("strace")
        .args([
            "-f",
            "-qq",
            "-c",
            "-e",
            "trace=open,openat,openat2,creat,read,pread64,readv,preadv,write,pwrite64,writev,pwritev,fsync,fdatasync,rename,renameat,renameat2,unlink,unlinkat",
            "-o",
        ])
        .arg(&trace)
        .arg(executable)
        .arg("--worker")
        .arg(mode)
        .arg(root)
        .output()
        .expect("run strace qualification pass");
    require_success(&output, mode);
    parse_strace_summary(&fs::read_to_string(trace).expect("read strace summary"))
}

fn require_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} worker failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse_strace_summary(summary: &str) -> SyscallCounts {
    let mut counts = SyscallCounts {
        process_scope: true,
        ..SyscallCounts::default()
    };
    for line in summary.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 5 || columns[0].starts_with('-') || columns[0] == "%" {
            continue;
        }
        let syscall = columns[columns.len() - 1];
        if syscall == "total" {
            continue;
        }
        let calls = columns[3].parse::<u64>().expect("strace call count");
        counts.total_traced = counts.total_traced.saturating_add(calls);
        match syscall {
            "open" | "openat" | "openat2" | "creat" => {
                counts.opens = counts.opens.saturating_add(calls);
            }
            "read" | "pread64" | "readv" | "preadv" => {
                counts.reads = counts.reads.saturating_add(calls);
            }
            "write" | "pwrite64" | "writev" | "pwritev" => {
                counts.writes = counts.writes.saturating_add(calls);
            }
            "fsync" | "fdatasync" => counts.syncs = counts.syncs.saturating_add(calls),
            "rename" | "renameat" | "renameat2" => {
                counts.renames = counts.renames.saturating_add(calls);
            }
            "unlink" | "unlinkat" => counts.unlinks = counts.unlinks.saturating_add(calls),
            _ => unreachable!("unclassified traced syscall {syscall}"),
        }
    }
    assert!(counts.total_traced > 0, "empty strace evidence");
    counts
}

async fn run_worker(mode: &str, root: &Path) -> WorkerOutcome {
    match mode {
        "derived-cold" => http_cold(root, false).await,
        "restart-warm" => http_cold(root, true).await,
        "process-hot" => process_hot(root).await,
        "range-cold/facts-warm" => range_cold(root).await,
        "live" => live_mode(root).await,
        "concurrent-identical" => concurrent_identical(root).await,
        "concurrent-disjoint" => concurrent_disjoint(root).await,
        "memory-only" => memory_only(root),
        "oracle-profile" => oracle_profile(root),
        _ => unreachable!("unknown qualification mode {mode}"),
    }
}

async fn http_cold(root: &Path, restart: bool) -> WorkerOutcome {
    let state = state(root, &OverviewConfig::new());
    let service = qualification_service(state.clone());
    let before = state.overview_loader.qualification_snapshot();
    let measurement = Measurement::start();
    let body = request_json(
        &service,
        &format!(
            "/v1/timeline/overview?source={SOURCE_ID}&from=1000000&to={}",
            dense_end()
        ),
    )
    .await;
    let after = state.overview_loader.qualification_snapshot();
    let work = loader_work(&before, &after, 1, body.len(), SAMPLES);
    assert_eq!(
        work.source_builds,
        u64::from(!restart),
        "only the derived-cold mode may build source facts"
    );
    assert_eq!(
        work.pgm_body_reads == 0,
        restart,
        "only restart-warm must avoid PGM body reads"
    );
    assert_eq!(
        work.sidecar_writes,
        u64::from(!restart),
        "only the derived-cold mode may publish the sidecar"
    );
    assert!(
        root.join("dense-hour.ovf").is_file(),
        "cold and restart modes must leave one durable sibling sidecar"
    );
    measurement.finish(work)
}

async fn process_hot(root: &Path) -> WorkerOutcome {
    let state = state(root, &OverviewConfig::new());
    let service = qualification_service(state.clone());
    let uri = format!(
        "/v1/timeline/overview?source={SOURCE_ID}&from=1000000&to={}",
        dense_end()
    );
    drop(request_json(&service, &uri).await);
    let before = state.overview_loader.qualification_snapshot();
    let measurement = Measurement::start();
    let body = request_json(&service, &uri).await;
    let after = state.overview_loader.qualification_snapshot();
    let work = loader_work(&before, &after, 1, body.len(), SAMPLES);
    assert_eq!(work.pgm_body_reads, 0, "a process-hot hit read PGM bodies");
    assert_eq!(work.fact_reads, 0, "a process-hot hit read the sidecar");
    assert_eq!(
        work.sidecar_writes, 0,
        "a process-hot hit rewrote the sidecar"
    );
    assert_eq!(work.source_builds, 0, "a process-hot hit rebuilt facts");
    measurement.finish(work)
}

async fn range_cold(root: &Path) -> WorkerOutcome {
    let state = state(root, &OverviewConfig::new());
    let service = qualification_service(state.clone());
    drop(
        request_json(
            &service,
            &format!(
                "/v1/timeline/overview?source={SOURCE_ID}&from=1000000&to={}",
                dense_end()
            ),
        )
        .await,
    );
    let sidecar = root.join("dense-hour.ovf");
    let sidecar_before = fs::read(&sidecar).expect("read policy-neutral sidecar");
    let metadata_before = fs::metadata(&sidecar).expect("stat policy-neutral sidecar");
    let identity_before = (
        metadata_before.dev(),
        metadata_before.ino(),
        metadata_before.mtime(),
        metadata_before.mtime_nsec(),
        metadata_before.len(),
    );
    let before = state.overview_loader.qualification_snapshot();
    let measurement = Measurement::start();
    let body = request_json(
        &service,
        "/v1/timeline/health?source=7&from=61000000&to=361000000&step=30000000",
    )
    .await;
    let after = state.overview_loader.qualification_snapshot();
    let work = loader_work(&before, &after, 1, body.len(), SAMPLES);
    assert_eq!(
        work.pgm_body_reads, 0,
        "a policy-only query read PGM bodies"
    );
    assert_eq!(work.fact_reads, 0, "a policy-only query reread the sidecar");
    assert_eq!(
        work.sidecar_writes, 0,
        "a policy-only query rewrote the sidecar"
    );
    assert_eq!(work.source_builds, 0, "a policy-only query rebuilt facts");
    assert!(
        work.decoded_cache_entries > 0,
        "the policy-only query did not reuse decoded canonical facts"
    );
    let outcome = measurement.finish(work);
    let metadata_after = fs::metadata(&sidecar).expect("restat policy-neutral sidecar");
    assert_eq!(
        (
            metadata_after.dev(),
            metadata_after.ino(),
            metadata_after.mtime(),
            metadata_after.mtime_nsec(),
            metadata_after.len(),
        ),
        identity_before,
        "a presentation-policy/range change rewrote canonical facts"
    );
    assert_eq!(
        fs::read(&sidecar).expect("reread policy-neutral sidecar"),
        sidecar_before,
        "a presentation-policy/range change changed OVF bytes"
    );
    outcome
}

async fn concurrent_identical(root: &Path) -> WorkerOutcome {
    let state = state(root, &OverviewConfig::new());
    let (snapshot, view) = state.overview_request_view();
    let plan = state
        .select_overview(
            view,
            &[SOURCE_ID],
            CoverageSpan::new(1_000_000, dense_end()).expect("identical range"),
        )
        .expect("identical plan");
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT_WORKERS + 1));
    let mut tasks = Vec::with_capacity(CONCURRENT_WORKERS);
    for _ in 0..CONCURRENT_WORKERS {
        let worker_state = state.clone();
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_plan = plan.clone();
        let worker_barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            worker_barrier.wait().await;
            worker_state
                .load_overview_selection(worker_snapshot, &worker_plan)
                .await
        }));
    }
    let before = state.overview_loader.qualification_snapshot();
    let measurement = Measurement::start();
    barrier.wait().await;
    for task in tasks {
        task.await
            .expect("identical worker task")
            .expect("identical fact view");
    }
    let after = state.overview_loader.qualification_snapshot();
    let work = loader_work(&before, &after, CONCURRENT_WORKERS, 0, SAMPLES);
    assert_eq!(
        work.singleflight_builds, 1,
        "identical requests did not share one singleflight leader"
    );
    assert_eq!(
        work.source_builds, 1,
        "identical requests performed more than one source build"
    );
    assert_eq!(
        work.successful_responses, CONCURRENT_WORKERS as u64,
        "an identical concurrent request did not complete"
    );
    assert!(
        work.max_inflight_builds <= 4,
        "identical work exceeded the worker capacity"
    );
    measurement.finish(work)
}

async fn concurrent_disjoint(root: &Path) -> WorkerOutcome {
    let mut config = OverviewConfig::new();
    config.cold.max_workers = 4;
    config.cold.per_request_parallelism = 1;
    config.cold.publications = 4;
    config.cold.file_descriptors = 16;
    let state = state(root, &config);
    let (snapshot, view) = state.overview_request_view();
    let mut plans = Vec::with_capacity(CONCURRENT_WORKERS);
    for worker in 0..CONCURRENT_WORKERS {
        let source = 100 + u64::try_from(worker).expect("worker source");
        let start = 1_000_000 + i64::try_from(worker).expect("worker range") * 1_000_000_000;
        plans.push(
            state
                .select_overview(
                    Arc::clone(&view),
                    &[source],
                    CoverageSpan::new(start, start + 32 * CADENCE_US).expect("disjoint range"),
                )
                .expect("disjoint plan"),
        );
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT_WORKERS + 1));
    let mut tasks = Vec::with_capacity(CONCURRENT_WORKERS);
    for plan in plans {
        let worker_state = state.clone();
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            worker_barrier.wait().await;
            worker_state
                .load_overview_selection(worker_snapshot, &plan)
                .await
        }));
    }
    let before = state.overview_loader.qualification_snapshot();
    let measurement = Measurement::start();
    barrier.wait().await;
    for task in tasks {
        task.await
            .expect("disjoint worker task")
            .expect("disjoint fact view");
    }
    let after = state.overview_loader.qualification_snapshot();
    let work = loader_work(&before, &after, CONCURRENT_WORKERS, 0, 32);
    assert_eq!(
        work.singleflight_builds, CONCURRENT_WORKERS as u64,
        "disjoint keys unexpectedly shared singleflight work"
    );
    assert_eq!(
        work.source_builds, CONCURRENT_WORKERS as u64,
        "a disjoint source build was lost"
    );
    assert_eq!(
        work.successful_responses, CONCURRENT_WORKERS as u64,
        "a disjoint concurrent request did not complete"
    );
    assert!(
        work.max_inflight_builds <= config.cold.max_workers,
        "disjoint work exceeded the worker capacity"
    );
    assert!(
        work.max_inflight_file_descriptors <= config.cold.file_descriptors,
        "disjoint work exceeded the file-descriptor capacity"
    );
    assert!(
        work.max_queue_depth <= config.cold.max_queue,
        "disjoint work exceeded the queue capacity"
    );
    measurement.finish(work)
}

async fn live_mode(root: &Path) -> WorkerOutcome {
    let state = state(root, &OverviewConfig::new());
    let service = qualification_service(state.clone());
    let before_loader = state.overview_loader.qualification_snapshot();
    let before_live = live_stats(&state);
    let second = lifecycle_part(dense_end() + 20, 42);
    let completed_frame = framed(&second);
    let next_header = FrameHeader {
        part_len: u64::try_from(second.len()).expect("active part length"),
    }
    .encode();
    let pending_from = fs::metadata(root.join("active.parts"))
        .expect("stat active journal")
        .len()
        .checked_add(u64::try_from(completed_frame.len()).expect("completed frame length"))
        .expect("pending tail offset");
    let pending_to = pending_from
        .checked_add(4)
        .expect("pending tail end offset");

    let measurement = Measurement::start();
    let mut journal = OpenOptions::new()
        .append(true)
        .open(root.join("active.parts"))
        .expect("open active journal");
    journal
        .write_all(&completed_frame)
        .expect("append completed active frame");
    journal
        .write_all(&next_header[..4])
        .expect("append pending tail");
    journal.sync_all().expect("sync active journal");

    let mut snapshot = state.snapshot().as_ref().clone();
    let delta = snapshot
        .refresh_incremental_delta()
        .expect("refresh appended active frame");
    assert_eq!(
        delta.journal.completed_parts.len(),
        1,
        "incremental refresh did not admit exactly one completed active part"
    );
    state
        .republish_store_view(snapshot, &delta)
        .expect("publish appended live view");
    let body = request_json(
        &service,
        &format!(
            "/v1/timeline/events?source={SOURCE_ID}&from=1000000&to={}&limit=100",
            dense_end() + 100
        ),
    )
    .await;
    let value: serde_json::Value = serde_json::from_slice(&body).expect("live JSON");
    let events = value["events"].as_array().expect("live event array");
    assert_eq!(
        events
            .iter()
            .filter(|event| { event["event_kind"] == "pg.lifecycle.child_signal_termination" })
            .count(),
        2,
        "both completed active lifecycle frames must be visible"
    );
    assert_eq!(
        value["meta"]["tail_pending"],
        serde_json::json!({
            "from_offset_bytes": pending_from,
            "to_offset_bytes": pending_to,
        }),
        "truncated next frame must publish its exact byte range"
    );
    let after_loader = state.overview_loader.qualification_snapshot();
    let after_live = live_stats(&state);
    let mut work = loader_work(&before_loader, &after_loader, 1, body.len(), SAMPLES);
    work.completed_active_parts = after_live
        .completed_parts
        .saturating_sub(before_live.completed_parts);
    work.pgm_body_reads = work.pgm_body_reads.saturating_add(
        after_live
            .pgm_body_reads
            .saturating_sub(before_live.pgm_body_reads),
    );
    work.pgm_body_bytes = work.pgm_body_bytes.saturating_add(
        after_live
            .pgm_body_bytes
            .saturating_sub(before_live.pgm_body_bytes),
    );
    work.visibility_lag_us =
        u64::try_from(measurement.started.elapsed().as_micros()).unwrap_or(u64::MAX);
    work.tail_pending_from_offset_bytes = pending_from;
    work.tail_pending_to_offset_bytes = pending_to;
    assert_eq!(
        work.completed_active_parts, 1,
        "live evidence did not account for the completed part"
    );
    assert!(
        work.visibility_lag_us <= 2_500_000,
        "live visibility exceeded the 2.5-second contract"
    );
    measurement.finish(work)
}

#[allow(
    clippy::too_many_lines,
    reason = "one mode must keep fallback, recovery, restart, and accounting evidence in a single measured process"
)]
fn memory_only(root: &Path) -> WorkerOutcome {
    let snapshot = LocalDirSnapshot::open(root).expect("memory snapshot");
    let descriptor = snapshot.sealed_descriptors()[0];
    let context = snapshot
        .sealed_context(&descriptor)
        .expect("memory context");
    let fallback_config =
        FallbackConfig::new(2, 16 * 1024 * 1024).expect("memory qualification bounds");
    let store = FactStore::with_fallback_config(root, fallback_config);
    store.qualification_inject_publish_faults([PersistError::TransientIo]);
    let measurement = Measurement::start();

    let first_unit = snapshot
        .open_sealed_by_descriptor(&descriptor)
        .expect("open first memory PGM");
    let fresh = store
        .load_or_build(&first_unit, &context, &LIMIT)
        .expect("memory source build");
    assert_eq!(
        fresh.origin(),
        FactOrigin::Rebuilt,
        "the first memory-only request did not build fresh facts"
    );
    assert_eq!(
        fresh.persist_error(),
        Some(PersistError::TransientIo),
        "the injected publication failure lost its typed taxonomy"
    );
    let resident = store.fallback_stats();
    assert_eq!(
        resident.resident_entries, 1,
        "the recoverable publication failure did not retain one fallback entry"
    );
    assert!(
        resident.resident_bytes <= fallback_config.bytes(),
        "the fallback exceeded its byte budget"
    );
    assert!(
        resident.resident_segment_hours <= fallback_config.segment_hours(),
        "the fallback exceeded its segment-hour budget"
    );

    let second = store
        .load_or_build(
            &snapshot
                .open_sealed_by_descriptor(&descriptor)
                .expect("open fallback metadata"),
            &context,
            &LIMIT,
        )
        .expect("memory fallback hit");
    assert_eq!(
        second.origin(),
        FactOrigin::FallbackHit,
        "the repeated request did not reuse the memory fallback"
    );
    assert_eq!(
        second.pgm_body_read_stats().read_calls,
        0,
        "the fallback hit reread PGM bodies"
    );

    store.qualification_force_persistence_probe_due();
    assert_eq!(
        store.probe_persistence(),
        PersistenceProbeOutcome::Succeeded,
        "the forced recovery probe did not restore persistence"
    );
    assert_eq!(
        store.qualification_recovery_gc_attempts(),
        0,
        "a transient write failure must not run capacity GC"
    );
    store
        .publish(fresh.facts(), &context, &LIMIT)
        .expect("recovered durable publication");
    assert_eq!(
        store.fallback_stats().resident_entries,
        0,
        "durable publication did not retire the fallback entry"
    );
    let restarted = FactStore::new(root)
        .load_or_build(
            &snapshot
                .open_sealed_by_descriptor(&descriptor)
                .expect("open restart metadata"),
            &context,
            &LIMIT,
        )
        .expect("restart after recovery");
    assert_eq!(
        restarted.origin(),
        FactOrigin::CacheHit,
        "the recovered sibling sidecar was not restart-admissible"
    );
    assert_eq!(
        restarted.pgm_body_read_stats().read_calls,
        0,
        "the recovered restart hit reread PGM bodies"
    );

    let pgm = fresh.pgm_body_read_stats();
    let fact = restarted.fact_read_stats().expect("recovered fact reads");
    measurement.finish(Work {
        pgm_body_reads: pgm.read_calls,
        pgm_body_bytes: pgm.stored_bytes_read,
        pgm_sections_decoded: pgm.read_calls,
        pgm_rows_decoded: SAMPLES as u64,
        fact_reads: fact.read_calls,
        fact_stored_bytes: fact.stored_bytes_read,
        fact_decoded_bytes: fact.decoded_bytes,
        sidecar_writes: 1,
        sidecar_write_bytes: restarted
            .facts()
            .encode(&LIMIT)
            .expect("memory canonical facts")
            .len() as u64,
        source_builds: 1,
        persistence_failures: 1,
        publication_attempts: store.qualification_publish_attempts(),
        retry_probes: 1,
        fallback_hits: 1,
        fallback_request_pgm_body_reads: second.pgm_body_read_stats().read_calls,
        recovered_restart_pgm_body_reads: restarted.pgm_body_read_stats().read_calls,
        fallback_resident_entries: resident.resident_entries,
        fallback_resident_segment_hours: resident.resident_segment_hours,
        fallback_resident_bytes: resident.resident_bytes,
        successful_responses: 3,
        ..Work::default()
    })
}

fn oracle_profile(root: &Path) -> WorkerOutcome {
    let snapshot = LocalDirSnapshot::open(root).expect("oracle snapshot");
    let descriptor = snapshot.sealed_descriptors()[0];
    let context = snapshot
        .sealed_context(&descriptor)
        .expect("oracle context");
    let measurement = Measurement::start();
    let raw_unit = snapshot
        .open_sealed_by_descriptor(&descriptor)
        .expect("open oracle PGM");
    let raw = SegmentFacts::extract(&raw_unit, &LIMIT).expect("forced raw oracle");
    let indexed = FactStore::new(root)
        .load_or_build(
            &snapshot
                .open_sealed_by_descriptor(&descriptor)
                .expect("open oracle fact metadata"),
            &context,
            &LIMIT,
        )
        .expect("oracle durable read");
    assert_eq!(
        indexed.origin(),
        FactOrigin::CacheHit,
        "the oracle profile did not use the prepared sibling sidecar"
    );
    for range in [
        CoverageSpan::new(1_000_000, dense_end()).expect("full oracle range"),
        CoverageSpan::new(61_000_000, 361_000_000).expect("partial oracle range"),
    ] {
        assert_eq!(
            raw.query(range, ORACLE_LIMITS).expect("raw oracle query"),
            indexed
                .facts()
                .query(range, ORACLE_LIMITS)
                .expect("indexed oracle query"),
            "forced raw and indexed facts diverged for range {range:?}"
        );
    }
    let pgm = raw_unit.body_read_stats();
    let fact = indexed.fact_read_stats().expect("oracle fact reads");
    measurement.finish(Work {
        pgm_body_reads: pgm.read_calls,
        pgm_body_bytes: pgm.stored_bytes_read,
        pgm_sections_decoded: pgm.read_calls,
        pgm_rows_decoded: SAMPLES as u64,
        fact_reads: fact.read_calls,
        fact_stored_bytes: fact.stored_bytes_read,
        fact_decoded_bytes: fact.decoded_bytes,
        successful_responses: 2,
        ..Work::default()
    })
}

fn state(root: &Path, config: &OverviewConfig) -> AppState {
    AppState::with_overview_config(
        LocalDirSnapshot::open(root).expect("open mode snapshot"),
        0,
        Duration::from_secs(10),
        config,
    )
    .expect("build qualification state")
}

fn qualification_service(state: AppState) -> Router {
    let recorder = PrometheusBuilder::new().build_recorder();
    app(state, None, recorder.handle())
}

async fn request_json(service: &Router, uri: &str) -> Vec<u8> {
    let response = service
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("build qualification request"),
        )
        .await
        .expect("serve qualification request");
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    response
        .into_body()
        .collect()
        .await
        .expect("collect qualification response")
        .to_bytes()
        .to_vec()
}

fn loader_work(
    before: &LoaderQualificationSnapshot,
    after: &LoaderQualificationSnapshot,
    responses: usize,
    response_bytes: usize,
    rows_per_build: usize,
) -> Work {
    let io = io_delta(before.io, after.io);
    let builds = after.flight.leaders.saturating_sub(before.flight.leaders);
    let persistence_failures = io.persistence_failures;
    assert_eq!(after.flight.active, 0, "singleflight entry leaked");
    assert_eq!(
        after.admission.used,
        crate::overview::admission::ColdWorkWeight::default(),
        "cold admission capacity leaked after the request"
    );
    assert_eq!(
        after.admission.queued, 0,
        "cold admission queue retained a completed request"
    );
    assert!(
        after.admission.peak_used.workers <= after.admission.capacity.workers,
        "observed workers exceeded cold admission capacity"
    );
    assert!(
        after.admission.peak_used.file_descriptors <= after.admission.capacity.file_descriptors,
        "observed file descriptors exceeded cold admission capacity"
    );
    Work {
        pgm_body_reads: io.pgm_body_reads,
        pgm_body_bytes: io.pgm_body_bytes,
        pgm_sections_decoded: io.pgm_body_reads,
        pgm_rows_decoded: io
            .source_builds
            .saturating_mul(u64::try_from(rows_per_build).expect("rows fit")),
        fact_reads: io.fact_reads,
        fact_stored_bytes: io.fact_stored_bytes,
        fact_decoded_bytes: io.fact_decoded_bytes,
        sidecar_writes: io.source_builds.saturating_sub(persistence_failures),
        sidecar_write_bytes: io.fact_write_bytes,
        source_builds: io.source_builds,
        singleflight_builds: builds,
        singleflight_waiters: after.flight.waiters.saturating_sub(before.flight.waiters),
        persistence_failures,
        max_inflight_builds: after.admission.peak_used.workers,
        max_inflight_file_descriptors: after.admission.peak_used.file_descriptors,
        max_queue_depth: after.admission.peak_queue,
        decoded_cache_entries: after.decoded.entries,
        decoded_cache_bytes: after.decoded.resident_bytes,
        fallback_hits: io.fallback_hits,
        fallback_resident_entries: after.fallback.resident_entries,
        fallback_resident_segment_hours: after.fallback.resident_segment_hours,
        fallback_resident_bytes: after.fallback.resident_bytes,
        successful_responses: u64::try_from(responses).expect("response count fits"),
        serialized_response_bytes: u64::try_from(response_bytes).expect("response bytes fit"),
        ..Work::default()
    }
}

const fn io_delta(before: LoaderIoSnapshot, after: LoaderIoSnapshot) -> LoaderIoSnapshot {
    LoaderIoSnapshot {
        decoded_hits: after.decoded_hits.saturating_sub(before.decoded_hits),
        durable_hits: after.durable_hits.saturating_sub(before.durable_hits),
        fallback_hits: after.fallback_hits.saturating_sub(before.fallback_hits),
        source_builds: after.source_builds.saturating_sub(before.source_builds),
        persistence_failures: after
            .persistence_failures
            .saturating_sub(before.persistence_failures),
        pgm_body_reads: after.pgm_body_reads.saturating_sub(before.pgm_body_reads),
        pgm_body_bytes: after.pgm_body_bytes.saturating_sub(before.pgm_body_bytes),
        fact_reads: after.fact_reads.saturating_sub(before.fact_reads),
        fact_stored_bytes: after
            .fact_stored_bytes
            .saturating_sub(before.fact_stored_bytes),
        fact_decoded_bytes: after
            .fact_decoded_bytes
            .saturating_sub(before.fact_decoded_bytes),
        fact_write_bytes: after
            .fact_write_bytes
            .saturating_sub(before.fact_write_bytes),
    }
}

fn live_stats(state: &AppState) -> LiveFoldStats {
    state
        .overview
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .qualification_live_fold_stats()
}

struct Measurement {
    started: Instant,
    cpu_ns: u128,
    io: ProcIo,
    fd_start: usize,
    sampler: FdSampler,
}

impl Measurement {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            cpu_ns: process_cpu_ns(),
            io: proc_io(),
            fd_start: open_file_descriptors(),
            sampler: FdSampler::start(),
        }
    }

    fn finish(self, work: Work) -> WorkerOutcome {
        let wall_ns = self.started.elapsed().as_nanos();
        let cpu_ns = process_cpu_ns().saturating_sub(self.cpu_ns);
        let fd_end = open_file_descriptors();
        let fd_peak = self.sampler.finish().max(self.fd_start).max(fd_end);
        WorkerOutcome {
            wall_ns,
            cpu_ns,
            process_peak_rss_bytes: process_peak_rss_bytes(),
            fd_start: self.fd_start,
            fd_peak,
            fd_end,
            proc_io: proc_io().saturating_sub(self.io),
            work,
        }
    }
}

struct FdSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    thread: std::thread::JoinHandle<()>,
}

impl FdSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicUsize::new(open_file_descriptors()));
        let worker_stop = Arc::clone(&stop);
        let worker_peak = Arc::clone(&peak);
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                worker_peak.fetch_max(open_file_descriptors(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        Self { stop, peak, thread }
    }

    fn finish(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().expect("join FD sampler");
        self.peak.load(Ordering::Relaxed)
    }
}

fn proc_io() -> ProcIo {
    let mut result = ProcIo::default();
    let contents = fs::read_to_string("/proc/self/io").expect("read /proc/self/io");
    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().parse::<u64>().expect("proc I/O value");
        match key {
            "rchar" => result.rchar = value,
            "wchar" => result.wchar = value,
            "syscr" => result.syscr = value,
            "syscw" => result.syscw = value,
            "read_bytes" => result.read_bytes = value,
            "write_bytes" => result.write_bytes = value,
            "cancelled_write_bytes" => result.cancelled_write_bytes = value,
            _ => {}
        }
    }
    result
}

fn process_cpu_ns() -> u128 {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    u128::try_from(time.tv_sec)
        .expect("non-negative process CPU seconds")
        .saturating_mul(1_000_000_000)
        .saturating_add(u128::try_from(time.tv_nsec).expect("non-negative process CPU nanos"))
}

fn process_peak_rss_bytes() -> u64 {
    fs::read_to_string("/proc/self/status")
        .expect("read /proc/self/status")
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("VmHWM:")?.trim();
            value
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|kb| kb.saturating_mul(1_024))
        })
        .expect("VmHWM")
}

fn open_file_descriptors() -> usize {
    fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}

fn mode_result(
    mode: &'static str,
    samples: Vec<WorkerOutcome>,
    syscalls: SyscallCounts,
) -> ModeResult {
    let mut wall = samples
        .iter()
        .map(|sample| sample.wall_ns)
        .collect::<Vec<_>>();
    let mut cpu = samples
        .iter()
        .map(|sample| sample.cpu_ns)
        .collect::<Vec<_>>();
    wall.sort_unstable();
    cpu.sort_unstable();
    validate_mode_samples(mode, &samples);
    ModeResult {
        mode,
        semantics: "fresh child process per sample; OS page cache uncontrolled/warm; storage-cold false",
        iterations: samples.len(),
        wall_p50_ns: percentile(&wall, 50),
        wall_p95_ns: percentile(&wall, 95),
        wall_p99_ns: percentile(&wall, 99),
        cpu_p50_ns: percentile(&cpu, 50),
        cpu_p95_ns: percentile(&cpu, 95),
        cpu_p99_ns: percentile(&cpu, 99),
        peak_rss_bytes: samples
            .iter()
            .map(|sample| sample.process_peak_rss_bytes)
            .max()
            .unwrap_or(0),
        peak_open_file_descriptors: samples
            .iter()
            .map(|sample| sample.fd_peak)
            .max()
            .unwrap_or(0),
        samples,
        syscalls,
    }
}

fn compact_performance(
    runtime_root: &Path,
    dense: &[u8],
    expected: &SegmentFacts,
) -> CompactPerformanceProfile {
    let root = runtime_root.join("compact-performance");
    fs::create_dir(&root).expect("create compact performance root");
    let context = SegmentContext::new("dense-hour.pgm").expect("compact context");
    let restart_root = root.join("restart-warm");
    fs::create_dir(&restart_root).expect("create compact restart root");
    fs::write(restart_root.join("dense-hour.pgm"), dense).expect("write compact restart PGM");
    FactStore::new(&restart_root)
        .publish(expected, &context, &LIMIT)
        .expect("seed compact restart facts");
    let full_range = CoverageSpan::new(1_000_000, dense_end()).expect("compact full range");
    let derived_roots = (0..iterations())
        .map(|iteration| {
            let data_dir = root.join(format!("derived-cold-{iteration:02}"));
            fs::create_dir(&data_dir).expect("create compact derived root");
            fs::write(data_dir.join("dense-hour.pgm"), dense).expect("write compact derived PGM");
            data_dir
        })
        .collect::<Vec<_>>();

    let derived = measure_compact("derived-cold", |iteration| {
        let data_dir = &derived_roots[iteration];
        let snapshot = LocalDirSnapshot::open(data_dir).expect("open compact derived snapshot");
        let descriptor = snapshot.sealed_descriptors()[0];
        let context = snapshot
            .sealed_context(&descriptor)
            .expect("compact derived context");
        let unit = snapshot
            .open_sealed_by_descriptor(&descriptor)
            .expect("open compact derived PGM");
        let loaded = FactStore::new(data_dir)
            .load_or_build(&unit, &context, &LIMIT)
            .expect("compact derived build");
        assert_eq!(
            loaded.origin(),
            FactOrigin::Rebuilt,
            "compact derived path did not rebuild"
        );
        assert_eq!(loaded.facts(), expected, "compact derived facts diverged");
        std::hint::black_box(
            loaded
                .facts()
                .query(full_range, ORACLE_LIMITS)
                .expect("compact derived query"),
        );
    });
    let restart = measure_compact("restart-warm", |_iteration| {
        let snapshot =
            LocalDirSnapshot::open(&restart_root).expect("open compact restart snapshot");
        let descriptor = snapshot.sealed_descriptors()[0];
        let context = snapshot
            .sealed_context(&descriptor)
            .expect("compact restart context");
        let unit = snapshot
            .open_sealed_by_descriptor(&descriptor)
            .expect("open compact restart PGM");
        let loaded = FactStore::new(&restart_root)
            .load_or_build(&unit, &context, &LIMIT)
            .expect("compact restart read");
        assert_eq!(
            loaded.origin(),
            FactOrigin::CacheHit,
            "compact restart path did not read durable facts"
        );
        assert_eq!(
            loaded.pgm_body_read_stats().read_calls,
            0,
            "compact restart path read PGM bodies"
        );
        assert_eq!(loaded.facts(), expected, "compact restart facts diverged");
        std::hint::black_box(
            loaded
                .facts()
                .query(full_range, ORACLE_LIMITS)
                .expect("compact restart query"),
        );
    });
    let process_hot = measure_compact("process-hot", |_iteration| {
        std::hint::black_box(
            expected
                .query(full_range, ORACLE_LIMITS)
                .expect("compact process-hot query"),
        );
    });
    let range_cold = measure_compact("range-cold/facts-warm", |iteration| {
        let offset = i64::try_from(iteration % 60).expect("compact iteration fits") * CADENCE_US;
        let range = CoverageSpan::new(1_000_000 + offset, 1_000_000 + offset + 300_000_000)
            .expect("compact partial range");
        std::hint::black_box(
            expected
                .query(range, ORACLE_LIMITS)
                .expect("compact range query"),
        );
    });

    CompactPerformanceProfile {
        semantics: "compact sealed facts read + bucket; excludes router, HTTP, JSON, and server bootstrap",
        modes: vec![derived, restart, process_hot, range_cold],
    }
}

fn measure_compact(mode: &'static str, mut operation: impl FnMut(usize)) -> CompactModeResult {
    let mut samples = Vec::with_capacity(iterations());
    for iteration in 0..iterations() {
        let started = Instant::now();
        operation(iteration);
        samples.push(started.elapsed().as_nanos());
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    CompactModeResult {
        mode,
        iterations: samples.len(),
        wall_p50_ns: percentile(&sorted, 50),
        wall_p95_ns: percentile(&sorted, 95),
        wall_p99_ns: percentile(&sorted, 99),
        samples_ns: samples,
    }
}

fn validate_mode_samples(mode: &str, samples: &[WorkerOutcome]) {
    assert_eq!(
        samples.len(),
        iterations(),
        "{mode} did not record the configured sample count"
    );
    for sample in samples {
        assert!(sample.wall_ns > 0, "{mode} recorded zero wall time");
        assert!(
            sample.fd_peak >= sample.fd_start,
            "{mode} peak FD count is below the starting count"
        );
        assert!(
            sample.fd_peak >= sample.fd_end,
            "{mode} peak FD count is below the ending count"
        );
        match mode {
            "restart-warm" => {
                assert_eq!(
                    sample.work.pgm_body_reads, 0,
                    "restart-warm read PGM bodies"
                );
                assert_eq!(
                    sample.work.source_builds, 0,
                    "restart-warm rebuilt source facts"
                );
                assert!(
                    sample.work.fact_reads > 0,
                    "restart-warm did not read the durable sidecar"
                );
            }
            "process-hot" | "range-cold/facts-warm" => {
                assert_eq!(sample.work.pgm_body_reads, 0, "{mode} read PGM bodies");
                assert_eq!(sample.work.fact_reads, 0, "{mode} reread the sidecar");
                assert_eq!(sample.work.sidecar_writes, 0, "{mode} rewrote the sidecar");
            }
            "concurrent-identical" => {
                assert_eq!(
                    sample.work.singleflight_builds, 1,
                    "identical requests did not share one leader"
                );
                assert_eq!(
                    sample.work.source_builds, 1,
                    "identical requests did not share one source build"
                );
                assert_eq!(
                    sample.work.successful_responses, 16,
                    "an identical concurrent response was lost"
                );
            }
            "concurrent-disjoint" => {
                assert_eq!(
                    sample.work.singleflight_builds, 16,
                    "disjoint keys unexpectedly shared leaders"
                );
                assert_eq!(
                    sample.work.source_builds, 16,
                    "a disjoint source build was lost"
                );
                assert_eq!(
                    sample.work.successful_responses, 16,
                    "a disjoint concurrent response was lost"
                );
                assert!(
                    sample.work.max_inflight_builds <= 4,
                    "disjoint work exceeded the worker capacity"
                );
                assert!(
                    sample.work.max_inflight_file_descriptors <= 16,
                    "disjoint work exceeded the file-descriptor capacity"
                );
            }
            "memory-only" => {
                assert_eq!(
                    sample.work.persistence_failures, 1,
                    "memory-only did not record one publication failure"
                );
                assert_eq!(
                    sample.work.fallback_hits, 1,
                    "memory-only did not record one fallback hit"
                );
                assert!(
                    sample.work.fallback_resident_entries > 0,
                    "memory-only did not retain the bounded fallback"
                );
            }
            "live" => {
                assert_eq!(
                    sample.work.completed_active_parts, 1,
                    "live mode did not fold one completed part"
                );
                assert!(
                    sample.work.visibility_lag_us <= 2_500_000,
                    "live mode exceeded the 2.5-second visibility contract"
                );
            }
            _ => {}
        }
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

fn dense_hour_pgm(source_id: u64, start_us: i64, samples: usize) -> Vec<u8> {
    let database = (0..samples)
        .map(|index| {
            let index = i64::try_from(index).expect("sample index");
            let index_f64 =
                f64::from(u32::try_from(index).expect("sample index fits exact f64 integer range"));
            PgStatDatabaseV1 {
                ts: Ts(start_us + index * CADENCE_US),
                datid: 16_384,
                datname: None,
                numbackends: Some(10 + i32::try_from(index % 5).expect("backend count")),
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
        .collect::<Vec<_>>();
    let coverage = database
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
        .collect::<Vec<_>>();
    let reset = [ResetMetadata {
        ts: Ts(start_us),
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
    let reset_body = ResetMetadata::encode(&reset).expect("encode dense reset");
    let coverage_body = SnapshotCoverageV1::encode(&coverage).expect("encode dense coverage");
    build_part(
        &[
            SectionInput {
                type_id: 1_005_001,
                rows: u32::try_from(database.len()).expect("database rows"),
                body: &database_body,
            },
            SectionInput {
                type_id: 1_020_001,
                rows: 1,
                body: &reset_body,
            },
            SectionInput {
                type_id: 1_038_001,
                rows: u32::try_from(coverage.len()).expect("coverage rows"),
                body: &coverage_body,
            },
        ],
        PartMeta {
            min_ts: start_us,
            max_ts: start_us
                + i64::try_from(samples.saturating_sub(1)).expect("sample count") * CADENCE_US,
            source_id,
        },
    )
}

fn lifecycle_part(ts_us: i64, pid: i32) -> Vec<u8> {
    let rows = [PgLogLifecycleV1 {
        ts: Ts(ts_us),
        kind: 0,
        pid: Some(pid),
        signal: Some(9),
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle part");
    build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: ts_us,
            max_ts: ts_us,
            source_id: SOURCE_ID,
        },
    )
}

fn framed(part: &[u8]) -> Vec<u8> {
    let mut bytes = FrameHeader {
        part_len: u64::try_from(part.len()).expect("frame length"),
    }
    .encode()
    .to_vec();
    bytes.extend_from_slice(part);
    bytes
}

fn dense_end() -> i64 {
    1_000_000 + i64::try_from(SAMPLES).expect("sample count") * CADENCE_US
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
        auxiliary_datasets: [
            "all-canonical-families-v2",
            "sparse-30-percent",
            "reset-at-segment-boundary",
            "duplicate-timestamps-and-rows",
            "fatal-burst-at-collector-limit",
            "explicit-pg-log-gap",
            "two-sources",
            "corrupt-ovf-block",
            "corrupt-pgm-section",
            "mixed-cadence-5-10-30-60-3600",
        ],
    }
}

fn accounting(
    facts: &SegmentFacts,
    file: &FactFile,
    fact_file_bytes: usize,
    metadata: &fs::Metadata,
) -> Accounting {
    let stored_block_bytes = file
        .directory()
        .iter()
        .map(|entry| entry.stored_len)
        .sum::<u64>();
    let decoded_block_bytes = file.directory().iter().map(|entry| entry.decoded_len).sum();
    let header_and_directory_bytes = u64::try_from(fact_file_bytes)
        .expect("fact bytes fit")
        .checked_sub(stored_block_bytes)
        .expect("stored blocks fit file");
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
    let resident_fact_bytes = facts.resident_bytes().expect("resident fact size");
    let pinned_fact_bytes = resident_fact_bytes
        .checked_add(size_of::<Arc<SegmentFacts>>())
        .and_then(|bytes| bytes.checked_add(size_of::<kronika_reader::FactBuildKey>()))
        .expect("pinned fact size");
    Accounting {
        fact_file_logical_bytes: fact_file_bytes,
        fact_file_allocated_bytes: metadata.blocks().saturating_mul(512),
        header_and_directory_bytes,
        stored_block_bytes,
        decoded_block_bytes,
        resident_fact_bytes,
        pinned_fact_bytes,
        fixed_metric_stored_bytes,
        variable_event_string_stored_bytes,
        retained_metric_samples,
        fixed_metric_bytes_per_sample_numerator: fixed_metric_stored_bytes,
        fixed_metric_bytes_per_sample_denominator: retained_metric_samples,
        identity_holds: header_and_directory_bytes.saturating_add(stored_block_bytes)
            == fact_file_bytes as u64,
    }
}

fn budgets(accounting: &Accounting) -> Budgets {
    let disk_bytes = env_u64("OVERVIEW_DENSE_DISK_BUDGET_BYTES");
    let resident_bytes = env_u64("OVERVIEW_DENSE_RESIDENT_BUDGET_BYTES");
    let disk_within_budget =
        disk_bytes.map(|limit| accounting.fact_file_logical_bytes as u64 <= limit);
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
        disk_bytes,
        resident_bytes,
        disk_within_budget,
        resident_within_budget,
        deployment_budget_status,
        qualification_blocked: matches!(
            deployment_budget_status,
            "exceeds_approved" | "incomplete_configuration"
        ),
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn iterations() -> usize {
    let configured = std::env::var("OVERVIEW_QUALIFICATION_ITERATIONS")
        .ok()
        .map(|value| value.parse::<usize>().expect("qualification iterations"));
    let iterations = configured.unwrap_or(ITERATIONS);
    assert!(
        (1..=ITERATIONS).contains(&iterations),
        "qualification iterations must be in 1..={ITERATIONS}"
    );
    iterations
}

fn host_profile(runtime_root: &Path) -> HostProfile {
    let metadata = fs::metadata(runtime_root).expect("stat runtime root");
    HostProfile {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        kernel: command_output("uname", &["-srv"]),
        filesystem: command_output(
            "stat",
            &["-f", "-c", "%T", runtime_root.to_str().expect("UTF-8 root")],
        ),
        filesystem_device: metadata.dev(),
        process_samples_are_fresh_children: true,
        syscall_trace_scope: "one complete fresh worker process, separate from latency samples",
        storage_cold: false,
    }
}

fn ci_profile() -> CiProfile {
    CiProfile {
        repository: std::env::var("GITHUB_REPOSITORY").ok(),
        run_id: std::env::var("GITHUB_RUN_ID").ok(),
        run_attempt: std::env::var("GITHUB_RUN_ATTEMPT").ok(),
        job: std::env::var("GITHUB_JOB").ok(),
        artifact_name: "overview-qualification-raw",
    }
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .expect("run qualification support command");
    assert!(output.status.success(), "{program} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos()
}

fn mode_slug(mode: &str) -> String {
    mode.replace('/', "-")
}

type EvidenceCoordinate = (&'static str, &'static str, &'static str);

const TIMELINE_BDD_EVIDENCE: [EvidenceCoordinate; 4] = [
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_overview.feature",
        "PostgreSQL 15 publishes one reconciled source-scoped timeline",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_overview.feature",
        "PostgreSQL 16 publishes one reconciled source-scoped timeline",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_overview.feature",
        "PostgreSQL 17 publishes one reconciled source-scoped timeline",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_overview.feature",
        "PostgreSQL 18 publishes one reconciled source-scoped timeline",
    ),
];

const LIFECYCLE_BDD_EVIDENCE: [EvidenceCoordinate; 4] = [
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
        "PostgreSQL 15 real web process recovers sibling indexes across lifecycle boundaries",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
        "PostgreSQL 16 real web process recovers sibling indexes across lifecycle boundaries",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
        "PostgreSQL 17 real web process recovers sibling indexes across lifecycle boundaries",
    ),
    (
        "bdd_scenario",
        "crates/kronika-bdd/features/timeline_web_lifecycle.feature",
        "PostgreSQL 18 real web process recovers sibling indexes across lifecycle boundaries",
    ),
];

struct AcceptanceSpec {
    requirement: &'static str,
    evidence: &'static [EvidenceCoordinate],
    timeline_bdd: bool,
    lifecycle_bdd: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "the exact 18-row dossier intentionally keeps every normative evidence coordinate auditable"
)]
fn acceptance_evidence() -> Vec<AcceptanceEvidence> {
    const ROWS: [AcceptanceSpec; 18] = [
        AcceptanceSpec {
            requirement: "restart-warm-zero-pgm",
            evidence: &[
                ("mode", "qualification", "restart-warm"),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "cold_build_and_cache_hit_report_exact_io_origins",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: true,
        },
        AcceptanceSpec {
            requirement: "raw-index-all-families",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "every_populated_canonical_block_matches_forced_raw_and_restart_warm",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "all_family_range_edges_use_half_open_ownership_and_one_left_halo",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
                ),
                ("mode", "qualification", "oracle-profile"),
            ],
            timeline_bdd: true,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "partition-seal-invariance",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "ten_thousand_random_partition_seal_and_merge_seeds_are_invariant",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "ovf-fault-fallback",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "corrupt_sidecar_is_atomically_replaced_at_the_same_path",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "wrong_source_at_the_expected_name_is_rejected",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "oversized_candidate_is_rebuilt_and_atomically_replaced",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/container.rs",
                    "admission_distinguishes_wrong_source_from_incompatible_versions",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "publication_failure_returns_fresh_facts_then_serves_the_fallback",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: true,
        },
        AcceptanceSpec {
            requirement: "source-damage-visible",
            evidence: &[
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_resilience.rs",
                    "scheduled_source_scrub_prevents_a_durable_fact_from_masking_damage",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "every_all_family_source_body_crc_failure_stays_a_source_error",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "policy-reuse",
            evidence: &[
                ("mode", "qualification", "range-cold/facts-warm"),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/overview/cache.rs",
                    "policy_versions_rekey_only_the_response_projection",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "preview_and_events_share_typed_fact_ids_and_canonical_order",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "cursor-exactness",
            evidence: &[
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "a_cursor_walks_the_retained_set_exactly_once",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "a_cursor_resolves_its_pinned_view_after_a_new_publication",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "a_cursor_presented_to_a_changed_query_is_a_mismatch",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: true,
        },
        AcceptanceSpec {
            requirement: "live-seal-identity",
            evidence: &[
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/overview/live.rs",
                    "append_then_seal_keeps_one_coherent_event_set",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/notable.rs",
                    "public_event_identity_ignores_lineage_but_retains_content",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "duplicate_segment_contents_do_not_invent_path_based_identity",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "lossless-live-builder",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "a_stream_split_into_parts_reports_the_unsplit_counts_and_coverage_envelope",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "an_incomplete_candidate_is_never_promoted",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "required-gap-unknown",
            evidence: &[
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "health_of_an_empty_range_is_unknown_not_green",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/health.rs",
                    "missing_required_penalty_is_unknown_even_with_complete_coverage",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/health.rs",
                    "partial_lossy_assumed_or_foreign_coverage_never_turns_green",
                ),
            ],
            timeline_bdd: true,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "trusted-floor-downsampling",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/health_line.rs",
                    "trusted_floors_and_unknown_scores_survive_partition_merge_and_downsample",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/live.rs",
                    "every_all_family_contiguous_partition_promotes_to_exact_cold_sealed_facts",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "factor-applicability-loss",
            evidence: &[
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_timeline.rs",
                    "all_supported_factor_families_reach_every_timeline_endpoint",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/health.rs",
                    "every_strict_coverage_axis_is_enforced",
                ),
            ],
            timeline_bdd: true,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "counter-halo-range-reset",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "all_family_range_edges_use_half_open_ownership_and_one_left_halo",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/reduce.rs",
                    "reset_gap_and_mixed_series_never_become_zero_deltas",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/reduce.rs",
                    "boundary_attribution_is_partition_invariant",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/reduce.rs",
                    "halo_bridge_is_counted_once_for_every_partition",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "source-taxonomy-units",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "every_populated_canonical_block_matches_forced_raw_and_restart_warm",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/facts.rs",
                    "extracts_registered_log_event_layouts_once_with_conservative_quality",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/metric_extract.rs",
                    "unsupported_factor_coverage_is_explicit",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/metric.rs",
                    "factor_codes_and_units_round_trip",
                ),
                (
                    "rust_test",
                    "crates/kronika-analytics/src/overview/fact.rs",
                    "event_taxonomy_codes_round_trip_exhaustively",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "admission-singleflight-bounds",
            evidence: &[
                ("mode", "qualification", "concurrent-identical"),
                ("mode", "qualification", "concurrent-disjoint"),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_admission.rs",
                    "an_exact_decoded_hit_bypasses_cold_admission",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/tests/overview_admission.rs",
                    "an_exact_durable_hit_bypasses_cold_admission_after_restart",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/overview/singleflight.rs",
                    "same_fact_key_with_distinct_lineages_runs_independently",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/overview/singleflight.rs",
                    "cancelling_the_request_does_not_cancel_the_leader",
                ),
                (
                    "rust_test",
                    "bins/pg_kronika-web/src/overview/admission.rs",
                    "cancelling_a_waiter_removes_its_ticket",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
        AcceptanceSpec {
            requirement: "memory-fallback-recovery",
            evidence: &[
                ("mode", "qualification", "memory-only"),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "production_fallback_enforces_lru_hour_byte_and_oversized_budgets",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "backoff_suppresses_a_second_publication_attempt",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/publish.rs",
                    "publication_failure_returns_fresh_facts_then_serves_the_fallback",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: true,
        },
        AcceptanceSpec {
            requirement: "quota-gc-safety",
            evidence: &[
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "quota_accounts_only_derived_files_in_the_owned_data_directory",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "optional_quota_blocks_publication_without_touching_the_source",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "data_directory_owner_contention_fails_closed",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "unlinked_bytes_come_from_the_reopened_validated_inode",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "source_entries_and_symlinks_are_never_followed_or_removed",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "concurrent_live_gc_read_and_publish_preserve_the_sidecar",
                ),
                (
                    "rust_test",
                    "crates/kronika-reader/src/overview/gc/tests.rs",
                    "complete_typed_live_set_preserves_each_sibling_sidecar",
                ),
            ],
            timeline_bdd: false,
            lifecycle_bdd: true,
        },
        AcceptanceSpec {
            requirement: "nine-modes-one-profile",
            evidence: &[("mode_set", "qualification", "all-nine-modes")],
            timeline_bdd: false,
            lifecycle_bdd: false,
        },
    ];
    ROWS.iter()
        .enumerate()
        .map(|(index, spec)| {
            let mut evidence = spec.evidence.to_vec();
            if spec.timeline_bdd {
                evidence.extend(TIMELINE_BDD_EVIDENCE);
            }
            if spec.lifecycle_bdd {
                evidence.extend(LIFECYCLE_BDD_EVIDENCE);
            }
            AcceptanceEvidence {
                id: u8::try_from(index + 1).expect("acceptance ID"),
                requirement: spec.requirement,
                implementation_status: "IMPLEMENTED",
                evidence: evidence
                    .iter()
                    .map(|(kind, path, name)| EvidenceRef {
                        kind,
                        binary: evidence_binary(kind, path),
                        path,
                        name,
                    })
                    .collect(),
                decision: "PENDING_EXACT_HEAD_CI",
            }
        })
        .collect()
}

fn evidence_binary(kind: &str, path: &str) -> &'static str {
    match (kind, path) {
        ("mode" | "mode_set", "qualification") => {
            "pg-kronika-web::example/overview_m6_qualification"
        }
        ("rust_test", path) if path.starts_with("crates/kronika-reader/") => "kronika-reader",
        ("rust_test", path) if path.starts_with("crates/kronika-analytics/") => "kronika-analytics",
        ("rust_test", path) if path.starts_with("bins/pg_kronika-web/") => "pg-kronika-web",
        ("bdd_scenario", path) if path.starts_with("crates/kronika-bdd/features/") => "kronika-bdd",
        _ => "unknown-evidence-binary",
    }
}
