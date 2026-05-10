use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{GroupRole, ListDetail, ListItemStatus, ListStatus, ReimbursementStatus};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    errors::ApiError,
    extract::{CurrentList, CurrentUser},
    lists::load_list_detail,
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

#[derive(Deserialize)]
pub(super) struct RecordFulfillmentBody {
    qty: i64,
    unit_price_isk: Decimal,
    bought_at_market_id: Option<Uuid>,
    bought_at_note: Option<String>,
    hauler_character_id: Option<Uuid>,
    claim_id: Option<Uuid>,
}

pub(super) async fn record_fulfillment(
    State(state): State<AppState>,
    Path((_list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
    Json(body): Json<RecordFulfillmentBody>,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;
    if body.qty <= 0 {
        return Err(ApiError::BadRequest("qty must be positive".into()));
    }
    if body.unit_price_isk < Decimal::ZERO {
        return Err(ApiError::BadRequest("unit_price_isk must be >= 0".into()));
    }
    // Validate market or note
    if body.bought_at_market_id.is_none() {
        match &body.bought_at_note {
            None => {
                return Err(ApiError::BadRequest(
                    "bought_at_note is required when no market is specified".into(),
                ))
            }
            Some(n) if n.trim().is_empty() => {
                return Err(ApiError::BadRequest(
                    "bought_at_note must not be blank".into(),
                ))
            }
            _ => {}
        }
    }

    let mut tx = state.pool.begin().await?;
    super::lock_list(&mut tx, list_id).await?;

    // Load the item
    let item_row: Option<(i64, i64, Uuid)> = sqlx::query_as(
        "SELECT qty_requested, qty_fulfilled, requested_by_user_id \
         FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
    )
    .bind(item_id)
    .bind(list_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (qty_requested, qty_fulfilled, requested_by_user_id) =
        item_row.ok_or_else(ApiError::not_found)?;

    if body.qty + qty_fulfilled > qty_requested {
        return Err(ApiError::Conflict(format!(
            "fulfillment qty {} would exceed requested {} (already fulfilled: {})",
            body.qty, qty_requested, qty_fulfilled
        )));
    }

    // Reject if a non-pending reimbursement already exists for this
    // (list, requester-principal, hauler-principal) cycle: we'd have no place
    // to attach the new fulfillment, and reversal of past fulfillments is also
    // blocked once the row is settled. Resolve principals from the list's payer
    // so the lookup key matches what `upsert_reimbursement` will write.
    let existing_reimb_status: Option<String> = sqlx::query_scalar(
        r#"
        SELECT r.status FROM reimbursements r
        WHERE r.list_id = $1
          AND r.requester_principal_id = (
                SELECT p.id FROM lists l
                JOIN principals p ON (
                    (l.payer_corp_id IS NULL     AND p.kind = 'user' AND p.user_id = $2)
                 OR (l.payer_corp_id IS NOT NULL AND p.kind = 'corp' AND p.corp_id = l.payer_corp_id)
                )
                WHERE l.id = $1
                LIMIT 1
          )
          AND r.hauler_principal_id = (
                SELECT id FROM principals WHERE kind = 'user' AND user_id = $3
          )
        "#,
    )
    .bind(list_id)
    .bind(requested_by_user_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(s) = existing_reimb_status {
        let rs: ReimbursementStatus = s
            .parse()
            .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;
        if rs != ReimbursementStatus::Pending {
            return Err(ApiError::Conflict(format!(
                "reimbursement for this requester is already {s}; \
                 cannot record additional fulfillments"
            )));
        }
    }

    // Validate market and hauler character in one round-trip; nulls short-circuit to true.
    if body.bought_at_market_id.is_some() || body.hauler_character_id.is_some() {
        let (market_exists, char_owned): (bool, bool) = sqlx::query_as(
            r#"
            SELECT
                ($1::uuid IS NULL OR EXISTS(SELECT 1 FROM markets WHERE id = $1)) AS market_exists,
                ($2::uuid IS NULL OR EXISTS(
                    SELECT 1 FROM characters WHERE id = $2 AND user_id = $3
                )) AS char_owned
            "#,
        )
        .bind(body.bought_at_market_id)
        .bind(body.hauler_character_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;

        if !market_exists {
            return Err(ApiError::BadRequest(
                "bought_at_market_id does not exist".into(),
            ));
        }
        if !char_owned {
            return Err(ApiError::BadRequest(
                "hauler_character_id does not belong to you".into(),
            ));
        }
        // Note: a market that exists but isn't in list_markets is a soft warning, not an error.
    }

    // Permission gate: check active claim for this item
    let active_claim: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT c.id, c.hauler_user_id \
         FROM claim_items ci \
         JOIN claims c ON c.id = ci.claim_id \
         WHERE ci.list_item_id = $1 AND ci.active",
    )
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some((active_claim_id, active_hauler_user_id)) = active_claim {
        if user_id != active_hauler_user_id && role != GroupRole::Owner {
            return Err(ApiError::forbidden());
        }
        // Validate explicit claim_id if provided
        if let Some(explicit_claim_id) = body.claim_id {
            if explicit_claim_id != active_claim_id {
                return Err(ApiError::BadRequest(
                    "claim_id does not match the active claim for this item".into(),
                ));
            }
        }
    }
    // If no active claim, any group member may fulfill (role check via CurrentList)

    // Only attach a fulfillment to the currently-active claim. A stale claim_id
    // from the client is ignored when no claim is active for this item.
    let effective_claim_id: Option<Uuid> = active_claim.map(|(id, _)| id);

    sqlx::query(
        "INSERT INTO fulfillments \
         (list_item_id, claim_id, hauler_user_id, hauler_character_id, \
          qty, unit_price_isk, bought_at_market_id, bought_at_note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(item_id)
    .bind(effective_claim_id)
    .bind(user_id)
    .bind(body.hauler_character_id)
    .bind(body.qty)
    .bind(body.unit_price_isk)
    .bind(body.bought_at_market_id)
    .bind(body.bought_at_note.as_deref())
    .execute(&mut *tx)
    .await?;

    let new_status =
        super::recompute_item_status(&mut tx, item_id, super::DeliveredDemotion::Forbid).await?;

    // If item flipped to bought and there's an active claim, check if all claim items are done
    if new_status == ListItemStatus::Bought || new_status == ListItemStatus::Settled {
        if let Some((claim_id, _)) = active_claim {
            let all_done: bool = sqlx::query_scalar(
                r#"
                SELECT NOT EXISTS (
                    SELECT 1 FROM claim_items ci
                    JOIN list_items li ON li.id = ci.list_item_id
                    WHERE ci.claim_id = $1
                      AND li.status NOT IN ('bought','delivered','settled')
                )
                "#,
            )
            .bind(claim_id)
            .fetch_one(&mut *tx)
            .await?;

            if all_done {
                sqlx::query(
                    "UPDATE claims SET status = 'completed' WHERE id = $1 AND status = 'active'",
                )
                .bind(claim_id)
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    super::reimbursements::upsert_reimbursement(&mut tx, list_id, requested_by_user_id, user_id)
        .await?;

    tx.commit().await?;
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn reverse_fulfillment(
    State(state): State<AppState>,
    Path(fulfillment_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<ListDetail>, ApiError> {
    // Load fulfillment + its list context to get the group membership
    type ReverseRow = (Uuid, Uuid, Uuid, Option<DateTime<Utc>>, String);
    let row: Option<ReverseRow> = sqlx::query_as(
        "SELECT f.hauler_user_id, li.list_id, li.requested_by_user_id, f.reversed_at, l.status \
         FROM fulfillments f \
         JOIN list_items li ON li.id = f.list_item_id \
         JOIN lists l ON l.id = li.list_id \
         WHERE f.id = $1",
    )
    .bind(fulfillment_id)
    .fetch_optional(&state.pool)
    .await?;

    let (hauler_user_id, list_id, requested_by_user_id, reversed_at, list_status_str) =
        row.ok_or_else(ApiError::not_found)?;

    let list_status: ListStatus = list_status_str
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;
    if list_status == ListStatus::Archived {
        return Err(ApiError::Conflict(
            "list is archived; no changes can be made".into(),
        ));
    }

    if reversed_at.is_some() {
        return Err(ApiError::BadRequest(
            "fulfillment is already reversed".into(),
        ));
    }

    // Check group membership to determine role
    let role_str: Option<String> = sqlx::query_scalar(
        "SELECT gm.role FROM lists l \
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

    if user_id != hauler_user_id && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut tx = state.pool.begin().await?;
    super::lock_list(&mut tx, list_id).await?;

    // Check reimbursement is not settled; resolve principals from the list's
    // payer so the lookup key matches what `upsert_reimbursement` wrote.
    let reimb_status: Option<String> = sqlx::query_scalar(
        r#"
        SELECT r.status FROM reimbursements r
        WHERE r.list_id = $1
          AND r.requester_principal_id = (
                SELECT p.id FROM lists l
                JOIN principals p ON (
                    (l.payer_corp_id IS NULL     AND p.kind = 'user' AND p.user_id = $2)
                 OR (l.payer_corp_id IS NOT NULL AND p.kind = 'corp' AND p.corp_id = l.payer_corp_id)
                )
                WHERE l.id = $1
                LIMIT 1
          )
          AND r.hauler_principal_id = (
                SELECT id FROM principals WHERE kind = 'user' AND user_id = $3
          )
        "#,
    )
    .bind(list_id)
    .bind(requested_by_user_id)
    .bind(hauler_user_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(s) = reimb_status {
        let rs: ReimbursementStatus = s
            .parse()
            .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;
        if rs != ReimbursementStatus::Pending {
            return Err(ApiError::Conflict(
                "cannot reverse a fulfillment whose reimbursement is already settled".into(),
            ));
        }
    }

    let item_id: Uuid = sqlx::query_scalar(
        "UPDATE fulfillments SET reversed_at = now() WHERE id = $1 RETURNING list_item_id",
    )
    .bind(fulfillment_id)
    .fetch_one(&mut *tx)
    .await?;

    super::recompute_item_status(&mut tx, item_id, super::DeliveredDemotion::Allow).await?;
    super::reimbursements::upsert_reimbursement(
        &mut tx,
        list_id,
        requested_by_user_id,
        hauler_user_id,
    )
    .await?;

    tx.commit().await?;
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn mark_delivered(
    State(state): State<AppState>,
    Path((_list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;
    let mut tx = state.pool.begin().await?;
    super::lock_list(&mut tx, list_id).await?;

    // Check current status and last hauler
    let row: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT li.status, \
                (SELECT f.hauler_user_id FROM fulfillments f \
                 WHERE f.list_item_id = li.id AND f.reversed_at IS NULL \
                 ORDER BY f.bought_at DESC LIMIT 1) AS last_hauler \
         FROM list_items li WHERE li.id = $1 AND li.list_id = $2 FOR UPDATE",
    )
    .bind(item_id)
    .bind(list_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (status_str, last_hauler) = row.ok_or_else(ApiError::not_found)?;
    let item_status: ListItemStatus = status_str
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;

    if item_status != ListItemStatus::Bought {
        return Err(ApiError::BadRequest(format!(
            "item must be in 'bought' status to mark delivered (currently: {status_str})"
        )));
    }

    if user_id != last_hauler.unwrap_or(Uuid::nil()) && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    sqlx::query("UPDATE list_items SET status = 'delivered' WHERE id = $1")
        .bind(item_id)
        .execute(&mut *tx)
        .await?;

    let all_done: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(\
             SELECT 1 FROM list_items \
             WHERE list_id = $1 AND status NOT IN ('delivered','settled')\
         )",
    )
    .bind(list_id)
    .fetch_one(&mut *tx)
    .await?;

    let hauler_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    let (list_dest, group_id_for_wh): (Option<String>, Uuid) =
        sqlx::query_as("SELECT destination_label, group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;

    if all_done {
        fire_webhook(
            &state,
            group_id_for_wh,
            WebhookEvent::ListDelivered {
                list_destination: list_dest.unwrap_or_else(|| "(unnamed)".into()),
                hauler_name,
            },
        );
    }

    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}
