//! Admin read access to the audit log.
//!
//! `GET /api/audit` is admin-only and paginates newest-first with an
//! exclusive `(before, before_id)` cursor. Reading the log is itself audited
//! (`audit.read`), so access to the trail is visible in the trail.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{self, ClientIp, RequireAdmin, TS_FMT};
use crate::error::{self, ApiError, ErrorBody};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    /// Maximum entries to return (1–200). Defaults to 50.
    pub limit: Option<i64>,
    /// Exclusive cursor timestamp (RFC-3339, any offset; normalized to UTC).
    pub before: Option<String>,
    /// Exclusive entry id breaking ties at the `before` timestamp.
    pub before_id: Option<i64>,
}

/// One audit log entry.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: i64,
    pub actor_user_id: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    /// JSON object with action-specific detail, when the action recorded any.
    pub detail_json: Option<String>,
    pub ip: Option<String>,
    pub occurred_at: String,
}

/// The exclusive cursor position of the last entry of a page.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditCursor {
    /// The last entry's `occurred_at`, RFC-3339 UTC with milliseconds.
    pub before: String,
    /// The last entry's `id`, breaking ties with `before` at one millisecond.
    pub before_id: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditListResponse {
    pub entries: Vec<AuditEntry>,
    /// Cursor for the next (older) page: send its `before` and `before_id`
    /// back as the query parameters of the same name. Present only when the
    /// page is full (the next page may still be empty).
    pub next_cursor: Option<AuditCursor>,
}

/// Lists audit entries, newest first. Admin only.
///
/// Pagination is an exclusive cursor on `(occurred_at, id)`: `before` is an
/// RFC-3339 timestamp (any offset, normalized to UTC) and `before_id` is an
/// entry id that breaks ties between entries sharing one millisecond. The
/// response's `next_cursor` carries both fields of the last entry on a full
/// page; send them back as the `before` and `before_id` query parameters.
/// `before` on its own means "strictly older than this timestamp". A
/// hand-crafted `before` with sub-millisecond precision is truncated to
/// milliseconds when the cursor is normalized; API-generated cursors always
/// carry `%.3f` and are unaffected.
#[utoipa::path(
    get,
    path = "/api/audit",
    tag = "admin",
    params(
        ("limit" = Option<i64>, Query, description = "Maximum entries to return (1-200). Default 50."),
        ("before" = Option<String>, Query, description = "RFC-3339 cursor timestamp; only strictly older entries are returned."),
        ("before_id" = Option<i64>, Query, description = "Cursor entry id from next_cursor; breaks ties with before.")
    ),
    responses(
        (status = 200, description = "audit entries, newest first", body = AuditListResponse),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    query: Result<Query<AuditQuery>, QueryRejection>,
) -> Result<Json<AuditListResponse>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        // The rejection can quote the rejected value, so the caller gets a
        // generic message and the detail is only logged.
        tracing::debug!(error = %e, "query string rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let (before, before_id) = match (query.before, query.before_id) {
        (Some(raw), before_id) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(&raw).map_err(|_| {
                ApiError::bad_request("before must be an ISO-8601 timestamp")
                    .with_request_id(rid.clone())
            })?;
            // `parse_from_rfc3339` keeps the wire offset; the stored
            // `occurred_at` values are UTC, so normalize before comparing.
            (Some(parsed.to_utc().format(TS_FMT).to_string()), before_id)
        }
        (None, Some(_)) => {
            return Err(
                ApiError::bad_request("before_id requires before").with_request_id(rid.clone())
            );
        }
        (None, None) => (None, None),
    };

    let detail =
        serde_json::json!({ "limit": limit, "before": before, "before_id": before_id }).to_string();
    auth::audit(
        &state.pool,
        Some(&admin.user_id),
        "audit.read",
        Some("audit_log"),
        None,
        Some(&detail),
        Some(&ip.0),
    )
    .await
    .map_err(|e| ApiError::internal("auditing the audit read", e).with_request_id(rid.clone()))?;

    // The cursor is the composite `(occurred_at, id)`: comparing on the
    // timestamp alone would permanently exclude the same-millisecond rows
    // that share it with the last entry of a page.
    let entries = sqlx::query_as::<_, AuditEntry>(
        "SELECT id, actor_user_id, action, target_type, target_id, detail_json, ip, occurred_at \
         FROM audit_log \
         WHERE (? IS NULL OR occurred_at < ? \
                OR (? IS NOT NULL AND occurred_at = ? AND id < ?)) \
         ORDER BY occurred_at DESC, id DESC \
         LIMIT ?",
    )
    .bind(before.as_deref())
    .bind(before.as_deref())
    .bind(before_id)
    .bind(before.as_deref())
    .bind(before_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::internal("reading audit log", e).with_request_id(rid))?;

    let next_cursor = match (entries.len() as i64 == limit, entries.last()) {
        (true, Some(last)) => Some(AuditCursor {
            before: last.occurred_at.clone(),
            before_id: last.id,
        }),
        _ => None,
    };

    Ok(Json(AuditListResponse {
        entries,
        next_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::{Method, StatusCode};
    use serde_json::Value;

    /// Rewrites every `occurred_at` to one shared millisecond so every page
    /// boundary falls inside a tie group — the exact condition the composite
    /// cursor must survive.
    async fn tie_timestamps(app: &TestApp) {
        sqlx::query("UPDATE audit_log SET occurred_at = '2026-01-01T00:00:00.000Z'")
            .execute(&app.state.pool)
            .await
            .expect("tie timestamps");
    }

    fn cursor_of(page: &Value) -> Option<(String, i64)> {
        let cursor = &page["next_cursor"];
        match cursor.is_null() {
            true => None,
            false => Some((
                cursor["before"]
                    .as_str()
                    .expect("cursor before")
                    .to_string(),
                cursor["before_id"].as_i64().expect("cursor before_id"),
            )),
        }
    }

    #[tokio::test]
    async fn audit_is_admin_only() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();

        let anonymous = send(
            router.clone(),
            api_request(Method::GET, "/api/audit", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let (user_cookie, _) = test_support::login(&router, "user@example.com").await;
        let user = send(
            router.clone(),
            api_request(Method::GET, "/api/audit", None, Some(&user_cookie), None),
        )
        .await;
        assert_eq!(user.status, StatusCode::FORBIDDEN);
        assert_eq!(user.body["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn audit_lists_newest_first_and_paginates_without_gaps() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();

        // Generates a few audit rows (two logins and one failed read).
        test_support::login(&router, "user@example.com").await;
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let blocked = send(
            router.clone(),
            api_request(Method::GET, "/api/audit", None, None, None),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::UNAUTHORIZED);
        tie_timestamps(&app).await;

        // Page through with limit=2, following the composite cursor. Every
        // row shares one millisecond, so every boundary is a tie boundary.
        // Each read adds an `audit.read` row with the current timestamp (above
        // the cursor), so those rows are filtered out of the comparison below.
        let mut collected: Vec<i64> = Vec::new();
        let mut before: Option<String> = None;
        let mut before_id: Option<i64> = None;
        loop {
            let mut uri = "/api/audit?limit=2".to_string();
            if let Some(cursor) = &before {
                uri.push_str(&format!(
                    "&before={cursor}&before_id={}",
                    before_id.expect("before_id travels with before")
                ));
            }
            let page = send(
                router.clone(),
                api_request(Method::GET, &uri, None, Some(&admin_cookie), None),
            )
            .await;
            assert_eq!(page.status, StatusCode::OK, "page body: {}", page.body);

            let entries = page.body["entries"].as_array().expect("entries").clone();
            assert!(entries.len() <= 2);
            for entry in &entries {
                if entry["action"] != "audit.read" {
                    collected.push(entry["id"].as_i64().expect("numeric id"));
                }
            }
            match cursor_of(&page.body) {
                Some((next_before, next_id)) => {
                    before = Some(next_before);
                    before_id = Some(next_id);
                }
                None => break,
            }
        }

        let expected: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM audit_log WHERE action != 'audit.read' ORDER BY occurred_at DESC, id DESC",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("all ids");
        assert!(expected.len() >= 2, "the setup must write audit rows");
        assert_eq!(collected, expected, "every row exactly once, newest first");
    }

    #[tokio::test]
    async fn tied_timestamps_are_not_dropped_at_page_boundaries() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        // Three rows sharing one millisecond, distinct ids: with limit=2 the
        // page boundary falls inside the tie group and the third row must
        // still be returned (the timestamp-only cursor lost it forever).
        let mut tied_ids: Vec<i64> = Vec::new();
        for _ in 0..3 {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO audit_log (action, occurred_at) \
                 VALUES ('test.tie', '2030-01-01T00:00:00.000Z') RETURNING id",
            )
            .fetch_one(&app.state.pool)
            .await
            .expect("insert tied row");
            tied_ids.push(id);
        }

        let page1 = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?limit=2",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(page1.status, StatusCode::OK, "page body: {}", page1.body);
        let entries1 = page1.body["entries"].as_array().expect("entries");
        assert_eq!(
            entries1[0]["id"].as_i64(),
            Some(tied_ids[2]),
            "the two newest tied rows fill page one"
        );
        assert_eq!(entries1[1]["id"].as_i64(), Some(tied_ids[1]));
        assert_eq!(
            cursor_of(&page1.body),
            Some(("2030-01-01T00:00:00.000Z".to_string(), tied_ids[1])),
            "the cursor carries the composite (occurred_at, id) position"
        );

        let page2 = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!(
                    "/api/audit?limit=2&before=2030-01-01T00:00:00.000Z&before_id={}",
                    tied_ids[1]
                ),
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(page2.status, StatusCode::OK, "page body: {}", page2.body);
        let entries2 = page2.body["entries"].as_array().expect("entries");
        assert_eq!(
            entries2[0]["id"].as_i64(),
            Some(tied_ids[0]),
            "the third tied row must survive the page boundary"
        );
    }

    #[tokio::test]
    async fn tied_timestamps_paginate_across_many_pages_without_gaps() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        // Seven rows sharing one millisecond: with limit=2 that is four
        // pages, every page boundary falling inside the one tie group.
        let mut expected: Vec<i64> = Vec::new();
        for _ in 0..7 {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO audit_log (action, occurred_at) \
                 VALUES ('test.tied', '2030-01-01T00:00:00.000Z') RETURNING id",
            )
            .fetch_one(&app.state.pool)
            .await
            .expect("insert tied row");
            expected.push(id);
        }
        // Newest first: the tie group orders by id DESC.
        expected.reverse();

        // Each read adds an `audit.read` row below the cursor, so those rows
        // are filtered out of the comparison.
        let mut collected: Vec<i64> = Vec::new();
        let mut pages = 0;
        let mut before: Option<String> = None;
        let mut before_id: Option<i64> = None;
        loop {
            let mut uri = "/api/audit?limit=2".to_string();
            if let Some(cursor) = &before {
                uri.push_str(&format!(
                    "&before={cursor}&before_id={}",
                    before_id.expect("before_id travels with before")
                ));
            }
            let page = send(
                router.clone(),
                api_request(Method::GET, &uri, None, Some(&admin_cookie), None),
            )
            .await;
            assert_eq!(page.status, StatusCode::OK, "page body: {}", page.body);
            pages += 1;

            for entry in page.body["entries"].as_array().expect("entries") {
                if entry["action"] == "test.tied" {
                    collected.push(entry["id"].as_i64().expect("numeric id"));
                }
            }
            match cursor_of(&page.body) {
                Some((next_before, next_id)) => {
                    before = Some(next_before);
                    before_id = Some(next_id);
                }
                None => break,
            }
        }

        assert!(
            pages >= 4,
            "seven rows in pages of two need at least four pages, got {pages}"
        );
        assert_eq!(
            collected, expected,
            "every tied row exactly once, newest first across all pages"
        );
    }

    #[tokio::test]
    async fn before_cursor_accepts_and_normalizes_non_utc_offsets() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        for (action, at) in [
            ("test.tz-old", "2026-01-01T09:00:00.000Z"),
            ("test.tz-at", "2026-01-01T10:00:00.000Z"),
            ("test.tz-new", "2026-01-01T11:00:00.000Z"),
        ] {
            sqlx::query("INSERT INTO audit_log (action, occurred_at) VALUES (?, ?)")
                .bind(action)
                .bind(at)
                .execute(&app.state.pool)
                .await
                .expect("insert row");
        }

        // 2026-01-01T12:00:00+02:00 is 10:00:00Z; only strictly older rows
        // belong to the page. Mis-normalizing the offset to 12:00Z would
        // wrongly include the rows at 10:00Z and 11:00Z.
        let page = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?limit=200&before=2026-01-01T12:00:00%2B02:00",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(page.status, StatusCode::OK, "page body: {}", page.body);
        let actions: Vec<&str> = page.body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|e| e["action"].as_str())
            .filter(|a| a.starts_with("test.tz"))
            .collect();
        assert_eq!(
            actions,
            ["test.tz-old"],
            "the cursor is exclusive at 10:00Z"
        );
    }

    #[tokio::test]
    async fn reading_the_audit_log_is_audited() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?limit=5",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);

        let reads: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'audit.read'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit.read count");
        assert_eq!(reads, 1);
    }

    #[tokio::test]
    async fn invalid_before_cursor_is_rejected() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?before=not-a-timestamp",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn before_id_without_before_is_rejected() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?before_id=7",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_request");
        // The message is this validation's own: a generic `QueryRejection`
        // carries the same code, so the code alone does not pin the path.
        assert_eq!(
            response.body["error"]["message"], "before_id requires before",
            "body: {}",
            response.body
        );
    }
}
