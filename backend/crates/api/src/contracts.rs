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
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::ContractStatus;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    extract::{CurrentGroup, CurrentUser},
    state::AppState,
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
    contract_status: String,
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
    state: String,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

async fn list_suggestions(
    State(state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
) -> Result<Json<Vec<SuggestionDto>>, ContractError> {
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
            ru.display_name       AS requester_display_name,
            hu.display_name       AS hauler_display_name,
            r.total_isk           AS reimbursement_total_isk,
            s.score,
            s.exact_match,
            s.state,
            s.created_at,
            s.decided_at
        FROM contract_match_suggestions s
        JOIN contracts c       ON c.id = s.contract_id
        JOIN reimbursements r  ON r.id = s.reimbursement_id
        JOIN lists l           ON l.id = r.list_id
        JOIN users ru          ON ru.id = r.requester_user_id
        JOIN users hu          ON hu.id = r.hauler_user_id
        WHERE l.group_id = $1
          AND r.hauler_user_id = $2
          AND s.state IN ('pending','confirmed')
        ORDER BY s.state, s.created_at DESC
        LIMIT 200
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow)]
struct BoundContractDto {
    contract_id: Uuid,
    esi_contract_id: i64,
    status: String,
    price_isk: Decimal,
    expected_total_isk: Option<Decimal>,
    settlement_delta_isk: Option<Decimal>,
    date_completed: Option<DateTime<Utc>>,
    bound_reimbursement_count: i64,
}

async fn list_contracts(
    State(state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
) -> Result<Json<Vec<BoundContractDto>>, ContractError> {
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
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    Ok(Json(rows))
}

async fn confirm_suggestion(
    State(state): State<AppState>,
    Path(suggestion_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<SuggestionDecision>, ContractError> {
    Ok(Json(do_confirm(&state.pool, user_id, suggestion_id).await?))
}

pub async fn do_confirm(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    suggestion_id: Uuid,
) -> Result<SuggestionDecision, ContractError> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let row: Option<LinkLockRow> = sqlx::query_as(
        r#"
        SELECT s.state         AS suggestion_state,
               s.contract_id,
               c.contract_type,
               c.status        AS contract_status,
               c.issuer_user_id,
               c.assignee_user_id,
               s.reimbursement_id,
               r.status        AS reimbursement_status,
               r.contract_id   AS reimbursement_contract_id,
               r.hauler_user_id,
               r.requester_user_id,
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
    .await
    .map_err(internal)?;
    let row = row.ok_or(ContractError::NotFound)?;

    if row.suggestion_state.as_deref() != Some("pending") {
        return Err(ContractError::Conflict(format!(
            "suggestion is already {}",
            row.suggestion_state.as_deref().unwrap_or("(unknown)")
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

    let settled = finalize_link(&mut tx, &ctx).await?;

    tx.commit().await.map_err(internal)?;
    Ok(SuggestionDecision {
        suggestion_id,
        state: "confirmed".into(),
        settled,
    })
}

/// Shared lock-row for confirm and manual-link. `suggestion_state` is `None`
/// in the manual-link path (the query SELECTs `NULL::text`).
#[derive(sqlx::FromRow)]
struct LinkLockRow {
    suggestion_state: Option<String>,
    contract_id: Uuid,
    contract_type: String,
    contract_status: String,
    issuer_user_id: Option<Uuid>,
    assignee_user_id: Option<Uuid>,
    reimbursement_id: Uuid,
    reimbursement_status: String,
    reimbursement_contract_id: Option<Uuid>,
    hauler_user_id: Uuid,
    requester_user_id: Uuid,
    group_id: Uuid,
}

async fn validate_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &LinkLockRow,
    user_id: Uuid,
) -> Result<(), ContractError> {
    if ctx.contract_type != "item_exchange" {
        return Err(ContractError::Conflict(
            "contract is not item_exchange".into(),
        ));
    }
    if ctx.reimbursement_status != "pending" || ctx.reimbursement_contract_id.is_some() {
        return Err(ContractError::Conflict(
            "reimbursement is no longer eligible".into(),
        ));
    }
    if ctx.issuer_user_id != Some(user_id) || ctx.hauler_user_id != user_id {
        return Err(ContractError::Forbidden);
    }
    if ctx
        .assignee_user_id
        .is_some_and(|a| a != ctx.requester_user_id)
    {
        return Err(ContractError::Conflict(
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
    .await
    .map_err(internal)?;
    if !is_member {
        return Err(ContractError::Forbidden);
    }
    Ok(())
}

/// Bind the reimbursement to the contract, refresh totals, and run
/// settle-via-contract if the contract is already in a terminal-success state.
/// Returns `true` iff the contract finished settling.
async fn finalize_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &LinkLockRow,
) -> Result<bool, ContractError> {
    sqlx::query("UPDATE reimbursements SET contract_id = $1, updated_at = now() WHERE id = $2")
        .bind(ctx.contract_id)
        .bind(ctx.reimbursement_id)
        .execute(&mut **tx)
        .await
        .map_err(internal)?;

    settlement::recompute_contract_expected_total(tx, ctx.contract_id)
        .await
        .map_err(|e| internal(anyhow::anyhow!("recompute_contract_expected_total: {e}")))?;

    let already_finished = ctx
        .contract_status
        .parse::<ContractStatus>()
        .map(|s| s.is_terminal_success())
        .unwrap_or(false);
    if already_finished {
        settlement::settle_via_contract(tx, ctx.contract_id)
            .await
            .map_err(|e| internal(anyhow::anyhow!("settle_via_contract: {e}")))?;
    }
    Ok(already_finished)
}

fn map_confirmed_dup(e: sqlx::Error) -> ContractError {
    match e {
        sqlx::Error::Database(db)
            if db.constraint() == Some("one_confirmed_suggestion_per_reimbursement") =>
        {
            ContractError::Conflict(
                "reimbursement already confirmed against another contract".into(),
            )
        }
        other => internal(other),
    }
}

#[derive(Serialize, Debug)]
pub struct SuggestionDecision {
    pub suggestion_id: Uuid,
    pub state: String,
    pub settled: bool,
}

async fn reject_suggestion(
    State(state): State<AppState>,
    Path(suggestion_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<SuggestionDecision>, ContractError> {
    Ok(Json(do_reject(&state.pool, user_id, suggestion_id).await?))
}

pub async fn do_reject(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    suggestion_id: Uuid,
) -> Result<SuggestionDecision, ContractError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let row: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT s.state, c.issuer_user_id \
         FROM contract_match_suggestions s \
         JOIN contracts c ON c.id = s.contract_id \
         WHERE s.id = $1 FOR UPDATE OF s",
    )
    .bind(suggestion_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let (cur_state, issuer_user_id) = row.ok_or(ContractError::NotFound)?;
    if cur_state != "pending" {
        return Err(ContractError::Conflict(format!(
            "suggestion is already {cur_state}"
        )));
    }
    if issuer_user_id != Some(user_id) {
        return Err(ContractError::Forbidden);
    }
    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'rejected', decided_at = now(), decided_by_user_id = $1 \
         WHERE id = $2",
    )
    .bind(user_id)
    .bind(suggestion_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
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
    Json(body): Json<ManualLinkBody>,
) -> Result<Json<SuggestionDecision>, ContractError> {
    Ok(Json(
        do_manual_link(&state.pool, user_id, contract_id, body.reimbursement_id).await?,
    ))
}

pub async fn do_manual_link(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    contract_id: Uuid,
    reimbursement_id: Uuid,
) -> Result<SuggestionDecision, ContractError> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let row: Option<LinkLockRow> = sqlx::query_as(
        r#"
        SELECT
            NULL::text        AS suggestion_state,
            c.id              AS contract_id,
            c.contract_type,
            c.status          AS contract_status,
            c.issuer_user_id,
            c.assignee_user_id,
            r.id              AS reimbursement_id,
            r.status          AS reimbursement_status,
            r.contract_id     AS reimbursement_contract_id,
            r.hauler_user_id,
            r.requester_user_id,
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
    .await
    .map_err(internal)?;
    let ctx = row.ok_or(ContractError::NotFound)?;
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

    let settled = finalize_link(&mut tx, &ctx).await?;

    tx.commit().await.map_err(internal)?;
    Ok(SuggestionDecision {
        suggestion_id: ctx.reimbursement_id,
        state: "confirmed".into(),
        settled,
    })
}

async fn unlink_contract(
    State(state): State<AppState>,
    Path(contract_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<SuggestionDecision>, ContractError> {
    Ok(Json(do_unlink(&state.pool, user_id, contract_id).await?))
}

pub async fn do_unlink(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    contract_id: Uuid,
) -> Result<SuggestionDecision, ContractError> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let row: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT issuer_user_id, status FROM contracts WHERE id = $1 FOR UPDATE")
            .bind(contract_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;
    let (issuer_user_id, status) = row.ok_or(ContractError::NotFound)?;

    if issuer_user_id != Some(user_id) {
        return Err(ContractError::Forbidden);
    }
    let is_finished = status
        .parse::<ContractStatus>()
        .map(|s| s.is_terminal_success())
        .unwrap_or(false);
    if is_finished {
        return Err(ContractError::Conflict(
            "cannot unlink a finished contract; that would unwind a settlement".into(),
        ));
    }

    sqlx::query(
        "UPDATE reimbursements SET contract_id = NULL, updated_at = now() \
         WHERE contract_id = $1 AND status = 'pending'",
    )
    .bind(contract_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'superseded', decided_at = now() \
         WHERE contract_id = $1 AND state = 'confirmed'",
    )
    .bind(contract_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    settlement::recompute_contract_expected_total(&mut tx, contract_id)
        .await
        .map_err(|e| internal(anyhow::anyhow!("recompute_contract_expected_total: {e}")))?;

    tx.commit().await.map_err(internal)?;
    Ok(SuggestionDecision {
        suggestion_id: contract_id,
        state: "unlinked".into(),
        settled: false,
    })
}

#[derive(Debug)]
pub enum ContractError {
    NotFound,
    Forbidden,
    Conflict(String),
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> ContractError {
    ContractError::Internal(e.into())
}

impl IntoResponse for ContractError {
    fn into_response(self) -> Response {
        match self {
            ContractError::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            ContractError::Forbidden => {
                (StatusCode::FORBIDDEN, "you cannot perform this action").into_response()
            }
            ContractError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
            ContractError::Internal(e) => {
                tracing::error!(error = ?e, "contracts handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
