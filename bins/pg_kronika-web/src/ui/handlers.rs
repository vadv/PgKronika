//! HTTP adapters for stable UI metadata.

use std::collections::BTreeSet;

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use super::catalog::ProjectionCatalog;
use crate::AppState;
use crate::params::{QueryParams, parse_u64};
use crate::problem::{ApiProblem, QueryParameter};

/// Maximum serialized projection catalog response.
const MAX_CATALOG_RESPONSE_BYTES: usize = 512 * 1024;
const CATALOG_PARAMS: &[QueryParameter] = &[QueryParameter::Source];

/// `GET /v1/ui/catalog?source=<id>` — source-aware stable UI projections.
pub(crate) async fn catalog(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response<Body>, ApiProblem> {
    let params = QueryParams::parse(raw.as_deref(), CATALOG_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let snapshot = state.snapshot();
    let observed = tokio::task::spawn_blocking(move || observed_type_ids(&snapshot, source))
        .await
        .map_err(|join| {
            let problem = ApiProblem::internal_error();
            tracing::error!(
                event = "api_ui_catalog_worker_failed",
                request_id = problem.request_id(),
                error = ?join,
                "UI catalog worker failed"
            );
            problem
        })?
        .map_err(|read| {
            let problem = ApiProblem::store_read_failed();
            tracing::error!(
                event = "api_ui_catalog_read_failed",
                request_id = problem.request_id(),
                error = %read,
                source,
                "UI catalog metadata read failed"
            );
            problem
        })?
        .ok_or_else(|| ApiProblem::unknown_source(source))?;

    let catalog = ProjectionCatalog::for_type_ids(&observed);
    let body = serde_json::to_vec(&catalog).map_err(|error| {
        let problem = ApiProblem::internal_error();
        tracing::error!(
            event = "api_ui_catalog_serialize_failed",
            request_id = problem.request_id(),
            error = %error,
            "UI catalog serialization failed"
        );
        problem
    })?;
    if body.len() > MAX_CATALOG_RESPONSE_BYTES {
        let problem = ApiProblem::internal_error();
        tracing::error!(
            event = "api_ui_catalog_response_oversized",
            request_id = problem.request_id(),
            observed_bytes = body.len(),
            limit_bytes = MAX_CATALOG_RESPONSE_BYTES,
            "static UI catalog exceeded its response contract"
        );
        return Err(problem);
    }

    let etag = catalog_etag(&body);
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|candidate| candidate == etag)
    {
        return Ok(response(StatusCode::NOT_MODIFIED, etag, Body::empty()));
    }
    Ok(response(StatusCode::OK, etag, Body::from(body)))
}

fn observed_type_ids(
    snapshot: &kronika_reader::LocalDirSnapshot,
    source: u64,
) -> Result<Option<BTreeSet<u32>>, kronika_reader::ReadError> {
    let units = snapshot.units();
    let mut source_found = false;
    let mut observed = BTreeSet::new();
    for (unit_idx, unit) in units.iter().enumerate() {
        if unit.source_id != source {
            continue;
        }
        source_found = true;
        let Some(catalog) = snapshot.unit_catalog(unit_idx)? else {
            continue;
        };
        observed.extend(
            catalog
                .entries
                .iter()
                .filter(|entry| entry.rows != 0)
                .map(|entry| entry.type_id),
        );
    }
    Ok(source_found.then_some(observed))
}

fn catalog_etag(body: &[u8]) -> HeaderValue {
    let digest = Sha256::digest(body);
    let token = URL_SAFE_NO_PAD.encode(digest);
    HeaderValue::from_str(&format!("\"sha256-{token}\""))
        .expect("base64url SHA-256 is always a valid quoted ETag")
}

fn response(status: StatusCode, etag: HeaderValue, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(header::ETAG, etag);
    if status == StatusCode::OK {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    response
}
