use axum::{
    routing::{delete, get, patch, post},
    Router,
};
use domain::{ClaimStatus, GroupRole, ListItemStatus};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{errors::ApiError, state::AppState};

pub mod claims;
pub mod fulfillments;
pub mod reimbursements;
pub mod runs;
pub mod tips;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/runs", get(runs::runs))
        .route("/lists/{id}/claims", post(claims::create_claim))
        .route("/claims/{id}/items", post(claims::add_claim_items))
        .route(
            "/claims/{id}/items/{item_id}",
            delete(claims::remove_claim_item),
        )
        .route("/claims/{id}", delete(claims::release_claim))
        .route(
            "/lists/{id}/items/{item_id}/fulfillments",
            post(fulfillments::record_fulfillment),
        )
        .route(
            "/fulfillments/{id}/reverse",
            post(fulfillments::reverse_fulfillment),
        )
        .route(
            "/lists/{id}/items/{item_id}/mark-delivered",
            post(fulfillments::mark_delivered),
        )
        .route("/lists/{id}/tip", patch(tips::set_list_tip))
        .route(
            "/reimbursements/{id}/settle",
            post(reimbursements::settle_reimbursement),
        )
        .route(
            "/groups/{id}/default-tip",
            patch(tips::set_group_default_tip),
        )
}

// ── Shared helpers (visible to all submodules via `super::`) ──────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveredDemotion {
    Forbid,
    Allow,
}

pub(crate) async fn lock_list(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
) -> Result<(), ApiError> {
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
        .bind(list_id)
        .fetch_optional(&mut **tx)
        .await?;
    exists.ok_or_else(ApiError::not_found).map(|_| ())
}

pub(crate) fn validate_tip_pct(v: Decimal) -> Result<(), ApiError> {
    if v < Decimal::ZERO || v > Decimal::ONE {
        return Err(ApiError::BadRequest(
            "tip_pct must be between 0 and 1".into(),
        ));
    }
    Ok(())
}

pub(crate) fn ensure_claim_writable(
    user_id: Uuid,
    hauler_user_id: Uuid,
    role: GroupRole,
    status: ClaimStatus,
) -> Result<(), ApiError> {
    if user_id != hauler_user_id && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }
    if status != ClaimStatus::Active {
        return Err(ApiError::Conflict(format!("claim is {status}, not active")));
    }
    Ok(())
}

/// Recompute open/claimed/bought status for an item based on current fulfillments and claims.
/// Never demotes settled. Only demotes delivered when `demotion == Allow` and the item is
/// no longer fully fulfilled (e.g. after a fulfillment reversal).
pub(crate) async fn recompute_item_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item_id: Uuid,
    demotion: DeliveredDemotion,
) -> Result<ListItemStatus, ApiError> {
    let current: String = sqlx::query_scalar("SELECT status FROM list_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(&mut **tx)
        .await?;

    let current_status: ListItemStatus = current
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;

    if current_status == ListItemStatus::Settled {
        return Ok(current_status);
    }

    if current_status == ListItemStatus::Delivered && demotion == DeliveredDemotion::Forbid {
        return Ok(current_status);
    }

    let (qty_requested, qty_fulfilled, has_active_claim): (i64, i64, bool) = sqlx::query_as(
        "SELECT li.qty_requested, \
                COALESCE((SELECT SUM(qty) FROM fulfillments \
                          WHERE list_item_id = $1 AND reversed_at IS NULL), 0)::bigint, \
                EXISTS(SELECT 1 FROM claim_items WHERE list_item_id = $1 AND active) \
         FROM list_items li WHERE li.id = $1",
    )
    .bind(item_id)
    .fetch_one(&mut **tx)
    .await?;

    let base_status = if qty_fulfilled >= qty_requested {
        ListItemStatus::Bought
    } else if has_active_claim {
        ListItemStatus::Claimed
    } else {
        ListItemStatus::Open
    };

    // Preserve delivered when the item is still fully fulfilled.
    let new_status =
        if current_status == ListItemStatus::Delivered && base_status == ListItemStatus::Bought {
            ListItemStatus::Delivered
        } else {
            base_status
        };

    sqlx::query("UPDATE list_items SET status = $1, qty_fulfilled = $2 WHERE id = $3")
        .bind(new_status.as_str())
        .bind(qty_fulfilled)
        .bind(item_id)
        .execute(&mut **tx)
        .await?;

    Ok(new_status)
}

/// Set-based recompute for many items in one round-trip. Uses Forbid semantics
/// (delivered/settled are never demoted) — used by claim mutations where reversal isn't possible.
pub(crate) async fn recompute_item_statuses_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item_ids: &[Uuid],
) -> Result<(), ApiError> {
    if item_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"
        WITH agg AS (
            SELECT
                li.id,
                COALESCE((
                    SELECT SUM(qty) FROM fulfillments
                    WHERE list_item_id = li.id AND reversed_at IS NULL
                ), 0)::bigint AS qty_fulfilled,
                EXISTS(
                    SELECT 1 FROM claim_items
                    WHERE list_item_id = li.id AND active
                ) AS has_active_claim
            FROM list_items li
            WHERE li.id = ANY($1::uuid[])
        )
        UPDATE list_items li
        SET qty_fulfilled = agg.qty_fulfilled,
            status = CASE
                WHEN li.status IN ('delivered','settled') THEN li.status
                WHEN agg.qty_fulfilled >= li.qty_requested THEN 'bought'
                WHEN agg.has_active_claim THEN 'claimed'
                ELSE 'open'
            END
        FROM agg
        WHERE li.id = agg.id
        "#,
    )
    .bind(item_ids)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) fn is_claim_conflict(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.constraint() == Some("claim_items_one_active"))
}
