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
    paths(
        crate::routes::health::get,
        crate::routes::openapi::get,
        crate::routes::auth::login,
        crate::routes::auth::logout,
        crate::routes::auth::me,
        crate::routes::audit::list,
    ),
    components(schemas(
        crate::routes::health::Health,
        crate::routes::auth::LoginRequest,
        crate::routes::auth::UserSummary,
        crate::routes::auth::LoginResponse,
        crate::routes::auth::MeResponse,
        crate::routes::audit::AuditEntry,
        crate::routes::audit::AuditCursor,
        crate::routes::audit::AuditListResponse,
        crate::error::ErrorBody,
        crate::error::ErrorDetail,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_doc_covers_the_m1_surface() {
        let doc = ApiDoc::build("test");
        let paths: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
        for expected in [
            "/health",
            "/openapi.json",
            "/api/auth/login",
            "/api/auth/logout",
            "/api/auth/me",
            "/api/audit",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }
    }
}
