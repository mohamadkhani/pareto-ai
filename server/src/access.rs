//! Access control (M4, decision D9): the `grants` matrix and its evaluation.
//!
//! A grant binds one subject (`user` or `api_key`) to one resource (`model`,
//! `offering`, or `credential`) with `allow` or `deny`. The verdict for one
//! subject on one request is:
//!
//! 1. any `deny` row matching one of the request's resources denies
//!    (`deny` beats `allow`),
//! 2. else any `allow` row matching one of them allows,
//! 3. else the subject's role default: **allow for `admin`, deny for `user`**.
//!
//! The effective verdict is the intersection over subjects: the user must be
//! allowed **and**, when the request authenticated with an API key, the key
//! must be allowed too. A key of a `user` therefore needs explicit grants on
//! both subjects; an admin's requests are allowed by default and only a
//! `deny` row can restrict them.
//!
//! Every mutation writes its `audit_log` row in the same transaction
//! (`grant.create` / `grant.delete`).

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use utoipa::ToSchema;

use crate::auth::Role;
use crate::catalog::{self, Actor};

/// One grant as the API returns it.
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct GrantView {
    pub id: String,
    /// `user` or `api_key`.
    pub subject_type: String,
    pub subject_id: String,
    /// `model`, `offering`, or `credential`.
    pub resource_type: String,
    pub resource_id: String,
    /// `allow` or `deny`.
    pub mode: String,
    pub created_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GrantInput {
    /// `user` or `api_key`.
    pub subject_type: String,
    pub subject_id: String,
    /// `model`, `offering`, or `credential`.
    pub resource_type: String,
    pub resource_id: String,
    /// `allow` or `deny`.
    pub mode: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("a grant for this subject and resource already exists")]
    Conflict,
}

impl AccessError {
    /// Maps the domain error onto the uniform API error shape.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            AccessError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            AccessError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            AccessError::Conflict => {
                ApiError::conflict("a grant for this subject and resource already exists")
                    .with_request_id(rid)
            }
            other => ApiError::internal("grants", other).with_request_id(rid),
        }
    }
}

/// Lists grants, optionally filtered by subject.
pub async fn list_grants(
    pool: &SqlitePool,
    subject_type: Option<&str>,
    subject_id: Option<&str>,
) -> Result<Vec<GrantView>, AccessError> {
    Ok(sqlx::query_as::<_, GrantView>(
        "SELECT id, subject_type, subject_id, resource_type, resource_id, mode, created_by, created_at \
         FROM grants \
         WHERE (?1 IS NULL OR subject_type = ?1) \
           AND (?2 IS NULL OR subject_id = ?2) \
         ORDER BY created_at, id",
    )
    .bind(subject_type)
    .bind(subject_id)
    .fetch_all(pool)
    .await?)
}

/// Creates one grant, audited as `grant.create`.
///
/// Failure mode: [`AccessError::Invalid`] for unknown enum values,
/// [`AccessError::NotFound`] for an unknown subject or resource,
/// [`AccessError::Conflict`] when the (subject, resource) pair already has a
/// grant (update it by deleting and re-creating).
pub async fn create_grant(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    input: &GrantInput,
) -> Result<GrantView, AccessError> {
    let subject_type = match input.subject_type.as_str() {
        "user" | "api_key" => input.subject_type.clone(),
        _ => {
            return Err(AccessError::Invalid(
                "subject_type must be `user` or `api_key`".to_string(),
            ));
        }
    };
    let resource_type = match input.resource_type.as_str() {
        "model" | "offering" | "credential" => input.resource_type.clone(),
        _ => {
            return Err(AccessError::Invalid(
                "resource_type must be `model`, `offering`, or `credential`".to_string(),
            ));
        }
    };
    let mode = match input.mode.as_str() {
        "allow" | "deny" => input.mode.clone(),
        _ => {
            return Err(AccessError::Invalid(
                "mode must be `allow` or `deny`".to_string(),
            ));
        }
    };

    let now = catalog::now_ts();
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;

    // Write-first (review m3): the INSERT is the transaction's first
    // statement so it takes SQLite's write lock up front; the existence
    // checks run after it and roll the insert back when they fail.
    let result = sqlx::query(
        "INSERT INTO grants (id, subject_type, subject_id, resource_type, resource_id, mode, \
                             created_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&subject_type)
    .bind(&input.subject_id)
    .bind(&resource_type)
    .bind(&input.resource_id)
    .bind(&mode)
    .bind(actor.user_id)
    .bind(&now)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(ref db)) = result
        && db.is_unique_violation()
    {
        return Err(AccessError::Conflict);
    }
    result?;

    let subject_table = match subject_type.as_str() {
        "user" => "users",
        _ => "api_keys",
    };
    let subject: Option<i64> =
        sqlx::query_scalar(&format!("SELECT 1 FROM {subject_table} WHERE id = ?"))
            .bind(&input.subject_id)
            .fetch_optional(&mut *tx)
            .await?;
    if subject.is_none() {
        return Err(AccessError::NotFound("subject"));
    }
    let resource_table = match resource_type.as_str() {
        "model" => "models",
        "offering" => "offerings",
        _ => "provider_credentials",
    };
    let resource: Option<i64> =
        sqlx::query_scalar(&format!("SELECT 1 FROM {resource_table} WHERE id = ?"))
            .bind(&input.resource_id)
            .fetch_optional(&mut *tx)
            .await?;
    if resource.is_none() {
        return Err(AccessError::NotFound("resource"));
    }

    let detail = serde_json::json!({
        "subject_type": subject_type,
        "subject_id": input.subject_id,
        "resource_type": resource_type,
        "resource_id": input.resource_id,
        "mode": mode,
    });
    catalog::audit(&mut tx, actor, "grant.create", "grant", &id, &detail).await?;
    tx.commit().await?;

    let row = sqlx::query_as::<_, GrantView>(
        "SELECT id, subject_type, subject_id, resource_type, resource_id, mode, created_by, created_at \
         FROM grants WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Deletes one grant, audited as `grant.delete`. Returns whether it existed.
/// Write-first (review m3): the `DELETE ... RETURNING` is both the mutation
/// and the existence check.
pub async fn delete_grant(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    grant_id: &str,
) -> Result<bool, AccessError> {
    let mut tx = pool.begin().await?;
    let existing: Option<GrantView> = sqlx::query_as::<_, GrantView>(
        "DELETE FROM grants WHERE id = ? \
         RETURNING id, subject_type, subject_id, resource_type, resource_id, mode, created_by, \
                   created_at",
    )
    .bind(grant_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    let detail = serde_json::json!({
        "subject_type": existing.subject_type,
        "subject_id": existing.subject_id,
        "resource_type": existing.resource_type,
        "resource_id": existing.resource_id,
        "mode": existing.mode,
    });
    catalog::audit(&mut tx, actor, "grant.delete", "grant", grant_id, &detail).await?;
    tx.commit().await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct GrantRow {
    subject_type: String,
    subject_id: String,
    resource_type: String,
    resource_id: String,
    mode: String,
}

/// The grants of the user and (when present) the key, loaded once per
/// request and evaluated in memory.
#[derive(Debug, Clone, Default)]
pub struct Grants {
    rows: Vec<GrantRow>,
}

impl Grants {
    /// Loads every grant of `user_id` and of `api_key_id` (when given).
    /// Failure mode: [`sqlx::Error`].
    pub async fn load(
        pool: &SqlitePool,
        user_id: &str,
        api_key_id: Option<&str>,
    ) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query_as::<_, GrantRow>(
            "SELECT subject_type, subject_id, resource_type, resource_id, mode \
             FROM grants \
             WHERE (subject_type = 'user' AND subject_id = ?1) \
                OR (?2 IS NOT NULL AND subject_type = 'api_key' AND subject_id = ?2)",
        )
        .bind(user_id)
        .bind(api_key_id)
        .fetch_all(pool)
        .await?;
        Ok(Self { rows })
    }

    /// Whether the caller may use one candidate's resources.
    ///
    /// A key with no grants of its own inherits its owner's verdict (D59): the
    /// owner is the principal, so every key the account creates works without a
    /// separate grant set. A key with at least one grant is an explicit scope
    /// and still intersects with the owner's verdict — a key may narrow the
    /// owner's access but never widen it.
    ///
    /// `credential_id` is `None` when the candidate has no credential (the
    /// credential resource is then not evaluated; model and offering rows
    /// still apply).
    pub fn allows(
        &self,
        role: Role,
        user_id: &str,
        api_key_id: Option<&str>,
        model_id: &str,
        offering_id: &str,
        credential_id: Option<&str>,
    ) -> bool {
        let mut resources: Vec<(&str, &str)> = vec![("model", model_id), ("offering", offering_id)];
        if let Some(credential_id) = credential_id {
            resources.push(("credential", credential_id));
        }
        let user_ok = self.subject_verdict("user", user_id, role, &resources);
        let key_ok = match api_key_id {
            Some(key_id) if !self.subject_has_rows("api_key", key_id) => true,
            Some(key_id) => self.subject_verdict("api_key", key_id, role, &resources),
            None => true,
        };
        user_ok && key_ok
    }

    /// Whether this grant set holds at least one row for one subject.
    fn subject_has_rows(&self, subject_type: &str, subject_id: &str) -> bool {
        self.rows
            .iter()
            .any(|row| row.subject_type == subject_type && row.subject_id == subject_id)
    }

    /// One subject's verdict on the candidate's resources: `deny` beats
    /// `allow`, otherwise the role default (admin allow, user deny).
    fn subject_verdict(
        &self,
        subject_type: &str,
        subject_id: &str,
        role: Role,
        resources: &[(&str, &str)],
    ) -> bool {
        let mut explicitly_allowed = false;
        for row in &self.rows {
            if row.subject_type != subject_type || row.subject_id != subject_id {
                continue;
            }
            let matches = resources
                .iter()
                .any(|(kind, id)| row.resource_type == *kind && row.resource_id == *id);
            if !matches {
                continue;
            }
            if row.mode == "deny" {
                return false;
            }
            explicitly_allowed = true;
        }
        explicitly_allowed || role.is_admin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grants set built straight from rows, for matrix tests.
    fn grants(rows: &[(&str, &str, &str, &str, &str)]) -> Grants {
        Grants {
            rows: rows
                .iter()
                .map(
                    |(subject_type, subject_id, resource_type, resource_id, mode)| GrantRow {
                        subject_type: (*subject_type).to_string(),
                        subject_id: (*subject_id).to_string(),
                        resource_type: (*resource_type).to_string(),
                        resource_id: (*resource_id).to_string(),
                        mode: (*mode).to_string(),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn admin_default_allows_and_user_default_denies() {
        let empty = Grants::default();
        assert!(empty.allows(Role::Admin, "u", None, "m", "o", Some("c")));
        assert!(!empty.allows(Role::User, "u", None, "m", "o", Some("c")));
    }

    #[test]
    fn one_allow_row_grants_a_user_access() {
        let g = grants(&[("user", "u", "model", "m", "allow")]);
        assert!(g.allows(Role::User, "u", None, "m", "o", Some("c")));
        assert!(
            !g.allows(Role::User, "u", None, "other", "o", Some("c")),
            "the grant covers one model only"
        );
    }

    #[test]
    fn deny_beats_allow() {
        let g = grants(&[
            ("user", "u", "model", "m", "allow"),
            ("user", "u", "credential", "c", "deny"),
        ]);
        assert!(
            !g.allows(Role::User, "u", None, "m", "o", Some("c")),
            "a deny anywhere wins over an allow"
        );
        assert!(
            g.allows(Role::User, "u", None, "m", "o", Some("other")),
            "only the denied credential is blocked"
        );
    }

    #[test]
    fn key_without_grants_inherits_its_owner() {
        // Granting the account is enough: every key the user creates works
        // without a key-level grant set (D59).
        let g = grants(&[("user", "u", "model", "m", "allow")]);
        assert!(g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));
        assert!(
            !g.allows(Role::User, "u", Some("k"), "other", "o", Some("c")),
            "the key inherits the owner's limits, not more"
        );

        // Inheritance never widens the owner's verdict: the owner's deny still
        // blocks the key.
        let g = grants(&[
            ("user", "u", "model", "m", "allow"),
            ("user", "u", "credential", "c", "deny"),
        ]);
        assert!(!g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));

        // A user-role key of an account with no grants still denies.
        assert!(!Grants::default().allows(Role::User, "u", Some("k"), "m", "o", None));
    }

    #[test]
    fn user_and_key_grants_intersect_when_the_key_has_grants() {
        // Both subjects allow: effective allow.
        let g = grants(&[
            ("user", "u", "model", "m", "allow"),
            ("api_key", "k", "model", "m", "allow"),
        ]);
        assert!(g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));

        // The key's own grants do not cover this resource: the explicit scope
        // denies even though the owner allows.
        let g = grants(&[
            ("user", "u", "model", "m", "allow"),
            ("api_key", "k", "model", "other", "allow"),
        ]);
        assert!(!g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));

        // The key allows, the user does not: denied as well.
        let g = grants(&[("api_key", "k", "model", "m", "allow")]);
        assert!(!g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));

        // A key-level deny blocks even when the user is allowed.
        let g = grants(&[
            ("user", "u", "model", "m", "allow"),
            ("api_key", "k", "model", "m", "deny"),
        ]);
        assert!(!g.allows(Role::User, "u", Some("k"), "m", "o", Some("c")));
    }

    #[test]
    fn key_verdict_follows_the_owners_role_default() {
        // An admin's key with no grants inherits the admin default.
        let empty = Grants::default();
        assert!(empty.allows(Role::Admin, "u", Some("k"), "m", "o", None));

        // An explicit allow on the key is enough alongside the role default
        // of the user (admin).
        let g = grants(&[("api_key", "k", "offering", "o", "allow")]);
        assert!(g.allows(Role::Admin, "u", Some("k"), "m", "o", None));
    }

    #[test]
    fn resource_types_match_independently() {
        // An offering grant allows that offering regardless of the model and
        // credential rows (allow-by-any, deny-by-any).
        let g = grants(&[("user", "u", "offering", "o", "allow")]);
        assert!(g.allows(Role::User, "u", None, "m", "o", Some("c")));
        assert!(!g.allows(Role::User, "u", None, "m", "other", Some("c")));

        // Without a credential to check, credential rows cannot match and the
        // verdict falls back to the role default.
        let g = grants(&[("user", "u", "credential", "c", "deny")]);
        assert!(
            !g.allows(Role::User, "u", None, "m", "o", None),
            "user default deny"
        );
        assert!(
            g.allows(Role::Admin, "u", None, "m", "o", None),
            "admin default allow"
        );
    }

    async fn pool() -> (std::path::PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-grants-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open");
        (dir, pool)
    }

    async fn seed_user(pool: &SqlitePool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, ?, 'G', 'x', 'user', 1)",
        )
        .bind(&id)
        .bind(format!("{id}@example.com"))
        .execute(pool)
        .await
        .expect("insert user");
        id
    }

    async fn seed_model(pool: &SqlitePool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO models (id, canonical_name) VALUES (?, ?)")
            .bind(&id)
            .bind(format!("m-{id}"))
            .execute(pool)
            .await
            .expect("insert model");
        id
    }

    #[tokio::test]
    async fn grant_crud_validates_references_and_audits() {
        let (dir, pool) = pool().await;
        let admin = seed_user(&pool).await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let actor = Actor {
            user_id: &admin,
            ip: "127.0.0.1",
        };

        let grant = create_grant(
            &pool,
            &actor,
            &GrantInput {
                subject_type: "user".to_string(),
                subject_id: user.clone(),
                resource_type: "model".to_string(),
                resource_id: model.clone(),
                mode: "allow".to_string(),
            },
        )
        .await
        .expect("create grant");
        assert_eq!(grant.mode, "allow");
        assert_eq!(grant.created_by.as_deref(), Some(admin.as_str()));

        // The pair is unique; the same (subject, resource) cannot be granted
        // twice.
        let err = create_grant(
            &pool,
            &actor,
            &GrantInput {
                subject_type: "user".to_string(),
                subject_id: user.clone(),
                resource_type: "model".to_string(),
                resource_id: model.clone(),
                mode: "deny".to_string(),
            },
        )
        .await
        .expect_err("duplicate");
        assert!(matches!(err, AccessError::Conflict), "{err:?}");

        // Unknown subjects, resources, and enum values are refused.
        for bad in [
            GrantInput {
                subject_type: "robot".to_string(),
                subject_id: user.clone(),
                resource_type: "model".to_string(),
                resource_id: model.clone(),
                mode: "allow".to_string(),
            },
            GrantInput {
                subject_type: "user".to_string(),
                subject_id: "nobody".to_string(),
                resource_type: "model".to_string(),
                resource_id: model.clone(),
                mode: "allow".to_string(),
            },
            GrantInput {
                subject_type: "user".to_string(),
                subject_id: user.clone(),
                resource_type: "model".to_string(),
                resource_id: "nothing".to_string(),
                mode: "allow".to_string(),
            },
            GrantInput {
                subject_type: "user".to_string(),
                subject_id: user.clone(),
                resource_type: "model".to_string(),
                resource_id: model.clone(),
                mode: "maybe".to_string(),
            },
        ] {
            assert!(create_grant(&pool, &actor, &bad).await.is_err());
        }

        assert!(
            delete_grant(&pool, &actor, &grant.id)
                .await
                .expect("delete")
        );
        assert!(
            !delete_grant(&pool, &actor, &grant.id)
                .await
                .expect("second delete")
        );

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit");
        assert_eq!(actions, ["grant.create", "grant.delete"]);

        // Loaded evaluation matches the matrix: the deleted grant is gone.
        let loaded = Grants::load(&pool, &user, None).await.expect("load");
        assert!(!loaded.allows(Role::User, &user, None, &model, "o", None));
        std::fs::remove_dir_all(&dir).ok();
    }
}
