//! Combo models (M5): operator-defined routing policies, addressable as the
//! model id `combo:<name>` in `POST /v1/chat/completions` (spec M5 acceptance
//! 5 — the model-id namespace was chosen over a separate `/v1/chat/combos`
//! endpoint so an OpenAI SDK addresses a combo exactly like a model).
//!
//! A combo expands to its member **offerings** (the routable unit: provider +
//! model + credential), then the router applies the combo's strategy
//! (decision D6):
//!
//! * `ordered` — members in their stored order, a fixed fallback list;
//! * `weighted` — [`crate::routing::weighted_order`] with the combo's weights
//!   (defaults or a preset) and per-member weight multipliers;
//! * `round_robin` — a stable rotation over the members, persisted in
//!   `combo_models.rr_cursor` and advanced once per request that actually
//!   dispatches upstream (a request refused before dispatch does not consume
//!   the rotation — review nit).
//!
//! An unknown or disabled combo answers `404 model_not_found`, exactly like an
//! unknown model. Enabled combos are advertised by `GET /v1/models` as
//! `combo:<name>` entries ([`list_v1_entries`]) so clients can discover them.

use sqlx::SqlitePool;

use crate::catalog::{self, Actor};
use crate::routing::{RoutingError, Strategy, Weights};

/// The model-id namespace: `combo:<name>`.
pub const NAMESPACE: &str = "combo:";

/// Combo storage/validation failures. Failure mode: `400`-worthy
/// [`ComboError::Invalid`] on bad input, `500`-worthy storage errors.
#[derive(Debug, thiserror::Error)]
pub enum ComboError {
    #[error("combo configuration is invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error("combo storage failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("combo audit failed: {0}")]
    Audit(#[from] catalog::CatalogError),
}

impl ComboError {
    /// The uniform API error for this failure.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        match self {
            Self::Invalid(message) => {
                crate::error::ApiError::bad_request(message).with_request_id(rid)
            }
            Self::Routing(e) => {
                crate::error::ApiError::bad_request(e.to_string()).with_request_id(rid)
            }
            Self::Db(e) => {
                crate::error::ApiError::internal("combo storage", e).with_request_id(rid)
            }
            Self::Audit(e) => e.into_api_error(rid),
        }
    }
}

/// One member of a combo: an offering with an optional per-member weight
/// (the `weighted` strategy multiplies its score with it).
#[derive(Debug, Clone, PartialEq)]
pub struct ComboMember {
    pub offering_id: String,
    pub weight: f64,
}

/// A loaded combo row.
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub id: String,
    pub name: String,
    pub strategy: Strategy,
    pub weights: Weights,
    /// The named preset behind `weights`, when `weights_json` named one.
    pub preset: Option<&'static str>,
    pub members: Vec<ComboMember>,
    pub enabled: bool,
    /// The persisted round-robin rotation counter.
    pub rr_cursor: i64,
}

/// One member as created.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct MemberInput {
    pub offering_id: String,
    /// Score multiplier (`weighted` strategy); defaults to 1.0.
    pub weight: Option<f64>,
}

/// A combo as created (or replaced, on update).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct ComboInput {
    /// The name behind the `combo:<name>` namespace (immutable after create).
    pub name: String,
    /// `ordered`, `weighted`, or `round_robin`.
    pub strategy: String,
    /// Raw `weights_json`: null, `{"preset": "..."}`, or a partial object.
    pub weights: Option<serde_json::Value>,
    pub members: Vec<MemberInput>,
    pub enabled: Option<bool>,
}

/// One combo member as returned by the admin API.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MemberView {
    pub offering_id: String,
    pub weight: f64,
}

/// A combo as returned by the admin API.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ComboView {
    pub id: String,
    /// The `combo:<name>` name.
    pub name: String,
    pub strategy: String,
    /// The effective scoring weights.
    pub weights: serde_json::Value,
    /// The named preset behind `weights`, when one.
    pub preset: Option<String>,
    pub members: Vec<MemberView>,
    pub enabled: bool,
    /// The persisted round-robin rotation counter.
    pub rr_cursor: i64,
}

impl From<&Combo> for ComboView {
    fn from(combo: &Combo) -> Self {
        Self {
            id: combo.id.clone(),
            name: combo.name.clone(),
            strategy: combo.strategy.as_str().to_string(),
            weights: combo.weights.to_json(),
            preset: combo.preset.map(str::to_owned),
            members: combo
                .members
                .iter()
                .map(|member| MemberView {
                    offering_id: member.offering_id.clone(),
                    weight: member.weight,
                })
                .collect(),
            enabled: combo.enabled,
            rr_cursor: combo.rr_cursor,
        }
    }
}

/// Parses `members_json`: `[{"offering_id": "...", "weight": 1.0}, ...]`.
/// Failure mode: [`ComboError::Invalid`].
fn parse_members(raw: &str) -> Result<Vec<ComboMember>, ComboError> {
    let values: Vec<serde_json::Value> = serde_json::from_str(raw)
        .map_err(|e| ComboError::Invalid(format!("members_json is not a list: {e}")))?;
    let mut members = Vec::with_capacity(values.len());
    for value in &values {
        let offering_id = value
            .get("offering_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ComboError::Invalid("member offering_id must be a string".to_string())
            })?;
        let weight = match value.get("weight") {
            None | Some(serde_json::Value::Null) => 1.0,
            Some(weight) => weight.as_f64().ok_or_else(|| {
                ComboError::Invalid(format!("member {offering_id} weight must be a number"))
            })?,
        };
        members.push(ComboMember {
            offering_id,
            weight,
        });
    }
    validate_members(&members)?;
    Ok(members)
}

/// Members must be a non-empty list of unique offerings with positive finite
/// weights. Failure mode: [`ComboError::Invalid`].
fn validate_members(members: &[ComboMember]) -> Result<(), ComboError> {
    if members.is_empty() {
        return Err(ComboError::Invalid("members must not be empty".to_string()));
    }
    for member in members {
        if member.offering_id.trim().is_empty() {
            return Err(ComboError::Invalid(
                "member offering_id must not be empty".to_string(),
            ));
        }
        if !member.weight.is_finite() || member.weight <= 0.0 {
            return Err(ComboError::Invalid(format!(
                "member {} weight must be a positive number",
                member.offering_id
            )));
        }
    }
    for (index, member) in members.iter().enumerate() {
        if members[..index]
            .iter()
            .any(|earlier| earlier.offering_id == member.offering_id)
        {
            return Err(ComboError::Invalid(format!(
                "member {} is listed twice",
                member.offering_id
            )));
        }
    }
    Ok(())
}

/// Serializes members back to the stored `members_json` shape.
fn members_json(members: &[ComboMember]) -> String {
    serde_json::Value::Array(
        members
            .iter()
            .map(|member| {
                serde_json::json!({
                    "offering_id": member.offering_id,
                    "weight": member.weight,
                })
            })
            .collect(),
    )
    .to_string()
}

/// The validated shape of a combo input, ready to persist.
struct Validated {
    name: String,
    strategy: Strategy,
    weights: Weights,
    preset: Option<&'static str>,
    weights_raw: Option<String>,
    members: Vec<ComboMember>,
    enabled: bool,
}

/// Validates a combo input (name, strategy, weights, members, and that every
/// member offering exists). Failure mode: [`ComboError::Invalid`] naming the
/// offending field.
async fn validate(pool: &SqlitePool, input: &ComboInput) -> Result<Validated, ComboError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ComboError::Invalid("name must not be empty".to_string()));
    }
    if name.contains(':') {
        return Err(ComboError::Invalid(
            "name must not contain ':' (the combo: namespace separator)".to_string(),
        ));
    }
    let strategy = Strategy::parse(&input.strategy)?;
    let weights_raw = input.weights.as_ref().map(|value| value.to_string());
    let (weights, preset) = Weights::from_json(weights_raw.as_deref())?;
    let members: Vec<ComboMember> = input
        .members
        .iter()
        .map(|member| ComboMember {
            offering_id: member.offering_id.clone(),
            weight: member.weight.unwrap_or(1.0),
        })
        .collect();
    validate_members(&members)?;
    for member in &members {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM offerings WHERE id = ?")
            .bind(&member.offering_id)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Err(ComboError::Invalid(format!(
                "member offering {} does not exist",
                member.offering_id
            )));
        }
    }
    Ok(Validated {
        name: name.to_string(),
        strategy,
        weights,
        preset,
        weights_raw,
        members,
        enabled: input.enabled.unwrap_or(true),
    })
}

/// Validates and creates a combo (audited as `combo.create`). Failure mode:
/// [`ComboError::Invalid`] for bad input (unknown strategy, bad weights,
/// duplicate/unknown offerings, taken name), [`ComboError::Db`] on storage
/// errors.
pub async fn create(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    input: &ComboInput,
) -> Result<Combo, ComboError> {
    let validated = validate(pool, input).await?;
    let mut tx = pool.begin().await?;
    let taken: Option<String> = sqlx::query_scalar("SELECT id FROM combo_models WHERE name = ?")
        .bind(&validated.name)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(ComboError::Invalid(format!(
            "combo name {:?} is already in use",
            validated.name
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = catalog::now_ts();
    sqlx::query(
        "INSERT INTO combo_models \
           (id, name, strategy, weights_json, members_json, enabled, rr_cursor, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(&validated.name)
    .bind(validated.strategy.as_str())
    .bind(&validated.weights_raw)
    .bind(members_json(&validated.members))
    .bind(i64::from(validated.enabled))
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "combo.create",
        "combo",
        &id,
        &serde_json::json!({
            "name": validated.name,
            "strategy": validated.strategy.as_str(),
            "members": validated.members.len(),
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(Combo {
        id,
        name: validated.name,
        strategy: validated.strategy,
        weights: validated.weights,
        preset: validated.preset,
        members: validated.members,
        enabled: validated.enabled,
        rr_cursor: 0,
    })
}

type ComboRow = (String, String, String, Option<String>, String, i64, i64);

const SELECT_COMBO_COLUMNS: &str =
    "id, name, strategy, weights_json, members_json, enabled, rr_cursor";

/// Parses one combo row (storage shape → [`Combo`]).
fn parse_row(row: ComboRow) -> Result<Combo, ComboError> {
    let (id, name, strategy, weights_json, members_json, enabled, rr_cursor) = row;
    let strategy = Strategy::parse(&strategy)?;
    let (weights, preset) = Weights::from_json(weights_json.as_deref())?;
    let members = parse_members(&members_json)?;
    Ok(Combo {
        id,
        name,
        strategy,
        weights,
        preset,
        members,
        enabled: enabled == 1,
        rr_cursor,
    })
}

/// Loads one combo by id. Failure mode: [`ComboError`].
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Combo>, ComboError> {
    let row = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(parse_row).transpose()
}

/// Lists every combo, by name. Failure mode: [`ComboError`].
pub async fn list(pool: &SqlitePool) -> Result<Vec<Combo>, ComboError> {
    let rows = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models ORDER BY name"
    ))
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(parse_row).collect()
}

/// Replaces a combo's strategy, weights, members, and enabled flag (its name
/// and rotation cursor are immutable). Audited as `combo.update`. `None` when
/// the id is unknown. Failure mode: [`ComboError`].
pub async fn update(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    id: &str,
    input: &ComboInput,
) -> Result<Option<Combo>, ComboError> {
    let Some(current) = get(pool, id).await? else {
        return Ok(None);
    };
    // The name is immutable: validate under the stored name.
    let merged = ComboInput {
        name: current.name.clone(),
        ..input.clone()
    };
    let validated = validate(pool, &merged).await?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE combo_models SET strategy = ?, weights_json = ?, members_json = ?, \
         enabled = ?, updated_at = ? WHERE id = ?",
    )
    .bind(validated.strategy.as_str())
    .bind(&validated.weights_raw)
    .bind(members_json(&validated.members))
    .bind(i64::from(validated.enabled))
    .bind(catalog::now_ts())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "combo.update",
        "combo",
        id,
        &serde_json::json!({
            "strategy": validated.strategy.as_str(),
            "members": validated.members.len(),
            "enabled": validated.enabled,
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(Some(Combo {
        id: current.id,
        name: current.name,
        strategy: validated.strategy,
        weights: validated.weights,
        preset: validated.preset,
        members: validated.members,
        enabled: validated.enabled,
        rr_cursor: current.rr_cursor,
    }))
}

/// Deletes a combo (audited as `combo.delete`). Returns whether it existed.
/// Failure mode: [`ComboError`].
pub async fn delete(pool: &SqlitePool, actor: &Actor<'_>, id: &str) -> Result<bool, ComboError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query("DELETE FROM combo_models WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() > 0 {
        catalog::audit(
            &mut tx,
            actor,
            "combo.delete",
            "combo",
            id,
            &serde_json::json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Loads a combo by its `combo:<name>` name (without the prefix), whether
/// enabled or not (the caller turns a disabled combo into `404
/// model_not_found`). Failure mode: [`ComboError`] on storage errors or a
/// corrupt row.
pub async fn load_by_name(pool: &SqlitePool, name: &str) -> Result<Option<Combo>, ComboError> {
    let row = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models WHERE name = ?"
    ))
    .bind(name)
    .fetch_optional(pool)
    .await?;
    row.map(parse_row).transpose()
}

/// Advances a combo's round-robin cursor and returns its **new** value (the
/// first call returns 1). Called once per request that actually dispatches
/// upstream: refusals before dispatch (budget, context, breaker) do not
/// consume the rotation. Failure mode: [`ComboError::Db`].
pub async fn advance_cursor(pool: &SqlitePool, combo_id: &str) -> Result<i64, ComboError> {
    let cursor: i64 = sqlx::query_scalar(
        "UPDATE combo_models SET rr_cursor = rr_cursor + 1, updated_at = ? \
         WHERE id = ? RETURNING rr_cursor",
    )
    .bind(catalog::now_ts())
    .bind(combo_id)
    .fetch_one(pool)
    .await?;
    Ok(cursor)
}

/// One advertised combo for `GET /v1/models` (review m4 — combos must be
/// discoverable, not a hidden namespace): the `combo:<name>` entry plus its
/// members' `(model_id, offering_id)` pairs for access filtering.
#[derive(Debug)]
pub struct ComboV1Entry {
    pub model: catalog::V1Model,
    /// `(model_id, offering_id)` of every member that still exists.
    pub members: Vec<(String, String)>,
}

/// The enabled combos as advertised by `GET /v1/models`: one `combo:<name>`
/// entry per enabled combo. Limits and price are null — they vary per member
/// and the chat pipeline checks them per candidate. Failure mode:
/// [`ComboError`].
pub async fn list_v1_entries(pool: &SqlitePool) -> Result<Vec<ComboV1Entry>, ComboError> {
    let mut entries = Vec::new();
    for combo in list(pool).await? {
        if !combo.enabled {
            continue;
        }
        let mut members = Vec::with_capacity(combo.members.len());
        for member in &combo.members {
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT id, model_id FROM offerings WHERE id = ?")
                    .bind(&member.offering_id)
                    .fetch_optional(pool)
                    .await?;
            if let Some((offering_id, model_id)) = row {
                members.push((model_id, offering_id));
            }
        }
        entries.push(ComboV1Entry {
            model: catalog::V1Model {
                id: format!("{NAMESPACE}{}", combo.name),
                name: combo.name.clone(),
                context_length: None,
                max_output_tokens: None,
                modalities: catalog::V1Modalities {
                    input: Vec::new(),
                    output: Vec::new(),
                },
                price: None,
            },
            members,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::catalog::{CredentialInput, ModelInput, OfferingInput, ProviderInput};
    use crate::test_support::TestApp;

    struct Seeded {
        app: TestApp,
        uid: String,
        offering_ids: Vec<String>,
    }

    async fn seeded() -> Seeded {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        let provider = catalog::create_provider(
            &app.state.pool,
            &actor,
            &ProviderInput {
                name: "p1".to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                kind: None,
                enabled: None,
            },
        )
        .await
        .expect("provider");
        catalog::create_credential(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &provider.id,
            &CredentialInput {
                label: "k".to_string(),
                secret: "sk-test".to_string(),
                headers: None,
                max_concurrency: None,
                balance_usd: None,
                balance_source: None,
                balance_low_threshold: None,
            },
        )
        .await
        .expect("credential");
        let model = catalog::create_model(
            &app.state.pool,
            &actor,
            &ModelInput {
                canonical_name: "m".to_string(),
                family: None,
                modalities_input: None,
                modalities_output: None,
                context_length: None,
                max_output_tokens: None,
                capabilities: None,
            },
        )
        .await
        .expect("model");
        let mut offering_ids = Vec::new();
        for i in 0..2 {
            let offering = catalog::create_offering(
                &app.state.pool,
                &app.state.secrets,
                &actor,
                &OfferingInput {
                    provider_id: provider.id.clone(),
                    model_id: model.id.clone(),
                    upstream_model_id: format!("up-{i}"),
                    context_override: None,
                    max_output_override: None,
                    enabled: None,
                    headers: None,
                },
            )
            .await
            .expect("offering");
            offering_ids.push(offering.id);
        }
        Seeded {
            app,
            uid,
            offering_ids,
        }
    }

    fn input<S: AsRef<str>>(name: &str, strategy: &str, members: &[S]) -> ComboInput {
        ComboInput {
            name: name.to_string(),
            strategy: strategy.to_string(),
            weights: None,
            members: members
                .iter()
                .map(|offering_id| MemberInput {
                    offering_id: offering_id.as_ref().to_string(),
                    weight: None,
                })
                .collect(),
            enabled: None,
        }
    }

    #[tokio::test]
    async fn a_combo_round_trips_with_default_weights_and_members() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let created = create(
            &s.app.state.pool,
            &actor,
            &input("test", "ordered", &s.offering_ids),
        )
        .await
        .expect("create");
        assert_eq!(created.strategy, Strategy::Ordered);
        assert_eq!(created.preset, Some("balanced"));
        assert_eq!(created.weights, Weights::default());

        let loaded = load_by_name(&s.app.state.pool, "test")
            .await
            .expect("load")
            .expect("combo exists");
        assert_eq!(loaded, created);
        assert_eq!(
            loaded.members,
            s.offering_ids
                .iter()
                .map(|id| ComboMember {
                    offering_id: id.clone(),
                    weight: 1.0,
                })
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn weights_accept_presets_and_partial_overrides() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let mut preset_input = input("cheap", "weighted", &s.offering_ids[..1]);
        preset_input.weights = Some(serde_json::json!({"preset": "cheapest"}));
        let cheap = create(&s.app.state.pool, &actor, &preset_input)
            .await
            .expect("create");
        assert_eq!(cheap.preset, Some("cheapest"));
        assert_eq!(cheap.weights.price, 1.0);

        let mut partial_input = input("custom", "weighted", &s.offering_ids[..1]);
        partial_input.weights = Some(serde_json::json!({"price": 0.9}));
        partial_input.members[0].weight = Some(2.5);
        let custom = create(&s.app.state.pool, &actor, &partial_input)
            .await
            .expect("create");
        assert_eq!(custom.preset, None);
        assert_eq!(custom.weights.price, 0.9);
        assert_eq!(custom.weights.speed, 0.30, "missing keys keep defaults");
        assert_eq!(custom.members[0].weight, 2.5);
    }

    #[tokio::test]
    async fn invalid_combos_are_rejected_by_reason() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let pool = &s.app.state.pool;

        let err = create(pool, &actor, &input("x", "zoo", &s.offering_ids[..1]))
            .await
            .expect_err("strategy");
        assert!(err.to_string().contains("round_robin"), "{err}");

        let err = create(pool, &actor, &input("x", "ordered", &[] as &[&str]))
            .await
            .expect_err("members");
        assert!(err.to_string().contains("members"), "{err}");

        let err = create(
            pool,
            &actor,
            &input("x", "ordered", &[&s.offering_ids[0], &s.offering_ids[0]]),
        )
        .await
        .expect_err("duplicates");
        assert!(err.to_string().contains("twice"), "{err}");

        let err = create(pool, &actor, &input("x", "ordered", &["missing-offering"]))
            .await
            .expect_err("offering");
        assert!(err.to_string().contains("does not exist"), "{err}");

        let mut bad_weight = input("x", "weighted", &s.offering_ids[..1]);
        bad_weight.members[0].weight = Some(-1.0);
        let err = create(pool, &actor, &bad_weight).await.expect_err("weight");
        assert!(err.to_string().contains("weight"), "{err}");

        let mut bad_name = input("bad:name", "ordered", &s.offering_ids[..1]);
        bad_name.name = "bad:name".to_string();
        let err = create(pool, &actor, &bad_name).await.expect_err("name");
        assert!(err.to_string().contains("namespace separator"), "{err}");

        create(
            pool,
            &actor,
            &input("taken", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect("first");
        let err = create(
            pool,
            &actor,
            &input("taken", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect_err("name taken");
        assert!(err.to_string().contains("already in use"), "{err}");
    }

    #[tokio::test]
    async fn the_round_robin_cursor_persists_across_requests() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let combo = create(
            &s.app.state.pool,
            &actor,
            &input("rr", "round_robin", &s.offering_ids),
        )
        .await
        .expect("create");
        assert_eq!(combo.rr_cursor, 0);

        assert_eq!(
            advance_cursor(&s.app.state.pool, &combo.id)
                .await
                .expect("1"),
            1
        );
        assert_eq!(
            advance_cursor(&s.app.state.pool, &combo.id)
                .await
                .expect("2"),
            2
        );
        let reloaded = load_by_name(&s.app.state.pool, "rr")
            .await
            .expect("load")
            .expect("combo");
        assert_eq!(reloaded.rr_cursor, 2, "persisted in the row");
    }

    #[tokio::test]
    async fn unknown_names_load_none() {
        let s = seeded().await;
        assert!(
            load_by_name(&s.app.state.pool, "nope")
                .await
                .expect("load")
                .is_none(),
            "unknown combos are not found"
        );
    }
}
