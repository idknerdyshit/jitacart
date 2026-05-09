//! Corp management endpoints (Phase 7).
//!
//! Allows group owners to link EVE corporations, add/remove ambassadors,
//! set the payer-corp on lists, and browse the corp wallet journal.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use domain::{GroupRole, ListDetail};

use crate::{
    extract::{CurrentGroup, CurrentUser},
    lists::{load_list_detail, ListError},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Corp management
        .route("/groups/{id}/corps", get(list_corps))
        .route("/groups/{id}/corps/link", post(link_corp))
        .route("/groups/{id}/corps/{corp_id}", delete(unlink_corp))
        .route(
            "/groups/{id}/corps/{corp_id}/ambassadors",
            post(add_ambassador),
        )
        .route(
            "/groups/{id}/corps/{corp_id}/ambassadors/{character_id}",
            delete(remove_ambassador),
        )
        // Wallet journal
        .route("/groups/{id}/corps/{corp_id}/journal", get(list_journal))
        // List payer patch
        .route("/lists/{id}/payer", patch(patch_list_payer))
}

// ── DTOs ──────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct CorpsResponse {
    pub corps: Vec<CorpDto>,
    pub role: GroupRole,
}

#[derive(Serialize)]
pub struct CorpDto {
    pub id: Uuid,
    pub esi_corporation_id: i64,
    pub name: String,
    pub ticker: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_auth_error_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub linked_at: DateTime<Utc>,
    pub linked_by_user_id: Uuid,
    pub is_ambassador: bool,
    pub ambassadors: Vec<AmbassadorDto>,
    pub wallet_divisions: Vec<WalletDivisionDto>,
}

#[derive(Serialize)]
pub struct AmbassadorDto {
    pub character_id: Uuid,
    pub character_name: String,
    pub granted_scopes: Vec<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub last_auth_error_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct WalletDivisionDto {
    pub division: i16,
    pub name: Option<String>,
    pub balance_isk: Option<Decimal>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct JournalEntryDto {
    pub id: Uuid,
    pub division: i16,
    pub esi_journal_ref_id: i64,
    pub date: DateTime<Utc>,
    pub ref_type: String,
    pub amount: Decimal,
    pub balance: Decimal,
    pub first_party_id: Option<i64>,
    pub second_party_id: Option<i64>,
    pub context_id: Option<i64>,
    pub context_id_type: Option<String>,
    pub reason: Option<String>,
    /// Only included for ambassadors/group-owners.
    pub raw_json: Option<serde_json::Value>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn list_corps(
    State(state): State<AppState>,
    CurrentGroup {
        group_id,
        user_id,
        role,
    }: CurrentGroup,
) -> Result<Json<CorpsResponse>, CorpError> {
    let corps = list_corps_inner(&state.pool, group_id, user_id, role)
        .await
        .map_err(internal)?;
    Ok(Json(CorpsResponse { corps, role }))
}

#[derive(Deserialize)]
struct LinkCorpBody {
    /// The EVE character_id of the ambassador character.
    character_id: i64,
}

async fn link_corp(
    State(state): State<AppState>,
    CurrentGroup {
        user_id,
        group_id,
        role,
    }: CurrentGroup,
    Json(body): Json<LinkCorpBody>,
) -> Result<Json<CorpDto>, CorpError> {
    if role != domain::GroupRole::Owner {
        return Err(CorpError::Forbidden);
    }

    // Verify the character belongs to this user and has corp scopes.
    let char_row: Option<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT id, scopes FROM characters \
         WHERE character_id = $1 AND user_id = $2",
    )
    .bind(body.character_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let (char_uuid, granted_scopes) = char_row.ok_or(CorpError::BadRequest(
        "character not found or not yours".into(),
    ))?;

    // Fetch corp info from ESI (via the character's affiliation).
    let client = state
        .token_store
        .authed_client_for(char_uuid)
        .await
        .map_err(CorpError::Internal)?;

    let affiliations = client
        .character_affiliation(&[body.character_id])
        .await
        .map_err(|e| CorpError::Internal(e.into()))?;

    let esi_corp_id = affiliations
        .into_iter()
        .find(|a| a.character_id == body.character_id)
        .map(|a| a.corporation_id)
        .ok_or_else(|| CorpError::Internal(anyhow::anyhow!("affiliation not found")))?;

    let corp_info = client
        .get_corporation(esi_corp_id)
        .await
        .map_err(|e| CorpError::Internal(e.into()))?;

    let mut tx = state.pool.begin().await.map_err(internal)?;

    // Upsert corp row.
    let corp_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO corps (esi_corporation_id, name, ticker)
        VALUES ($1, $2, $3)
        ON CONFLICT (esi_corporation_id) DO UPDATE
            SET name = EXCLUDED.name,
                ticker = EXCLUDED.ticker,
                disabled_at = NULL,
                last_synced_at = now()
        RETURNING id
        "#,
    )
    .bind(esi_corp_id)
    .bind(&corp_info.name)
    .bind(&corp_info.ticker)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    // Upsert corp principal.
    sqlx::query(
        r#"
        INSERT INTO principals (kind, corp_id)
        VALUES ('corp', $1)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(corp_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    // Link corp to group. One row per pair: activate (clear unlinked_at) on relink.
    sqlx::query(
        r#"
        INSERT INTO group_corps (group_id, corp_id, linked_by_user_id)
        VALUES ($1, $2, $3)
        ON CONFLICT (group_id, corp_id) DO UPDATE
            SET unlinked_at        = NULL,
                linked_at          = CASE
                                         WHEN group_corps.unlinked_at IS NOT NULL THEN now()
                                         ELSE group_corps.linked_at
                                     END,
                linked_by_user_id  = EXCLUDED.linked_by_user_id
        "#,
    )
    .bind(group_id)
    .bind(corp_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    // Upsert ambassador.
    sqlx::query(
        r#"
        INSERT INTO corp_ambassadors (corp_id, character_id, granted_scopes)
        VALUES ($1, $2, $3)
        ON CONFLICT (corp_id, character_id) DO UPDATE
            SET granted_scopes = EXCLUDED.granted_scopes,
                disabled_at = NULL,
                last_auth_error_at = NULL
        "#,
    )
    .bind(corp_id)
    .bind(char_uuid)
    .bind(&granted_scopes)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    tx.commit().await.map_err(internal)?;

    // Return updated corp dto.
    let corps = list_corps_inner(&state.pool, group_id, user_id, role)
        .await
        .map_err(internal)?;
    let dto = corps
        .into_iter()
        .find(|c| c.id == corp_id)
        .ok_or_else(|| internal(anyhow::anyhow!("corp not found after link")))?;

    Ok(Json(dto))
}

#[derive(Deserialize)]
struct AddAmbassadorBody {
    character_id: i64,
}

async fn add_ambassador(
    State(state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup { user_id, role, .. }: CurrentGroup,
    Json(body): Json<AddAmbassadorBody>,
) -> Result<StatusCode, CorpError> {
    if role != domain::GroupRole::Owner {
        return Err(CorpError::Forbidden);
    }
    require_corp_in_group(&state.pool, group_id, corp_id).await?;

    let char_row: Option<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT id, scopes FROM characters \
         WHERE character_id = $1 AND user_id = $2",
    )
    .bind(body.character_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let (char_uuid, granted_scopes) = char_row.ok_or(CorpError::BadRequest(
        "character not found or not yours".into(),
    ))?;

    sqlx::query(
        r#"
        INSERT INTO corp_ambassadors (corp_id, character_id, granted_scopes, contributed_via_group_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (corp_id, character_id) DO UPDATE
            SET granted_scopes = EXCLUDED.granted_scopes,
                contributed_via_group_id = EXCLUDED.contributed_via_group_id,
                disabled_at = NULL,
                last_auth_error_at = NULL
        "#,
    )
    .bind(corp_id)
    .bind(char_uuid)
    .bind(&granted_scopes)
    .bind(group_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn remove_ambassador(
    State(state): State<AppState>,
    Path((group_id, corp_id, character_id)): Path<(Uuid, Uuid, Uuid)>,
    CurrentGroup { role, .. }: CurrentGroup,
) -> Result<StatusCode, CorpError> {
    if role != domain::GroupRole::Owner {
        return Err(CorpError::Forbidden);
    }
    require_corp_in_group(&state.pool, group_id, corp_id).await?;

    sqlx::query(
        "UPDATE corp_ambassadors SET disabled_at = now() \
         WHERE corp_id = $1 AND character_id = $2 AND disabled_at IS NULL",
    )
    .bind(corp_id)
    .bind(character_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn unlink_corp(
    State(state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup { role, .. }: CurrentGroup,
) -> Result<StatusCode, CorpError> {
    if role != domain::GroupRole::Owner {
        return Err(CorpError::Forbidden);
    }
    require_corp_in_group(&state.pool, group_id, corp_id).await?;

    let mut tx = state.pool.begin().await.map_err(internal)?;

    sqlx::query(
        "UPDATE group_corps SET unlinked_at = now() \
         WHERE group_id = $1 AND corp_id = $2 AND unlinked_at IS NULL",
    )
    .bind(group_id)
    .bind(corp_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    // Disable ambassadors contributed through this group.
    sqlx::query(
        "UPDATE corp_ambassadors SET disabled_at = now() \
         WHERE corp_id = $1 AND contributed_via_group_id = $2 AND disabled_at IS NULL",
    )
    .bind(corp_id)
    .bind(group_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    // If no other group links this corp, disable it (stop polling).
    let other_links: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM group_corps \
         WHERE corp_id = $1 AND unlinked_at IS NULL",
    )
    .bind(corp_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    if other_links == 0 {
        sqlx::query("UPDATE corps SET disabled_at = now() WHERE id = $1")
            .bind(corp_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
    }

    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct JournalQuery {
    #[serde(default = "default_journal_limit")]
    limit: i64,
    before: Option<DateTime<Utc>>,
    division: Option<i16>,
}

fn default_journal_limit() -> i64 {
    200
}

async fn list_journal(
    State(state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup {
        user_id,
        group_id: _gid,
        ..
    }: CurrentGroup,
    Query(q): Query<JournalQuery>,
) -> Result<Json<Vec<JournalEntryDto>>, CorpError> {
    require_corp_in_group(&state.pool, group_id, corp_id).await?;

    // Determine visibility: ambassador or group owner sees raw_json.
    let is_ambassador: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM corp_ambassadors ca
            JOIN characters ch ON ch.id = ca.character_id
            WHERE ca.corp_id = $1 AND ch.user_id = $2 AND ca.disabled_at IS NULL
        )",
    )
    .bind(corp_id)
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;

    let is_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM group_memberships
            WHERE group_id = $1 AND user_id = $2 AND role = 'owner'
        )",
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;

    let full_visibility = is_ambassador || is_owner;
    if !full_visibility {
        return Err(CorpError::Forbidden);
    }

    #[derive(sqlx::FromRow)]
    struct JournalRow {
        id: Uuid,
        division: i16,
        esi_journal_ref_id: i64,
        date: DateTime<Utc>,
        ref_type: String,
        amount: Decimal,
        balance: Decimal,
        first_party_id: Option<i64>,
        second_party_id: Option<i64>,
        context_id: Option<i64>,
        context_id_type: Option<String>,
        reason: Option<String>,
        raw_json: serde_json::Value,
    }

    let rows: Vec<JournalRow> = sqlx::query_as(
        r#"
        SELECT j.id, j.division, j.esi_journal_ref_id, j.date, j.ref_type,
               j.amount, j.balance, j.first_party_id, j.second_party_id,
               j.context_id, j.context_id_type, j.reason, j.raw_json
        FROM corp_wallet_journal j
        WHERE j.corp_id = $1
          AND ($2::timestamptz IS NULL OR j.date < $2)
          AND ($4::smallint IS NULL OR j.division = $4)
        ORDER BY j.date DESC
        LIMIT $3
        "#,
    )
    .bind(corp_id)
    .bind(q.before)
    .bind(q.limit.min(500))
    .bind(q.division)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    let entries = rows
        .into_iter()
        .map(|r| JournalEntryDto {
            id: r.id,
            division: r.division,
            esi_journal_ref_id: r.esi_journal_ref_id,
            date: r.date,
            ref_type: r.ref_type,
            amount: r.amount,
            balance: r.balance,
            first_party_id: r.first_party_id,
            second_party_id: r.second_party_id,
            context_id: r.context_id,
            context_id_type: r.context_id_type,
            reason: r.reason,
            raw_json: if full_visibility {
                Some(r.raw_json)
            } else {
                None
            },
        })
        .collect();

    Ok(Json(entries))
}

#[derive(Deserialize)]
struct PatchListPayerBody {
    payer_corp_id: Option<Uuid>,
    payer_division: Option<i16>,
}

async fn patch_list_payer(
    State(state): State<AppState>,
    Path(list_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
    Json(body): Json<PatchListPayerBody>,
) -> Result<Json<ListDetail>, CorpError> {
    // Verify both or neither.
    match (&body.payer_corp_id, body.payer_division) {
        (Some(_), None) | (None, Some(_)) => {
            return Err(CorpError::BadRequest(
                "payer_corp_id and payer_division must both be set or both be null".into(),
            ));
        }
        _ => {}
    }

    // Load list group; require group owner (same guard as link/unlink/add-ambassador).
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT l.group_id FROM lists l \
         JOIN group_memberships gm ON gm.group_id = l.group_id AND gm.user_id = $2 \
         WHERE l.id = $1 \
           AND gm.role = 'owner'",
    )
    .bind(list_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let group_id = row.ok_or(CorpError::Forbidden)?.0;

    // Determine the caller's actual role for the response payload.
    let role_str: Option<String> = sqlx::query_scalar(
        "SELECT role FROM group_memberships WHERE group_id = $1 AND user_id = $2",
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    let actual_role: GroupRole = role_str
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(GroupRole::Member);

    // Mid-flight rule: reject if reimbursements already exist.
    let has_reimbs: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM reimbursements WHERE list_id = $1)")
            .bind(list_id)
            .fetch_one(&state.pool)
            .await
            .map_err(internal)?;

    if has_reimbs {
        return Err(CorpError::Conflict(
            "payer cannot be changed after reimbursements have been generated".into(),
        ));
    }

    // If setting a corp, verify it's linked to this group.
    if let Some(corp_id) = body.payer_corp_id {
        require_corp_in_group(&state.pool, group_id, corp_id).await?;
    }

    sqlx::query("UPDATE lists SET payer_corp_id = $2, payer_division = $3 WHERE id = $1")
        .bind(list_id)
        .bind(body.payer_corp_id)
        .bind(body.payer_division)
        .execute(&state.pool)
        .await
        .map_err(internal)?;

    let detail = load_list_detail(&state, list_id, user_id, actual_role)
        .await
        .map_err(|e| match e {
            ListError::NotFound => CorpError::NotFound,
            ListError::Forbidden => CorpError::Forbidden,
            ListError::Internal(e) => internal(e),
            ListError::BadRequest(m) => CorpError::BadRequest(m),
            ListError::Conflict(m) => CorpError::Conflict(m),
        })?;
    Ok(Json(detail))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn require_corp_in_group(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    corp_id: Uuid,
) -> Result<(), CorpError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM group_corps \
         WHERE group_id = $1 AND corp_id = $2 AND unlinked_at IS NULL)",
    )
    .bind(group_id)
    .bind(corp_id)
    .fetch_one(pool)
    .await
    .map_err(internal)?;

    if !exists {
        return Err(CorpError::NotFound);
    }
    Ok(())
}

async fn list_corps_inner(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    caller_user_id: Uuid,
    caller_role: GroupRole,
) -> anyhow::Result<Vec<CorpDto>> {
    #[derive(sqlx::FromRow)]
    struct CorpRow {
        id: Uuid,
        esi_corporation_id: i64,
        name: String,
        ticker: String,
        last_synced_at: Option<DateTime<Utc>>,
        last_auth_error_at: Option<DateTime<Utc>>,
        disabled_at: Option<DateTime<Utc>>,
        linked_at: DateTime<Utc>,
        linked_by_user_id: Uuid,
    }

    let corps: Vec<CorpRow> = sqlx::query_as(
        r#"
        SELECT co.id, co.esi_corporation_id, co.name, co.ticker,
               co.last_synced_at, co.last_auth_error_at, co.disabled_at,
               gc.linked_at, gc.linked_by_user_id
        FROM group_corps gc
        JOIN corps co ON co.id = gc.corp_id
        WHERE gc.group_id = $1 AND gc.unlinked_at IS NULL
        ORDER BY gc.linked_at
        "#,
    )
    .bind(group_id)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::new();
    for corp in corps {
        #[derive(sqlx::FromRow)]
        struct AmbRow {
            character_id: Uuid,
            character_name: String,
            granted_scopes: Vec<String>,
            last_used_at: Option<DateTime<Utc>>,
            last_auth_error_at: Option<DateTime<Utc>>,
            disabled_at: Option<DateTime<Utc>>,
        }

        let ambassadors: Vec<AmbRow> = sqlx::query_as(
            r#"
            SELECT ca.character_id, ch.character_name,
                   ca.granted_scopes, ca.last_used_at,
                   ca.last_auth_error_at, ca.disabled_at
            FROM corp_ambassadors ca
            JOIN characters ch ON ch.id = ca.character_id
            WHERE ca.corp_id = $1
            ORDER BY ca.disabled_at NULLS FIRST, ca.last_used_at NULLS FIRST
            "#,
        )
        .bind(corp.id)
        .fetch_all(pool)
        .await?;

        #[derive(sqlx::FromRow)]
        struct DivRow {
            division: i16,
            name: Option<String>,
            balance_isk: Decimal,
            last_synced_at: Option<DateTime<Utc>>,
        }
        let divisions: Vec<DivRow> = sqlx::query_as(
            "SELECT division, name, balance_isk, last_synced_at \
             FROM corp_wallet_divisions WHERE corp_id = $1 ORDER BY division",
        )
        .bind(corp.id)
        .fetch_all(pool)
        .await?;

        let is_ambassador: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM corp_ambassadors ca
                JOIN characters ch ON ch.id = ca.character_id
                WHERE ca.corp_id = $1 AND ch.user_id = $2 AND ca.disabled_at IS NULL
            )
            "#,
        )
        .bind(corp.id)
        .bind(caller_user_id)
        .fetch_one(pool)
        .await?;

        result.push(CorpDto {
            id: corp.id,
            esi_corporation_id: corp.esi_corporation_id,
            name: corp.name,
            ticker: corp.ticker,
            last_synced_at: corp.last_synced_at,
            last_auth_error_at: corp.last_auth_error_at,
            disabled_at: corp.disabled_at,
            linked_at: corp.linked_at,
            linked_by_user_id: corp.linked_by_user_id,
            is_ambassador,
            ambassadors: ambassadors
                .into_iter()
                .map(|a| AmbassadorDto {
                    character_id: a.character_id,
                    character_name: a.character_name,
                    granted_scopes: a.granted_scopes,
                    last_used_at: a.last_used_at,
                    last_auth_error_at: a.last_auth_error_at,
                    disabled_at: a.disabled_at,
                })
                .collect(),
            wallet_divisions: divisions
                .into_iter()
                .map(|d| WalletDivisionDto {
                    division: d.division,
                    name: d.name,
                    balance_isk: if caller_role == GroupRole::Owner || is_ambassador {
                        Some(d.balance_isk)
                    } else {
                        None
                    },
                    last_synced_at: d.last_synced_at,
                })
                .collect(),
        });
    }

    Ok(result)
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CorpError {
    NotFound,
    Forbidden,
    BadRequest(String),
    Conflict(String),
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> CorpError {
    CorpError::Internal(e.into())
}

impl IntoResponse for CorpError {
    fn into_response(self) -> Response {
        match self {
            CorpError::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            CorpError::Forbidden => {
                (StatusCode::FORBIDDEN, "you cannot perform this action").into_response()
            }
            CorpError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            CorpError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
            CorpError::Internal(e) => {
                tracing::error!(error = ?e, "corps handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
