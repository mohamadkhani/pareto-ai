//! OpenAPI document for the HTTP surface.
//!
//! Served as `GET /openapi.json` so clients and the dashboard can generate
//! typed calls from one source of truth. Every handler that appears here keeps
//! its `#[utoipa::path]` attribute in sync with the route table.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "pareto-ai gateway",
        description = "OpenAI-compatible gateway with price/speed/quality-driven routing.",
    ),
    paths(crate::routes::health::get, crate::routes::openapi::get),
    components(schemas(crate::routes::health::Health))
)]
pub struct ApiDoc;

impl ApiDoc {
    /// The document with the running version filled in, so the served spec
    /// always matches the binary that serves it.
    pub fn build(version: &str) -> utoipa::openapi::OpenApi {
        let mut doc = Self::openapi();
        doc.info.version = version.to_string();
        doc
    }
}
