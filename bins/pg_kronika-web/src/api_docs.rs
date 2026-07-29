//! One registry for documented HTTP routes and their generated `OpenAPI` document.

use axum::Router;
use utoipa::openapi::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::AppState;

#[derive(utoipa::OpenApi)]
struct ApiDoc;

fn configured() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(<ApiDoc as utoipa::OpenApi>::openapi())
        .routes(routes!(crate::handlers::v1::version))
        .routes(routes!(crate::handlers::v1::sections))
        .routes(routes!(crate::handlers::v1::segments))
        .routes(routes!(crate::handlers::v1::section_data))
        .routes(routes!(crate::handlers::v1::sections_batch))
        .routes(routes!(crate::handlers::v1::section_diff))
        .routes(routes!(crate::handlers::v1::sections_batch_diff))
        .routes(routes!(crate::overview::handlers::overview))
        .routes(routes!(crate::overview::handlers::events))
        .routes(routes!(crate::overview::handlers::health))
        .routes(routes!(crate::ui::handlers::heatmap))
        .routes(routes!(crate::ui::handlers::catalog))
        .routes(routes!(crate::ui::handlers::summary))
        .routes(routes!(crate::handlers::anomalies::anomalies))
        .routes(routes!(crate::handlers::incidents::incidents))
}

pub(crate) fn router_and_document() -> (Router<AppState>, OpenApi) {
    configured().split_for_parts()
}

pub(crate) fn document() -> OpenApi {
    let (_, document) = router_and_document();
    document
}
