//! The `pg_kronika-web` binary: serve the JSON API over a local store directory.
//!
//! The router, state, and handlers live in [`pg_kronika_web`]; this binary
//! wires them to a socket and runs the refresh loop and its timer policy.
#![allow(
    unused_crate_dependencies,
    reason = "this thin binary consumes the pg_kronika_web library and a runtime; the package's other dependencies belong to the library and its tests"
)]

use std::sync::Arc;
use std::time::Duration;

use kronika_reader::LocalDirSnapshot;
use pg_kronika_web::{AppState, app};

/// How often the refresh task re-scans the store directory.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Environment variable naming the store directory to serve.
const DIR_ENV: &str = "KRONIKA_WEB_DIR";

/// Environment variable overriding the listen address (see [`DEFAULT_ADDR`]).
const ADDR_ENV: &str = "KRONIKA_WEB_ADDR";

/// Default listen address: loopback only. The v1 API has no auth, so it stays
/// off-network unless [`ADDR_ENV`] opts in.
const DEFAULT_ADDR: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args_os().nth(1).map_or_else(
        || std::env::var_os(DIR_ENV).map(std::path::PathBuf::from),
        |arg| Some(std::path::PathBuf::from(arg)),
    );
    let Some(dir) = dir else {
        eprintln!("usage: pg_kronika-web <dir>   (or set {DIR_ENV})");
        std::process::exit(2);
    };

    let snapshot = LocalDirSnapshot::open(&dir)?;
    let state = AppState::new(snapshot);

    // The refresh task owns its own mutable snapshot and publishes a fresh
    // clone after each incremental scan. It logs any change in the warning and
    // damaged-region counts, so serving partial data over corruption leaves an
    // operator-visible trace instead of being silently dropped.
    let shared = Arc::clone(&state.snapshot);
    tokio::spawn(async move {
        let mut snap = shared.load().as_ref().clone();
        let mut health = (snap.warnings().len(), snap.damages().len());
        if health != (0, 0) {
            eprintln!(
                "store opened with {} warning(s), {} damaged region(s)",
                health.0, health.1
            );
        }
        loop {
            tokio::time::sleep(REFRESH_INTERVAL).await;
            match snap.refresh_incremental() {
                Ok(()) => {
                    let current = (snap.warnings().len(), snap.damages().len());
                    if current != health {
                        eprintln!(
                            "store health changed: {} warning(s), {} damaged region(s)",
                            current.0, current.1
                        );
                        health = current;
                    }
                    shared.store(Arc::new(snap.clone()));
                }
                Err(err) => eprintln!("refresh failed: {err}"),
            }
        }
    });

    let addr = std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let listener = tokio::net::TcpListener::bind(addr.as_str()).await?;

    // Drain in-flight requests on SIGTERM/SIGINT rather than dropping them, so a
    // rolling restart does not cut off readers mid-response.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv() => {}
            }
        })
        .await?;
    Ok(())
}
