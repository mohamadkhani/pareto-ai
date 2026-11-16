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
        crate::routes::providers::list,
        crate::routes::providers::create,
        crate::routes::providers::get,
        crate::routes::providers::update,
        crate::routes::providers::delete,
        crate::routes::providers::list_credentials,
        crate::routes::providers::create_credential,
        crate::routes::providers::get_credential,
        crate::routes::providers::update_credential,
        crate::routes::providers::delete_credential,
        crate::routes::providers::test_credential,
        crate::routes::catalog::list_models,
        crate::routes::catalog::create_model,
        crate::routes::catalog::get_model,
        crate::routes::catalog::update_model,
        crate::routes::catalog::delete_model,
        crate::routes::catalog::list_offerings,
        crate::routes::catalog::create_offering,
        crate::routes::catalog::update_offering,
        crate::routes::catalog::delete_offering,
        crate::routes::catalog::override_price,
        crate::routes::catalog::get_price,
        crate::routes::catalog::v1_models,
        crate::routes::sync::trigger,
        crate::routes::sync::status,
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
        crate::catalog::ProviderView,
        crate::catalog::ProviderInput,
        crate::catalog::ProviderPatch,
        crate::catalog::CredentialView,
        crate::catalog::CredentialInput,
        crate::catalog::CredentialPatch,
        crate::catalog::CredentialTestView,
        crate::catalog::ModelView,
        crate::catalog::ModelInput,
        crate::catalog::ModelPatch,
        crate::catalog::OfferingView,
        crate::catalog::OfferingInput,
        crate::catalog::OfferingPatch,
        crate::catalog::PriceView,
        crate::catalog::PriceInput,
        crate::catalog::SyncOutcomeView,
        crate::catalog::SyncRunView,
        crate::catalog::SyncStatusView,
        crate::catalog::V1Model,
        crate::catalog::V1ModelList,
        crate::catalog::V1Modalities,
        crate::catalog::V1Price,
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
    fn api_doc_covers_the_m1_and_m2_surface() {
        let doc = ApiDoc::build("test");
        let paths: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
        for expected in [
            "/health",
            "/openapi.json",
            "/api/auth/login",
            "/api/auth/logout",
            "/api/auth/me",
            "/api/audit",
            "/api/providers",
            "/api/providers/{id}",
            "/api/providers/{id}/credentials",
            "/api/credentials/{id}",
            "/api/credentials/{id}/test",
            "/api/models",
            "/api/models/{id}",
            "/api/offerings",
            "/api/offerings/{id}",
            "/api/offerings/{id}/price",
            "/api/sync/openrouter",
            "/api/sync/openrouter/status",
            "/v1/models",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }
    }
}
