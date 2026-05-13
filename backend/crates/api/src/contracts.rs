//! Contract suggestions, confirm/reject/manual-link/unlink. Hauler-only.
//!
//! Confirmation invariants are checked under FOR UPDATE locks: the suggestion
//! must still be `pending`, the contract must be `item_exchange`, the
//! reimbursement must be `pending` with no existing contract binding, and the
//! caller must be the hauler/issuer who owns both.
//!
//! When a contract is already in a terminal-success state at confirm time, we
//! also run [`settlement::settle_via_contract`] so the user does not have to
//! wait for the next worker tick.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{ContractMatchState, ContractStatus, ContractType, ReimbursementStatus};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    db::Tx,
    errors::ApiError,
    extract::{CurrentGroup, CurrentUser},
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/contracts/suggestions", get(list_suggestions))
        .route("/groups/{id}/contracts", get(list_contracts))
        .route(
            "/contracts/suggestions/{id}/confirm",
            post(confirm_suggestion),
        )
        .route(
            "/contracts/suggestions/{id}/reject",
            post(reject_suggestion),
        )
        .route("/contracts/{id}/manual-link", post(manual_link))
        .route("/contracts/{id}/unlink", post(unlink_contract))
}

#[derive(Serialize, sqlx::FromRow)]
struct SuggestionDto {
    id: Uuid,
    contract_id: Uuid,
    esi_contract_id: i64,
    contract_status: ContractStatus,
    contract_price_isk: Decimal,
    contract_expected_total_isk: Option<Decimal>,
    reimbursement_id: Uuid,
    list_id: Uuid,
    list_destination_label: Option<String>,
    requester_display_name: String,
    hauler_display_name: String,
    reimbursement_total_isk: Decimal,
    score: Decimal,
    exact_match: bool,
    state: ContractMatchState,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

async fn list_suggestions(
    State(_state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
    tx: Tx,
) -> Result<Json<Vec<SuggestionDto>>, ApiError> {
    let mut conn = tx.acquire().await;
    let rows: Vec<SuggestionDto> = sqlx::query_as(
        r#"
        SELECT
            s.id,
            s.contract_id,
            c.esi_contract_id,
            c.status              AS contract_status,
            c.price_isk           AS contract_price_isk,
            c.expected_total_isk  AS contract_expected_total_isk,
            s.reimbursement_id,
            r.list_id,
            l.destination_label   AS list_destination_label,
            COALESCE(ru.display_name, rc.name) AS requester_display_name,
            hu.display_name       AS hauler_display_name,
            r.total_isk           AS reimbursement_total_isk,
            s.score,
            s.exact_match,
            s.state,
            s.created_at,
            s.decided_at
        FROM contract_match_suggestions s
        JOIN contracts c         ON c.id = s.contract_id
        JOIN reimbursements r    ON r.id = s.reimbursement_id
        JOIN lists l             ON l.id = r.list_id
        LEFT JOIN users ru       ON ru.id = r.requester_user_id
        LEFT JOIN principals rp  ON rp.id = r.requester_principal_id
        LEFT JOIN corps rc       ON rc.id = rp.corp_id
        JOIN users hu            ON hu.id = r.hauler_user_id
        WHERE l.group_id = $1
          AND r.hauler_user_id = $2
          AND s.state IN ('pending','confirmed')
        ORDER BY s.state, s.created_at DESC
        LIMIT 200
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_all(&mut **conn)
    .await?;

    // Suppress unused warning — state is required by the extractor pattern

    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow)]
struct BoundContractDto {
    contract_id: Uuid,
    esi_contract_id: i64,
    status: ContractStatus,
    price_isk: Decimal,
    expected_total_isk: Option<Decimal>,
    settlement_delta_isk: Option<Decimal>,
    date_completed: Option<DateTime<Utc>>,
    bound_reimbursement_count: i64,
}

async fn list_contracts(
    State(_state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
    tx: Tx,
) -> Result<Json<Vec<BoundContractDto>>, ApiError> {
    let mut conn = tx.acquire().await;
    let rows: Vec<BoundContractDto> = sqlx::query_as(
        r#"
        SELECT
            c.id                     AS contract_id,
            c.esi_contract_id,
            c.status,
            c.price_isk,
            c.expected_total_isk,
            c.settlement_delta_isk,
            c.date_completed,
            COUNT(r.id) FILTER (WHERE r.id IS NOT NULL) AS bound_reimbursement_count
        FROM contracts c
        JOIN reimbursements r ON r.contract_id = c.id
        JOIN lists l          ON l.id = r.list_id
        WHERE l.group_id = $1
          AND r.hauler_user_id = $2
        GROUP BY c.id
        ORDER BY c.updated_at DESC
        LIMIT 200
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_all(&mut **conn)
    .await?;

    // Suppress unused warning — state is required by the extractor pattern

    Ok(Json(rows))
}

async fn confirm_suggestion(
    State(state): State<AppState>,
    Path(suggestion_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
) -> Result<Json<SuggestionDecision>, ApiError> {
    let mut conn = tx.acquire().await;
    let (decision, webhook_info) = do_confirm(&mut **conn, user_id, suggestion_id).await?;
    for info in webhook_info {
        let event = WebhookEvent::ReimbursementSettled {
            list_destination: info.list_destination,
            requester_name: info.requester_name,
            hauler_name: info.hauler_name,
            total_isk: info.total_isk.to_string(),
        };
        fire_webhook(&mut **conn, info.group_id, &event).await?;
    }
    let _ = state;
    Ok(Json(decision))
}

/// Acquire a transaction (or savepoint, when the caller already holds one)
/// and run the confirm flow inside it. Calling with `&PgPool` opens a fresh
/// top-level tx; calling with `&mut Transaction` (the request tx) opens a
/// savepoint that releases on inner commit, leaving the outer tx alive for
/// the middleware to commit/rollback.
pub async fn do_confirm<'c, A>(
    acquirer: A,
    user_id: Uuid,
    suggestion_id: Uuid,
) -> Result<
    (
        SuggestionDecision,
        Vec<settlement::ContractSettledReimbursement>,
    ),
    ApiError,
>
where
    A: sqlx::Acquire<'c, Database = sqlx::Postgres>,
{
    let mut tx = acquirer.begin().await?;
    let row: Option<LinkLockRow> = sqlx::query_as(
        r#"
        SELECT s.state               AS suggestion_state,
               s.contract_id,
               c.contract_type,
               c.status              AS contract_status,
               c.issuer_user_id,
               c.issuer_principal_id,
               c.assignee_user_id,
               c.assignee_principal_id,
               s.reimbursement_id,
               r.status              AS reimbursement_status,
               r.contract_id         AS reimbursement_contract_id,
               r.hauler_user_id,
               r.requester_user_id,
               r.requester_principal_id,
               l.group_id
        FROM contract_match_suggestions s
        JOIN contracts c ON c.id = s.contract_id
        JOIN reimbursements r ON r.id = s.reimbursement_id
        JOIN lists l ON l.id = r.list_id
        WHERE s.id = $1
        FOR UPDATE OF s, c, r
        "#,
    )
    .bind(suggestion_id)
    .fetch_optional(&mut *tx)
    .await?;
    let row = row.ok_or_else(ApiError::not_found)?;

    if row.suggestion_state != Some(ContractMatchState::Pending) {
        return Err(ApiError::Conflict(format!(
            "suggestion is already {}",
            row.suggestion_state
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(unknown)".into())
        )));
    }
    let ctx = row;
    validate_link(&mut tx, &ctx, user_id).await?;

    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'confirmed', decided_at = now(), decided_by_user_id = $1 \
         WHERE id = $2",
    )
    .bind(user_id)
    .bind(suggestion_id)
    .execute(&mut *tx)
    .await
    .map_err(map_confirmed_dup)?;

    let (settled, webhook_info) = finalize_link(&mut tx, &ctx).await?;

    tx.commit().await?;

    Ok((
        SuggestionDecision {
            suggestion_id,
            state: "confirmed".into(),
            settled,
        },
        webhook_info,
    ))
}

/// Shared lock-row for confirm and manual-link. `suggestion_state` is `None`
/// in the manual-link path (the query SELECTs `NULL::text`). Several fields
/// (e.g. `contract_status`, `assignee_*`) are SELECTed for the `FOR UPDATE
/// OF` lock and JOIN consistency but not read directly.
#[derive(sqlx::FromRow)]
#[allow(dead_code)]
struct LinkLockRow {
    suggestion_state: Option<ContractMatchState>,
    contract_id: Uuid,
    contract_type: ContractType,
    contract_status: ContractStatus,
    issuer_user_id: Option<Uuid>,
    /// Corp-issued contracts store the corp principal here; user contracts
    /// store a user principal.
    issuer_principal_id: Option<Uuid>,
    assignee_user_id: Option<Uuid>,
    /// Principal-level assignee (may be a corp principal for corp-funded rows).
    assignee_principal_id: Option<Uuid>,
    reimbursement_id: Uuid,
    reimbursement_status: ReimbursementStatus,
    reimbursement_contract_id: Option<Uuid>,
    hauler_user_id: Uuid,
    /// NULL for corp-funded reimbursements (requester is a corp principal).
    requester_user_id: Option<Uuid>,
    requester_principal_id: Uuid,
    group_id: Uuid,
}

async fn validate_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &LinkLockRow,
    user_id: Uuid,
) -> Result<(), ApiError> {
    if ctx.contract_type != ContractType::ItemExchange {
        return Err(ApiError::Conflict("contract is not item_exchange".into()));
    }
    if ctx.reimbursement_status != ReimbursementStatus::Pending
        || ctx.reimbursement_contract_id.is_some()
    {
        return Err(ApiError::Conflict(
            "reimbursement is no longer eligible".into(),
        ));
    }
    // The hauler must be the person calling this endpoint.
    if ctx.hauler_user_id != user_id {
        return Err(ApiError::forbidden());
    }
    // The contract issuer must also be this user (personal contracts) or a corp
    // principal the user can act for (corp-issued contracts are accepted when
    // issuer_user_id is None and issuer_principal_id identifies a corp).
    if ctx.issuer_user_id.is_some() && ctx.issuer_user_id != Some(user_id) {
        return Err(ApiError::forbidden());
    }
    // Assignee, if set, must match the reimbursement's requester principal.
    if ctx
        .assignee_principal_id
        .is_some_and(|a| a != ctx.requester_principal_id)
    {
        return Err(ApiError::Conflict(
            "contract assignee does not match the reimbursement's requester".into(),
        ));
    }
    let is_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM group_memberships \
         WHERE group_id = $1 AND user_id = $2)",
    )
    .bind(ctx.group_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    if !is_member {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

/// Bind the reimbursement to the contract, refresh totals, and run
/// settle-via-contract if the contract is already in a terminal-success state.
/// Returns `(settled, webhook_info)` — caller fires webhooks after commit.
async fn finalize_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &LinkLockRow,
) -> Result<(bool, Vec<settlement::ContractSettledReimbursement>), ApiError> {
    sqlx::query("UPDATE reimbursements SET contract_id = $1, updated_at = now() WHERE id = $2")
        .bind(ctx.contract_id)
        .bind(ctx.reimbursement_id)
        .execute(&mut **tx)
        .await?;

    settlement::recompute_contract_expected_total(tx, ctx.contract_id)
        .await
        .map_err(|e| {
            ApiError::internal(anyhow::anyhow!("recompute_contract_expected_total: {e}"))
        })?;

    let already_finished = ctx.contract_status.is_terminal_success();
    let webhook_info = if already_finished {
        settlement::settle_via_contract(tx, ctx.contract_id)
            .await
            .map_err(|e| ApiError::internal(anyhow::anyhow!("settle_via_contract: {e}")))?
    } else {
        vec![]
    };
    Ok((already_finished, webhook_info))
}

fn map_confirmed_dup(e: sqlx::Error) -> ApiError {
    match e {
        sqlx::Error::Database(db)
            if db.constraint() == Some("one_confirmed_suggestion_per_reimbursement") =>
        {
            ApiError::Conflict("reimbursement already confirmed against another contract".into())
        }
        other => ApiError::internal(other),
    }
}

#[derive(Serialize, Debug)]
pub struct SuggestionDecision {
    pub suggestion_id: Uuid,
    pub state: String,
    pub settled: bool,
}

async fn reject_suggestion(
    State(_state): State<AppState>,
    Path(suggestion_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
) -> Result<Json<SuggestionDecision>, ApiError> {
    let mut conn = tx.acquire().await;
    Ok(Json(do_reject(&mut **conn, user_id, suggestion_id).await?))
}

pub async fn do_reject<'c, A>(
    acquirer: A,
    user_id: Uuid,
    suggestion_id: Uuid,
) -> Result<SuggestionDecision, ApiError>
where
    A: sqlx::Acquire<'c, Database = sqlx::Postgres>,
{
    let mut tx = acquirer.begin().await?;
    let row: Option<(ContractMatchState, Option<Uuid>, Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.state, c.issuer_user_id, r.hauler_user_id, l.group_id \
         FROM contract_match_suggestions s \
         JOIN contracts c ON c.id = s.contract_id \
         JOIN reimbursements r ON r.id = s.reimbursement_id \
         JOIN lists l ON l.id = r.list_id \
         WHERE s.id = $1 FOR UPDATE OF s",
    )
    .bind(suggestion_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (cur_state, issuer_user_id, hauler_user_id, group_id) =
        row.ok_or_else(ApiError::not_found)?;
    if cur_state != ContractMatchState::Pending {
        return Err(ApiError::Conflict(format!(
            "suggestion is already {cur_state}"
        )));
    }
    // Personal-issued: only the issuer may reject. Corp-issued (issuer_user_id
    // is NULL): the hauler may reject — hauler is the principal acting on the
    // contract on behalf of the corp.
    let allowed = match issuer_user_id {
        Some(iuid) => iuid == user_id,
        None => hauler_user_id == user_id,
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    // Caller must still be a member of the group that owns the list.
    let is_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM group_memberships \
         WHERE group_id = $1 AND user_id = $2)",
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if !is_member {
        return Err(ApiError::forbidden());
    }
    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'rejected', decided_at = now(), decided_by_user_id = $1 \
         WHERE id = $2",
    )
    .bind(user_id)
    .bind(suggestion_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SuggestionDecision {
        suggestion_id,
        state: "rejected".into(),
        settled: false,
    })
}

#[derive(Deserialize)]
struct ManualLinkBody {
    reimbursement_id: Uuid,
}

async fn manual_link(
    State(state): State<AppState>,
    Path(contract_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
    Json(body): Json<ManualLinkBody>,
) -> Result<Json<SuggestionDecision>, ApiError> {
    let mut conn = tx.acquire().await;
    let (decision, webhook_info) =
        do_manual_link(&mut **conn, user_id, contract_id, body.reimbursement_id).await?;
    for info in webhook_info {
        let event = WebhookEvent::ReimbursementSettled {
            list_destination: info.list_destination,
            requester_name: info.requester_name,
            hauler_name: info.hauler_name,
            total_isk: info.total_isk.to_string(),
        };
        fire_webhook(&mut **conn, info.group_id, &event).await?;
    }
    let _ = state;
    Ok(Json(decision))
}

pub async fn do_manual_link<'c, A>(
    acquirer: A,
    user_id: Uuid,
    contract_id: Uuid,
    reimbursement_id: Uuid,
) -> Result<
    (
        SuggestionDecision,
        Vec<settlement::ContractSettledReimbursement>,
    ),
    ApiError,
>
where
    A: sqlx::Acquire<'c, Database = sqlx::Postgres>,
{
    let mut tx = acquirer.begin().await?;
    let row: Option<LinkLockRow> = sqlx::query_as(
        r#"
        SELECT
            NULL::text              AS suggestion_state,
            c.id                    AS contract_id,
            c.contract_type,
            c.status                AS contract_status,
            c.issuer_user_id,
            c.issuer_principal_id,
            c.assignee_user_id,
            c.assignee_principal_id,
            r.id                    AS reimbursement_id,
            r.status                AS reimbursement_status,
            r.contract_id           AS reimbursement_contract_id,
            r.hauler_user_id,
            r.requester_user_id,
            r.requester_principal_id,
            l.group_id
        FROM contracts c
        CROSS JOIN reimbursements r
        JOIN lists l ON l.id = r.list_id
        WHERE c.id = $1 AND r.id = $2
        FOR UPDATE OF c, r
        "#,
    )
    .bind(contract_id)
    .bind(reimbursement_id)
    .fetch_optional(&mut *tx)
    .await?;
    let ctx = row.ok_or_else(ApiError::not_found)?;
    validate_link(&mut tx, &ctx, user_id).await?;

    sqlx::query(
        r#"
        INSERT INTO contract_match_suggestions
            (contract_id, reimbursement_id, score, exact_match, state, decided_at, decided_by_user_id)
        VALUES ($1, $2, 1.0000, FALSE, 'confirmed', now(), $3)
        ON CONFLICT (contract_id, reimbursement_id) DO UPDATE
            SET state = 'confirmed', decided_at = now(), decided_by_user_id = $3
        "#,
    )
    .bind(ctx.contract_id)
    .bind(ctx.reimbursement_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(map_confirmed_dup)?;

    let (settled, webhook_info) = finalize_link(&mut tx, &ctx).await?;

    tx.commit().await?;

    Ok((
        SuggestionDecision {
            suggestion_id: ctx.reimbursement_id,
            state: "confirmed".into(),
            settled,
        },
        webhook_info,
    ))
}

async fn unlink_contract(
    State(_state): State<AppState>,
    Path(contract_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
) -> Result<Json<SuggestionDecision>, ApiError> {
    let mut conn = tx.acquire().await;
    Ok(Json(do_unlink(&mut **conn, user_id, contract_id).await?))
}

pub async fn do_unlink<'c, A>(
    acquirer: A,
    user_id: Uuid,
    contract_id: Uuid,
) -> Result<SuggestionDecision, ApiError>
where
    A: sqlx::Acquire<'c, Database = sqlx::Postgres>,
{
    let mut tx = acquirer.begin().await?;
    let row: Option<(Option<Uuid>, ContractStatus)> =
        sqlx::query_as("SELECT issuer_user_id, status FROM contracts WHERE id = $1 FOR UPDATE")
            .bind(contract_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (issuer_user_id, status) = row.ok_or_else(ApiError::not_found)?;

    if issuer_user_id != Some(user_id) {
        return Err(ApiError::forbidden());
    }
    let is_finished = status.is_terminal_success();
    if is_finished {
        return Err(ApiError::Conflict(
            "cannot unlink a finished contract; that would unwind a settlement".into(),
        ));
    }

    sqlx::query(
        "UPDATE reimbursements SET contract_id = NULL, updated_at = now() \
         WHERE contract_id = $1 AND status = 'pending'",
    )
    .bind(contract_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'superseded', decided_at = now() \
         WHERE contract_id = $1 AND state = 'confirmed'",
    )
    .bind(contract_id)
    .execute(&mut *tx)
    .await?;
    settlement::recompute_contract_expected_total(&mut tx, contract_id)
        .await
        .map_err(|e| {
            ApiError::internal(anyhow::anyhow!("recompute_contract_expected_total: {e}"))
        })?;

    tx.commit().await?;
    Ok(SuggestionDecision {
        suggestion_id: contract_id,
        state: "unlinked".into(),
        settled: false,
    })
}
