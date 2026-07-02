//! Steps for features/smoke.feature.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};

use crate::BddWorld;
use crate::steps::docstring;

/// For every cluster in the matrix, run the docstring SQL and assert the
/// returned integer equals the cluster's declared major.
///
/// The SQL must return exactly one row and one column of integer type, e.g.
/// `SELECT current_setting('server_version_num')::int / 10000`.
#[then("each cluster's declared major matches the result of:")]
async fn smoke_version_num(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for cluster in world.clusters {
        let dsn = cluster.conn_string();
        let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
            .await
            .with_context(|| format!("postgres {}: connect for smoke check", cluster.major()))?;
        let driver = tokio::spawn(async move { drop(connection.await) });
        let result: Result<()> = async {
            let row = client
                .query_one(sql, &[])
                .await
                .with_context(|| format!("postgres {}: run smoke SQL: {sql}", cluster.major()))?;
            let returned: i64 = row.try_get(0).with_context(|| {
                format!(
                    "postgres {}: smoke SQL result is not an integer",
                    cluster.major()
                )
            })?;
            let declared = i64::from(cluster.major());
            anyhow::ensure!(
                returned == declared,
                "postgres {}: server_version_num / 10000 = {returned}, expected {declared}",
                cluster.major()
            );
            Ok(())
        }
        .await;
        driver.abort();
        result?;
    }
    Ok(())
}
