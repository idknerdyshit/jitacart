//! Lists, list items, and list-market sets.
//!
//! Input limits enforced on every endpoint that fans out to ESI:
//! - `multibuy` body: <= [`MAX_MULTIBUY_BYTES`] bytes
//! - parsed lines (post blank-strip):  <= [`MAX_PARSED_LINES`]
//! - distinct dedup'd type names:      <= [`MAX_DISTINCT_NAMES`]
//! - `market_ids`:                     <= [`MAX_MARKET_IDS`]
//!
//! Each `market_id` must point to either a public NPC hub or a citadel that
//! the calling group has explicitly tracked (enforced by
//! [`validate_markets_for_group`]).

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{
    multibuy::{parse_multibuy, LineError, ParsedLine},
    Claim, ClaimStatus, Fulfillment, FulfillmentSource, GroupRole, List, ListDetail, ListItem,
    ListItemStatus, ListStatus, ListSummary, LiveItemPrice, Market, MarketKind, Reimbursement,
    ReimbursementStatus, ResolvedType,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    errors::ApiError,
    extract::{CurrentGroup, CurrentList},
    markets::MarketRow,
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

pub const MAX_MULTIBUY_BYTES: usize = 64 * 1024;
pub const MAX_PARSED_LINES: usize = 5000;
pub const MAX_DISTINCT_NAMES: usize = 1000;
pub const MAX_MARKET_IDS: usize = 8;
const RECOMPUTE_RETRY_LIMIT: usize = 3;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/lists/preview", post(preview))
        .route("/groups/{id}/lists", post(create).get(list_for_group))
        .route(
            "/lists/{id}",
            get(detail).patch(patch_list).delete(delete_list),
        )
        .route("/lists/{id}/markets", post(replace_markets))
        .route("/lists/{id}/items", post(add_items))
        .route(
            "/lists/{id}/items/{item_id}",
            patch(patch_item).delete(delete_item),
        )
}

#[derive(Deserialize)]
struct PreviewBody {
    multibuy: String,
    market_ids: Vec<Uuid>,
}

#[derive(Serialize)]
struct PreviewResponse {
    lines: Vec<PreviewLine>,
    unresolved_names: Vec<String>,
    errors: Vec<LineError>,
}

#[derive(Serialize)]
struct PreviewLine {
    line_nos: Vec<u32>,
    name: String,
    type_id: Option<i64>,
    type_name: Option<String>,
    qty: i64,
    /// Per-market price aggregate keyed by `market_id` (UUID stringified).
    prices: HashMap<String, PreviewPrice>,
    error: Option<String>,
}

#[derive(Serialize)]
struct PreviewPrice {
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: i64,
    buy_volume: i64,
    computed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct CreateBody {
    destination_label: Option<String>,
    notes: Option<String>,
    market_ids: Vec<Uuid>,
    primary_market_id: Uuid,
    multibuy: String,
}

#[derive(Deserialize)]
struct PatchListBody {
    destination_label: Option<Option<String>>,
    notes: Option<Option<String>>,
    status: Option<ListStatus>,
}

#[derive(Deserialize)]
struct ReplaceMarketsBody {
    market_ids: Vec<Uuid>,
    primary_market_id: Uuid,
}

#[derive(Deserialize)]
struct AddItemsBody {
    multibuy: Option<String>,
    type_name: Option<String>,
    qty: Option<i64>,
}

#[derive(Deserialize)]
struct PatchItemBody {
    qty_requested: Option<i64>,
    type_name: Option<String>,
}

async fn preview(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
    Json(body): Json<PreviewBody>,
) -> Result<Json<PreviewResponse>, ApiError> {
    validate_multibuy_size(&body.multibuy)?;
    validate_market_ids_size(&body.market_ids)?;

    let parsed = parse_multibuy(&body.multibuy);
    if parsed.lines.len() > MAX_PARSED_LINES {
        return Err(ApiError::BadRequest(format!(
            "too many parsed lines ({}); max {}",
            parsed.lines.len(),
            MAX_PARSED_LINES
        )));
    }
    let distinct_names: Vec<String> = parsed.lines.iter().map(|l| l.name.clone()).collect();
    if distinct_names.len() > MAX_DISTINCT_NAMES {
        return Err(ApiError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            distinct_names.len(),
            MAX_DISTINCT_NAMES
        )));
    }

    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &distinct_names)
        .await
        .map_err(ApiError::internal)?;

    let type_ids = dedup_type_ids(resolved.values().map(|r| r.type_id));
    let prices_by_market = fetch_prices_for_markets(&state, group_id, &markets, &type_ids).await?;

    let lines: Vec<PreviewLine> = parsed
        .lines
        .into_iter()
        .map(|p: ParsedLine| {
            let key = market::types::normalize_key(&p.name);
            let resolved_for_line: Option<&ResolvedType> = resolved.get(&key);
            let mut prices_out: HashMap<String, PreviewPrice> = HashMap::new();
            if let Some(r) = resolved_for_line {
                for m in &markets {
                    let agg = prices_by_market
                        .get(&m.id)
                        .and_then(|map| map.get(&r.type_id));
                    prices_out.insert(
                        m.id.to_string(),
                        PreviewPrice {
                            best_sell: agg.and_then(|a| a.best_sell),
                            best_buy: agg.and_then(|a| a.best_buy),
                            sell_volume: agg.map(|a| a.sell_volume).unwrap_or(0),
                            buy_volume: agg.map(|a| a.buy_volume).unwrap_or(0),
                            computed_at: agg.map(|a| a.computed_at),
                        },
                    );
                }
            }
            let error = if resolved_for_line.is_none() {
                Some(format!("unknown item: {}", p.name))
            } else {
                None
            };
            PreviewLine {
                line_nos: p.line_nos,
                name: p.name.clone(),
                type_id: resolved_for_line.map(|r| r.type_id),
                type_name: resolved_for_line.map(|r| r.type_name.clone()),
                qty: p.qty,
                prices: prices_out,
                error,
            }
        })
        .collect();

    Ok(Json(PreviewResponse {
        lines,
        unresolved_names: unresolved,
        errors: parsed.errors,
    }))
}

async fn create(
    State(state): State<AppState>,
    CurrentGroup {
        user_id,
        group_id,
        role,
    }: CurrentGroup,
    Json(body): Json<CreateBody>,
) -> Result<Json<ListDetail>, ApiError> {
    validate_multibuy_size(&body.multibuy)?;
    validate_market_ids_size(&body.market_ids)?;
    if !body.market_ids.contains(&body.primary_market_id) {
        return Err(ApiError::BadRequest(
            "primary_market_id must be in market_ids".into(),
        ));
    }

    let parsed = parse_multibuy(&body.multibuy);
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
    let distinct_names: Vec<String> = parsed.lines.iter().map(|l| l.name.clone()).collect();
    if distinct_names.len() > MAX_DISTINCT_NAMES {
        return Err(ApiError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            distinct_names.len(),
            MAX_DISTINCT_NAMES
        )));
    }
    let items_cap = state.config.limits.items_per_list;
    if (parsed.lines.len() as i64) > items_cap {
        return Err(ApiError::QuotaExceeded(format!(
            "list has {} items; per-list cap is {}",
            parsed.lines.len(),
            items_cap
        )));
    }
    require_lists_quota(&state, group_id).await?;

    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &distinct_names)
        .await
        .map_err(ApiError::internal)?;
    if !unresolved.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    let type_ids = dedup_type_ids(resolved.values().map(|r| r.type_id));
    let prices_by_market = fetch_prices_for_markets(&state, group_id, &markets, &type_ids).await?;

    struct ItemRow {
        type_id: i64,
        type_name: String,
        qty: i64,
        line_no: i32,
        est_unit: Option<Decimal>,
        est_market: Option<Uuid>,
    }
    let mut items_to_insert: Vec<ItemRow> = Vec::with_capacity(parsed.lines.len());
    let mut total: Decimal = Decimal::ZERO;
    for p in &parsed.lines {
        let key = market::types::normalize_key(&p.name);
        let r = resolved
            .get(&key)
            .ok_or_else(|| ApiError::internal(anyhow::anyhow!("resolved missing for {}", p.name)))?;
        let (est_unit, est_market) = pick_cheapest(&markets, &prices_by_market, r.type_id);
        if let Some(u) = est_unit {
            total += u * Decimal::from(p.qty);
        }
        items_to_insert.push(ItemRow {
            type_id: r.type_id,
            type_name: r.type_name.clone(),
            qty: p.qty,
            line_no: *p.line_nos.first().unwrap_or(&0) as i32,
            est_unit,
            est_market,
        });
    }

    let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;

    let list_id: Uuid = sqlx::query_scalar(
        "INSERT INTO lists \
         (group_id, created_by_user_id, destination_label, notes, total_estimate_isk, \
          tip_pct) \
         VALUES ($1, $2, $3, $4, $5, \
                 (SELECT default_tip_pct FROM groups WHERE id = $1)) \
         RETURNING id",
    )
    .bind(group_id)
    .bind(user_id)
    .bind(body.destination_label.as_deref())
    .bind(body.notes.as_deref())
    .bind(total)
    .fetch_one(&mut *tx)
    .await
    .map_err(ApiError::internal)?;

    for m in &markets {
        sqlx::query(
            "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
        )
        .bind(list_id)
        .bind(m.id)
        .bind(m.id == body.primary_market_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
    }

    for it in &items_to_insert {
        sqlx::query(
            "INSERT INTO list_items \
             (list_id, type_id, type_name, qty_requested, est_unit_price_isk, \
              est_priced_market_id, requested_by_user_id, source_line_no) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(list_id)
        .bind(it.type_id)
        .bind(&it.type_name)
        .bind(it.qty)
        .bind(it.est_unit)
        .bind(it.est_market)
        .bind(user_id)
        .bind(it.line_no)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
    }

    tx.commit().await.map_err(ApiError::internal)?;

    let creator_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::internal)?;
    fire_webhook(
        &state,
        group_id,
        WebhookEvent::ListCreated {
            list_destination: body.destination_label.unwrap_or_else(|| "(unnamed)".into()),
            item_count: items_to_insert.len() as i64,
            estimate_isk: total.to_string(),
            creator_name,
        },
    );

    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

#[derive(Deserialize)]
struct ListForGroupQuery {
    #[serde(default)]
    include_archived: bool,
}

async fn list_for_group(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
    Query(q): Query<ListForGroupQuery>,
) -> Result<Json<Vec<ListSummary>>, ApiError> {
    let rows: Vec<ListSummaryRow> = sqlx::query_as(
        r#"
        SELECT l.id,
               l.destination_label,
               l.status,
               l.total_estimate_isk,
               l.created_at,
               (SELECT count(*) FROM list_items li WHERE li.list_id = l.id) AS item_count,
               (SELECT m.short_label
                  FROM list_markets lm JOIN markets m ON m.id = lm.market_id
                  WHERE lm.list_id = l.id AND lm.is_primary
                  LIMIT 1) AS primary_short_label
        FROM lists l
        WHERE l.group_id = $1 AND ($2 OR l.status <> 'archived')
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(group_id)
    .bind(q.include_archived)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;

    rows.into_iter()
        .map(ListSummaryRow::into_summary)
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json)
        .map_err(ApiError::internal)
}

async fn detail(
    State(state): State<AppState>,
    CurrentList {
        list_id,
        user_id,
        role,
        ..
    }: CurrentList,
) -> Result<Json<ListDetail>, ApiError> {
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

async fn patch_list(
    State(state): State<AppState>,
    cur: CurrentList,
    Json(body): Json<PatchListBody>,
) -> Result<Json<ListDetail>, ApiError> {
    if body.destination_label.is_none() && body.notes.is_none() && body.status.is_none() {
        return Err(ApiError::BadRequest("nothing to update".into()));
    }
    if body.status.is_none() {
        cur.require_mutable()
            .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    }
    if body.status.is_some() {
        cur.require_can_manage().map_err(|_| ApiError::forbidden())?;
    }
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;

    let mut q = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE lists SET updated_at = now()");
    if let Some(label_opt) = &body.destination_label {
        q.push(", destination_label = ");
        q.push_bind(label_opt.as_ref().map(String::as_str));
    }
    if let Some(notes_opt) = &body.notes {
        q.push(", notes = ");
        q.push_bind(notes_opt.as_ref().map(String::as_str));
    }
    if let Some(status) = body.status {
        q.push(", status = ");
        q.push_bind(status.as_str());
    }
    q.push(" WHERE id = ");
    q.push_bind(list_id);
    q.build().execute(&state.pool).await.map_err(ApiError::internal)?;

    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

/// Apply a status transition to a list, checking that the caller is the
/// list's creator or a group owner. Extracted so tests can drive the archive
/// transition without going through the extractor stack.
pub async fn do_patch_list_status(
    pool: &sqlx::PgPool,
    list_id: Uuid,
    user_id: Uuid,
    new_status: ListStatus,
) -> Result<(), ApiError> {
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT l.created_by_user_id, gm.role \
         FROM lists l \
         LEFT JOIN group_memberships gm \
           ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE l.id = $2",
    )
    .bind(user_id)
    .bind(list_id)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::internal)?;

    let (created_by, role_raw) = row.ok_or_else(ApiError::not_found)?;
    let role_raw = role_raw.ok_or_else(ApiError::forbidden)?;
    let role: GroupRole = role_raw
        .parse()
        .map_err(|e: String| ApiError::internal(anyhow::anyhow!(e)))?;
    if user_id != created_by && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    sqlx::query("UPDATE lists SET status = $1, updated_at = now() WHERE id = $2")
        .bind(new_status.as_str())
        .bind(list_id)
        .execute(pool)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

async fn delete_list(
    State(state): State<AppState>,
    cur: CurrentList,
) -> Result<StatusCode, ApiError> {
    if cur.created_by_user_id != cur.user_id && cur.role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }
    let r = sqlx::query("DELETE FROM lists WHERE id = $1")
        .bind(cur.list_id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::internal)?;
    if r.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn replace_markets(
    State(state): State<AppState>,
    cur: CurrentList,
    Json(body): Json<ReplaceMarketsBody>,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let CurrentList {
        list_id,
        group_id,
        user_id,
        role,
        ..
    } = cur;
    validate_market_ids_size(&body.market_ids)?;
    if !body.market_ids.contains(&body.primary_market_id) {
        return Err(ApiError::BadRequest(
            "primary_market_id must be in market_ids".into(),
        ));
    }
    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::not_found)?;

        sqlx::query("DELETE FROM list_markets WHERE list_id = $1")
            .bind(list_id)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::internal)?;
        for m in &markets {
            sqlx::query(
                "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
            )
            .bind(list_id)
            .bind(m.id)
            .bind(m.id == body.primary_market_id)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        tx.commit().await.map_err(ApiError::internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

async fn add_items(
    State(state): State<AppState>,
    cur: CurrentList,
    Json(body): Json<AddItemsBody>,
) -> Result<Json<ListDetail>, ApiError> {
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
            validate_multibuy_size(&mb)?;
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
    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &names)
        .await
        .map_err(ApiError::internal)?;
    if !unresolved.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::not_found)?;
        // Enforce items-per-list cap against the locked row, so concurrent
        // add_items calls can't both squeak past it.
        let cur_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM list_items WHERE list_id = $1")
                .bind(list_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(ApiError::internal)?;
        let cap = state.config.limits.items_per_list;
        if cur_count + (new_lines.len() as i64) > cap {
            return Err(ApiError::QuotaExceeded(format!(
                "adding {} items would push the list past its per-list cap ({} of {})",
                new_lines.len(),
                cur_count,
                cap
            )));
        }
        for (name, qty, line_no) in &new_lines {
            let key = market::types::normalize_key(name);
            let r = resolved
                .get(&key)
                .ok_or_else(|| ApiError::internal(anyhow::anyhow!("resolved missing for {}", name)))?;
            sqlx::query(
                "INSERT INTO list_items \
                 (list_id, type_id, type_name, qty_requested, requested_by_user_id, source_line_no) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(list_id)
            .bind(r.type_id)
            .bind(&r.type_name)
            .bind(*qty)
            .bind(user_id)
            .bind(*line_no)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        tx.commit().await.map_err(ApiError::internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, user_id, role).await?;
    Ok(Json(detail))
}

async fn patch_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
    Json(body): Json<PatchItemBody>,
) -> Result<Json<ListDetail>, ApiError> {
    debug_assert_eq!(cur.list_id, list_id);
    cur.require_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    if body.qty_requested.is_none() && body.type_name.is_none() {
        return Err(ApiError::BadRequest("nothing to update".into()));
    }

    let resolved_type: Option<ResolvedType> = if let Some(name) = &body.type_name {
        let (resolved, unresolved) =
            market::resolve_type_ids(&state.pool, &state.esi, std::slice::from_ref(name))
                .await
                .map_err(ApiError::internal)?;
        if !unresolved.is_empty() {
            return Err(ApiError::BadRequest(format!("unknown item: {name}")));
        }
        let key = market::types::normalize_key(name);
        Some(
            resolved
                .get(&key)
                .cloned()
                .ok_or_else(|| ApiError::internal(anyhow::anyhow!("resolved missing for {name}")))?,
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
        let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::not_found)?;
        let item_status: String = sqlx::query_scalar(
            "SELECT status FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
        )
        .bind(item_id)
        .bind(list_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?;
        // Once a hauler has claimed or fulfilled this item, qty/type edits would
        // make status, qty_fulfilled, and reimbursement totals inconsistent.
        // The hauler must release the claim or reverse fulfillments first.
        if item_status != "open" {
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
                .await
                .map_err(ApiError::internal)?;
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
            .await
            .map_err(ApiError::internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        tx.commit().await.map_err(ApiError::internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, cur.user_id, cur.role).await?;
    Ok(Json(detail))
}

async fn delete_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
) -> Result<Json<ListDetail>, ApiError> {
    debug_assert_eq!(cur.list_id, list_id);
    cur.require_mutable()
        .map_err(|_| ApiError::Conflict("list is archived; no changes can be made".into()))?;
    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::not_found)?;
        let item_status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE",
        )
        .bind(item_id)
        .bind(list_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        let item_status = item_status.ok_or_else(ApiError::not_found)?;
        // Deleting cascades fulfillments and claim_items, but leaves
        // reimbursements with stale totals (and still settleable). Block
        // until the item is back to 'open' — i.e. no active claim and no
        // non-reversed fulfillments.
        if item_status != "open" {
            return Err(ApiError::Conflict(format!(
                "cannot delete item with status '{item_status}'; \
                 release the claim or reverse fulfillments first"
            )));
        }
        let r = sqlx::query("DELETE FROM list_items WHERE id = $1 AND list_id = $2")
            .bind(item_id)
            .bind(list_id)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::internal)?;
        if r.rows_affected() == 0 {
            return Err(ApiError::not_found());
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        tx.commit().await.map_err(ApiError::internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id, cur.user_id, cur.role).await?;
    Ok(Json(detail))
}

fn dedup_type_ids(iter: impl IntoIterator<Item = i64>) -> Vec<i64> {
    let mut v: Vec<i64> = iter.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Fan out price lookups to all markets in parallel.
///
/// NPC hubs can safely refresh on demand through the anonymous regional market
/// endpoint. Citadels are intentionally cache-only on API request paths: the
/// worker proves group access, refreshes the structure order book, and records
/// which group member succeeded before these handlers expose cached rows.
async fn fetch_prices_for_markets(
    state: &AppState,
    group_id: Uuid,
    markets: &[Market],
    type_ids: &[i64],
) -> Result<HashMap<Uuid, HashMap<i64, market::PriceAggregate>>, ApiError> {
    let npc_ttl = state.config.esi.poll_intervals_secs.market_prices as i64;
    let citadel_ttl = state
        .config
        .esi
        .poll_intervals_secs
        .citadel_orders
        .saturating_mul(2) as i64;
    let futs = markets.iter().map(|m| async move {
        let map = match m.kind {
            MarketKind::NpcHub => {
                market::get_or_refresh_prices(&state.pool, &state.esi, m, type_ids, npc_ttl).await?
            }
            MarketKind::PublicStructure => {
                read_group_citadel_prices(state, group_id, m, type_ids, citadel_ttl).await?
            }
        };
        Ok::<_, anyhow::Error>((m.id, map))
    });
    let results = futures_util::future::try_join_all(futs)
        .await
        .map_err(ApiError::internal)?;
    Ok(results.into_iter().collect())
}

async fn read_group_citadel_prices(
    state: &AppState,
    group_id: Uuid,
    market: &Market,
    type_ids: &[i64],
    ttl_secs: i64,
) -> anyhow::Result<HashMap<i64, market::PriceAggregate>> {
    if !market.is_public || type_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let accessible = accessible_market_ids(&state.pool, group_id, &[market.id]).await?;
    if !accessible.contains(&market.id) {
        return Ok(HashMap::new());
    }
    read_cached_prices(&state.pool, market.id, type_ids, ttl_secs).await
}

/// Subset of `market_ids` that the group can read prices for: NPC hubs are
/// always accessible; citadels are accessible iff some group member has an
/// `ok` `character_structure_access` row plus the structure-markets scope.
pub(crate) async fn accessible_market_ids(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    market_ids: &[Uuid],
) -> anyhow::Result<HashSet<Uuid>> {
    if market_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT m.id
        FROM markets m
        WHERE m.id = ANY($2::uuid[])
          AND (
              m.kind = 'npc_hub'
              OR EXISTS (
                  SELECT 1
                  FROM characters c
                  JOIN group_memberships gm
                    ON gm.user_id = c.user_id AND gm.group_id = $1
                  JOIN character_structure_access csa
                    ON csa.character_id = c.id AND csa.market_id = m.id
                  WHERE csa.market_status = 'ok'
                    AND c.scopes @> ARRAY['esi-markets.structure_markets.v1']
              )
          )
        "#,
    )
    .bind(group_id)
    .bind(market_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

async fn read_cached_prices(
    pool: &sqlx::PgPool,
    market_id: Uuid,
    type_ids: &[i64],
    ttl_secs: i64,
) -> anyhow::Result<HashMap<i64, market::PriceAggregate>> {
    if type_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::seconds(ttl_secs.max(0));
    let rows: Vec<CachedPriceRow> = sqlx::query_as(
        "SELECT type_id, best_sell, best_buy, sell_volume, buy_volume, computed_at \
         FROM market_prices \
         WHERE market_id = $1 AND type_id = ANY($2::bigint[]) AND computed_at >= $3",
    )
    .bind(market_id)
    .bind(type_ids)
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| (r.type_id, r.into())).collect())
}

#[derive(sqlx::FromRow)]
struct CachedPriceRow {
    type_id: i64,
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: i64,
    buy_volume: i64,
    computed_at: DateTime<Utc>,
}

impl From<CachedPriceRow> for market::PriceAggregate {
    fn from(r: CachedPriceRow) -> Self {
        Self {
            best_sell: r.best_sell,
            best_buy: r.best_buy,
            sell_volume: r.sell_volume,
            buy_volume: r.buy_volume,
            computed_at: r.computed_at,
        }
    }
}

fn validate_multibuy_size(mb: &str) -> Result<(), ApiError> {
    if mb.len() > MAX_MULTIBUY_BYTES {
        return Err(ApiError::BadRequest(format!(
            "multibuy too large ({} bytes); max {}",
            mb.len(),
            MAX_MULTIBUY_BYTES
        )));
    }
    Ok(())
}

fn validate_market_ids_size(ids: &[Uuid]) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Err(ApiError::BadRequest("market_ids must not be empty".into()));
    }
    if ids.len() > MAX_MARKET_IDS {
        return Err(ApiError::BadRequest(format!(
            "too many market_ids ({}); max {}",
            ids.len(),
            MAX_MARKET_IDS
        )));
    }
    let dedup: HashSet<Uuid> = ids.iter().copied().collect();
    if dedup.len() != ids.len() {
        return Err(ApiError::BadRequest(
            "market_ids contains duplicates".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn load_markets(state: &AppState, ids: &[Uuid]) -> Result<Vec<Market>, ApiError> {
    let rows: Vec<MarketRow> = sqlx::query_as(
        "SELECT id, kind, esi_location_id, region_id, name, short_label, is_hub, is_public \
         FROM markets WHERE id = ANY($1::uuid[])",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;

    if rows.len() != ids.len() {
        return Err(ApiError::BadRequest(
            "one or more market_ids do not exist".into(),
        ));
    }
    rows.into_iter()
        .map(MarketRow::into_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)
}

/// Allow if every market is either an NPC hub or present in
/// `group_tracked_markets` for the given group.
pub(crate) async fn validate_markets_for_group(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    markets: &[Market],
) -> Result<(), ApiError> {
    if markets.is_empty() {
        return Ok(());
    }
    let citadel_ids: Vec<Uuid> = markets
        .iter()
        .filter(|m| m.kind == MarketKind::PublicStructure)
        .map(|m| m.id)
        .collect();

    let tracked: std::collections::HashSet<Uuid> = if citadel_ids.is_empty() {
        std::collections::HashSet::new()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT market_id FROM group_tracked_markets \
             WHERE group_id = $1 AND market_id = ANY($2::uuid[])",
        )
        .bind(group_id)
        .bind(&citadel_ids)
        .fetch_all(pool)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .collect()
    };

    for m in markets {
        let label = m.short_label.as_deref().unwrap_or("(unnamed)");
        if !m.is_public {
            return Err(ApiError::BadRequest(format!(
                "market {label} is not public"
            )));
        }
        match m.kind {
            MarketKind::NpcHub => {
                if !m.is_hub {
                    return Err(ApiError::BadRequest(format!(
                        "market {label} is marked as NPC hub but is_hub=false"
                    )));
                }
            }
            MarketKind::PublicStructure => {
                if !tracked.contains(&m.id) {
                    return Err(ApiError::BadRequest(format!(
                        "citadel '{label}' is not tracked by this group"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn pick_cheapest(
    markets: &[Market],
    prices_by_market: &HashMap<Uuid, HashMap<i64, market::PriceAggregate>>,
    type_id: i64,
) -> (Option<Decimal>, Option<Uuid>) {
    let best = markets
        .iter()
        .filter_map(|m| {
            prices_by_market
                .get(&m.id)
                .and_then(|map| map.get(&type_id))
                .and_then(|agg| agg.best_sell)
                .map(|sell| (sell, m.id))
        })
        .min_by_key(|(price, _)| *price);
    match best {
        Some((p, id)) => (Some(p), Some(id)),
        None => (None, None),
    }
}

/// Optimistic-lock recompute: read markets/items + fetch prices outside any
/// tx, then validate that `updated_at` hasn't moved before writing. Retries
/// on concurrent mutation up to [`RECOMPUTE_RETRY_LIMIT`] times.
async fn recompute_estimates(
    state: &AppState,
    list_id: Uuid,
    mut updated_after: DateTime<Utc>,
) -> Result<(), ApiError> {
    for _ in 0..RECOMPUTE_RETRY_LIMIT {
        let group_id: Uuid = sqlx::query_scalar("SELECT group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::not_found)?;

        let market_rows: Vec<MarketRow> = sqlx::query_as(
            "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                    m.is_hub, m.is_public \
             FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
             WHERE lm.list_id = $1",
        )
        .bind(list_id)
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::internal)?;
        let markets: Vec<Market> = market_rows
            .into_iter()
            .map(MarketRow::into_market)
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(ApiError::internal)?;

        let items: Vec<(Uuid, i64)> =
            sqlx::query_as("SELECT id, type_id FROM list_items WHERE list_id = $1")
                .bind(list_id)
                .fetch_all(&state.pool)
                .await
                .map_err(ApiError::internal)?;

        let type_ids = dedup_type_ids(items.iter().map(|(_, t)| *t));
        let prices_by_market =
            fetch_prices_for_markets(state, group_id, &markets, &type_ids).await?;

        let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
        let current_updated: DateTime<Utc> =
            sqlx::query_scalar("SELECT updated_at FROM lists WHERE id = $1 FOR UPDATE")
                .bind(list_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(ApiError::internal)?
                .ok_or_else(ApiError::not_found)?;

        if current_updated != updated_after {
            updated_after = current_updated;
            tx.rollback().await.map_err(ApiError::internal)?;
            continue;
        }

        let mut total: Decimal = Decimal::ZERO;
        for (item_id, type_id) in &items {
            let (est_unit, est_market) = pick_cheapest(&markets, &prices_by_market, *type_id);
            let qty: i64 = sqlx::query_scalar(
                "UPDATE list_items \
                 SET est_unit_price_isk = $1, est_priced_market_id = $2 \
                 WHERE id = $3 RETURNING qty_requested",
            )
            .bind(est_unit)
            .bind(est_market)
            .bind(item_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::internal)?;
            if let Some(u) = est_unit {
                total += u * Decimal::from(qty);
            }
        }

        sqlx::query(
            "UPDATE lists \
             SET total_estimate_isk = $1, \
                 updated_at = CASE WHEN total_estimate_isk IS DISTINCT FROM $1 \
                                   THEN now() ELSE updated_at END \
             WHERE id = $2",
        )
        .bind(total)
        .bind(list_id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::internal)?;
        tx.commit().await.map_err(ApiError::internal)?;
        return Ok(());
    }
    Err(ApiError::Conflict(
        "list was concurrently modified; please retry".into(),
    ))
}

pub(crate) async fn load_list_detail(
    state: &AppState,
    list_id: Uuid,
    viewer_user_id: Uuid,
    viewer_role: GroupRole,
) -> Result<ListDetail, ApiError> {
    let list_row: ListRow = sqlx::query_as(
        "SELECT id, group_id, created_by_user_id, destination_label, notes, status, \
                total_estimate_isk, tip_pct, created_at, updated_at, \
                payer_corp_id, payer_division \
         FROM lists WHERE id = $1",
    )
    .bind(list_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(ApiError::not_found)?;
    let group_id = list_row.group_id;
    let list = list_row.into_list().map_err(ApiError::internal)?;

    let item_rows: Vec<ListItemRow> = sqlx::query_as(
        "SELECT id, list_id, type_id, type_name, qty_requested, qty_fulfilled, \
                est_unit_price_isk, est_priced_market_id, status, source_line_no, \
                requested_by_user_id \
         FROM list_items WHERE list_id = $1 ORDER BY source_line_no NULLS LAST, created_at",
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let items = item_rows
        .into_iter()
        .map(ListItemRow::into_item)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)?;

    let market_rows: Vec<MarketWithPrimaryRow> = sqlx::query_as(
        "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                m.is_hub, m.is_public, lm.is_primary \
         FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
         WHERE lm.list_id = $1 ORDER BY lm.is_primary DESC, m.short_label",
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let primary_market_id = market_rows
        .iter()
        .find_map(|r| if r.is_primary { Some(r.id) } else { None })
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("list has no primary market")))?;
    let markets = market_rows
        .into_iter()
        .map(MarketWithPrimaryRow::into_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)?;

    let market_ids: Vec<Uuid> = markets.iter().map(|m| m.id).collect();
    let accessible: Vec<Uuid> = accessible_market_ids(&state.pool, group_id, &market_ids)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .collect();

    let live_rows: Vec<LivePriceRow> = sqlx::query_as(
        r#"
        SELECT li.id          AS list_item_id,
               m.id           AS market_id,
               mp.best_sell,
               mp.best_buy,
               mp.sell_volume,
               mp.buy_volume,
               mp.computed_at
        FROM list_items li
        JOIN list_markets lm ON lm.list_id = li.list_id
        JOIN markets       m ON m.id       = lm.market_id
        LEFT JOIN market_prices mp
          ON mp.market_id = m.id
         AND mp.type_id   = li.type_id
         AND mp.market_id = ANY($2::uuid[])
        WHERE li.list_id = $1
        "#,
    )
    .bind(list_id)
    .bind(&accessible)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let live_prices: Vec<LiveItemPrice> =
        live_rows.into_iter().map(LivePriceRow::into_live).collect();

    // Phase 5: claims
    let claim_rows: Vec<ClaimRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.list_id, c.hauler_user_id, c.status, c.note,
               c.created_at, c.released_at,
               u.display_name AS hauler_display_name,
               ARRAY_AGG(ci.list_item_id) FILTER (WHERE ci.list_item_id IS NOT NULL AND ci.active)
                   AS item_ids
        FROM claims c
        JOIN users u ON u.id = c.hauler_user_id
        LEFT JOIN claim_items ci ON ci.claim_id = c.id
        WHERE c.list_id = $1
        GROUP BY c.id, u.display_name
        ORDER BY c.created_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let claims = claim_rows
        .into_iter()
        .map(ClaimRow::into_claim)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)?;

    // Phase 5: fulfillments (non-reversed only)
    let fulfillment_rows: Vec<FulfillmentRow> = sqlx::query_as(
        r#"
        SELECT f.id, f.list_item_id, f.claim_id, f.hauler_user_id, f.hauler_character_id,
               f.source, f.qty, f.unit_price_isk, f.bought_at_market_id, f.bought_at_note,
               f.bought_at, f.reversed_at,
               ch.character_name AS hauler_character_name,
               m.short_label     AS bought_at_market_short_label
        FROM fulfillments f
        JOIN list_items li ON li.id = f.list_item_id
        LEFT JOIN characters ch ON ch.id = f.hauler_character_id
        LEFT JOIN markets    m  ON m.id  = f.bought_at_market_id
        WHERE li.list_id = $1
          AND f.reversed_at IS NULL
        ORDER BY f.bought_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let fulfillments = fulfillment_rows
        .into_iter()
        .map(FulfillmentRow::into_fulfillment)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)?;

    let reimbursement_rows: Vec<ReimbursementRow> = sqlx::query_as(
        r#"
        SELECT r.id, r.list_id, r.requester_user_id, r.hauler_user_id,
               r.subtotal_isk, r.tip_isk, r.total_isk, r.status,
               r.settled_at, r.settled_by_user_id, r.contract_id,
               r.created_at, r.updated_at,
               r.requester_principal_id, r.hauler_principal_id,
               r.is_corp_funded, r.verified_by_wallet, r.wallet_settlement_delta_isk,
               COALESCE(ru.display_name, corp_p.name, 'Corp') AS requester_display_name,
               hu.display_name AS hauler_display_name,
               c.esi_contract_id      AS contract_esi_contract_id,
               c.status               AS contract_status,
               c.price_isk            AS contract_price_isk,
               c.expected_total_isk   AS contract_expected_total_isk,
               c.settlement_delta_isk AS contract_settlement_delta_isk,
               c.date_completed       AS contract_date_completed
        FROM reimbursements r
        -- requester may be a user or a corp (corp-funded rows have no requester_user_id)
        LEFT JOIN users ru ON ru.id = r.requester_user_id
        LEFT JOIN principals rp ON rp.id = r.requester_principal_id AND rp.kind = 'corp'
        LEFT JOIN corps corp_p ON corp_p.id = rp.corp_id
        JOIN users hu ON hu.id = r.hauler_user_id
        LEFT JOIN contracts c ON c.id = r.contract_id
        WHERE r.list_id = $1
        ORDER BY r.created_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    let reimbursements = reimbursement_rows
        .into_iter()
        .map(ReimbursementRow::into_reimbursement)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(ApiError::internal)?;

    let last_hauler_character_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT COALESCE(
            (SELECT f.hauler_character_id
             FROM fulfillments f
             JOIN list_items li ON li.id = f.list_item_id
             WHERE li.list_id = $1
               AND f.hauler_user_id = $2
               AND f.reversed_at IS NULL
               AND f.hauler_character_id IS NOT NULL
             ORDER BY f.bought_at DESC
             LIMIT 1),
            (SELECT id FROM characters WHERE user_id = $2 ORDER BY created_at ASC LIMIT 1)
        )
        "#,
    )
    .bind(list_id)
    .bind(viewer_user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::internal)?
    .flatten();

    Ok(ListDetail {
        list,
        items,
        markets,
        primary_market_id,
        live_prices,
        claims,
        fulfillments,
        reimbursements,
        last_hauler_character_id,
        viewer_user_id,
        viewer_role,
    })
}

#[derive(sqlx::FromRow)]
struct ListRow {
    id: Uuid,
    group_id: Uuid,
    created_by_user_id: Uuid,
    destination_label: Option<String>,
    notes: Option<String>,
    status: String,
    total_estimate_isk: Decimal,
    tip_pct: Decimal,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    // Phase 7
    payer_corp_id: Option<Uuid>,
    payer_division: Option<i16>,
}

impl ListRow {
    fn into_list(self) -> anyhow::Result<List> {
        let status = self
            .status
            .parse::<ListStatus>()
            .map_err(anyhow::Error::msg)?;
        Ok(List {
            id: self.id,
            group_id: self.group_id,
            created_by_user_id: self.created_by_user_id,
            destination_label: self.destination_label,
            notes: self.notes,
            status,
            total_estimate_isk: self.total_estimate_isk,
            tip_pct: self.tip_pct,
            created_at: self.created_at,
            updated_at: self.updated_at,
            payer_corp_id: self.payer_corp_id,
            payer_division: self.payer_division,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ListItemRow {
    id: Uuid,
    list_id: Uuid,
    type_id: i64,
    type_name: String,
    qty_requested: i64,
    qty_fulfilled: i64,
    est_unit_price_isk: Option<Decimal>,
    est_priced_market_id: Option<Uuid>,
    status: String,
    source_line_no: Option<i32>,
    requested_by_user_id: Uuid,
}

impl ListItemRow {
    fn into_item(self) -> anyhow::Result<ListItem> {
        let status = self
            .status
            .parse::<ListItemStatus>()
            .map_err(anyhow::Error::msg)?;
        Ok(ListItem {
            id: self.id,
            list_id: self.list_id,
            type_id: self.type_id,
            type_name: self.type_name,
            qty_requested: self.qty_requested,
            qty_fulfilled: self.qty_fulfilled,
            est_unit_price_isk: self.est_unit_price_isk,
            est_priced_market_id: self.est_priced_market_id,
            status,
            source_line_no: self.source_line_no,
            requested_by_user_id: self.requested_by_user_id,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ListSummaryRow {
    id: Uuid,
    destination_label: Option<String>,
    status: String,
    total_estimate_isk: Decimal,
    created_at: DateTime<Utc>,
    item_count: i64,
    primary_short_label: Option<String>,
}

impl ListSummaryRow {
    fn into_summary(self) -> anyhow::Result<ListSummary> {
        let status = self
            .status
            .parse::<ListStatus>()
            .map_err(anyhow::Error::msg)?;
        Ok(ListSummary {
            id: self.id,
            destination_label: self.destination_label,
            status,
            item_count: self.item_count,
            total_estimate_isk: self.total_estimate_isk,
            primary_market_short_label: self.primary_short_label,
            created_at: self.created_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct MarketWithPrimaryRow {
    id: Uuid,
    kind: String,
    esi_location_id: i64,
    region_id: Option<i64>,
    name: Option<String>,
    short_label: Option<String>,
    is_hub: bool,
    is_public: bool,
    is_primary: bool,
}

impl MarketWithPrimaryRow {
    fn into_market(self) -> anyhow::Result<Market> {
        let kind = self
            .kind
            .parse::<MarketKind>()
            .map_err(anyhow::Error::msg)?;
        Ok(Market {
            id: self.id,
            kind,
            esi_location_id: self.esi_location_id,
            region_id: self.region_id,
            name: self.name,
            short_label: self.short_label,
            is_hub: self.is_hub,
            is_public: self.is_public,
        })
    }
}

#[derive(sqlx::FromRow)]
struct LivePriceRow {
    list_item_id: Uuid,
    market_id: Uuid,
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: Option<i64>,
    buy_volume: Option<i64>,
    computed_at: Option<DateTime<Utc>>,
}

impl LivePriceRow {
    fn into_live(self) -> LiveItemPrice {
        LiveItemPrice {
            list_item_id: self.list_item_id,
            market_id: self.market_id,
            best_sell: self.best_sell,
            best_buy: self.best_buy,
            sell_volume: self.sell_volume.unwrap_or(0),
            buy_volume: self.buy_volume.unwrap_or(0),
            computed_at: self.computed_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ClaimRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub hauler_user_id: Uuid,
    pub hauler_display_name: String,
    pub status: String,
    pub note: Option<String>,
    pub item_ids: Option<Vec<Uuid>>,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

impl ClaimRow {
    pub(crate) fn into_claim(self) -> anyhow::Result<Claim> {
        let status = self
            .status
            .parse::<ClaimStatus>()
            .map_err(anyhow::Error::msg)?;
        Ok(Claim {
            id: self.id,
            list_id: self.list_id,
            hauler_user_id: self.hauler_user_id,
            hauler_display_name: self.hauler_display_name,
            status,
            note: self.note,
            item_ids: self.item_ids.unwrap_or_default(),
            created_at: self.created_at,
            released_at: self.released_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct FulfillmentRow {
    pub id: Uuid,
    pub list_item_id: Uuid,
    pub claim_id: Option<Uuid>,
    pub hauler_user_id: Uuid,
    pub hauler_character_id: Option<Uuid>,
    pub source: String,
    pub qty: i64,
    pub unit_price_isk: Decimal,
    pub bought_at_market_id: Option<Uuid>,
    pub bought_at_note: Option<String>,
    pub bought_at: DateTime<Utc>,
    pub reversed_at: Option<DateTime<Utc>>,
    pub hauler_character_name: Option<String>,
    pub bought_at_market_short_label: Option<String>,
}

impl FulfillmentRow {
    pub(crate) fn into_fulfillment(self) -> anyhow::Result<Fulfillment> {
        let source = self
            .source
            .parse::<FulfillmentSource>()
            .map_err(anyhow::Error::msg)?;
        Ok(Fulfillment {
            id: self.id,
            list_item_id: self.list_item_id,
            claim_id: self.claim_id,
            hauler_user_id: self.hauler_user_id,
            hauler_character_id: self.hauler_character_id,
            hauler_character_name: self.hauler_character_name,
            source,
            qty: self.qty,
            unit_price_isk: self.unit_price_isk,
            bought_at_market_id: self.bought_at_market_id,
            bought_at_market_short_label: self.bought_at_market_short_label,
            bought_at_note: self.bought_at_note,
            bought_at: self.bought_at,
            reversed_at: self.reversed_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ReimbursementRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub requester_user_id: Option<Uuid>,
    pub hauler_user_id: Uuid,
    pub subtotal_isk: Decimal,
    pub tip_isk: Decimal,
    pub total_isk: Decimal,
    pub status: String,
    pub settled_at: Option<DateTime<Utc>>,
    pub settled_by_user_id: Option<Uuid>,
    pub contract_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub requester_display_name: String,
    pub hauler_display_name: String,
    pub contract_esi_contract_id: Option<i64>,
    pub contract_status: Option<String>,
    pub contract_price_isk: Option<Decimal>,
    pub contract_expected_total_isk: Option<Decimal>,
    pub contract_settlement_delta_isk: Option<Decimal>,
    pub contract_date_completed: Option<DateTime<Utc>>,
    // Phase 7
    pub requester_principal_id: Uuid,
    pub hauler_principal_id: Uuid,
    pub is_corp_funded: bool,
    pub verified_by_wallet: bool,
    pub wallet_settlement_delta_isk: Option<Decimal>,
}

impl ReimbursementRow {
    pub(crate) fn into_reimbursement(self) -> anyhow::Result<Reimbursement> {
        let status = self
            .status
            .parse::<ReimbursementStatus>()
            .map_err(anyhow::Error::msg)?;
        let contract = match (
            self.contract_esi_contract_id,
            self.contract_status.as_deref(),
            self.contract_price_isk,
        ) {
            (Some(esi_id), Some(status_str), Some(price)) => {
                let cstatus = status_str
                    .parse::<domain::ContractStatus>()
                    .map_err(anyhow::Error::msg)?;
                Some(domain::ContractSummary {
                    esi_contract_id: esi_id,
                    status: cstatus,
                    price_isk: price,
                    expected_total_isk: self.contract_expected_total_isk,
                    settlement_delta_isk: self.contract_settlement_delta_isk,
                    date_completed: self.contract_date_completed,
                })
            }
            _ => None,
        };
        Ok(Reimbursement {
            id: self.id,
            list_id: self.list_id,
            requester_user_id: self.requester_user_id,
            requester_display_name: self.requester_display_name,
            hauler_user_id: self.hauler_user_id,
            hauler_display_name: self.hauler_display_name,
            subtotal_isk: self.subtotal_isk,
            tip_isk: self.tip_isk,
            total_isk: self.total_isk,
            status,
            settled_at: self.settled_at,
            settled_by_user_id: self.settled_by_user_id,
            contract_id: self.contract_id,
            contract,
            created_at: self.created_at,
            updated_at: self.updated_at,
            requester_principal_id: self.requester_principal_id,
            hauler_principal_id: self.hauler_principal_id,
            is_corp_funded: self.is_corp_funded,
            verified_by_wallet: self.verified_by_wallet,
            wallet_settlement_delta_isk: self.wallet_settlement_delta_isk,
        })
    }
}

/// Enforce `limits.lists_per_group`. Counts non-archived lists; archive
/// is the existing escape hatch for groups that hit the ceiling.
pub async fn check_lists_quota(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    cap: i64,
) -> Result<(), ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lists WHERE group_id = $1 AND status != 'archived'",
    )
    .bind(group_id)
    .fetch_one(pool)
    .await
    .map_err(ApiError::internal)?;
    if count >= cap {
        return Err(ApiError::QuotaExceeded(format!(
            "this group already has {count} active lists (cap {cap}); archive some before adding more"
        )));
    }
    Ok(())
}

async fn require_lists_quota(state: &AppState, group_id: Uuid) -> Result<(), ApiError> {
    check_lists_quota(&state.pool, group_id, state.config.limits.lists_per_group).await
}
