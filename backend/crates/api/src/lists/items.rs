use chrono::{DateTime, Utc};
use domain::{ListDetail, ListItemStatus, ResolvedType};
use serde::Deserialize;
use uuid::Uuid;

use axum::{
    extract::{Path, State},
    Json,
};

use super::detail::load_list_detail;
use super::pricing::recompute_estimates;
use crate::{errors::ApiError, extract::CurrentList, state::AppState};

#[derive(Deserialize)]
pub(super) struct AddItemsBody {
    pub multibuy: Option<String>,
    pub type_name: Option<String>,
    pub qty: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct PatchItemBody {
    pub qty_requested: Option<i64>,
    pub type_name: Option<String>,
}

pub(super) async fn add_items(
    State(state): State<AppState>,
    cur: CurrentList,
    Json(body): Json<AddItemsBody>,
) -> Result<Json<ListDetail>, ApiError> {
    use super::{MAX_DISTINCT_NAMES, MAX_PARSED_LINES};
    use domain::multibuy::parse_multibuy;

    cur.require_open().map_err(|_| {
        ApiError::Conflict(format!(
            "list is {}; items can only be added to open lists",
            cur.status
        ))
    })?;
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;
    let new_lines: Vec<(String, i64, i32)> = match (body.multibuy, body.type_name, body.qty) {
        (Some(mb), _, _) => {
            super::validate_multibuy_size(&mb)?;
            let parsed = parse_multibuy(&mb);
            if !parsed.errors.is_empty() {
                return Err(ApiError::BadRequest(format!(
                    "{} multibuy line(s) failed to parse",
                    parsed.errors.len()
                )));
            }
            if parsed.lines.is_empty() {
                return Err(ApiError::BadRequest("multibuy is empty".into()));
            }
            if parsed.lines.len() > MAX_PARSED_LINES {
                return Err(ApiError::BadRequest(format!(
                    "too many parsed lines ({}); max {}",
                    parsed.lines.len(),
                    MAX_PARSED_LINES
                )));
            }
            parsed
                .lines
                .into_iter()
                .map(|l| (l.name, l.qty, *l.line_nos.first().unwrap_or(&0) as i32))
                .collect()
        }
        (None, Some(name), Some(qty)) => {
            if qty <= 0 {
                return Err(ApiError::BadRequest("qty must be positive".into()));
            }
            vec![(name, qty, 0)]
        }
        _ => {
            return Err(ApiError::BadRequest(
                "provide either {multibuy} or {type_name, qty}".into(),
            ))
        }
    };

    let names: Vec<String> = new_lines.iter().map(|(n, _, _)| n.clone()).collect();
    if names.len() > MAX_DISTINCT_NAMES {
        return Err(ApiError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            names.len(),
            MAX_DISTINCT_NAMES
        )));
    }
    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &names).await?;
    if !unresolved.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    let updated_after = {
        let mut tx = state.pool.begin().await?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
        // Enforce items-per-list cap against the locked row, so concurrent
        // add_items calls can't both squeak past it.
        let cur_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM list_items WHERE list_id = $1")
                .bind(list_id)
                .fetch_one(&mut *tx)
                .await?;
        let cap = state.config.limits.items_per_list;
        if cur_count + (new_lines.len() as i64) > cap {
            return Err(ApiError::QuotaExceeded(format!(
                "adding {} items would push the list past its per-list cap ({} of {})",
                new_lines.len(),
                cur_count,
                cap
            )));
        }
        let n = new_lines.len();
        let mut type_ids: Vec<i64> = Vec::with_capacity(n);
        let mut type_names: Vec<String> = Vec::with_capacity(n);
        let mut qtys: Vec<i64> = Vec::with_capacity(n);
        let mut line_nos: Vec<i32> = Vec::with_capacity(n);
        for (name, qty, line_no) in &new_lines {
            let key = market::types::normalize_key(name);
            let r = resolved.get(&key).ok_or_else(|| {
                ApiError::internal(anyhow::anyhow!("resolved missing for {}", name))
            })?;
            type_ids.push(r.type_id);
            type_names.push(r.type_name.clone());
            qtys.push(*qty);
            line_nos.push(*line_no);
        }
        sqlx::query(
            "INSERT INTO list_items \
             (list_id, type_id, type_name, qty_requested, requested_by_user_id, source_line_no) \
             SELECT $1, type_id, type_name, qty_requested, $2, source_line_no \
             FROM UNNEST($3::bigint[], $4::text[], $5::bigint[], $6::int[]) \
                  AS t(type_id, type_name, qty_requested, source_line_no)",
        )
        .bind(list_id)
        .bind(user_id)
        .bind(&type_ids)
        .bind(&type_names)
        .bind(&qtys)
        .bind(&line_nos)
        .execute(&mut *tx)
        .await?;
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn patch_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
    Json(body): Json<PatchItemBody>,
) -> Result<Json<ListDetail>, ApiError> {
    debug_assert_eq!(cur.list_id, list_id);
    cur.require_mutable()?;
    if body.qty_requested.is_none() && body.type_name.is_none() {
        return Err(ApiError::BadRequest("nothing to update".into()));
    }

    let resolved_type: Option<ResolvedType> = if let Some(name) = &body.type_name {
        let (resolved, unresolved) =
            market::resolve_type_ids(&state.pool, &state.esi, std::slice::from_ref(name)).await?;
        if !unresolved.is_empty() {
            return Err(ApiError::BadRequest(format!("unknown item: {name}")));
        }
        let key = market::types::normalize_key(name);
        Some(
            resolved.get(&key).cloned().ok_or_else(|| {
                ApiError::internal(anyhow::anyhow!("resolved missing for {name}"))
            })?,
        )
    } else {
        None
    };

    if let Some(qty) = body.qty_requested {
        if qty <= 0 {
            return Err(ApiError::BadRequest(
                "qty_requested must be positive".into(),
            ));
        }
    }

    let updated_after = {
        let mut tx = state.pool.begin().await?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
        let item_status: ListItemStatus = sqlx::query_scalar(
            "SELECT status FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
        )
        .bind(item_id)
        .bind(list_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
        // Once a hauler has claimed or fulfilled this item, qty/type edits would
        // make status, qty_fulfilled, and reimbursement totals inconsistent.
        // The hauler must release the claim or reverse fulfillments first.
        if item_status != ListItemStatus::Open {
            return Err(ApiError::Conflict(format!(
                "cannot edit item with status '{item_status}'; \
                 release the claim or reverse fulfillments first"
            )));
        }
        if let Some(qty) = body.qty_requested {
            sqlx::query("UPDATE list_items SET qty_requested = $1 WHERE id = $2 AND list_id = $3")
                .bind(qty)
                .bind(item_id)
                .bind(list_id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(rt) = &resolved_type {
            sqlx::query(
                "UPDATE list_items SET type_id = $1, type_name = $2 \
                 WHERE id = $3 AND list_id = $4",
            )
            .bind(rt.type_id)
            .bind(&rt.type_name)
            .bind(item_id)
            .bind(list_id)
            .execute(&mut *tx)
            .await?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, cur.user_id, cur.role).await?;
    Ok(Json(detail))
}

pub(super) async fn delete_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
) -> Result<Json<ListDetail>, ApiError> {
    debug_assert_eq!(cur.list_id, list_id);
    cur.require_mutable()?;
    let updated_after = {
        let mut tx = state.pool.begin().await?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
        let item_status: Option<ListItemStatus> = sqlx::query_scalar(
            "SELECT status FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
        )
        .bind(item_id)
        .bind(list_id)
        .fetch_optional(&mut *tx)
        .await?;
        let item_status = item_status.ok_or_else(ApiError::not_found)?;
        // Deleting cascades fulfillments and claim_items, but leaves
        // reimbursements with stale totals (and still settleable). Block
        // until the item is back to 'open' — i.e. no active claim and no
        // non-reversed fulfillments.
        if item_status != ListItemStatus::Open {
            return Err(ApiError::Conflict(format!(
                "cannot delete item with status '{item_status}'; \
                 release the claim or reverse fulfillments first"
            )));
        }
        let r = sqlx::query("DELETE FROM list_items WHERE id = $1 AND list_id = $2")
            .bind(item_id)
            .bind(list_id)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() == 0 {
            return Err(ApiError::not_found());
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, cur.user_id, cur.role).await?;
    Ok(Json(detail))
}
