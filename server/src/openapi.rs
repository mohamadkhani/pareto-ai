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
        crate::routes::auth::change_password,
        crate::routes::audit::list,
        crate::routes::users::list_users,
        crate::routes::users::create_user,
        crate::routes::users::update_user,
        crate::routes::users::create_key,
        crate::routes::users::list_keys,
        crate::routes::users::update_key,
        crate::routes::users::revoke_key,
        crate::routes::grants::list_grants,
        crate::routes::grants::create_grant,
        crate::routes::grants::delete_grant,
        crate::routes::money::list_budgets,
        crate::routes::money::set_budget,
        crate::routes::money::clear_budget,
        crate::routes::money::list_token_caps,
        crate::routes::money::set_token_cap,
        crate::routes::money::clear_token_cap,
        crate::routes::usage::usage_me,
        crate::routes::usage::usage_admin,
        crate::routes::usage::logs,
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
        crate::routes::chat::chat_completions,
        crate::routes::sync::trigger,
        crate::routes::sync::status,
        crate::routes::routing::list_combos,
        crate::routes::routing::create_combo,
        crate::routes::routing::update_combo,
        crate::routes::routing::delete_combo,
        crate::routes::routing::import_benchmarks,
        crate::routes::routing::set_quality_override,
        crate::routes::routing::clear_quality_override,
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
        crate::users::UserView,
        crate::users::UserInput,
        crate::users::UserPatch,
        crate::users::PasswordChange,
        crate::api_keys::ApiKeyView,
        crate::api_keys::ApiKeyInput,
        crate::api_keys::ApiKeyPatch,
        crate::routes::users::ApiKeyCreated,
        crate::access::GrantView,
        crate::access::GrantInput,
        crate::money::BudgetView,
        crate::money::TokenCapView,
        crate::routes::money::BudgetInput,
        crate::routes::money::TokenCapInput,
        crate::reports::UsageTotals,
        crate::reports::UsageGroup,
        crate::reports::UsageDay,
        crate::reports::MyUsageReport,
        crate::reports::UsageReport,
        crate::reports::RequestLogEntry,
        crate::reports::LogCursor,
        crate::reports::LogListResponse,
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
        crate::routes::chat::ChatCompletionRequest,
        crate::routes::chat::ChatCompletionResponse,
        crate::routes::chat::ChatUsage,
        crate::combos::ComboInput,
        crate::combos::ComboView,
        crate::combos::MemberInput,
        crate::combos::MemberView,
        crate::routes::routing::ImportOutcomeView,
        crate::routes::routing::QualityOverrideInput,
        crate::routes::routing::QualityOverrideView,
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
    fn api_doc_covers_the_whole_surface() {
        let doc = ApiDoc::build("test");
        let paths: Vec<&str> = doc.paths.paths.keys().map(String::as_str).collect();
        for expected in [
            "/health",
            "/openapi.json",
            "/api/auth/login",
            "/api/auth/logout",
            "/api/auth/me",
            "/api/auth/change-password",
            "/api/users",
            "/api/users/{id}",
            "/api/users/{id}/keys",
            "/api/keys/{id}",
            "/api/grants",
            "/api/grants/{id}",
            "/api/users/{id}/budgets",
            "/api/users/{id}/budgets/{period}",
            "/api/users/{id}/token-caps",
            "/api/users/{id}/token-caps/{model_id}/{period}",
            "/api/usage/me",
            "/api/usage",
            "/api/logs",
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
            "/v1/chat/completions",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }

        // The SSE body is documented as text/event-stream (M3 deliverable).
        let json = serde_json::to_value(&doc).expect("document serializes");
        let ok = &json["paths"]["/v1/chat/completions"]["post"]["responses"]["200"];
        assert!(
            ok["content"].get("text/event-stream").is_some(),
            "SSE content type documented: {ok}"
        );
    }
}
