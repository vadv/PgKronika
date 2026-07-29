use crate::config::validate_settings_row_count;
use crate::logging::{
    LogLevel, duration_ms, field, layout_id, log_collection_failure, log_collection_finish,
    log_collection_start, log_event, section_name,
};
use crate::plans_source::PlansSourceCache;
use crate::reset_source::{ExtensionResetSource, collect_reset_metadata};
use crate::scheduler::{DueSet, SourceKind};
use crate::statements_source::{StatementsSourceCache, statement_client};
use anyhow::{Context, Result};
use kronika_source_os::{OsInstanceFacts, collect_os_instance_facts};
use kronika_source_pg::instance_metadata::{
    PgInstanceFacts, collect_pg_instance_facts, pg_system_identifier,
};
use kronika_source_pg::pool::ConnectionPool;
use kronika_source_pg::reset_metadata::{ResetBase, ResetExtensions};
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
    statements_cache: &StatementsSourceCache,
    plans_cache: &PlansSourceCache,
    due: &DueSet,
    instance: Option<InstanceFacts>,
    plan_reset: Option<(ResetBase, ResetExtensions)>,
) -> Result<ServiceSections> {
    let reset = if let Some(reset) = plan_reset {
        Some(reset)
    } else if due.has(SourceKind::ResetMetadata) {
        let statements = statements_cache
            .selected
            .as_ref()
            .map(|cached| ExtensionResetSource {
                client: statement_client(pool, &cached.source),
                version: &cached.extversion,
                label: cached.source.label(),
            });
        let plans = plans_cache
            .selected
            .as_ref()
            .map(|cached| ExtensionResetSource {
                client: statement_client(pool, &cached.source),
                version: &cached.extversion,
                label: cached.source.label(),
            });
        Some(collect_reset_metadata(pool.main(), major, statements.as_ref(), plans.as_ref()).await?)
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

/// Collect due factual `PostgreSQL` and host metadata.
pub(crate) async fn collect_due_instance(
    pool: &ConnectionPool,
    due: &DueSet,
) -> Result<Option<InstanceFacts>> {
    if due.has(SourceKind::InstanceMetadata) {
        return Ok(Some(collect_instance_facts(pool.main()).await?));
    }
    Ok(None)
}

/// Fields written to `instance_metadata`, joined from `PostgreSQL` and the host.
#[derive(Debug)]
pub(crate) struct InstanceFacts {
    pub(crate) pg: PgInstanceFacts,
    /// `None` when `pg_control_system()` is not executable under this role.
    pub(crate) system_identifier: Option<i64>,
    pub(crate) os: OsInstanceFacts,
}

/// Collect passive metadata; only the system identifier may degrade.
async fn collect_instance_facts(client: &Client) -> Result<InstanceFacts> {
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
    let facts = InstanceFacts {
        pg,
        system_identifier,
        os,
    };
    log_collection_finish(type_id, "main", 1, started.elapsed());
    Ok(facts)
}
