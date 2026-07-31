//! Exact single-PGM context projection for the UI shell.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_reader::{
    LocalDirSnapshot, OutRow, QueryError, QueryLimits, SealedQuerySession, Value, WebIndexReadError,
};
use serde::Serialize;
use utoipa::ToSchema;

use super::snapshot::resolve_snapshot_at;

const CONTEXT_SECTIONS: &[&str] = &[
    "instance_metadata",
    "pg_stat_database",
    "replication_instance",
    "pg_stat_replication",
    "os_topology",
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextLimits {
    rows: usize,
    cells: usize,
    bytes: usize,
}

impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            rows: 10_000,
            cells: 250_000,
            bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub(crate) enum ContextError {
    WebIndex(WebIndexReadError),
    Query(QueryError),
    Arithmetic,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebIndex(error) => write!(f, "{error}"),
            Self::Query(error) => write!(f, "context query failed: {error:?}"),
            Self::Arithmetic => f.write_str("context projection arithmetic overflow"),
        }
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::WebIndex(error) => Some(error),
            Self::Query(_) | Self::Arithmetic => None,
        }
    }
}

impl From<WebIndexReadError> for ContextError {
    fn from(error: WebIndexReadError) -> Self {
        Self::WebIndex(error)
    }
}

impl From<QueryError> for ContextError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ContextResponse {
    snapshot_ts_us: String,
    instance: ContextInstance,
    host: ContextHost,
    databases: Vec<ContextDatabase>,
    replication: ContextReplication,
    quality: ContextQuality,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextInstance {
    hostname: Option<String>,
    pg_version_num: Option<i64>,
    pg_system_identifier: Option<String>,
    pg_system_identifier_reason: Option<&'static str>,
    role: Option<&'static str>,
    role_reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextHost {
    logical_cpu_count: Option<usize>,
    logical_cpu_count_reason: Option<&'static str>,
    kernel_version: Option<String>,
    boot_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextDatabase {
    entity: String,
    oid: u32,
    name: String,
    visibility: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextReplication {
    instance: Option<ContextReplicationInstance>,
    replicas: Vec<ContextReplica>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextReplicationInstance {
    timeline_id: i64,
    streaming_replicas: i64,
    replay_lag_us: Option<i64>,
    replay_lag_reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextReplica {
    entity: String,
    pid: i64,
    application_name: Option<String>,
    state: Option<String>,
    sync_state: Option<String>,
    replay_lag_us: Option<i64>,
    replay_lag_reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextQuality {
    status: &'static str,
    gaps: Vec<ContextGap>,
    gated: Vec<&'static str>,
    active_tail: bool,
}

#[derive(Debug, Serialize, ToSchema)]
struct ContextGap {
    from_us: String,
    to_us: String,
}

pub(crate) fn build_context(
    snapshot: &LocalDirSnapshot,
    at_us: i64,
    limits: ContextLimits,
) -> Result<ContextResponse, ContextError> {
    let Some(resolved) = resolve_snapshot_at(snapshot, at_us)? else {
        return Ok(empty_context(at_us));
    };
    let query_limits = QueryLimits::with_bytes(limits.rows, limits.cells, limits.bytes);
    let mut query = SealedQuerySession::new(snapshot, query_limits);
    let pages = query.sections(
        &resolved.descriptor,
        resolved.descriptor.min_ts,
        resolved.timestamp_us,
        CONTEXT_SECTIONS,
        &BTreeMap::new(),
    )?;
    if pages.values().any(|page| page.next_cursor.is_some()) {
        return Err(ContextError::Query(QueryError::RowsTooLarge {
            max_rows: limits.rows,
        }));
    }
    let snapshot_ts_us = pages
        .values()
        .flat_map(|page| page.rows.iter())
        .filter_map(row_timestamp)
        .filter(|timestamp| *timestamp <= resolved.timestamp_us)
        .max()
        .unwrap_or(resolved.timestamp_us);

    let instance_row = latest_row(page_rows(&pages, "instance_metadata"), snapshot_ts_us);
    let replication_row = latest_row(page_rows(&pages, "replication_instance"), snapshot_ts_us);
    let instance = context_instance(instance_row, replication_row);
    let host = context_host(
        instance_row,
        page_rows(&pages, "os_topology"),
        snapshot_ts_us,
    );
    let system_identifier = instance_row.and_then(|row| optional_i64(row, "pg_system_identifier"));
    let databases = context_databases(
        page_rows(&pages, "pg_stat_database"),
        snapshot_ts_us,
        system_identifier,
    )?;
    let replication = context_replication(
        replication_row,
        page_rows(&pages, "pg_stat_replication"),
        snapshot_ts_us,
    )?;
    // Zero walsenders is a fact about a standalone primary, not missing data.
    let mut gated = CONTEXT_SECTIONS
        .iter()
        .copied()
        .filter(|section| *section != "pg_stat_replication")
        .filter(|section| page_rows(&pages, section).is_empty())
        .collect::<Vec<_>>();
    gated.sort_unstable();
    let gaps = pages
        .values()
        .flat_map(|page| page.gaps.iter())
        .map(|gap| ContextGap {
            from_us: gap.from.to_string(),
            to_us: gap.to.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(ContextResponse {
        snapshot_ts_us: snapshot_ts_us.to_string(),
        instance,
        host,
        databases,
        replication,
        quality: ContextQuality {
            status: if gated.is_empty() && gaps.is_empty() {
                "complete"
            } else {
                "partial"
            },
            gaps,
            gated,
            active_tail: false,
        },
    })
}

fn empty_context(at_us: i64) -> ContextResponse {
    ContextResponse {
        snapshot_ts_us: at_us.to_string(),
        instance: ContextInstance {
            hostname: None,
            pg_version_num: None,
            pg_system_identifier: None,
            pg_system_identifier_reason: Some("not_collected"),
            role: None,
            role_reason: Some("not_collected"),
        },
        host: ContextHost {
            logical_cpu_count: None,
            logical_cpu_count_reason: Some("not_collected"),
            kernel_version: None,
            boot_id: None,
        },
        databases: Vec::new(),
        replication: ContextReplication {
            instance: None,
            replicas: Vec::new(),
        },
        quality: ContextQuality {
            status: "partial",
            gaps: Vec::new(),
            gated: CONTEXT_SECTIONS.to_vec(),
            active_tail: false,
        },
    }
}

fn context_instance(instance: Option<&OutRow>, replication: Option<&OutRow>) -> ContextInstance {
    let system_identifier = instance.and_then(|row| optional_i64(row, "pg_system_identifier"));
    let role = replication.and_then(|row| {
        bool_value(row, "is_in_recovery").map(|standby| if standby { "standby" } else { "primary" })
    });
    ContextInstance {
        hostname: instance.and_then(|row| text(row, "hostname").map(ToOwned::to_owned)),
        pg_version_num: instance.and_then(|row| integer(row, "pg_version_num")),
        pg_system_identifier: system_identifier.map(|value| value.to_string()),
        pg_system_identifier_reason: if system_identifier.is_some() {
            None
        } else if instance.is_some() {
            Some("permission")
        } else {
            Some("not_collected")
        },
        role,
        role_reason: role.is_none().then_some("not_collected"),
    }
}

fn context_host(
    instance: Option<&OutRow>,
    topology: &[OutRow],
    snapshot_ts_us: i64,
) -> ContextHost {
    let latest_topology = topology
        .iter()
        .filter_map(row_timestamp)
        .filter(|timestamp| *timestamp <= snapshot_ts_us)
        .max();
    let mut cpus = BTreeSet::new();
    for row in topology {
        if row_timestamp(row) != latest_topology || integer(row, "scope") != Some(0) {
            continue;
        }
        if let Some(cpu_id) = integer(row, "cpu_id").and_then(|value| i32::try_from(value).ok()) {
            cpus.insert(cpu_id);
        }
    }
    ContextHost {
        logical_cpu_count: (!cpus.is_empty()).then_some(cpus.len()),
        logical_cpu_count_reason: cpus.is_empty().then_some("not_collected"),
        kernel_version: instance.and_then(|row| text(row, "kernel_version").map(ToOwned::to_owned)),
        boot_id: instance.and_then(|row| text(row, "boot_id").map(ToOwned::to_owned)),
    }
}

fn context_databases(
    rows: &[OutRow],
    snapshot_ts_us: i64,
    system_identifier: Option<i64>,
) -> Result<Vec<ContextDatabase>, ContextError> {
    let latest = rows
        .iter()
        .filter_map(row_timestamp)
        .filter(|timestamp| *timestamp <= snapshot_ts_us)
        .max();
    let mut databases = rows
        .iter()
        .filter(|row| row_timestamp(row) == latest)
        .filter_map(|row| {
            let oid = integer(row, "datid").and_then(|value| u32::try_from(value).ok())?;
            let name = text(row, "datname")?;
            (oid != 0).then(|| ContextDatabase {
                entity: database_entity(system_identifier, oid),
                oid,
                name: name.to_owned(),
                visibility: "full",
            })
        })
        .collect::<Vec<_>>();
    if databases.len() > 10_000 {
        return Err(ContextError::Arithmetic);
    }
    databases.sort_by_key(|database| database.oid);
    Ok(databases)
}

fn context_replication(
    instance: Option<&OutRow>,
    rows: &[OutRow],
    snapshot_ts_us: i64,
) -> Result<ContextReplication, ContextError> {
    let instance = instance.map(|row| {
        let primary = bool_value(row, "is_in_recovery") == Some(false);
        let replay_lag_us = optional_i64(row, "replay_lag_s")
            .and_then(|seconds| seconds.checked_mul(1_000_000).filter(|value| *value >= 0));
        ContextReplicationInstance {
            timeline_id: integer(row, "timeline_id").unwrap_or_default(),
            streaming_replicas: integer(row, "streaming_replicas").unwrap_or_default(),
            replay_lag_us,
            replay_lag_reason: replay_lag_us.is_none().then_some(if primary {
                "primary"
            } else {
                "not_observed"
            }),
        }
    });
    let latest = rows
        .iter()
        .filter_map(row_timestamp)
        .filter(|timestamp| *timestamp <= snapshot_ts_us)
        .max();
    let mut replicas = rows
        .iter()
        .filter(|row| row_timestamp(row) == latest)
        .filter_map(|row| {
            let pid = integer(row, "pid")?;
            let application_name = text(row, "application_name").map(ToOwned::to_owned);
            let replay_lag_us = optional_i64(row, "replay_lag_us");
            Some(ContextReplica {
                entity: replica_entity(pid, application_name.as_deref()),
                pid,
                application_name,
                state: text(row, "state").map(ToOwned::to_owned),
                sync_state: text(row, "sync_state").map(ToOwned::to_owned),
                replay_lag_us,
                replay_lag_reason: replay_lag_us.is_none().then_some("not_observed"),
            })
        })
        .collect::<Vec<_>>();
    if replicas.len() > 10_000 {
        return Err(ContextError::Arithmetic);
    }
    replicas.sort_by_key(|replica| replica.pid);
    Ok(ContextReplication { instance, replicas })
}

fn database_entity(system_identifier: Option<i64>, oid: u32) -> String {
    let mut identity = Vec::with_capacity(15);
    identity.extend_from_slice(&1_u16.to_le_bytes());
    identity.push(u8::from(system_identifier.is_some()));
    if let Some(system_identifier) = system_identifier {
        identity.extend_from_slice(&system_identifier.to_le_bytes());
    }
    identity.extend_from_slice(&oid.to_le_bytes());
    URL_SAFE_NO_PAD.encode(identity)
}

fn replica_entity(pid: i64, application_name: Option<&str>) -> String {
    let application_name = application_name.unwrap_or_default().as_bytes();
    let name_len = u16::try_from(application_name.len()).unwrap_or(u16::MAX);
    let mut identity = Vec::with_capacity(12 + usize::from(name_len));
    identity.extend_from_slice(&1_u16.to_le_bytes());
    identity.extend_from_slice(&pid.to_le_bytes());
    identity.extend_from_slice(&name_len.to_le_bytes());
    identity.extend_from_slice(&application_name[..usize::from(name_len)]);
    URL_SAFE_NO_PAD.encode(identity)
}

fn page_rows<'a>(
    pages: &'a BTreeMap<String, kronika_reader::SectionPage>,
    section: &str,
) -> &'a [OutRow] {
    pages.get(section).map_or(&[], |page| page.rows.as_slice())
}

fn latest_row(rows: &[OutRow], at_us: i64) -> Option<&OutRow> {
    rows.iter()
        .filter_map(|row| row_timestamp(row).map(|timestamp| (timestamp, row)))
        .filter(|(timestamp, _row)| *timestamp <= at_us)
        .max_by_key(|(timestamp, _row)| *timestamp)
        .map(|(_timestamp, row)| row)
}

fn value<'a>(row: &'a OutRow, name: &str) -> Option<&'a Value> {
    row.iter()
        .find_map(|(candidate, value)| (candidate == name).then_some(value))
}

fn row_timestamp(row: &OutRow) -> Option<i64> {
    match value(row, "ts") {
        Some(Value::Ts(timestamp)) => Some(*timestamp),
        _ => None,
    }
}

fn integer(row: &OutRow, name: &str) -> Option<i64> {
    match value(row, name) {
        Some(Value::I64(value)) => Some(*value),
        Some(Value::U64(value)) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn optional_i64(row: &OutRow, name: &str) -> Option<i64> {
    integer(row, name)
}

fn bool_value(row: &OutRow, name: &str) -> Option<bool> {
    match value(row, name) {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn text<'a>(row: &'a OutRow, name: &str) -> Option<&'a str> {
    match value(row, name) {
        Some(Value::Str(value) | Value::Blob { text: value, .. }) => Some(value),
        _ => None,
    }
}
