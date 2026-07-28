//! HTTP adapters for stable UI metadata.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use super::catalog::ProjectionCatalog;
use super::data::view_summary;
use super::heatmap::{HeatmapError, HeatmapRequest, heatmap as build_heatmap};
use crate::AppState;
use crate::params::{QueryParams, parse_i64};
use crate::problem::{
    ApiProblem, ExpectedValue, LimitResource, QueryConstraint, QueryParameter, count_u64,
};

/// Maximum serialized projection catalog response.
const MAX_CATALOG_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_HEATMAP_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_HEATMAP_SPAN_US: i64 = 24 * 60 * 60 * 1_000_000;
const DEFAULT_HEATMAP_BUCKETS: usize = 56;
const MAX_HEATMAP_BUCKETS: usize = 256;
const DEFAULT_HEATMAP_TOP: usize = 8;
const MAX_HEATMAP_TOP: usize = 64;
const CATALOG_PARAMS: &[QueryParameter] = &[];
const SUMMARY_PARAMS: &[QueryParameter] = &[QueryParameter::At];
const HEATMAP_PARAMS: &[QueryParameter] = &[
    QueryParameter::View,
    QueryParameter::Metric,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Buckets,
    QueryParameter::Top,
];

/// `GET /v1/views/summary?at=<us>` — exact indexed populations.
pub(crate) async fn summary(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<axum::Json<super::data::ViewSummaryResponse>, ApiProblem> {
    let params = QueryParams::parse(raw.as_deref(), SUMMARY_PARAMS)?;
    let at_us = parse_i64(&params, QueryParameter::At)?;
    let (snapshot, descriptor_view) = state.overview_request_view();
    let live = Arc::clone(descriptor_view.live());
    let response = tokio::task::spawn_blocking(move || view_summary(&snapshot, &live, at_us))
        .await
        .map_err(|join| {
            let problem = ApiProblem::internal_error();
            tracing::error!(
                event = "api_ui_summary_worker_failed",
                request_id = problem.request_id(),
                error = ?join,
                "UI summary worker failed"
            );
            problem
        })?
        .map_err(|read| {
            let problem = ApiProblem::store_read_failed();
            tracing::error!(
                event = "api_ui_summary_read_failed",
                request_id = problem.request_id(),
                error = %read,
                at_us,
                "UI summary OVF read failed"
            );
            problem
        })?;
    Ok(axum::Json(response))
}

/// `GET /v1/timeline/heatmap` — bounded web-index entity-series merge.
pub(crate) async fn heatmap(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Response<Body>, ApiProblem> {
    let params = QueryParams::parse(raw.as_deref(), HEATMAP_PARAMS)?;
    let from_us = parse_i64(&params, QueryParameter::From)?;
    let to_us = parse_i64(&params, QueryParameter::To)?;
    let span = to_us
        .checked_sub(from_us)
        .filter(|span| *span > 0)
        .ok_or_else(|| ApiProblem::invalid_query_constraint(QueryConstraint::FromBeforeTo))?;
    if span > MAX_HEATMAP_SPAN_US {
        return Err(ApiProblem::query_shape_limit_exceeded(
            LimitResource::QuerySpanUs,
            u64::try_from(MAX_HEATMAP_SPAN_US).expect("positive constant"),
            u64::try_from(span).ok(),
        ));
    }
    let view_name = required_projection_code(&params, QueryParameter::View)?;
    let view = kronika_analytics::web_projection::web_view_by_name(view_name).ok_or_else(|| {
        ApiProblem::invalid_query_parameter(QueryParameter::View, ExpectedValue::ProjectionCode)
    })?;
    let metric_name = required_projection_code(&params, QueryParameter::Metric)?;
    let metric = view
        .metrics
        .iter()
        .find(|metric| metric.name == metric_name)
        .ok_or_else(|| {
            ApiProblem::invalid_query_parameter(
                QueryParameter::Metric,
                ExpectedValue::ProjectionCode,
            )
        })?;
    let buckets = bounded_count(
        &params,
        QueryParameter::Buckets,
        DEFAULT_HEATMAP_BUCKETS,
        MAX_HEATMAP_BUCKETS,
    )?;
    let top = bounded_count(
        &params,
        QueryParameter::Top,
        DEFAULT_HEATMAP_TOP,
        MAX_HEATMAP_TOP,
    )?;
    let (snapshot, descriptor_view) = state.overview_request_view();
    let live = Arc::clone(descriptor_view.live());
    let response = tokio::task::spawn_blocking(move || {
        build_heatmap(
            &snapshot,
            &live,
            HeatmapRequest {
                view,
                metric,
                from_us,
                to_us,
                bucket_count: buckets,
                top,
            },
        )
    })
    .await
    .map_err(|join| {
        let problem = ApiProblem::internal_error();
        tracing::error!(
            event = "api_ui_heatmap_worker_failed",
            request_id = problem.request_id(),
            error = ?join,
            "UI heatmap worker failed"
        );
        problem
    })?
    .map_err(|error| heatmap_problem(&error))?;
    let body = serde_json::to_vec(&response).map_err(|error| {
        let problem = ApiProblem::internal_error();
        tracing::error!(
            event = "api_ui_heatmap_serialize_failed",
            request_id = problem.request_id(),
            error = %error,
            "UI heatmap serialization failed"
        );
        problem
    })?;
    if body.len() > MAX_HEATMAP_RESPONSE_BYTES {
        return Err(ApiProblem::query_limit_exceeded(
            LimitResource::Bytes,
            count_u64(MAX_HEATMAP_RESPONSE_BYTES),
            Some(count_u64(body.len())),
        ));
    }
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

fn required_projection_code(
    params: &QueryParams,
    parameter: QueryParameter,
) -> Result<&str, ApiProblem> {
    params
        .get(parameter)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiProblem::missing_query_parameter(parameter))
}

fn bounded_count(
    params: &QueryParams,
    parameter: QueryParameter,
    default: usize,
    maximum: usize,
) -> Result<usize, ApiProblem> {
    params.get(parameter).map_or(Ok(default), |raw| {
        raw.parse::<usize>()
            .ok()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or_else(|| {
                ApiProblem::invalid_query_parameter(parameter, ExpectedValue::PositiveInteger)
            })
    })
}

fn heatmap_problem(error: &HeatmapError) -> ApiProblem {
    match error {
        HeatmapError::Read(read) => {
            let problem = ApiProblem::store_read_failed();
            tracing::error!(
                event = "api_ui_heatmap_read_failed",
                request_id = problem.request_id(),
                error = %read,
                "UI heatmap OVF read failed"
            );
            problem
        }
        HeatmapError::TooManySegments => ApiProblem::query_shape_limit_exceeded(
            LimitResource::SelectedSegments,
            count_u64(crate::overview::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS),
            None,
        ),
        HeatmapError::TooManyCandidates => {
            ApiProblem::query_shape_limit_exceeded(LimitResource::Rows, 16_384, None)
        }
        HeatmapError::Arithmetic => ApiProblem::internal_error(),
    }
}

/// `GET /v1/ui/catalog` — stable UI projections.
pub(crate) async fn catalog(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response<Body>, ApiProblem> {
    QueryParams::parse(raw.as_deref(), CATALOG_PARAMS)?;
    let snapshot = state.snapshot();
    let observed = tokio::task::spawn_blocking(move || observed_type_ids(&snapshot))
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
                "UI catalog metadata read failed"
            );
            problem
        })?;

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
) -> Result<BTreeSet<u32>, kronika_reader::ReadError> {
    let units = snapshot.units();
    let mut observed = BTreeSet::new();
    for unit_idx in 0..units.len() {
        let Some(catalog) = snapshot.unit_catalog(unit_idx)? else {
            continue;
        };
        observed.extend(catalog.entries.iter().map(|entry| entry.type_id));
    }
    Ok(observed)
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
