use crate::logging::{
    LogLevel, field, layout_id, log_collection_failure, log_collection_finish,
    log_collection_start, log_event, section_name,
};
use anyhow::{Context, Result};
use kronika_source_pg::reset_metadata::{
    ResetBase, ResetExtensions, collect_reset_base, compute_query_id_setting, statements_reset_at,
    store_plans_reset_at,
};
use tokio_postgres::Client;

/// One discovered extension source used to assemble `reset_metadata`.
pub(crate) struct ExtensionResetSource<'a> {
    pub(crate) client: Option<&'a Client>,
    pub(crate) version: &'a str,
    pub(crate) label: String,
}

/// Collect the base reset state and the extension state from their actual
/// source connections.
pub(crate) async fn collect_reset_metadata(
    main: &Client,
    major: u32,
    statements: Option<&ExtensionResetSource<'_>>,
    plans: Option<&ExtensionResetSource<'_>>,
) -> Result<(ResetBase, ResetExtensions)> {
    collect_reset_metadata_inner(main, major, statements, plans, false).await
}

/// Collect reset metadata for a plan snapshot.
///
/// The plan info view may legitimately be absent, but an error while probing
/// or reading it makes reset continuity unknowable and rejects the snapshot.
pub(crate) async fn collect_plan_reset_metadata(
    main: &Client,
    major: u32,
    statements: Option<&ExtensionResetSource<'_>>,
    plans: &ExtensionResetSource<'_>,
) -> Result<(ResetBase, ResetExtensions)> {
    let mut reset =
        collect_reset_metadata_inner(main, major, statements, Some(plans), true).await?;
    let client = plans
        .client
        .context("pg_store_plans source unavailable for reset metadata")?;
    reset.0.compute_query_id = compute_query_id_setting(client)
        .await
        .context("collect pg_store_plans compute_query_id")?;
    Ok(reset)
}

async fn collect_reset_metadata_inner(
    main: &Client,
    major: u32,
    statements: Option<&ExtensionResetSource<'_>>,
    plans: Option<&ExtensionResetSource<'_>>,
    require_plan_info_read: bool,
) -> Result<(ResetBase, ResetExtensions)> {
    let type_id = 1_020_001;
    let started = std::time::Instant::now();
    log_collection_start(type_id, "main");
    let base = match collect_reset_base(main, major).await {
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
    if let Some(source) = statements {
        ext.statements_version = Some(source.version.to_owned());
        if let Some(client) = source.client {
            match statements_reset_at(client).await {
                Ok(reset) => ext.statements_reset_at = reset,
                Err(err) => log_extension_failure(
                    type_id,
                    &source.label,
                    "pg_stat_statements_info_failed",
                    &err,
                ),
            }
        }
    }
    if let Some(source) = plans {
        ext.store_plans_version = Some(source.version.to_owned());
        if let Some(client) = source.client {
            match store_plans_reset_at(client).await {
                Ok(reset) => ext.store_plans_reset_at = reset,
                Err(err) => {
                    log_extension_failure(
                        type_id,
                        &source.label,
                        "pg_store_plans_info_failed",
                        &err,
                    );
                    if require_plan_info_read {
                        return Err(err).context("collect pg_store_plans reset metadata");
                    }
                }
            }
        }
    }
    Ok((base, ext))
}

fn log_extension_failure(
    type_id: u32,
    source: &str,
    reason: &'static str,
    error: &tokio_postgres::Error,
) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
            field("error", error),
        ],
    );
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlanResetContext<'a> {
    postmaster_start_time: i64,
    reset_at: Option<i64>,
    extension_version: Option<&'a str>,
    compute_query_id: Option<&'a str>,
}

/// State that must remain unchanged while one plan snapshot is read.
pub(crate) fn plan_reset_context(reset: &(ResetBase, ResetExtensions)) -> PlanResetContext<'_> {
    PlanResetContext {
        postmaster_start_time: reset.0.postmaster_start_time,
        reset_at: reset.1.store_plans_reset_at,
        extension_version: reset.1.store_plans_version.as_deref(),
        compute_query_id: reset.0.compute_query_id.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use kronika_source_pg::reset_metadata::{ResetBase, ResetExtensions};

    use super::plan_reset_context;

    fn reset(ts: i64) -> (ResetBase, ResetExtensions) {
        (
            ResetBase {
                ts,
                postmaster_start_time: 1,
                pg_stat_database_reset_max_at: None,
                pg_stat_bgwriter_reset_at: None,
                pg_stat_checkpointer_reset_at: None,
                pg_stat_wal_reset_at: None,
                pg_stat_archiver_reset_at: None,
                pg_stat_io_reset_at: None,
                compute_query_id: Some("auto".to_owned()),
                track_io_timing: Some(true),
                track_wal_io_timing: Some(false),
            },
            ResetExtensions {
                statements_version: None,
                statements_reset_at: None,
                store_plans_version: Some("1.10".to_owned()),
                store_plans_reset_at: Some(7),
            },
        )
    }

    #[test]
    fn plan_context_ignores_observation_time_but_detects_reset_changes() {
        let before = reset(10);
        let mut after = reset(20);
        assert_eq!(
            plan_reset_context(&before),
            plan_reset_context(&after),
            "the two real observations bracket the plan read"
        );

        after.1.store_plans_reset_at = Some(11);
        assert_ne!(plan_reset_context(&before), plan_reset_context(&after));

        let mut after = reset(20);
        after.1.store_plans_version = Some("1.11".to_owned());
        assert_ne!(plan_reset_context(&before), plan_reset_context(&after));

        let mut after = reset(20);
        after.0.compute_query_id = Some("off".to_owned());
        assert_ne!(plan_reset_context(&before), plan_reset_context(&after));
    }
}
