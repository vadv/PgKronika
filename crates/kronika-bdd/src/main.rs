//! BDD runner for Docker-only integration scenarios.
//!
//! Nix supplies `PostgreSQL` versions through `KRONIKA_PG_MATRIX`. Host
//! `cargo test --workspace` stays database-free.
#![allow(
    clippy::trivial_regex,
    reason = "cucumber step phrases are literal English, matched as plain text, not real regexes"
)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "cucumber's dependency tree pulls duplicate transitive versions outside our control"
)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber passes &mut World to every step by contract, even read-only ones"
)]

mod cluster;
mod collector;
mod harness;
mod steps;

use std::path::Path;

use anyhow::Context;
use cucumber::{World, event, given, then};
use harness::HarnessState;
use kronika_format::{Entry, crc32c};
use kronika_reader::{Dictionary, Resolved, Segment};
use kronika_registry::{
    Bytes, MAX_SECTION_BYTES, Section, StrId, VerifiedSection,
    bgwriter_checkpointer::BgwriterCheckpointer,
    pg_stat_statements::{
        PgStatStatementsV1, PgStatStatementsV2, PgStatStatementsV3, PgStatStatementsV4,
        PgStatStatementsV5, PgStatStatementsV6,
    },
    pg_stat_user_indexes::{PgStatUserIndexesV1, PgStatUserIndexesV2},
    pg_stat_user_tables::{
        PgStatUserTablesV1, PgStatUserTablesV2, PgStatUserTablesV3, PgStatUserTablesV4,
    },
    pg_stat_wal::{PgStatWalV1, PgStatWalV2},
    replication_instance::ReplicationInstance,
};
use kronika_source_pg::collect_bgwriter_checkpointer;

use kronika_source_pg::statements::{StatementsVersion, statements_extversion, statements_version};
use kronika_source_pg::user_indexes::{UserIndexesVersion, indexdef_max_len, user_indexes_version};
use kronika_source_pg::user_tables::{UserTablesVersion, user_tables_version};
use kronika_source_pg::wal::{WalVersion, wal_version};

const PG17_MAJOR: u32 = 17;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

const PG_STAT_STATEMENTS_V1_TYPE_ID: u32 = 1_002_001;
const PG_STAT_STATEMENTS_V2_TYPE_ID: u32 = 1_002_002;
const PG_STAT_STATEMENTS_V3_TYPE_ID: u32 = 1_002_003;
const PG_STAT_STATEMENTS_V4_TYPE_ID: u32 = 1_002_004;
const PG_STAT_STATEMENTS_V5_TYPE_ID: u32 = 1_002_005;
const PG_STAT_STATEMENTS_V6_TYPE_ID: u32 = 1_002_006;

const PG_STAT_USER_TABLES_V1_TYPE_ID: u32 = 1_013_001;
const PG_STAT_USER_TABLES_V2_TYPE_ID: u32 = 1_013_002;
const PG_STAT_USER_TABLES_V3_TYPE_ID: u32 = 1_013_003;
const PG_STAT_USER_TABLES_V4_TYPE_ID: u32 = 1_013_004;

const PG_STAT_USER_INDEXES_V1_TYPE_ID: u32 = 1_014_001;
const PG_STAT_USER_INDEXES_V2_TYPE_ID: u32 = 1_014_002;

/// Databases seeded by the user-tables scenario; each gets one probe table.
const SEEDED_DATABASES: [&str; 2] = ["kronika_ut_a", "kronika_ut_b"];

const PG_REPLICATION_INSTANCE_TYPE_ID: u32 = 1_015_001;
const PG_STAT_WAL_V1_TYPE_ID: u32 = 1_007_001;
const PG_STAT_WAL_V2_TYPE_ID: u32 = 1_007_002;

/// Cucumber state for one scenario.
///
/// `clusters` borrows the process-wide matrix (booted once, not per scenario).
/// `harness` holds the new-style per-scenario state — named sessions, the
/// selected database, and the last snapshot — and is torn down in the `after`
/// hook.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: &'static [cluster::Cluster],
    harness: HarnessState,
}

#[given("the PostgreSQL matrix is booted")]
async fn boot_matrix_step(world: &mut BddWorld) -> anyhow::Result<()> {
    world.clusters = cluster::shared_matrix().await?;
    Ok(())
}

#[then("every version reports valid bgwriter/checkpointer stats")]
async fn every_version_reports_stats(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let conn = db.connect().await?;
        let major = conn
            .major()
            .with_context(|| format!("postgres {}: server reported no version", db.major()))?;
        anyhow::ensure!(
            major == db.major(),
            "postgres {}: handshake reported major {major} instead",
            db.major()
        );
        let snapshot = collect_bgwriter_checkpointer(conn.client(), major)
            .await
            .with_context(|| format!("collect type 1_006_001 on postgres {}", db.major()))?;
        check_snapshot(db.major(), now_micros()?, &snapshot)?;
    }
    Ok(())
}

fn now_micros() -> anyhow::Result<i64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(since_epoch.as_micros()).context("unix microseconds overflow i64")
}

/// Basic invariants for a row read directly from `PostgreSQL`.
fn check_snapshot(major: u32, host_now: i64, snap: &BgwriterCheckpointer) -> anyhow::Result<()> {
    anyhow::ensure!(
        snap.ts.0 > 0 && (snap.ts.0 - host_now).abs() < 300_000_000,
        "postgres {major}: snapshot ts {} is not within 5 min of the runner clock {host_now}",
        snap.ts.0
    );
    anyhow::ensure!(
        snap.checkpoints_timed >= 0 && snap.buffers_clean >= 0 && snap.buffers_alloc >= 0,
        "postgres {major}: a counter came back negative"
    );
    anyhow::ensure!(
        snap.bgwriter_stats_reset.0 > 0 && snap.bgwriter_stats_reset.0 <= snap.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        snap.bgwriter_stats_reset.0,
        snap.ts.0
    );
    assert_version_columns(major, snap)
}

/// PG17 moved checkpoint fields out of `pg_stat_bgwriter`; older versions keep
/// the old column set.
fn assert_version_columns(major: u32, snap: &BgwriterCheckpointer) -> anyhow::Result<()> {
    if major >= PG17_MAJOR {
        anyhow::ensure!(
            snap.restartpoints_timed.is_some()
                && snap.restartpoints_req.is_some()
                && snap.restartpoints_done.is_some()
                && snap.checkpointer_stats_reset.is_some(),
            "postgres {major}: PG17+ checkpointer columns came back NULL"
        );
        anyhow::ensure!(
            snap.buffers_backend.is_none() && snap.buffers_backend_fsync.is_none(),
            "postgres {major}: PG17 dropped buffers_backend, but the snapshot has it"
        );
    } else {
        anyhow::ensure!(
            snap.buffers_backend.is_some() && snap.buffers_backend_fsync.is_some(),
            "postgres {major}: pre-PG17 buffers_backend came back NULL"
        );
        anyhow::ensure!(
            snap.restartpoints_timed.is_none()
                && snap.restartpoints_req.is_none()
                && snap.restartpoints_done.is_none()
                && snap.checkpointer_stats_reset.is_none(),
            "postgres {major}: pre-PG17 must not fill the checkpointer columns"
        );
    }
    Ok(())
}

#[then("every version is collected into a sealed segment with section 1_006_001")]
async fn every_version_seals_a_segment(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_sealed_section(db.major(), &segment)?;
    }
    Ok(())
}

fn assert_sealed_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.type_id == BGWRITER_CHECKPOINTER_TYPE_ID)
        .with_context(|| format!("postgres {major}: segment has no section 1_006_001"))?;

    // Check typed values, not just section presence.
    let row = decode_sealed_row(path, entry)
        .with_context(|| format!("postgres {major}: read back section 1_006_001"))?;

    ensure_ts_in_segment_range(
        major,
        "section 1_006_001",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts,
    )?;
    anyhow::ensure!(
        row.bgwriter_stats_reset.0 > 0 && row.bgwriter_stats_reset.0 <= row.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        row.bgwriter_stats_reset.0,
        row.ts.0
    );
    assert_version_columns(major, &row)
}

/// Read the catalog-bounded section and decode its single typed row.
fn decode_sealed_row(path: &Path, entry: &Entry) -> anyhow::Result<BgwriterCheckpointer> {
    use std::os::unix::fs::FileExt;

    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    let mut rows = BgwriterCheckpointer::decode(verified)
        .context("typed decode of section 1_006_001")?
        .into_iter();
    let row = rows.next().context("section decoded to no rows")?;
    anyhow::ensure!(
        rows.next().is_none(),
        "section unexpectedly decoded to multiple rows"
    );
    Ok(row)
}

fn ensure_ts_in_segment_range(
    major: u32,
    section: &str,
    ts: i64,
    min_ts: i64,
    max_ts: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        ts > 0 && ts >= min_ts && ts <= max_ts,
        "postgres {major}: {section} ts {ts} outside segment range {min_ts}..={max_ts}"
    );
    Ok(())
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_section_rows<T: Section>(
    path: &Path,
    segment: &Segment,
    type_id: u32,
) -> anyhow::Result<Vec<T>> {
    use std::os::unix::fs::FileExt;

    let entry = segment
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("segment has no section {type_id}"))?;
    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    T::decode(verified).context("typed decode of the sealed section")
}

#[then(
    "each matrix cluster seals pg_stat_user_tables rows from two seeded databases with dictionary-backed names"
)]
async fn every_version_seals_user_tables(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        for datname in SEEDED_DATABASES {
            seed_user_table_database(db, datname).await?;
        }
        // The collector refreshes the pool on SIGUSR2, so the seeded databases
        // are enumerated and walked in this snapshot.
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_user_tables_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Create a database with one probe table carrying rows and fresh statistics, so
/// the table lands in the size and activity candidate axes. `CREATE DATABASE`
/// cannot run inside a transaction block, hence the separate statements.
// Interim: these fixed-name databases will be replaced with unique-per-scenario
// names per docs/bdd-testing-guide.md §6 when user_tables/user_indexes are
// converted to the new oracle style.
async fn seed_user_table_database(db: &cluster::Cluster, datname: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        datname.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
        "seed database name {datname:?} is not a safe identifier"
    );
    let admin = db.connect().await?;
    let exists = admin
        .client()
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&datname])
        .await
        .with_context(|| format!("postgres {}: probe database {datname}", db.major()))?;
    if exists.is_none()
        && let Err(e) = admin
            .client()
            .batch_execute(&format!("CREATE DATABASE {datname}"))
            .await
    {
        let is_dup = e.as_db_error().is_some_and(|db_err| {
            *db_err.code() == tokio_postgres::error::SqlState::DUPLICATE_DATABASE
        });
        if !is_dup {
            return Err(anyhow::Error::from(e))
                .with_context(|| format!("postgres {}: create database {datname}", db.major()));
        }
    }
    drop(admin);

    let dsn = db.conn_string_db(datname);
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .with_context(|| format!("postgres {}: connect to {datname}", db.major()))?;
    let driver = tokio::spawn(connection);
    let result = async {
        let long_predicate = long_partial_index_predicate();
        let seed_sql = format!(
            "CREATE TABLE IF NOT EXISTS kronika_ut_probe (id int primary key, payload text); \
             INSERT INTO kronika_ut_probe \
               SELECT g, repeat('x', 16) FROM generate_series(1, 200) g \
               ON CONFLICT (id) DO NOTHING; \
             DROP INDEX IF EXISTS kronika_ut_probe_long_idx; \
             CREATE INDEX kronika_ut_probe_long_idx \
               ON kronika_ut_probe (lower(payload || '_' || id::text)) \
               WHERE {long_predicate};",
        );
        client
            .batch_execute(&seed_sql)
            .await
            .with_context(|| format!("postgres {}: seed table in {datname}", db.major()))?;
        // VACUUM cannot run inside a transaction block, and a multi-statement
        // simple query executes as one implicit transaction — so VACUUM ANALYZE
        // must be its own statement.
        client
            .batch_execute("VACUUM ANALYZE kronika_ut_probe;")
            .await
            .with_context(|| format!("postgres {}: vacuum analyze in {datname}", db.major()))?;
        if db.major() >= 16 {
            client
                .batch_execute(
                    "BEGIN; \
                     SET LOCAL enable_seqscan = off; \
                     SELECT payload FROM kronika_ut_probe WHERE id = 1; \
                     COMMIT;",
                )
                .await
                .with_context(|| format!("postgres {}: scan pkey in {datname}", db.major()))?;
            client
                .batch_execute("SELECT pg_stat_force_next_flush();")
                .await
                .with_context(|| format!("postgres {}: flush stats in {datname}", db.major()))?;
        }
        Ok(())
    }
    .await;
    driver.abort();
    result
}

/// Build a predicate whose `pg_get_indexdef` text exceeds the collector cap.
fn long_partial_index_predicate() -> String {
    // A large IN-list makes old PostgreSQL versions reject the pg_index row as
    // too wide. One long text constant keeps the catalog tuple smaller while the
    // deparsed index definition still exceeds the SQL-side cap.
    let literal = "x".repeat(5_200);
    format!("payload IS NOT NULL AND payload <> '{literal}'")
}

/// Database row fields covered by the live BDD matrix for user-tables layouts.
#[derive(Debug, Clone, Copy)]
struct UserTableObservation {
    datname: StrId,
    schemaname: StrId,
    relname: StrId,
    ts: i64,
}

/// Decode the sealed `pg_stat_user_tables` section for the selected layout, then
/// check one snapshot timestamp, that the two seeded databases both contributed
/// the probe table, and that datname/schemaname/relname resolve through the
/// dictionary.
fn assert_user_tables_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let observations = match user_tables_version(major) {
        UserTablesVersion::V4 => decode_section_rows::<PgStatUserTablesV4>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V4_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_004"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V3 => decode_section_rows::<PgStatUserTablesV3>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V3_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_003"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V2 => decode_section_rows::<PgStatUserTablesV2>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V2_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_002"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V1 => decode_section_rows::<PgStatUserTablesV1>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V1_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_001"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
    };
    check_user_tables_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded user-tables rows.
fn check_user_tables_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[UserTableObservation],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_user_tables section decoded to no rows"
    );

    // One statement_timestamp() per database, but every snapshot ts must fall in
    // the segment range; the rows share a single collection window.
    for row in rows {
        ensure_ts_in_segment_range(major, "section 1_013", row.ts, min_ts, max_ts)?;
    }

    // Every label resolves, and the probe table from both seeded databases is
    // present (datname is the discriminator: the table name is identical).
    let mut seeded_with_probe: std::collections::HashSet<Vec<u8>> =
        std::collections::HashSet::new();
    for row in rows {
        let datname = resolve_string(major, dict, "datname", row.datname.0)?;
        let schemaname = resolve_string(major, dict, "schemaname", row.schemaname.0)?;
        let relname = resolve_string(major, dict, "relname", row.relname.0)?;
        anyhow::ensure!(
            !schemaname.is_empty(),
            "postgres {major}: schemaname resolved to an empty string"
        );
        if relname == b"kronika_ut_probe"
            && SEEDED_DATABASES
                .iter()
                .any(|name| name.as_bytes() == datname.as_slice())
        {
            seeded_with_probe.insert(datname);
        }
    }
    for datname in SEEDED_DATABASES {
        anyhow::ensure!(
            seeded_with_probe.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe row for database {datname}"
        );
    }
    Ok(())
}

#[then(
    "each matrix cluster seals pg_stat_user_indexes rows from two seeded databases with dictionary-backed names"
)]
async fn every_version_seals_user_indexes(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        for datname in SEEDED_DATABASES {
            seed_user_table_database(db, datname).await?;
        }
        // The collector refreshes the pool on SIGUSR2, so the seeded databases
        // are enumerated and walked in this snapshot.
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_user_indexes_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Index row fields covered by the live BDD matrix for user-indexes layouts.
#[derive(Debug, Clone, Copy)]
struct UserIndexObservation {
    datname: StrId,
    schemaname: StrId,
    indexrelname: StrId,
    amname: StrId,
    indexdef: StrId,
    indisprimary: bool,
    indisexclusion: bool,
    indisready: bool,
    main_fork_bytes: i64,
    idx_blks_read: i64,
    idx_blks_hit: i64,
    /// Scan recency in unix microseconds on V2 (PG16+); `None` on V1 layouts and
    /// when the index has never been scanned.
    last_idx_scan: Option<i64>,
    ts: i64,
}

/// Decode the sealed `pg_stat_user_indexes` section for the selected layout, then
/// check one snapshot timestamp, that the two seeded databases both contributed
/// the probe table's primary-key index, and that the label strings resolve
/// through the dictionary.
fn assert_user_indexes_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let observations = match user_indexes_version(major) {
        UserIndexesVersion::V2 => decode_section_rows::<PgStatUserIndexesV2>(
            path,
            &segment,
            PG_STAT_USER_INDEXES_V2_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_014_002"))?
        .iter()
        .map(|r| UserIndexObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            indexrelname: r.indexrelname,
            amname: r.amname,
            indexdef: r.indexdef,
            indisprimary: r.indisprimary,
            indisexclusion: r.indisexclusion,
            indisready: r.indisready,
            main_fork_bytes: r.main_fork_bytes,
            idx_blks_read: r.idx_blks_read,
            idx_blks_hit: r.idx_blks_hit,
            last_idx_scan: r.last_idx_scan.map(|t| t.0),
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserIndexesVersion::V1 => decode_section_rows::<PgStatUserIndexesV1>(
            path,
            &segment,
            PG_STAT_USER_INDEXES_V1_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_014_001"))?
        .iter()
        .map(|r| UserIndexObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            indexrelname: r.indexrelname,
            amname: r.amname,
            indexdef: r.indexdef,
            indisprimary: r.indisprimary,
            indisexclusion: r.indisexclusion,
            indisready: r.indisready,
            main_fork_bytes: r.main_fork_bytes,
            idx_blks_read: r.idx_blks_read,
            idx_blks_hit: r.idx_blks_hit,
            last_idx_scan: None,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
    };
    check_user_indexes_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded user-indexes rows.
fn check_user_indexes_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[UserIndexObservation],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_user_indexes section decoded to no rows"
    );

    for row in rows {
        ensure_ts_in_segment_range(major, "section 1_014", row.ts, min_ts, max_ts)?;
        // The buffer counters are COALESCE'd to 0 in SQL, so they always decode as
        // a non-negative i64 even when pg_statio has no row yet.
        anyhow::ensure!(
            row.idx_blks_read >= 0 && row.idx_blks_hit >= 0,
            "postgres {major}: an index buffer counter came back negative"
        );
        // last_idx_scan is a V2-only column; it must be absent below PG16.
        if major < 16 {
            anyhow::ensure!(
                row.last_idx_scan.is_none(),
                "postgres {major}: pre-PG16 row carries a last_idx_scan value"
            );
        }
    }

    // The probe table has a primary key, so both seeded databases must contribute
    // its pkey index. datname is the discriminator: the index name is identical.
    let mut seeded_with_pkey: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    // The long-expression index must also survive, with its (truncated) definition
    // resolving to a non-empty CREATE statement.
    let mut seeded_with_long_index: std::collections::HashSet<Vec<u8>> =
        std::collections::HashSet::new();
    for row in rows {
        let datname = resolve_string(major, dict, "datname", row.datname.0)?;
        let schemaname = resolve_string(major, dict, "schemaname", row.schemaname.0)?;
        let indexrelname = resolve_string(major, dict, "indexrelname", row.indexrelname.0)?;
        let amname = resolve_string(major, dict, "amname", row.amname.0)?;
        let indexdef = resolve_dictionary_bytes(major, dict, "indexdef", row.indexdef.0)?;
        anyhow::ensure!(
            !schemaname.is_empty(),
            "postgres {major}: schemaname resolved to an empty string"
        );
        let seeded = SEEDED_DATABASES
            .iter()
            .any(|name| name.as_bytes() == datname.as_slice());
        if indexrelname == b"kronika_ut_probe_pkey" && seeded {
            anyhow::ensure!(
                row.indisprimary,
                "postgres {major}: kronika_ut_probe_pkey is not flagged as a primary key"
            );
            // A plain primary key is a ready, non-exclusion index.
            anyhow::ensure!(
                !row.indisexclusion,
                "postgres {major}: a plain pkey is flagged as an exclusion index"
            );
            anyhow::ensure!(
                row.indisready,
                "postgres {major}: a live pkey is not flagged as ready"
            );
            anyhow::ensure!(
                amname == b"btree",
                "postgres {major}: kronika_ut_probe_pkey amname is not btree"
            );
            anyhow::ensure!(
                indexdef.windows(6).any(|w| w == b"CREATE"),
                "postgres {major}: kronika_ut_probe_pkey indexdef is not a CREATE statement"
            );
            // The probe table carries rows, so its pkey has real storage.
            anyhow::ensure!(
                row.main_fork_bytes > 0,
                "postgres {major}: kronika_ut_probe_pkey main_fork_bytes is not positive"
            );
            if major >= 16 {
                anyhow::ensure!(
                    row.last_idx_scan.is_some(),
                    "postgres {major}: kronika_ut_probe_pkey has no last_idx_scan after a forced pkey scan"
                );
            }
            seeded_with_pkey.insert(datname.clone());
        }
        if indexrelname == b"kronika_ut_probe_long_idx" && seeded {
            anyhow::ensure!(
                !indexdef.is_empty() && indexdef.windows(6).any(|w| w == b"CREATE"),
                "postgres {major}: kronika_ut_probe_long_idx indexdef did not survive truncation"
            );
            let indexdef_cap =
                usize::try_from(indexdef_max_len()).context("indexdef cap fits usize")?;
            anyhow::ensure!(
                indexdef.len() == indexdef_cap,
                "postgres {major}: kronika_ut_probe_long_idx indexdef len {} is not the cap {indexdef_cap}",
                indexdef.len()
            );
            if major >= 16 {
                anyhow::ensure!(
                    row.last_idx_scan.is_none(),
                    "postgres {major}: never-scanned kronika_ut_probe_long_idx has last_idx_scan"
                );
            }
            seeded_with_long_index.insert(datname);
        }
    }
    for datname in SEEDED_DATABASES {
        anyhow::ensure!(
            seeded_with_pkey.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe_pkey row for database {datname}"
        );
        anyhow::ensure!(
            seeded_with_long_index.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe_long_idx row for database {datname}"
        );
    }
    Ok(())
}

#[then(
    "each matrix cluster installs pg_stat_statements and seals rows with dictionary-backed query text"
)]
async fn every_version_seals_statements(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let version = install_statements_and_run_workload(db).await?;
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_statements_section(db.major(), version, &segment)?;
    }
    Ok(())
}

/// Install the extension, run a probe workload so the view has a row, and
/// return the layout the installed extension version maps to.
///
/// The test reads `pg_extension.extversion` instead of deriving the layout from
/// the server major.
async fn install_statements_and_run_workload(
    db: &cluster::Cluster,
) -> anyhow::Result<StatementsVersion> {
    let conn = db.connect().await?;
    conn.client()
        .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .await
        .with_context(|| format!("postgres {}: create pg_stat_statements", db.major()))?;
    // A distinctive statement the assertion can find in the sealed rows. Run it a
    // few times so it accrues calls and lands in both candidate axes.
    for _ in 0..3 {
        conn.client()
            .query("SELECT 42 AS kronika_pgss_probe", &[])
            .await
            .with_context(|| format!("postgres {}: run probe query", db.major()))?;
    }
    let extversion = statements_extversion(conn.client())
        .await
        .with_context(|| format!("postgres {}: read extension version", db.major()))?
        .with_context(|| {
            format!(
                "postgres {}: extension not installed after CREATE",
                db.major()
            )
        })?;
    Ok(statements_version(&extversion))
}

/// Decode the sealed `pg_stat_statements` section for the layout the extension
/// version selects, then check one snapshot timestamp, dictionary-backed query
/// text, and that the probe statement is present.
fn assert_statements_section(
    major: u32,
    version: StatementsVersion,
    path: &Path,
) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    // Project each layout to `(query, ts)`: query resolution and the probe check
    // are the same across versions.
    let observations = match version {
        StatementsVersion::V6 => {
            decode_section_rows::<PgStatStatementsV6>(path, &segment, PG_STAT_STATEMENTS_V6_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_006"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V5 => {
            decode_section_rows::<PgStatStatementsV5>(path, &segment, PG_STAT_STATEMENTS_V5_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_005"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V4 => {
            decode_section_rows::<PgStatStatementsV4>(path, &segment, PG_STAT_STATEMENTS_V4_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_004"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V3 => {
            decode_section_rows::<PgStatStatementsV3>(path, &segment, PG_STAT_STATEMENTS_V3_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_003"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V2 => {
            decode_section_rows::<PgStatStatementsV2>(path, &segment, PG_STAT_STATEMENTS_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_002"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V1 => {
            decode_section_rows::<PgStatStatementsV1>(path, &segment, PG_STAT_STATEMENTS_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_001"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
    };
    check_statements_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded `(query, ts)` projection: at least one row,
/// every ts in the segment range, every present query text resolves, and the
/// probe statement is among them.
fn check_statements_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[(Option<StrId>, i64)],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_statements section decoded to no rows"
    );

    let mut saw_probe = false;
    for (query, ts) in rows {
        ensure_ts_in_segment_range(major, "section 1_002", *ts, min_ts, max_ts)?;
        // The collector runs as a superuser, so query text is present, not NULL.
        let id = query.with_context(|| {
            format!("postgres {major}: a pg_stat_statements row has a NULL query text")
        })?;
        let text = resolve_string(major, dict, "query", id.0)?;
        if text
            .windows(b"kronika_pgss_probe".len())
            .any(|w| w == b"kronika_pgss_probe")
        {
            saw_probe = true;
        }
    }
    anyhow::ensure!(
        saw_probe,
        "postgres {major}: the probe statement was not found in the sealed rows"
    );
    Ok(())
}

/// Resolve a dictionary id to its bytes or fail with context.
///
/// Query text that exceeds the blob threshold is classified as `Resolved::Blob`
/// rather than `Resolved::String`. Both variants carry the stored bytes.
fn resolve_string(major: u32, dict: &Dictionary, label: &str, id: u64) -> anyhow::Result<Vec<u8>> {
    match dict.resolve(id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => Ok(bytes.to_vec()),
        other => anyhow::bail!(
            "postgres {major}: {label} str_id {id} did not resolve to a string: {other:?}"
        ),
    }
}

/// Resolve a dictionary id stored either in `dict.strings` or `dict.blobs`.
fn resolve_dictionary_bytes(
    major: u32,
    dict: &Dictionary,
    label: &str,
    id: u64,
) -> anyhow::Result<Vec<u8>> {
    match dict.resolve(id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => Ok(bytes.to_vec()),
        other => anyhow::bail!(
            "postgres {major}: {label} str_id {id} did not resolve to bytes: {other:?}"
        ),
    }
}

#[then("each matrix cluster seals its replication instance status")]
async fn every_version_seals_replication_instance(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_replication_instance_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Decode the sealed instance-replication section: one row, ts in range, a
/// positive timeline, and the standalone-primary shape (not in recovery,
/// `current_wal_lsn` set, standby/receiver columns NULL). The standby shape is
/// covered by source and codec tests, since the matrix runs standalone primaries.
fn assert_replication_instance_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let rows =
        decode_section_rows::<ReplicationInstance>(path, &segment, PG_REPLICATION_INSTANCE_TYPE_ID)
            .with_context(|| format!("postgres {major}: read back section 1_015_001"))?;
    anyhow::ensure!(
        rows.len() == 1,
        "postgres {major}: expected one replication-instance row, got {}",
        rows.len()
    );
    let row = &rows[0];
    ensure_ts_in_segment_range(
        major,
        "section 1_015_001",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts,
    )?;
    anyhow::ensure!(
        row.timeline_id >= 1,
        "postgres {major}: timeline_id {} is not positive",
        row.timeline_id
    );
    anyhow::ensure!(
        !row.is_in_recovery,
        "postgres {major}: a standalone cluster reports being in recovery"
    );
    anyhow::ensure!(
        row.current_wal_lsn.is_some(),
        "postgres {major}: a primary reports no current_wal_lsn"
    );
    anyhow::ensure!(
        row.standby_receive_lsn.is_none()
            && row.standby_replay_lsn.is_none()
            && row.replay_lag_s.is_none()
            && row.standby_last_replay_at.is_none()
            && row.sender_host.is_none()
            && row.wal_receiver_status.is_none()
            && row.sender_port.is_none()
            && row.slot_name.is_none()
            && row.latest_end_lsn.is_none()
            && row.latest_end_time.is_none()
            && row.received_tli.is_none(),
        "postgres {major}: a primary must not fill standby receiver columns"
    );
    anyhow::ensure!(
        row.streaming_replicas == 0,
        "postgres {major}: a standalone cluster reports {} streaming replicas",
        row.streaming_replicas
    );
    // synchronous_standby_names defaults to an empty string, so resolution must
    // accept an empty value; only the dictionary lookup itself must succeed.
    for (label, id) in [
        ("synchronous_standby_names", row.synchronous_standby_names.0),
        ("synchronous_commit", row.synchronous_commit.0),
    ] {
        anyhow::ensure!(
            matches!(dict.resolve(id), Some(Resolved::String(_))),
            "postgres {major}: {label} str_id {id} did not resolve to a string"
        );
    }
    Ok(())
}

#[then("each matrix cluster seals a single-row pg_stat_wal section")]
async fn every_version_seals_wal(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_wal_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Check selected layout, timestamp, singleton shape, reset, and counters.
fn assert_wal_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let has = |type_id: u32| {
        segment
            .catalog()
            .entries
            .iter()
            .any(|entry| entry.type_id == type_id)
    };
    match wal_version(major) {
        Some(WalVersion::V1) => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V2_TYPE_ID),
                "postgres {major}: PG14-17 sealed the PG18 wal layout 1_007_002"
            );
            let rows = decode_section_rows::<PgStatWalV1>(path, &segment, PG_STAT_WAL_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_007_001"))?;
            let row = single_wal_row(major, rows)?;
            ensure_ts_in_segment_range(major, "section 1_007_001", row.ts.0, min_ts, max_ts)?;
            anyhow::ensure!(
                row.wal_records >= 0
                    && row.wal_fpi >= 0
                    && row.wal_bytes >= 0
                    && row.wal_buffers_full >= 0
                    && row.wal_write >= 0
                    && row.wal_sync >= 0,
                "postgres {major}: a pg_stat_wal counter came back negative"
            );
            anyhow::ensure!(
                row.wal_write_time.is_finite()
                    && row.wal_write_time >= 0.0
                    && row.wal_sync_time.is_finite()
                    && row.wal_sync_time >= 0.0,
                "postgres {major}: pg_stat_wal timing came back invalid"
            );
            check_wal_stats_reset(major, row.stats_reset.map(|ts| ts.0), row.ts.0)?;
        }
        Some(WalVersion::V2) => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V1_TYPE_ID),
                "postgres {major}: PG18 sealed the PG14-17 wal layout 1_007_001"
            );
            let rows = decode_section_rows::<PgStatWalV2>(path, &segment, PG_STAT_WAL_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_007_002"))?;
            let row = single_wal_row(major, rows)?;
            ensure_ts_in_segment_range(major, "section 1_007_002", row.ts.0, min_ts, max_ts)?;
            anyhow::ensure!(
                row.wal_records >= 0
                    && row.wal_fpi >= 0
                    && row.wal_bytes >= 0
                    && row.wal_buffers_full >= 0,
                "postgres {major}: a pg_stat_wal counter came back negative"
            );
            check_wal_stats_reset(major, row.stats_reset.map(|ts| ts.0), row.ts.0)?;
        }
        None => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V1_TYPE_ID) && !has(PG_STAT_WAL_V2_TYPE_ID),
                "postgres {major}: pg_stat_wal section present before PG14"
            );
        }
    }
    Ok(())
}

fn has_section(segment: &Segment, type_id: u32) -> bool {
    segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == type_id)
}

/// `stats_reset`, when present, must not be after the snapshot ts.
fn check_wal_stats_reset(major: u32, reset: Option<i64>, ts: i64) -> anyhow::Result<()> {
    if let Some(reset) = reset {
        anyhow::ensure!(
            reset <= ts,
            "postgres {major}: wal stats_reset {reset} is after snapshot ts {ts}"
        );
    }
    Ok(())
}

/// Extract the one row a `pg_stat_wal` snapshot must hold.
fn single_wal_row<T>(major: u32, rows: Vec<T>) -> anyhow::Result<T> {
    let mut rows = rows.into_iter();
    let row = rows
        .next()
        .with_context(|| format!("postgres {major}: pg_stat_wal section decoded to no rows"))?;
    anyhow::ensure!(
        rows.next().is_none(),
        "postgres {major}: pg_stat_wal section decoded to multiple rows"
    );
    Ok(row)
}

#[then("each matrix cluster opens per-database pool connections")]
async fn every_cluster_opens_per_db_pool_connections(world: &mut BddWorld) -> anyhow::Result<()> {
    use kronika_source_pg::pool::{
        ConnectionPool, DEFAULT_MAX_DATABASES, SessionConfig, enumerate_databases,
    };
    use std::collections::HashSet;
    use std::time::Duration;
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    let session = SessionConfig {
        statement_timeout_ms: 15_000,
        lock_timeout_ms: 1_000,
        idle_in_tx_timeout_ms: 10_000,
    };
    for db in world.clusters {
        let dsn = db.conn_string();
        let mut pool =
            ConnectionPool::connect(&dsn, "pg_kronika-bdd", session, HashSet::new()).await?;
        pool.refresh(Duration::from_secs(0), DEFAULT_MAX_DATABASES)
            .await?;
        anyhow::ensure!(
            !pool.per_db().is_empty(),
            "postgres {}: no per-db connections",
            db.major()
        );
        anyhow::ensure!(
            pool.uncovered().is_empty(),
            "postgres {}: databases without pool connection: {:?}",
            db.major(),
            pool.uncovered()
        );
        let names = enumerate_databases(pool.main(), &HashSet::new()).await?;
        anyhow::ensure!(
            !names.iter().any(|n| n == "template0" || n == "template1"),
            "postgres {}: template database was enumerated",
            db.major()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    // Every scenario: tear down its harness state (held transactions, blocking
    // tasks, temp databases). On failure: dump each booted cluster's PostgreSQL
    // server log so CI shows the server-side cause (e.g. a rejected statement)
    // instead of only the opaque step panic. PostgreSQL logs errors regardless
    // of DEBUG.
    BddWorld::cucumber()
        .fail_on_skipped()
        .after(|_feature, _rule, _scenario, ev, world| {
            Box::pin(async move {
                let failed = matches!(
                    ev,
                    event::ScenarioFinished::StepFailed(..)
                        | event::ScenarioFinished::BeforeHookFailed(_)
                );
                if let event::ScenarioFinished::StepFailed(_, _, err) = ev {
                    eprintln!("=== BDD step failed: {err} ===");
                }
                if let Some(world) = world {
                    if failed {
                        for cluster in world.clusters {
                            eprintln!(
                                "=== postgres {} server.log ===\n{}\n=== end postgres {} server.log ===",
                                cluster.major(),
                                cluster.server_log(),
                                cluster.major(),
                            );
                        }
                    }
                    world.harness.cleanup().await;
                }
            })
        })
        .run_and_exit(features)
        .await;
}

#[cfg(test)]
mod tests {
    use super::single_wal_row;

    #[test]
    fn single_wal_row_accepts_exactly_one() {
        assert_eq!(single_wal_row(15, vec![42_i32]).expect("one row"), 42);
    }

    #[test]
    fn single_wal_row_rejects_no_rows() {
        assert!(single_wal_row::<i32>(15, Vec::new()).is_err());
    }

    #[test]
    fn single_wal_row_rejects_multiple_rows() {
        assert!(single_wal_row(15, vec![1_i32, 2]).is_err());
    }

    #[test]
    fn resolve_string_accepts_blob_entry() {
        use super::resolve_string;
        use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
        use kronika_reader::{Resolved, Segment};

        // A blob_threshold of 4 forces any value longer than 3 bytes into
        // dict.blobs. "hello" (5 bytes) lands there.
        let limits = DictLimits::new(4, 1 << 20).expect("limits");
        let mut interner = kronika_writer::Interner::new(limits);
        let id = interner.intern_blob(b"hello").expect("intern blob");

        let dict_sections = kronika_writer::dict::encode(interner.window()).expect("encode");
        let section_inputs: Vec<_> = dict_sections
            .iter()
            .map(|s| SectionInput {
                type_id: s.type_id,
                rows: s.rows,
                body: &s.body,
            })
            .collect();
        let bytes = build_part(
            &section_inputs,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.pgm");
        std::fs::write(&path, &bytes).expect("write segment");

        let segment = Segment::open(&path).expect("open segment");
        let dict = segment.dictionary().expect("read dictionary");

        assert_eq!(
            dict.resolve(id.get()),
            Some(Resolved::Blob {
                bytes: b"hello",
                full_len: 5,
                truncated: false
            }),
            "entry is classified as Blob"
        );
        let result = resolve_string(15, &dict, "query", id.get());
        assert_eq!(result.expect("resolve_string accepts Blob"), b"hello");
    }
}
