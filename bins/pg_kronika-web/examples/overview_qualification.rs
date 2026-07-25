//! Exact-head Overview M6 qualification entry point.

// This target deliberately delegates to the feature-gated library module. The
// package-level dependency lint checks every target independently.
use arc_swap as _;
use axum as _;
use base64 as _;
use bytes as _;
use criterion as _;
use form_urlencoded as _;
use http_body_util as _;
use kronika_analytics as _;
use kronika_format as _;
use kronika_reader as _;
use kronika_registry as _;
use kronika_writer as _;
use metrics as _;
use metrics_exporter_prometheus as _;
use mimalloc as _;
use rust_embed as _;
use rustix as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use subtle as _;
use tempfile as _;
use tokio as _;
use tower as _;
use tower_http as _;
use tracing as _;
use tracing_subscriber as _;

fn main() {
    pg_kronika_web::qualification::run_cli();
}
