use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::handlers::v1::version,
        crate::handlers::v1::sections,
        crate::handlers::v1::segments,
        crate::handlers::v1::section_data,
        crate::handlers::v1::sections_batch,
        crate::handlers::v1::section_diff,
        crate::handlers::v1::sections_batch_diff,
        crate::overview::handlers::overview,
        crate::overview::handlers::events,
        crate::overview::handlers::health,
        crate::ui::handlers::heatmap,
        crate::ui::handlers::catalog,
        crate::ui::handlers::summary,
        crate::handlers::anomalies::anomalies,
        crate::handlers::incidents::incidents,
    ),
    components(schemas(crate::api_error::ApiError))
)]
struct ApiDoc;

pub(crate) fn document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
