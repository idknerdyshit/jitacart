use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{
    ClaimStatus, GroupRole, ListDetail, ListItemStatus, ReimbursementStatus, RunMarketRef,
    RunSummary,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    extract::{CurrentClaim, CurrentGroup, CurrentList, CurrentUser},
    lists::load_list_detail,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/runs", get(runs))
        .route("/lists/{id}/claims", post(create_claim))
        .route("/claims/{id}/items", post(add_claim_items))
        .route("/claims/{id}/items/{item_id}", delete(remove_claim_item))
        .route("/claims/{id}", delete(release_claim))
        .route(
            "/lists/{id}/items/{item_id}/fulfillments",
            post(record_fulfillment),
        )
        .route("/fulfillments/{id}/reverse", post(reverse_fulfillment))
        .route(
            "/lists/{id}/items/{item_id}/mark-delivered",
            post(mark_delivered),
        )
        .route("/lists/{id}/tip", patch(set_list_tip))
        .route("/reimbursements/{id}/settle", post(settle_reimbursement))
        .route("/groups/{id}/default-tip", patch(set_group_default_tip))
}

// ── Request bodies ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateClaimBody {
    item_ids: Vec<Uuid>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct AddClaimItemsBody {
    item_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
struct RecordFulfillmentBody {
    qty: i64,
    unit_price_isk: Decimal,
    bought_at_market_id: Option<Uuid>,
    bought_at_note: Option<String>,
    hauler_character_id: Option<Uuid>,
    claim_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct SetTipBody {
    tip_pct: Decimal,
}

// ── Runs ──────────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    destination_label: Option<String>,
    status: String,
    created_at: DateTime<Utc>,
    total_estimate_isk: Decimal,
    items_open: i64,
    items_claimed: i64,
    items_bought: i64,
    items_delivered: i64,
    items_settled: i64,
    claimed_by_me: bool,
    my_active_claim_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct RunMarketRow {
    list_id: Uuid,
    market_id: Uuid,
    short_label: Option<String>,
    is_primary: bool,
}

async fn runs(
    State(state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
) -> Result<Json<Vec<RunSummary>>, FulfillmentError> {
    let rows: Vec<RunRow> = sqlx::query_as(
        r#"
        WITH item_agg AS (
            SELECT
                li.list_id,
                COUNT(*) FILTER (WHERE li.status = 'open')      AS items_open,
                COUNT(*) FILTER (WHERE li.status = 'claimed')   AS items_claimed,
                COUNT(*) FILTER (WHERE li.status = 'bought')    AS items_bought,
                COUNT(*) FILTER (WHERE li.status = 'delivered') AS items_delivered,
                COUNT(*) FILTER (WHERE li.status = 'settled')   AS items_settled
            FROM list_items li
            JOIN lists l ON l.id = li.list_id
            WHERE l.group_id = $1 AND l.status = 'open'
            GROUP BY li.list_id
        ),
        my_claim AS (
            SELECT DISTINCT ON (c.list_id)
                   c.list_id,
                   c.id AS my_active_claim_id
            FROM claims c
            JOIN lists l ON l.id = c.list_id
            WHERE l.group_id = $1 AND l.status = 'open'
              AND c.hauler_user_id = $2 AND c.status = 'active'
            ORDER BY c.list_id, c.created_at DESC, c.id DESC
        )
        SELECT
            l.id,
            l.destination_label,
            l.status,
            l.created_at,
            l.total_estimate_isk,
            COALESCE(ia.items_open,      0) AS items_open,
            COALESCE(ia.items_claimed,   0) AS items_claimed,
            COALESCE(ia.items_bought,    0) AS items_bought,
            COALESCE(ia.items_delivered, 0) AS items_delivered,
            COALESCE(ia.items_settled,   0) AS items_settled,
            (mc.my_active_claim_id IS NOT NULL) AS claimed_by_me,
            mc.my_active_claim_id            AS my_active_claim_id
        FROM lists l
        LEFT JOIN item_agg ia ON ia.list_id = l.id
        LEFT JOIN my_claim mc ON mc.list_id = l.id
        WHERE l.group_id = $1
          AND l.status = 'open'
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    if rows.is_empty() {
        return Ok(Json(vec![]));
    }

    let list_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let market_rows: Vec<RunMarketRow> = sqlx::query_as(
        r#"
        SELECT lm.list_id, m.id AS market_id, m.short_label, lm.is_primary
        FROM list_markets lm
        JOIN markets m ON m.id = lm.market_id
        WHERE lm.list_id = ANY($1::uuid[])
        ORDER BY lm.list_id, lm.is_primary DESC, m.short_label
        "#,
    )
    .bind(&list_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    let mut markets_by_list: std::collections::HashMap<Uuid, Vec<RunMarketRef>> =
        std::collections::HashMap::new();
    for mr in market_rows {
        markets_by_list
            .entry(mr.list_id)
            .or_default()
            .push(RunMarketRef {
                market_id: mr.market_id,
                short_label: mr.short_label,
                is_primary: mr.is_primary,
            });
    }

    let summaries = rows
        .into_iter()
        .map(|r| {
            let status = r
                .status
                .parse::<domain::ListStatus>()
                .map_err(|e| internal(anyhow::anyhow!(e)))?;
            Ok(RunSummary {
                list_id: r.id,
                destination_label: r.destination_label,
                status,
                created_at: r.created_at,
                accepted_markets: markets_by_list.remove(&r.id).unwrap_or_default(),
                items_open: r.items_open,
                items_claimed: r.items_claimed,
                items_bought: r.items_bought,
                items_delivered: r.items_delivered,
                items_settled: r.items_settled,
                total_estimate_isk: r.total_estimate_isk,
                claimed_by_me: r.claimed_by_me,
                my_active_claim_id: r.my_active_claim_id,
            })
        })
        .collect::<Result<Vec<_>, FulfillmentError>>()?;

    Ok(Json(summaries))
}

// ── Claims ────────────────────────────────────────────────────────────────────

async fn create_claim(
    State(state): State<AppState>,
    CurrentList {
        list_id,
        user_id,
        role,
        ..
    }: CurrentList,
    Json(body): Json<CreateClaimBody>,
) -> Result<Json<ListDetail>, FulfillmentError> {
    if body.item_ids.is_empty() {
        return Err(FulfillmentError::BadRequest(
            "item_ids must not be empty".into(),
        ));
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    validate_claimable_items(&mut tx, list_id, &body.item_ids).await?;

    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (list_id, hauler_user_id, note) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(list_id)
    .bind(user_id)
    .bind(body.note.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    insert_claim_items(&mut tx, claim_id, &body.item_ids).await?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn add_claim_items(
    State(state): State<AppState>,
    CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    }: CurrentClaim,
    Json(body): Json<AddClaimItemsBody>,
) -> Result<Json<ListDetail>, FulfillmentError> {
    ensure_claim_writable(user_id, hauler_user_id, role, status)?;
    if body.item_ids.is_empty() {
        return Err(FulfillmentError::BadRequest(
            "item_ids must not be empty".into(),
        ));
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    validate_claimable_items(&mut tx, list_id, &body.item_ids).await?;

    insert_claim_items(&mut tx, claim_id, &body.item_ids).await?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn remove_claim_item(
    State(state): State<AppState>,
    Path((_claim_id, item_id)): Path<(Uuid, Uuid)>,
    CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    }: CurrentClaim,
) -> Result<Json<ListDetail>, FulfillmentError> {
    ensure_claim_writable(user_id, hauler_user_id, role, status)?;

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    let r = sqlx::query("DELETE FROM claim_items WHERE claim_id = $1 AND list_item_id = $2")
        .bind(claim_id)
        .bind(item_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

    if r.rows_affected() == 0 {
        return Err(FulfillmentError::NotFound);
    }

    recompute_item_status(&mut tx, item_id, DeliveredDemotion::Forbid).await?;
    tx.commit().await.map_err(internal)?;

    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn release_claim(
    State(state): State<AppState>,
    CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    }: CurrentClaim,
) -> Result<Json<ListDetail>, FulfillmentError> {
    ensure_claim_writable(user_id, hauler_user_id, role, status)?;

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    // Load items before releasing so we can recompute them
    let item_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT list_item_id FROM claim_items WHERE claim_id = $1")
            .bind(claim_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(internal)?;

    sqlx::query("UPDATE claims SET status = 'released', released_at = now() WHERE id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    // Trigger fires and flips claim_items.active = false

    recompute_item_statuses_bulk(&mut tx, &item_ids).await?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

// ── Fulfillments ──────────────────────────────────────────────────────────────

async fn record_fulfillment(
    State(state): State<AppState>,
    Path((_list_id, item_id)): Path<(Uuid, Uuid)>,
    CurrentList {
        list_id,
        user_id,
        role,
        ..
    }: CurrentList,
    Json(body): Json<RecordFulfillmentBody>,
) -> Result<Json<ListDetail>, FulfillmentError> {
    if body.qty <= 0 {
        return Err(FulfillmentError::BadRequest("qty must be positive".into()));
    }
    if body.unit_price_isk < Decimal::ZERO {
        return Err(FulfillmentError::BadRequest(
            "unit_price_isk must be >= 0".into(),
        ));
    }
    // Validate market or note
    if body.bought_at_market_id.is_none() {
        match &body.bought_at_note {
            None => {
                return Err(FulfillmentError::BadRequest(
                    "bought_at_note is required when no market is specified".into(),
                ))
            }
            Some(n) if n.trim().is_empty() => {
                return Err(FulfillmentError::BadRequest(
                    "bought_at_note must not be blank".into(),
                ))
            }
            _ => {}
        }
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    // Load the item
    let item_row: Option<(i64, i64, Uuid)> = sqlx::query_as(
        "SELECT qty_requested, qty_fulfilled, requested_by_user_id \
         FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
    )
    .bind(item_id)
    .bind(list_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let (qty_requested, qty_fulfilled, requested_by_user_id) =
        item_row.ok_or(FulfillmentError::NotFound)?;

    if body.qty + qty_fulfilled > qty_requested {
        return Err(FulfillmentError::Conflict(format!(
            "fulfillment qty {} would exceed requested {} (already fulfilled: {})",
            body.qty, qty_requested, qty_fulfilled
        )));
    }

    // Reject if a non-pending reimbursement already exists for this
    // (list, requester, hauler) cycle: we'd have no place to attach the new
    // fulfillment, and reversal of past fulfillments is also blocked once
    // the row is settled. See `upsert_reimbursement` and `reverse_fulfillment`.
    let existing_reimb_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM reimbursements \
         WHERE list_id = $1 AND requester_user_id = $2 AND hauler_user_id = $3",
    )
    .bind(list_id)
    .bind(requested_by_user_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    if let Some(s) = existing_reimb_status {
        let rs: ReimbursementStatus = s
            .parse()
            .map_err(|e: String| internal(anyhow::anyhow!(e)))?;
        if rs != ReimbursementStatus::Pending {
            return Err(FulfillmentError::Conflict(format!(
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
        .await
        .map_err(internal)?;

        if !market_exists {
            return Err(FulfillmentError::BadRequest(
                "bought_at_market_id does not exist".into(),
            ));
        }
        if !char_owned {
            return Err(FulfillmentError::BadRequest(
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
    .await
    .map_err(internal)?;

    if let Some((active_claim_id, active_hauler_user_id)) = active_claim {
        if user_id != active_hauler_user_id && role != GroupRole::Owner {
            return Err(FulfillmentError::Forbidden);
        }
        // Validate explicit claim_id if provided
        if let Some(explicit_claim_id) = body.claim_id {
            if explicit_claim_id != active_claim_id {
                return Err(FulfillmentError::BadRequest(
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
    .await
    .map_err(internal)?;

    let new_status = recompute_item_status(&mut tx, item_id, DeliveredDemotion::Forbid).await?;

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
            .await
            .map_err(internal)?;

            if all_done {
                sqlx::query(
                    "UPDATE claims SET status = 'completed' WHERE id = $1 AND status = 'active'",
                )
                .bind(claim_id)
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
            }
        }
    }

    upsert_reimbursement(&mut tx, list_id, requested_by_user_id, user_id).await?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn reverse_fulfillment(
    State(state): State<AppState>,
    Path(fulfillment_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<ListDetail>, FulfillmentError> {
    // Load fulfillment + its list context to get the group membership
    let row: Option<(Uuid, Uuid, Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT f.hauler_user_id, li.list_id, li.requested_by_user_id, f.reversed_at \
         FROM fulfillments f \
         JOIN list_items li ON li.id = f.list_item_id \
         WHERE f.id = $1",
    )
    .bind(fulfillment_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let (hauler_user_id, list_id, requested_by_user_id, reversed_at) =
        row.ok_or(FulfillmentError::NotFound)?;

    if reversed_at.is_some() {
        return Err(FulfillmentError::BadRequest(
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
    .await
    .map_err(internal)?;

    let role: GroupRole = role_str
        .ok_or(FulfillmentError::Forbidden)?
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

    if user_id != hauler_user_id && role != GroupRole::Owner {
        return Err(FulfillmentError::Forbidden);
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    // Check reimbursement is not settled
    let reimb_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM reimbursements \
         WHERE list_id = $1 AND requester_user_id = $2 AND hauler_user_id = $3",
    )
    .bind(list_id)
    .bind(requested_by_user_id)
    .bind(hauler_user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    if let Some(s) = reimb_status {
        let rs: ReimbursementStatus = s
            .parse()
            .map_err(|e: String| internal(anyhow::anyhow!(e)))?;
        if rs != ReimbursementStatus::Pending {
            return Err(FulfillmentError::Conflict(
                "cannot reverse a fulfillment whose reimbursement is already settled".into(),
            ));
        }
    }

    let item_id: Uuid = sqlx::query_scalar(
        "UPDATE fulfillments SET reversed_at = now() WHERE id = $1 RETURNING list_item_id",
    )
    .bind(fulfillment_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    recompute_item_status(&mut tx, item_id, DeliveredDemotion::Allow).await?;
    upsert_reimbursement(&mut tx, list_id, requested_by_user_id, hauler_user_id).await?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn mark_delivered(
    State(state): State<AppState>,
    Path((_list_id, item_id)): Path<(Uuid, Uuid)>,
    CurrentList {
        list_id,
        user_id,
        role,
        ..
    }: CurrentList,
) -> Result<Json<ListDetail>, FulfillmentError> {
    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

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
    .await
    .map_err(internal)?;

    let (status_str, last_hauler) = row.ok_or(FulfillmentError::NotFound)?;
    let item_status: ListItemStatus = status_str
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

    if item_status != ListItemStatus::Bought {
        return Err(FulfillmentError::BadRequest(format!(
            "item must be in 'bought' status to mark delivered (currently: {status_str})"
        )));
    }

    if user_id != last_hauler.unwrap_or(Uuid::nil()) && role != GroupRole::Owner {
        return Err(FulfillmentError::Forbidden);
    }

    sqlx::query("UPDATE list_items SET status = 'delivered' WHERE id = $1")
        .bind(item_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

// ── Tip management ────────────────────────────────────────────────────────────

async fn set_list_tip(
    State(state): State<AppState>,
    CurrentList {
        list_id,
        user_id,
        role,
        created_by_user_id,
        ..
    }: CurrentList,
    Json(body): Json<SetTipBody>,
) -> Result<Json<ListDetail>, FulfillmentError> {
    validate_tip_pct(body.tip_pct)?;

    let is_creator = user_id == created_by_user_id;
    if !is_creator && role != GroupRole::Owner {
        return Err(FulfillmentError::Forbidden);
    }

    // Creator is locked out once any fulfillment exists; owner can always edit
    if is_creator && role != GroupRole::Owner {
        let has_fulfillments: bool = sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM fulfillments f \
                JOIN list_items li ON li.id = f.list_item_id \
                WHERE li.list_id = $1 AND f.reversed_at IS NULL \
            )",
        )
        .bind(list_id)
        .fetch_one(&state.pool)
        .await
        .map_err(internal)?;
        if has_fulfillments {
            return Err(FulfillmentError::Forbidden);
        }
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    sqlx::query("UPDATE lists SET tip_pct = $1 WHERE id = $2")
        .bind(body.tip_pct)
        .bind(list_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

    // Recompute all pending reimbursements for this list
    sqlx::query(
        "UPDATE reimbursements \
         SET tip_isk   = subtotal_isk * $1, \
             total_isk = subtotal_isk * (1 + $1), \
             updated_at = now() \
         WHERE list_id = $2 AND status = 'pending'",
    )
    .bind(body.tip_pct)
    .bind(list_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

async fn set_group_default_tip(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    Json(body): Json<SetTipBody>,
) -> Result<Json<domain::Group>, FulfillmentError> {
    if role != GroupRole::Owner {
        return Err(FulfillmentError::Forbidden);
    }
    validate_tip_pct(body.tip_pct)?;

    let row: Option<GroupRow> = sqlx::query_as(
        "UPDATE groups SET default_tip_pct = $1 WHERE id = $2 \
         RETURNING id, name, invite_code, created_by_user_id, created_at, default_tip_pct",
    )
    .bind(body.tip_pct)
    .bind(group_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let group = row.ok_or(FulfillmentError::NotFound)?.into_group();
    Ok(Json(group))
}

// ── Reimbursements ────────────────────────────────────────────────────────────

async fn settle_reimbursement(
    State(state): State<AppState>,
    Path(reimb_id): Path<Uuid>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<ListDetail>, FulfillmentError> {
    // Load reimbursement
    let row: Option<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT list_id, requester_user_id, hauler_user_id, status \
         FROM reimbursements WHERE id = $1",
    )
    .bind(reimb_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let (list_id, requester_user_id, hauler_user_id, status_str) =
        row.ok_or(FulfillmentError::NotFound)?;
    let reimb_status: ReimbursementStatus = status_str
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

    if reimb_status != ReimbursementStatus::Pending {
        return Err(FulfillmentError::Conflict(format!(
            "reimbursement is already {status_str}"
        )));
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
    .await
    .map_err(internal)?;

    let role: GroupRole = role_str
        .ok_or(FulfillmentError::Forbidden)?
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

    let is_requester = user_id == requester_user_id;
    let is_owner = role == GroupRole::Owner;

    if !is_requester && !is_owner {
        return Err(FulfillmentError::Forbidden);
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    lock_list(&mut tx, list_id).await?;

    // Re-check status under lock
    let locked_status_str: String =
        sqlx::query_scalar("SELECT status FROM reimbursements WHERE id = $1 FOR UPDATE")
            .bind(reimb_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal)?;
    let locked_status: ReimbursementStatus = locked_status_str
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

    if locked_status != ReimbursementStatus::Pending {
        return Err(FulfillmentError::Conflict(format!(
            "reimbursement is already {locked_status_str}"
        )));
    }

    // Verify every item this hauler has touched for this requester is fully
    // delivered (or already settled). This catches both `bought` items that
    // haven't been marked delivered AND partial fulfillments where the item
    // is still `open`/`claimed` because qty_fulfilled < qty_requested.
    let not_delivered: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(DISTINCT li.id)
        FROM list_items li
        WHERE li.list_id = $1
          AND li.requested_by_user_id = $2
          AND li.status NOT IN ('delivered', 'settled')
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = $3
                AND f.reversed_at IS NULL
          )
        "#,
    )
    .bind(list_id)
    .bind(requester_user_id)
    .bind(hauler_user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    if not_delivered > 0 {
        return Err(FulfillmentError::Conflict(format!(
            "{not_delivered} item(s) must be fully bought and delivered before settling"
        )));
    }

    // Mark reimbursement settled
    sqlx::query(
        "UPDATE reimbursements \
         SET status = 'settled', settled_at = now(), settled_by_user_id = $1 \
         WHERE id = $2",
    )
    .bind(user_id)
    .bind(reimb_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    // Bulk-flip delivered items to settled — only if no other pending reimbursement covers them
    sqlx::query(
        r#"
        UPDATE list_items li
        SET status = 'settled'
        WHERE li.list_id = $1
          AND li.requested_by_user_id = $2
          AND li.status = 'delivered'
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = $3
                AND f.reversed_at IS NULL
          )
          AND NOT EXISTS (
              SELECT 1 FROM reimbursements r
              JOIN fulfillments f2
                ON f2.list_item_id = li.id
               AND f2.hauler_user_id = r.hauler_user_id
               AND f2.reversed_at IS NULL
              WHERE r.list_id = $1
                AND r.requester_user_id = $2
                AND r.hauler_user_id <> $3
                AND r.status = 'pending'
          )
        "#,
    )
    .bind(list_id)
    .bind(requester_user_id)
    .bind(hauler_user_id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;

    tx.commit().await.map_err(internal)?;
    let detail = load_list_detail(&state, list_id, user_id, role)
        .await
        .map_err(list_err)?;
    Ok(Json(detail))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveredDemotion {
    Forbid,
    Allow,
}

fn ensure_claim_writable(
    user_id: Uuid,
    hauler_user_id: Uuid,
    role: GroupRole,
    status: ClaimStatus,
) -> Result<(), FulfillmentError> {
    if user_id != hauler_user_id && role != GroupRole::Owner {
        return Err(FulfillmentError::Forbidden);
    }
    if status != ClaimStatus::Active {
        return Err(FulfillmentError::Conflict(format!(
            "claim is {status}, not active"
        )));
    }
    Ok(())
}

fn validate_tip_pct(v: Decimal) -> Result<(), FulfillmentError> {
    if v < Decimal::ZERO || v > Decimal::ONE {
        return Err(FulfillmentError::BadRequest(
            "tip_pct must be between 0 and 1".into(),
        ));
    }
    Ok(())
}

/// Verify the given item ids belong to the list and are all in `open` status.
/// Items in any other status (claimed/bought/delivered/settled) cannot be
/// added to a claim; the unique-active-claim index already prevents
/// double-active claims, but completed work shouldn't be re-claimed at all.
async fn validate_claimable_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
    item_ids: &[Uuid],
) -> Result<(), FulfillmentError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM list_items \
         WHERE id = ANY($1::uuid[]) AND list_id = $2",
    )
    .bind(item_ids)
    .bind(list_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(internal)?;

    if rows.len() != item_ids.len() {
        return Err(FulfillmentError::BadRequest(
            "one or more item_ids do not belong to this list".into(),
        ));
    }

    if let Some((_, bad)) = rows.iter().find(|(_, s)| s != "open") {
        return Err(FulfillmentError::Conflict(format!(
            "cannot claim item with status '{bad}'; only open items may be claimed"
        )));
    }

    Ok(())
}

async fn insert_claim_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claim_id: Uuid,
    item_ids: &[Uuid],
) -> Result<(), FulfillmentError> {
    if item_ids.is_empty() {
        return Ok(());
    }
    match sqlx::query(
        "INSERT INTO claim_items (claim_id, list_item_id) \
         SELECT $1, item_id FROM unnest($2::uuid[]) AS t(item_id)",
    )
    .bind(claim_id)
    .bind(item_ids)
    .execute(&mut **tx)
    .await
    {
        Ok(_) => {}
        Err(e) if is_claim_conflict(&e) => {
            return Err(FulfillmentError::Conflict(
                "one or more items are already claimed".into(),
            ));
        }
        Err(e) => return Err(internal(e)),
    }
    recompute_item_statuses_bulk(tx, item_ids).await?;
    Ok(())
}

async fn lock_list(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
) -> Result<(), FulfillmentError> {
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
        .bind(list_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(internal)?;
    exists.ok_or(FulfillmentError::NotFound).map(|_| ())
}

/// Recompute open/claimed/bought status for an item based on current fulfillments and claims.
/// Never demotes settled. Only demotes delivered when `demotion == Allow` and the item is
/// no longer fully fulfilled (e.g. after a fulfillment reversal).
async fn recompute_item_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item_id: Uuid,
    demotion: DeliveredDemotion,
) -> Result<ListItemStatus, FulfillmentError> {
    let current: String = sqlx::query_scalar("SELECT status FROM list_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(internal)?;

    let current_status: ListItemStatus = current
        .parse()
        .map_err(|e: String| internal(anyhow::anyhow!(e)))?;

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
    .await
    .map_err(internal)?;

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
        .await
        .map_err(internal)?;

    Ok(new_status)
}

/// Set-based recompute for many items in one round-trip. Uses Forbid semantics
/// (delivered/settled are never demoted) — used by claim mutations where reversal isn't possible.
async fn recompute_item_statuses_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item_ids: &[Uuid],
) -> Result<(), FulfillmentError> {
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
    .await
    .map_err(internal)?;
    Ok(())
}

/// Upsert (or recompute) the pending reimbursement row for (list, requester, hauler).
/// Never touches settled rows.
async fn upsert_reimbursement(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    list_id: Uuid,
    requester_user_id: Uuid,
    hauler_user_id: Uuid,
) -> Result<(), FulfillmentError> {
    sqlx::query(
        r#"
        WITH s AS (
            SELECT COALESCE(SUM(f.qty * f.unit_price_isk), 0) AS subtotal
            FROM fulfillments f
            JOIN list_items li ON li.id = f.list_item_id
            WHERE li.list_id = $1
              AND li.requested_by_user_id = $2
              AND f.hauler_user_id = $3
              AND f.reversed_at IS NULL
        ),
        t AS (SELECT tip_pct FROM lists WHERE id = $1)
        INSERT INTO reimbursements
            (list_id, requester_user_id, hauler_user_id, subtotal_isk, tip_isk, total_isk)
        SELECT $1, $2, $3,
               s.subtotal,
               s.subtotal * t.tip_pct,
               s.subtotal * (1 + t.tip_pct)
        FROM s, t
        ON CONFLICT (list_id, requester_user_id, hauler_user_id) DO UPDATE
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
    .await
    .map_err(internal)?;
    Ok(())
}

fn is_claim_conflict(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.constraint() == Some("claim_items_one_active"))
}

// ── Group row helper for default-tip endpoint ─────────────────────────────────

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: Uuid,
    name: String,
    invite_code: String,
    created_by_user_id: Uuid,
    created_at: DateTime<Utc>,
    default_tip_pct: Decimal,
}

impl GroupRow {
    fn into_group(self) -> domain::Group {
        domain::Group {
            id: self.id,
            name: self.name,
            invite_code: self.invite_code,
            created_by_user_id: self.created_by_user_id,
            created_at: self.created_at,
            default_tip_pct: self.default_tip_pct,
        }
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

pub enum FulfillmentError {
    BadRequest(String),
    NotFound,
    Forbidden,
    Conflict(String),
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> FulfillmentError {
    FulfillmentError::Internal(e.into())
}

fn list_err(e: crate::lists::ListError) -> FulfillmentError {
    match e {
        crate::lists::ListError::NotFound => FulfillmentError::NotFound,
        crate::lists::ListError::Forbidden => FulfillmentError::Forbidden,
        crate::lists::ListError::Conflict(msg) => FulfillmentError::Conflict(msg),
        crate::lists::ListError::BadRequest(m) => FulfillmentError::BadRequest(m),
        crate::lists::ListError::Internal(e) => FulfillmentError::Internal(e),
    }
}

impl IntoResponse for FulfillmentError {
    fn into_response(self) -> Response {
        match self {
            FulfillmentError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            FulfillmentError::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            FulfillmentError::Forbidden => {
                (StatusCode::FORBIDDEN, "you cannot perform this action").into_response()
            }
            FulfillmentError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
            FulfillmentError::Internal(e) => {
                tracing::error!(error = ?e, "fulfillment handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
