//! Embedded static assets with a single-page-app fallback.
//!
//! Files under `static/` are compiled into the release binary (debug reads them
//! from disk, so UI edits need no rebuild). A request that names an embedded
//! file gets it with its content type; any other path that is not an API path
//! falls back to `index.html` so the UI can route client-side.
#![allow(
    clippy::same_name_method,
    reason = "rust-embed's derive generates an inherent get alongside the RustEmbed trait method"
)]

use axum::Json;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::{EmbeddedFile, RustEmbed};
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

/// Serve the embedded asset for `uri`, or the SPA shell.
///
/// An unknown path under `/v1/` returns a JSON 404 rather than the shell, so a
/// mistyped API route is not masked by the UI.
pub(crate) async fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/v1/") {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
    }
    let asset_path = path.trim_start_matches('/');
    Assets::get(asset_path).map_or_else(serve_index, |file| serve_asset(asset_path, file))
}

/// Serve one embedded file: its guessed content type, and a cache policy that
/// pins hashed assets forever but keeps `index.html` revalidating.
fn serve_asset(path: &str, file: EmbeddedFile) -> Response {
    let cache = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

/// The SPA shell, or a plain 404 if the build embedded no `index.html`.
fn serve_index() -> Response {
    Assets::get("index.html").map_or_else(
        || (StatusCode::NOT_FOUND, "index.html not embedded").into_response(),
        |file| serve_asset("index.html", file),
    )
}
