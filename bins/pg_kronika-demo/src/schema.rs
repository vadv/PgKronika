//! Seeds the saturated demo schema.
//!
//! OLTP core (`accounts`, `orders`, `locked_resource`), a ~200 MB
//! `staging.large_scan` for buffer eviction, insert-heavy `audit` tables, and
//! filler tables/indexes that push the collector toward its top-500 caps.

use anyhow::{Context, Result};
use tokio_postgres::Client;

use crate::config::Config;

const CORE_SCHEMA: &str = "
CREATE TABLE accounts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    balance bigint NOT NULL DEFAULT 0,
    name text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
) TABLESPACE ts_hot;

CREATE TABLE orders (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id bigint NOT NULL REFERENCES accounts (id),
    amount bigint NOT NULL,
    status text NOT NULL DEFAULT 'new',
    created_at timestamptz NOT NULL DEFAULT now()
) TABLESPACE ts_hot;
CREATE INDEX orders_account_id_idx ON orders (account_id);
CREATE INDEX orders_status_idx ON orders (status);
CREATE INDEX orders_created_at_idx ON orders (created_at);

CREATE TABLE locked_resource (
    id int PRIMARY KEY,
    value bigint NOT NULL DEFAULT 0
);

CREATE SCHEMA staging;
CREATE TABLE staging.large_scan (
    id bigint PRIMARY KEY,
    payload text NOT NULL
) TABLESPACE ts_cold;

CREATE SCHEMA audit;
CREATE TABLE audit.logs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ts timestamptz NOT NULL DEFAULT now(),
    message text NOT NULL
);
CREATE TABLE audit.events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ts timestamptz NOT NULL DEFAULT now(),
    kind int NOT NULL,
    data text NOT NULL
);
";

pub(crate) async fn seed(client: &Client, config: &Config) -> Result<()> {
    client
        .batch_execute(CORE_SCHEMA)
        .await
        .context("create the core schema")?;
    client
        .execute(
            "INSERT INTO accounts (balance, name)
             SELECT g * 100, 'account-' || g FROM generate_series(1, 10000) g",
            &[],
        )
        .await
        .context("seed accounts")?;
    client
        .execute(
            "INSERT INTO locked_resource (id, value) SELECT g, 0 FROM generate_series(1, 10) g",
            &[],
        )
        .await
        .context("seed locked_resource")?;
    // ~12 md5 repetitions ≈ 384 payload bytes per row.
    client
        .execute(
            "INSERT INTO staging.large_scan (id, payload)
             SELECT g, repeat(md5(g::text), 12) FROM generate_series(1, $1::int) g",
            &[&i32::try_from(config.large_scan_rows).context("large_scan_rows exceeds i32")?],
        )
        .await
        .context("seed staging.large_scan")?;
    seed_filler(client, config).await?;
    client
        .batch_execute("VACUUM ANALYZE")
        .await
        .context("analyze after seeding")?;
    println!(
        "seed: core + {} filler tables, {} filler indexes, {} large_scan rows",
        config.filler_tables, config.filler_indexes, config.large_scan_rows
    );
    Ok(())
}

/// Small tables and extra indexes that saturate the top-500 object caps.
async fn seed_filler(client: &Client, config: &Config) -> Result<()> {
    for table in 0..config.filler_tables {
        client
            .batch_execute(&format!(
                "CREATE TABLE filler_{table} (id int PRIMARY KEY, v text);
                 INSERT INTO filler_{table} VALUES ({table}, 'seed-{table}');"
            ))
            .await
            .with_context(|| format!("create filler_{table}"))?;
    }
    // Primary keys already contribute one index per filler table; spread the
    // remainder as secondary indexes over the first tables.
    let secondary = config.filler_indexes.saturating_sub(config.filler_tables);
    for index in 0..secondary.min(config.filler_tables) {
        client
            .execute(
                &format!("CREATE INDEX filler_{index}_v_idx ON filler_{index} (v)"),
                &[],
            )
            .await
            .with_context(|| format!("index filler_{index}"))?;
    }
    Ok(())
}
