use crate::config::{Config, validate_settings_row_count};
use crate::logging::{
    LogLevel, duration_ms, field, layout_id, log_collection_failure, log_collection_finish,
    log_collection_start, log_event, section_name,
};
use crate::plans_source::PlansSourceCache;
use crate::scheduler::{DueSet, SourceKind};
use crate::statements_source::{StatementsSourceCache, statement_client};
use anyhow::{Context, Result};
use kronika_source_os::{OsInstanceFacts, collect_os_instance_facts};
use kronika_source_pg::instance_metadata::{
    PgInstanceFacts, collect_pg_instance_facts, pg_system_identifier,
};
use kronika_source_pg::pool::ConnectionPool;
use kronika_source_pg::reset_metadata::{
    ResetBase, ResetExtensions, collect_reset_base, statements_reset_at, store_plans_reset_at,
};
use kronika_source_pg::settings::{SettingsRow, collect_settings};
use std::time::Instant;
use tokio_postgres::Client;

/// Service rows gated by their scheduler intervals.
pub(crate) struct ServiceSections {
    pub(crate) reset: Option<(ResetBase, ResetExtensions)>,
    pub(crate) instance: Option<InstanceFacts>,
    pub(crate) settings: Vec<SettingsRow>,
}

/// Collect the due service sections.
pub(crate) async fn collect_service_sections(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &StatementsSourceCache,
    plans_cache: &PlansSourceCache,
    due: &DueSet,
) -> Result<ServiceSections> {
    let reset = if due.has(SourceKind::ResetMetadata) {
        Some(collect_reset_metadata_all(pool, major, statements_cache, plans_cache).await?)
    } else {
        None
    };
    let instance = if due.has(SourceKind::InstanceMetadata) {
        Some(collect_instance_facts(pool.main(), config).await?)
    } else {
        None
    };
    let settings = if due.has(SourceKind::Settings) {
        let type_id = 1_019_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        let settings = match collect_settings(pool.main()).await {
            Ok(settings) => {
                log_collection_finish(type_id, "main", settings.len(), started.elapsed());
                settings
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_settings");
            }
        };
        validate_settings_row_count(settings.len())?;
        settings
    } else {
        Vec::new()
    };
    Ok(ServiceSections {
        reset,
        instance,
        settings,
    })
}

/// Assemble `reset_metadata`: the base from the main connection plus the
/// extension info views read through the discovered statements and plans
/// sources. An info-view failure degrades that one timestamp to `NULL`.
async fn collect_reset_metadata_all(
    pool: &ConnectionPool,
    major: u32,
    statements_cache: &StatementsSourceCache,
    plans_cache: &PlansSourceCache,
) -> Result<(ResetBase, ResetExtensions)> {
    let type_id = 1_020_001;
    let started = Instant::now();
    log_collection_start(type_id, "main");
    let base = match collect_reset_base(pool.main(), major).await {
        Ok(base) => {
            log_collection_finish(type_id, "main", 1, started.elapsed());
            base
        }
        Err(err) => {
            log_collection_failure(type_id, "main", &err, started.elapsed());
            return Err(err).context("collect reset metadata");
        }
    };
    let mut ext = ResetExtensions::default();
    if let Some(cached) = &statements_cache.selected {
        ext.statements_version = Some(cached.extversion.clone());
        if let Some(client) = statement_client(pool, &cached.source) {
            match statements_reset_at(client).await {
                Ok(reset) => ext.statements_reset_at = reset,
                Err(err) => log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", cached.source.label()),
                        field("reason", "pg_stat_statements_info_failed"),
                        field("error", &err),
                    ],
                ),
            }
        }
    }
    if let Some(cached) = &plans_cache.selected {
        ext.store_plans_version = Some(cached.extversion.clone());
        if let Some(client) = statement_client(pool, &cached.source) {
            match store_plans_reset_at(client).await {
                Ok(reset) => ext.store_plans_reset_at = reset,
                Err(err) => log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", cached.source.label()),
                        field("reason", "pg_store_plans_info_failed"),
                        field("error", &err),
                    ],
                ),
            }
        }
    }
    Ok((base, ext))
}

/// Fields written to `instance_metadata`, joined from `PostgreSQL` and the host.
#[derive(Debug)]
pub(crate) struct InstanceFacts {
    pub(crate) pg: PgInstanceFacts,
    /// `None` when `pg_control_system()` is not executable under this role.
    pub(crate) system_identifier: Option<i64>,
    pub(crate) os: OsInstanceFacts,
    pub(crate) node_self_id: String,
}

/// Collect the instance fingerprint; only the system identifier may degrade.
async fn collect_instance_facts(client: &Client, config: &Config) -> Result<InstanceFacts> {
    let type_id = 1_021_001;
    let started = Instant::now();
    log_collection_start(type_id, "main");
    let pg = match collect_pg_instance_facts(client).await {
        Ok(pg) => pg,
        Err(err) => {
            log_collection_failure(type_id, "main", &err, started.elapsed());
            return Err(err).context("collect instance metadata");
        }
    };
    let system_identifier = match pg_system_identifier(client).await {
        Ok(id) => Some(id),
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "pg_control_system_unavailable"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            None
        }
    };
    let os = collect_os_instance_facts().context("collect OS instance facts")?;
    let node_self_id = config
        .node_self_id
        .clone()
        .unwrap_or_else(|| os.hostname.clone());
    let facts = InstanceFacts {
        pg,
        system_identifier,
        os,
        node_self_id,
    };
    log_collection_finish(type_id, "main", 1, started.elapsed());
    Ok(facts)
}
