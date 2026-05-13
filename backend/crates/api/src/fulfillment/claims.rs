use axum::{
    extract::{Path, State},
    Json,
};
use domain::ListDetail;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    db::Tx,
    errors::ApiError,
    extract::{CurrentClaim, CurrentList},
    lists::load_list_detail,
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

#[derive(Deserialize)]
pub(super) struct CreateClaimBody {
    item_ids: Vec<Uuid>,
    note: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AddClaimItemsBody {
    item_ids: Vec<Uuid>,
}

pub(super) async fn create_claim(
    State(state): State<AppState>,
    cur: CurrentList,
    tx: Tx,
    Json(body): Json<CreateClaimBody>,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_open().map_err(|_| {
        ApiError::Conflict(format!(
            "list is {}; claims can only be created on open lists",
            cur.status
        ))
    })?;
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;
    if body.item_ids.is_empty() {
        return Err(ApiError::BadRequest("item_ids must not be empty".into()));
    }

    let mut conn = tx.acquire().await;
    super::lock_list(&mut **conn, list_id).await?;

    validate_claimable_items(&mut **conn, list_id, &body.item_ids).await?;

    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (list_id, hauler_user_id, note) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(list_id)
    .bind(user_id)
    .bind(body.note.as_deref())
    .fetch_one(&mut **conn)
    .await?;

    insert_claim_items(&mut **conn, claim_id, &body.item_ids).await?;

    let hauler_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&mut **conn)
        .await?;
    let (list_dest, group_id_for_wh): (Option<String>, Uuid) =
        sqlx::query_as("SELECT destination_label, group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_one(&mut **conn)
            .await?;

    let event = WebhookEvent::ListClaimed {
        list_destination: list_dest.unwrap_or_else(|| "(unnamed)".into()),
        hauler_name,
        item_count: body.item_ids.len(),
    };
    fire_webhook(&mut **conn, group_id_for_wh, &event).await?;
    drop(conn);

    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn add_claim_items(
    State(state): State<AppState>,
    cur: CurrentClaim,
    tx: Tx,
    Json(body): Json<AddClaimItemsBody>,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_list_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    } = cur;
    super::ensure_claim_writable(user_id, hauler_user_id, role, status)?;
    if body.item_ids.is_empty() {
        return Err(ApiError::BadRequest("item_ids must not be empty".into()));
    }

    let mut conn = tx.acquire().await;
    super::lock_list(&mut **conn, list_id).await?;

    validate_claimable_items(&mut **conn, list_id, &body.item_ids).await?;

    insert_claim_items(&mut **conn, claim_id, &body.item_ids).await?;

    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn remove_claim_item(
    State(state): State<AppState>,
    Path((_claim_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentClaim,
    tx: Tx,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_list_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    } = cur;
    super::ensure_claim_writable(user_id, hauler_user_id, role, status)?;

    let mut conn = tx.acquire().await;
    super::lock_list(&mut **conn, list_id).await?;

    let r = sqlx::query("DELETE FROM claim_items WHERE claim_id = $1 AND list_item_id = $2")
        .bind(claim_id)
        .bind(item_id)
        .execute(&mut **conn)
        .await?;

    if r.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }

    super::recompute_item_status(&mut **conn, item_id, super::DeliveredDemotion::Forbid).await?;
    drop(conn);

    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn release_claim(
    State(state): State<AppState>,
    cur: CurrentClaim,
    tx: Tx,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_list_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentClaim {
        claim_id,
        list_id,
        user_id,
        hauler_user_id,
        role,
        status,
        ..
    } = cur;
    super::ensure_claim_writable(user_id, hauler_user_id, role, status)?;

    let mut conn = tx.acquire().await;
    super::lock_list(&mut **conn, list_id).await?;

    // Load items before releasing so we can recompute them
    let item_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT list_item_id FROM claim_items WHERE claim_id = $1")
            .bind(claim_id)
            .fetch_all(&mut **conn)
            .await?;

    sqlx::query("UPDATE claims SET status = 'released', released_at = now() WHERE id = $1")
        .bind(claim_id)
        .execute(&mut **conn)
        .await?;
    // Trigger fires and flips claim_items.active = false

    super::recompute_item_statuses_bulk(&mut **conn, &item_ids).await?;

    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Verify the given item ids belong to the list and are all in `open` status.
/// Items in any other status (claimed/bought/delivered/settled) cannot be
/// added to a claim; the unique-active-claim index already prevents
/// double-active claims, but completed work shouldn't be re-claimed at all.
async fn validate_claimable_items(
    conn: &mut sqlx::PgConnection,
    list_id: Uuid,
    item_ids: &[Uuid],
) -> Result<(), ApiError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM list_items \
         WHERE id = ANY($1::uuid[]) AND list_id = $2",
    )
    .bind(item_ids)
    .bind(list_id)
    .fetch_all(&mut *conn)
    .await?;

    if rows.len() != item_ids.len() {
        return Err(ApiError::BadRequest(
            "one or more item_ids do not belong to this list".into(),
        ));
    }

    if let Some((_, bad)) = rows.iter().find(|(_, s)| s != "open") {
        return Err(ApiError::Conflict(format!(
            "cannot claim item with status '{bad}'; only open items may be claimed"
        )));
    }

    Ok(())
}

async fn insert_claim_items(
    conn: &mut sqlx::PgConnection,
    claim_id: Uuid,
    item_ids: &[Uuid],
) -> Result<(), ApiError> {
    if item_ids.is_empty() {
        return Ok(());
    }
    match sqlx::query(
        "INSERT INTO claim_items (claim_id, list_item_id) \
         SELECT $1, item_id FROM unnest($2::uuid[]) AS t(item_id)",
    )
    .bind(claim_id)
    .bind(item_ids)
    .execute(&mut *conn)
    .await
    {
        Ok(_) => {}
        Err(e) if super::is_claim_conflict(&e) => {
            return Err(ApiError::Conflict(
                "one or more items are already claimed".into(),
            ));
        }
        Err(e) => return Err(ApiError::internal(e)),
    }
    super::recompute_item_statuses_bulk(conn, item_ids).await?;
    Ok(())
}
