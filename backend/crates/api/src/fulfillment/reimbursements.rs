use axum::{
    extract::{Path, State},
    Json,
};
use domain::{GroupRole, ListDetail, ReimbursementStatus};
use uuid::Uuid;

use crate::{
    errors::ApiError,
    extract::CurrentUser,
    lists::load_list_detail,
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

pub(super) async fn settle_reimbursement(
    State(state): State<AppState>,
    Path(reimb_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<ListDetail>, ApiError> {
    // Load reimbursement (also for permission checks).
    // requester_user_id is nullable for corp-funded rows.
    #[derive(sqlx::FromRow)]
    struct ReimbRow {
        list_id: Uuid,
        requester_user_id: Option<Uuid>,
        status: String,
        contract_id: Option<Uuid>,
    }
    let row: Option<ReimbRow> = sqlx::query_as(
        "SELECT list_id, requester_user_id, status, contract_id \
         FROM reimbursements WHERE id = $1",
    )
    .bind(reimb_id)
    .fetch_optional(&state.pool)
    .await?;

    let r = row.ok_or_else(ApiError::not_found)?;
    let (list_id, requester_user_id, contract_id) = (r.list_id, r.requester_user_id, r.contract_id);
    let reimb_status: ReimbursementStatus = r
        .status
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;

    if reimb_status != ReimbursementStatus::Pending {
        return Err(ApiError::Conflict(format!(
            "reimbursement is already {}",
            r.status
        )));
    }

    // Bound to a contract: ESI is the source of truth, manual settle would
    // race with the worker. Hauler must explicitly unlink first.
    if contract_id.is_some() {
        return Err(ApiError::Conflict(
            "this reimbursement is bound to a contract; unlink it before settling manually".into(),
        ));
    }

    // Check group membership and permissions. Only the requester (whose debt
    // is being settled) or a group owner may settle — list creators have no
    // standing to mark another member's reimbursement as paid.
    let role_str: Option<String> = sqlx::query_scalar(
        "SELECT gm.role \
         FROM lists l \
         JOIN group_memberships gm ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE l.id = $2",
    )
    .bind(user_id)
    .bind(list_id)
    .fetch_optional(&state.pool)
    .await?;

    let role: GroupRole = role_str
        .ok_or_else(ApiError::forbidden)?
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;

    // Corp-funded reimbursements have no user requester; only owners can settle.
    let is_requester = requester_user_id == Some(user_id);
    let is_owner = role == GroupRole::Owner;

    if !is_requester && !is_owner {
        return Err(ApiError::forbidden());
    }

    let mut tx = state.pool.begin().await?;
    super::lock_list(&mut tx, list_id).await?;

    settlement::settle_manual(&mut tx, reimb_id, user_id)
        .await
        .map_err(|e| match e {
            settlement::SettlementError::NotFound => ApiError::not_found(),
            settlement::SettlementError::NotPending(s) => {
                ApiError::Conflict(format!("reimbursement is already {s}"))
            }
            settlement::SettlementError::NotDelivered { count } => ApiError::Conflict(format!(
                "{count} item(s) must be fully bought and delivered before settling"
            )),
            settlement::SettlementError::Db(e) => ApiError::internal(e),
        })?;

    let (list_dest, group_id_for_wh): (Option<String>, Uuid) =
        sqlx::query_as("SELECT destination_label, group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_one(&mut *tx)
            .await?;

    let req_name: String = match requester_user_id {
        Some(uid) => {
            sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
                .bind(uid)
                .fetch_one(&mut *tx)
                .await?
        }
        None => "Corp".into(),
    };
    let (hauler_name, total_isk): (String, rust_decimal::Decimal) = sqlx::query_as(
        "SELECT hu.display_name, r.total_isk FROM reimbursements r \
         JOIN users hu ON hu.id = r.hauler_user_id \
         WHERE r.id = $1",
    )
    .bind(reimb_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    fire_webhook(
        &state,
        group_id_for_wh,
        WebhookEvent::ReimbursementSettled {
            list_destination: list_dest.unwrap_or_else(|| "(unnamed)".into()),
            requester_name: req_name,
            hauler_name,
            total_isk: total_isk.to_string(),
        },
    );

    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

/// Upsert (or recompute) the pending reimbursement row for (list, requester, hauler).
/// Resolves principal IDs from the list's payer_corp_id: corp-funded lists use the
/// corp principal as requester; personal lists use the item requester's user principal.
/// Never touches settled rows.
pub(super) async fn upsert_reimbursement(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
    requester_user_id: Uuid,
    hauler_user_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        WITH meta AS (
            SELECT tip_pct, payer_corp_id FROM lists WHERE id = $1
        ),
        hauler_p AS (
            SELECT id FROM principals WHERE kind = 'user' AND user_id = $3
        ),
        requester_p AS (
            SELECT p.id,
                   (m.payer_corp_id IS NOT NULL)                                        AS is_corp_funded,
                   CASE WHEN m.payer_corp_id IS NULL THEN $2::uuid ELSE NULL END        AS req_user_id
            FROM meta m
            JOIN principals p ON (
                    (m.payer_corp_id IS NULL     AND p.kind = 'user' AND p.user_id = $2)
                 OR (m.payer_corp_id IS NOT NULL AND p.kind = 'corp' AND p.corp_id  = m.payer_corp_id)
            )
        ),
        subtotal AS (
            SELECT COALESCE(SUM(f.qty * f.unit_price_isk), 0) AS subtotal
            FROM fulfillments f
            JOIN list_items   li ON li.id = f.list_item_id
            CROSS JOIN requester_p rp
            WHERE li.list_id      = $1
              AND f.hauler_user_id = $3
              AND f.reversed_at   IS NULL
              AND (rp.is_corp_funded OR li.requested_by_user_id = $2)
        )
        INSERT INTO reimbursements
            (list_id, requester_user_id, hauler_user_id,
             requester_principal_id, hauler_principal_id, is_corp_funded,
             subtotal_isk, tip_isk, total_isk)
        SELECT $1, rp.req_user_id, $3, rp.id, hp.id, rp.is_corp_funded,
               s.subtotal,
               s.subtotal * m.tip_pct,
               s.subtotal * (1 + m.tip_pct)
        FROM subtotal s, meta m, requester_p rp, hauler_p hp
        ON CONFLICT (list_id, requester_principal_id, hauler_principal_id)
            WHERE status <> 'cancelled'
        DO UPDATE
            SET subtotal_isk = EXCLUDED.subtotal_isk,
                tip_isk      = EXCLUDED.tip_isk,
                total_isk    = EXCLUDED.total_isk,
                updated_at   = now()
          WHERE reimbursements.status = 'pending'
        "#,
    )
    .bind(list_id)
    .bind(requester_user_id)
    .bind(hauler_user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
