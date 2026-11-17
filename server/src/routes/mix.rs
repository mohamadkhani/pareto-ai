//! Admin plane for token-mix profiles and the pricing setting (decision D34).
//!
//! A profile is the client's token shape; the mix price derived from it is
//! the routing price metric and is shown on the Models page. Billing never
//! uses it (`usage_events.cost_usd` keeps D22 component pricing).
//!
//! Every mutation is audited inside its transaction (`mix_profile.create`,
//! `mix_profile.update`, `mix_profile.delete`, `settings.pricing`). Built-in
//! presets can be cloned but not edited or deleted; editing one returns
//! `400`. The `/api` CSRF guard covers the state-changing calls.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::Actor;
use crate::error::{self, ApiError, ErrorBody};
use crate::mix::{self, MixProfile, MixProfileInput, MixProfileRef, MixProfiles};
use crate::routes::bad_body;

/// `PUT /api/settings/pricing` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PricingSettingsInput {
    /// A mix profile id or name; `null`/absent clears the setting so the
    /// built-in `code-agent` applies again.
    pub default_mix_profile: Option<String>,
}

/// The pricing settings as returned by `GET`/`PUT /api/settings/pricing`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PricingSettingsView {
    /// The stored selector (canonical profile id), or `null`.
    pub default_mix_profile: Option<String>,
    /// The profile routing actually uses: environment > setting > built-in.
    pub effective_profile: MixProfileRef,
    /// `GATEWAY_TOKEN_MIX_PROFILE` when set; it wins over the setting. Exposed
    /// in the same canonical form as `default_mix_profile`: a profile id when
    /// the selector resolves, otherwise the raw string (a visible
    /// misconfiguration).
    pub env_override: Option<String>,
}

/// Lists every token-mix profile, built-ins first. Admin only.
#[utoipa::path(
    get,
    path = "/api/mix-profiles",
    tag = "mix",
    responses(
        (status = 200, description = "all mix profiles", body = [MixProfile]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_profiles(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<Vec<MixProfile>>, ApiError> {
    let rid = error::request_id(&headers);
    let profiles = MixProfiles::list(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(profiles))
}

/// Creates a token-mix profile. Admin only, audited as
/// `mix_profile.create`.
#[utoipa::path(
    post,
    path = "/api/mix-profiles",
    tag = "mix",
    request_body = MixProfileInput,
    responses(
        (status = 201, description = "profile created", body = MixProfile),
        (status = 400, description = "invalid shares, kind, or name", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody)
    )
)]
pub async fn create_profile(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<MixProfileInput>, JsonRejection>,
) -> Result<(StatusCode, Json<MixProfile>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "mix profile create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let profile = MixProfiles::create(&state.pool, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    state.mix_profiles.invalidate();
    Ok((StatusCode::CREATED, Json(profile)))
}

/// Replaces a token-mix profile. Admin only, audited as
/// `mix_profile.update`. Built-ins are immutable (`400`).
#[utoipa::path(
    put,
    path = "/api/mix-profiles/{id}",
    tag = "mix",
    params(("id" = String, Path, description = "mix profile id")),
    request_body = MixProfileInput,
    responses(
        (status = 200, description = "profile updated", body = MixProfile),
        (status = 400, description = "invalid input or a built-in profile", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such profile", body = ErrorBody)
    )
)]
pub async fn update_profile(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<MixProfileInput>, JsonRejection>,
) -> Result<Json<MixProfile>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "mix profile update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let profile = MixProfiles::update(&state.pool, &actor, &id, &input)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    state.mix_profiles.invalidate();
    match profile {
        Some(profile) => Ok(Json(profile)),
        None => Err(ApiError::not_found("mix profile").with_request_id(rid)),
    }
}

/// Deletes a token-mix profile. Admin only, audited as
/// `mix_profile.delete`. Built-ins are protected (`400`).
#[utoipa::path(
    delete,
    path = "/api/mix-profiles/{id}",
    tag = "mix",
    params(("id" = String, Path, description = "mix profile id")),
    responses(
        (status = 204, description = "profile deleted"),
        (status = 400, description = "a built-in profile cannot be deleted", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such profile", body = ErrorBody)
    )
)]
pub async fn delete_profile(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let deleted = MixProfiles::delete(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    state.mix_profiles.invalidate();
    match deleted {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("mix profile").with_request_id(rid)),
    }
}

/// Reads the pricing setting and the effective profile. Admin only.
#[utoipa::path(
    get,
    path = "/api/settings/pricing",
    tag = "mix",
    responses(
        (status = 200, description = "the pricing setting", body = PricingSettingsView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn get_pricing_settings(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<PricingSettingsView>, ApiError> {
    let rid = error::request_id(&headers);
    pricing_settings_view(&state)
        .await
        .map_err(|e| e.into_api_error(rid))
        .map(Json)
}

/// Sets or clears the default token-mix profile. Admin only, audited as
/// `settings.pricing`. `null`/blank clears it (the built-in `code-agent`
/// applies again); an unknown selector is `404`.
#[utoipa::path(
    put,
    path = "/api/settings/pricing",
    tag = "mix",
    request_body = PricingSettingsInput,
    responses(
        (status = 200, description = "the updated pricing setting", body = PricingSettingsView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such mix profile", body = ErrorBody)
    )
)]
pub async fn put_pricing_settings(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<PricingSettingsInput>, JsonRejection>,
) -> Result<Json<PricingSettingsView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "pricing settings", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    MixProfiles::set_stored_default(&state.pool, &actor, input.default_mix_profile.as_deref())
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    // The setting selects the default profile: refresh the cached table so the
    // next request resolves the new default without a restart.
    state.mix_profiles.invalidate();
    pricing_settings_view(&state)
        .await
        .map_err(|e| e.into_api_error(rid))
        .map(Json)
}

async fn pricing_settings_view(state: &AppState) -> Result<PricingSettingsView, mix::MixError> {
    let stored = MixProfiles::stored_default(&state.pool).await?;
    let effective = state
        .mix_profiles
        .resolve_default(&state.pool, state.config.token_mix_profile.as_deref())
        .await?;
    // Expose the same canonical form as `default_mix_profile`: a profile id,
    // not the raw environment string. An unknown selector (which
    // `resolve_default` ignores) is echoed as-is so the operator can see the
    // misconfiguration instead of a silent null.
    let env_override = match state.config.token_mix_profile.as_deref() {
        Some(selector) => match state
            .mix_profiles
            .resolve_selector(&state.pool, selector)
            .await
        {
            Ok(Some(profile)) => Some(profile.id),
            _ => Some(selector.to_string()),
        },
        None => None,
    };
    Ok(PricingSettingsView {
        default_mix_profile: stored,
        effective_profile: MixProfileRef::from(&effective),
        env_override,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::{TestApp, api_request, login, send};
    use axum::http::Method;

    fn profile_body(name: &str, input: f64, output: f64, cache: f64) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "kind": "custom",
            "input_share": input,
            "output_share": output,
            "cache_read_share": cache,
            "cache_write_share": 0,
            "reasoning_share": 0,
        })
    }

    #[tokio::test]
    async fn mix_profile_mutations_require_admin_session_and_csrf() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        app.create_user("user@example.test", Role::User, true).await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;
        let (user_cookie, user_csrf) = login(&app.router(), "user@example.test").await;

        let anonymous = send(
            app.router(),
            api_request(Method::GET, "/api/mix-profiles", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let forbidden = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(profile_body("x", 1.0, 1.0, 0.0)),
                Some(&user_cookie),
                Some(&user_csrf),
            ),
        )
        .await;
        assert_eq!(
            forbidden.status,
            StatusCode::FORBIDDEN,
            "{}",
            forbidden.body
        );

        let no_csrf = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(profile_body("x", 1.0, 1.0, 0.0)),
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        assert_eq!(no_csrf.body["error"]["code"], "csrf_failed");

        let created = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(profile_body("x", 1.0, 1.0, 0.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["name"], "x");
    }

    #[tokio::test]
    async fn pricing_settings_canonicalise_the_env_override_and_reserve_preset() {
        let app = TestApp::with_config(|config| {
            config.token_mix_profile = Some("chat".to_string());
        })
        .await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        let settings = send(
            app.router(),
            api_request(
                Method::GET,
                "/api/settings/pricing",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
        // The environment selector is exposed in the same canonical form as
        // the stored setting: a profile id, not the raw env string.
        assert_eq!(settings.body["env_override"], "mix-chat");
        assert_eq!(settings.body["effective_profile"]["profile_name"], "chat");

        // Operator-created profiles may not claim the built-in `preset` kind.
        let preset = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(serde_json::json!({
                    "name": "fake-preset",
                    "kind": "preset",
                    "input_share": 1.0,
                    "output_share": 1.0,
                    "cache_read_share": 0.0,
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(preset.status, StatusCode::BAD_REQUEST, "{}", preset.body);
        assert!(
            preset.body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("preset")),
            "{}",
            preset.body
        );
    }

    #[tokio::test]
    async fn profile_crud_and_the_pricing_setting_round_trip_through_the_api() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        // The migration seeds the four built-ins.
        let listed = send(
            app.router(),
            api_request(Method::GET, "/api/mix-profiles", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert_eq!(listed.body.as_array().expect("array").len(), 4);
        assert_eq!(listed.body[0]["is_builtin"], true);

        let created = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(profile_body("team", 5.0, 1.0, 94.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"].as_str().expect("id").to_string();

        let updated = send(
            app.router(),
            api_request(
                Method::PUT,
                &format!("/api/mix-profiles/{id}"),
                Some(profile_body("team-v2", 6.0, 1.0, 93.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["name"], "team-v2");
        assert_eq!(updated.body["is_builtin"], false);

        // A built-in edit is a clear 400, not a silent no-op.
        let builtin = send(
            app.router(),
            api_request(
                Method::PUT,
                "/api/mix-profiles/mix-chat",
                Some(profile_body("chat", 70.0, 30.0, 0.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(builtin.status, StatusCode::BAD_REQUEST, "{}", builtin.body);
        assert!(
            builtin.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("built-in")
        );

        // The pricing setting defaults to code-agent, changes to chat, and
        // the stored selector is the canonical profile id.
        let defaults = send(
            app.router(),
            api_request(
                Method::GET,
                "/api/settings/pricing",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(defaults.status, StatusCode::OK, "{}", defaults.body);
        assert!(defaults.body["default_mix_profile"].is_null());
        assert_eq!(
            defaults.body["effective_profile"]["profile_name"],
            "code-agent"
        );

        let stored = send(
            app.router(),
            api_request(
                Method::PUT,
                "/api/settings/pricing",
                Some(serde_json::json!({"default_mix_profile": "chat"})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
        assert_eq!(stored.body["default_mix_profile"], "mix-chat");
        assert_eq!(stored.body["effective_profile"]["profile_name"], "chat");

        let cleared = send(
            app.router(),
            api_request(
                Method::PUT,
                "/api/settings/pricing",
                Some(serde_json::json!({"default_mix_profile": null})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        assert!(cleared.body["default_mix_profile"].is_null());
        assert_eq!(
            cleared.body["effective_profile"]["profile_name"],
            "code-agent"
        );

        let deleted = send(
            app.router(),
            api_request(
                Method::DELETE,
                &format!("/api/mix-profiles/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        let missing = send(
            app.router(),
            api_request(
                Method::DELETE,
                &format!("/api/mix-profiles/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.body["error"]["code"], "not_found");
    }

    /// The hot path caches the profile table (one load, then hash lookups).
    /// An admin edit must be visible on the next request without a restart:
    /// every profile and pricing mutation invalidates the cache.
    #[tokio::test]
    async fn profile_edits_take_effect_on_the_next_request() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        // Prime the cache through the same resolution the chat path uses.
        let default = app
            .state
            .mix_profiles
            .resolve_default(&app.state.pool, None)
            .await
            .expect("default");
        assert_eq!(default.name, crate::mix::CODE_AGENT);

        let created = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/mix-profiles",
                Some(profile_body("team", 5.0, 1.0, 94.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"].as_str().expect("id").to_string();

        let stored = send(
            app.router(),
            api_request(
                Method::PUT,
                "/api/settings/pricing",
                Some(serde_json::json!({"default_mix_profile": id})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
        let selected = app
            .state
            .mix_profiles
            .resolve_default(&app.state.pool, None)
            .await
            .expect("default");
        assert_eq!(selected.name, "team");

        // Edit the profile: the next resolution sees the new name and shares.
        let updated = send(
            app.router(),
            api_request(
                Method::PUT,
                &format!("/api/mix-profiles/{id}"),
                Some(profile_body("team-renamed", 10.0, 2.0, 88.0)),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        let refreshed = app
            .state
            .mix_profiles
            .resolve_default(&app.state.pool, None)
            .await
            .expect("default");
        assert_eq!(refreshed.id, selected.id);
        assert_eq!(refreshed.name, "team-renamed");
        assert_eq!(refreshed.input_share, 10.0);

        // Deleting the selected profile falls back to code-agent immediately.
        let deleted = send(
            app.router(),
            api_request(
                Method::DELETE,
                &format!("/api/mix-profiles/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        let fallback = app
            .state
            .mix_profiles
            .resolve_default(&app.state.pool, None)
            .await
            .expect("default");
        assert_eq!(fallback.name, crate::mix::CODE_AGENT);
    }
}
