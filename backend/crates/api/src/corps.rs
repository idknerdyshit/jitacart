//! Corp management endpoints.
//!
//! Allows group owners to link EVE corporations, add/remove ambassadors,
//! set the payer-corp on lists, and browse the corp wallet journal.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use domain::{GroupRole, ListDetail};

use crate::{
    db::Tx,
    errors::ApiError,
    extract::{CurrentGroup, CurrentUser},
    lists::load_list_detail,
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
    State(_state): State<AppState>,
    CurrentGroup {
        group_id,
        user_id,
        role,
    }: CurrentGroup,
    tx: Tx,
) -> Result<Json<CorpsResponse>, ApiError> {
    let mut conn = tx.acquire().await;
    let corps = list_corps_inner(&mut **conn, group_id, user_id, role).await?;

    // Suppress unused warning — state is required by the extractor pattern

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
    tx: Tx,
    Json(body): Json<LinkCorpBody>,
) -> Result<Json<CorpDto>, ApiError> {
    if role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut conn = tx.acquire().await;

    // Verify the character belongs to this user and has corp scopes.
    let char_row: Option<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT id, scopes FROM characters \
         WHERE character_id = $1 AND user_id = $2",
    )
    .bind(body.character_id)
    .bind(user_id)
    .fetch_optional(&mut **conn)
    .await?;

    let (char_uuid, granted_scopes) = char_row.ok_or(ApiError::BadRequest(
        "character not found or not yours".into(),
    ))?;

    // Drop the conn guard while doing ESI calls so we don't hold the tx
    // connection during network I/O.
    // TODO(rls): ESI call inside the request tx — split into multiple txs if
    //            probing shows lock-hold latency is a problem
    drop(conn);

    // Fetch corp info from ESI (via the character's affiliation).
    let client = state
        .token_store
        .authed_client_for(char_uuid)
        .await
        .map_err(ApiError::Internal)?;

    let affiliations = client
        .character_affiliation(&[body.character_id])
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let esi_corp_id = affiliations
        .into_iter()
        .find(|a| a.character_id == body.character_id)
        .map(|a| a.corporation_id)
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("affiliation not found")))?;

    let corp_info = client
        .get_corporation(esi_corp_id)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut conn = tx.acquire().await;

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
    .fetch_one(&mut **conn)
    .await?;

    // Upsert corp principal.
    sqlx::query(
        r#"
        INSERT INTO principals (kind, corp_id)
        VALUES ('corp', $1)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(corp_id)
    .execute(&mut **conn)
    .await?;

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
    .execute(&mut **conn)
    .await?;

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
    .execute(&mut **conn)
    .await?;

    // Return updated corp dto (within the same tx so caller sees their own write).
    let corps = list_corps_inner(&mut **conn, group_id, user_id, role).await?;
    let dto = corps
        .into_iter()
        .find(|c| c.id == corp_id)
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("corp not found after link")))?;

    Ok(Json(dto))
}

#[derive(Deserialize)]
struct AddAmbassadorBody {
    character_id: i64,
}

async fn add_ambassador(
    State(_state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup { user_id, role, .. }: CurrentGroup,
    tx: Tx,
    Json(body): Json<AddAmbassadorBody>,
) -> Result<StatusCode, ApiError> {
    if role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut conn = tx.acquire().await;
    require_corp_in_group(&mut **conn, group_id, corp_id).await?;

    let char_row: Option<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT id, scopes FROM characters \
         WHERE character_id = $1 AND user_id = $2",
    )
    .bind(body.character_id)
    .bind(user_id)
    .fetch_optional(&mut **conn)
    .await?;

    let (char_uuid, granted_scopes) = char_row.ok_or(ApiError::BadRequest(
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
    .execute(&mut **conn)
    .await?;

    // Suppress unused warning — state is required by the extractor pattern

    Ok(StatusCode::NO_CONTENT)
}

async fn remove_ambassador(
    State(_state): State<AppState>,
    Path((group_id, corp_id, character_id)): Path<(Uuid, Uuid, Uuid)>,
    CurrentGroup { role, .. }: CurrentGroup,
    tx: Tx,
) -> Result<StatusCode, ApiError> {
    if role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut conn = tx.acquire().await;
    require_corp_in_group(&mut **conn, group_id, corp_id).await?;

    sqlx::query(
        "UPDATE corp_ambassadors SET disabled_at = now() \
         WHERE corp_id = $1 AND character_id = $2 AND disabled_at IS NULL",
    )
    .bind(corp_id)
    .bind(character_id)
    .execute(&mut **conn)
    .await?;

    // Suppress unused warning — state is required by the extractor pattern

    Ok(StatusCode::NO_CONTENT)
}

async fn unlink_corp(
    State(_state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup { role, .. }: CurrentGroup,
    tx: Tx,
) -> Result<StatusCode, ApiError> {
    if role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut conn = tx.acquire().await;
    require_corp_in_group(&mut **conn, group_id, corp_id).await?;

    // TODO(rls): ESI call inside the request tx — split into multiple txs if
    //            probing shows lock-hold latency is a problem

    sqlx::query(
        "UPDATE group_corps SET unlinked_at = now() \
         WHERE group_id = $1 AND corp_id = $2 AND unlinked_at IS NULL",
    )
    .bind(group_id)
    .bind(corp_id)
    .execute(&mut **conn)
    .await?;

    // Disable ambassadors contributed through this group.
    sqlx::query(
        "UPDATE corp_ambassadors SET disabled_at = now() \
         WHERE corp_id = $1 AND contributed_via_group_id = $2 AND disabled_at IS NULL",
    )
    .bind(corp_id)
    .bind(group_id)
    .execute(&mut **conn)
    .await?;

    // If no other group links this corp, disable it (stop polling).
    let other_links: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM group_corps \
         WHERE corp_id = $1 AND unlinked_at IS NULL",
    )
    .bind(corp_id)
    .fetch_one(&mut **conn)
    .await?;

    if other_links == 0 {
        sqlx::query("UPDATE corps SET disabled_at = now() WHERE id = $1")
            .bind(corp_id)
            .execute(&mut **conn)
            .await?;
    }

    // Suppress unused warning — state is required by the extractor pattern

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
    State(_state): State<AppState>,
    Path((group_id, corp_id)): Path<(Uuid, Uuid)>,
    CurrentGroup {
        user_id,
        group_id: _gid,
        ..
    }: CurrentGroup,
    tx: Tx,
    Query(q): Query<JournalQuery>,
) -> Result<Json<Vec<JournalEntryDto>>, ApiError> {
    let mut conn = tx.acquire().await;
    require_corp_in_group(&mut **conn, group_id, corp_id).await?;

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
    .fetch_one(&mut **conn)
    .await?;

    let is_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM group_memberships
            WHERE group_id = $1 AND user_id = $2 AND role = $3
        )",
    )
    .bind(group_id)
    .bind(user_id)
    .bind(GroupRole::Owner)
    .fetch_one(&mut **conn)
    .await?;

    let full_visibility = is_ambassador || is_owner;
    if !full_visibility {
        return Err(ApiError::forbidden());
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
    .bind(q.limit.clamp(1, 500))
    .bind(q.division)
    .fetch_all(&mut **conn)
    .await?;

    // Suppress unused warning — state is required by the extractor pattern

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
    tx: Tx,
    Json(body): Json<PatchListPayerBody>,
) -> Result<Json<ListDetail>, ApiError> {
    // Verify both or neither.
    match (&body.payer_corp_id, body.payer_division) {
        (Some(_), None) | (None, Some(_)) => {
            return Err(ApiError::BadRequest(
                "payer_corp_id and payer_division must both be set or both be null".into(),
            ));
        }
        _ => {}
    }

    let mut conn = tx.acquire().await;

    // Load list group; require group owner (same guard as link/unlink/add-ambassador).
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT l.group_id FROM lists l \
         JOIN group_memberships gm ON gm.group_id = l.group_id AND gm.user_id = $2 \
         WHERE l.id = $1 \
           AND gm.role = $3",
    )
    .bind(list_id)
    .bind(user_id)
    .bind(GroupRole::Owner)
    .fetch_optional(&mut **conn)
    .await?;

    let group_id = row.ok_or_else(ApiError::forbidden)?.0;

    // First query gated on role = Owner; the caller's role here is necessarily Owner.
    let actual_role = GroupRole::Owner;

    // Mid-flight rule: reject if reimbursements already exist.
    let has_reimbs: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM reimbursements WHERE list_id = $1)")
            .bind(list_id)
            .fetch_one(&mut **conn)
            .await?;

    if has_reimbs {
        return Err(ApiError::Conflict(
            "payer cannot be changed after reimbursements have been generated".into(),
        ));
    }

    // If setting a corp, verify it's linked to this group.
    if let Some(corp_id) = body.payer_corp_id {
        require_corp_in_group(&mut **conn, group_id, corp_id).await?;
    }

    sqlx::query("UPDATE lists SET payer_corp_id = $2, payer_division = $3 WHERE id = $1")
        .bind(list_id)
        .bind(body.payer_corp_id)
        .bind(body.payer_division)
        .execute(&mut **conn)
        .await?;

    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, actual_role).await?;
    Ok(Json(detail))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn require_corp_in_group(
    conn: &mut sqlx::PgConnection,
    group_id: Uuid,
    corp_id: Uuid,
) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM group_corps \
         WHERE group_id = $1 AND corp_id = $2 AND unlinked_at IS NULL)",
    )
    .bind(group_id)
    .bind(corp_id)
    .fetch_one(conn)
    .await?;

    if !exists {
        return Err(ApiError::not_found());
    }
    Ok(())
}

async fn list_corps_inner(
    conn: &mut sqlx::PgConnection,
    group_id: Uuid,
    caller_user_id: Uuid,
    caller_role: GroupRole,
) -> anyhow::Result<Vec<CorpDto>> {
    use std::collections::{HashMap, HashSet};

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
    .fetch_all(&mut *conn)
    .await?;

    if corps.is_empty() {
        return Ok(Vec::new());
    }

    let corp_ids: Vec<Uuid> = corps.iter().map(|c| c.id).collect();

    #[derive(sqlx::FromRow)]
    struct AmbRow {
        corp_id: Uuid,
        character_id: Uuid,
        character_name: String,
        granted_scopes: Vec<String>,
        last_used_at: Option<DateTime<Utc>>,
        last_auth_error_at: Option<DateTime<Utc>>,
        disabled_at: Option<DateTime<Utc>>,
        character_user_id: Uuid,
    }

    let amb_rows: Vec<AmbRow> = sqlx::query_as(
        r#"
        SELECT ca.corp_id, ca.character_id, ch.character_name,
               ca.granted_scopes, ca.last_used_at,
               ca.last_auth_error_at, ca.disabled_at,
               ch.user_id AS character_user_id
        FROM corp_ambassadors ca
        JOIN characters ch ON ch.id = ca.character_id
        WHERE ca.corp_id = ANY($1::uuid[])
        ORDER BY ca.disabled_at NULLS FIRST, ca.last_used_at NULLS FIRST
        "#,
    )
    .bind(&corp_ids)
    .fetch_all(&mut *conn)
    .await?;

    let mut ambassadors_by_corp: HashMap<Uuid, Vec<AmbassadorDto>> = HashMap::new();
    let mut ambassador_corps_for_caller: HashSet<Uuid> = HashSet::new();
    for r in amb_rows {
        if r.character_user_id == caller_user_id && r.disabled_at.is_none() {
            ambassador_corps_for_caller.insert(r.corp_id);
        }
        ambassadors_by_corp
            .entry(r.corp_id)
            .or_default()
            .push(AmbassadorDto {
                character_id: r.character_id,
                character_name: r.character_name,
                granted_scopes: r.granted_scopes,
                last_used_at: r.last_used_at,
                last_auth_error_at: r.last_auth_error_at,
                disabled_at: r.disabled_at,
            });
    }

    #[derive(sqlx::FromRow)]
    struct DivRow {
        corp_id: Uuid,
        division: i16,
        name: Option<String>,
        balance_isk: Decimal,
        last_synced_at: Option<DateTime<Utc>>,
    }
    let div_rows: Vec<DivRow> = sqlx::query_as(
        "SELECT corp_id, division, name, balance_isk, last_synced_at \
         FROM corp_wallet_divisions WHERE corp_id = ANY($1::uuid[]) \
         ORDER BY corp_id, division",
    )
    .bind(&corp_ids)
    .fetch_all(&mut *conn)
    .await?;

    let mut divisions_by_corp: HashMap<Uuid, Vec<DivRow>> = HashMap::new();
    for d in div_rows {
        divisions_by_corp.entry(d.corp_id).or_default().push(d);
    }

    let mut result = Vec::with_capacity(corps.len());
    for corp in corps {
        let is_ambassador = ambassador_corps_for_caller.contains(&corp.id);
        let ambassadors = ambassadors_by_corp.remove(&corp.id).unwrap_or_default();
        let divisions = divisions_by_corp.remove(&corp.id).unwrap_or_default();
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
            ambassadors,
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
