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
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{
    multibuy::{parse_multibuy, LineError, ParsedLine},
    List, ListDetail, ListItem, ListItemStatus, ListStatus, ListSummary, LiveItemPrice, Market,
    MarketKind, ResolvedType,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    extract::{CurrentGroup, CurrentList},
    markets::MarketRow,
    state::AppState,
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
) -> Result<Json<PreviewResponse>, ListError> {
    validate_multibuy_size(&body.multibuy)?;
    validate_market_ids_size(&body.market_ids)?;

    let parsed = parse_multibuy(&body.multibuy);
    if parsed.lines.len() > MAX_PARSED_LINES {
        return Err(ListError::BadRequest(format!(
            "too many parsed lines ({}); max {}",
            parsed.lines.len(),
            MAX_PARSED_LINES
        )));
    }
    let distinct_names: Vec<String> = parsed.lines.iter().map(|l| l.name.clone()).collect();
    if distinct_names.len() > MAX_DISTINCT_NAMES {
        return Err(ListError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            distinct_names.len(),
            MAX_DISTINCT_NAMES
        )));
    }

    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &distinct_names)
        .await
        .map_err(internal)?;

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
        user_id, group_id, ..
    }: CurrentGroup,
    Json(body): Json<CreateBody>,
) -> Result<Json<ListDetail>, ListError> {
    validate_multibuy_size(&body.multibuy)?;
    validate_market_ids_size(&body.market_ids)?;
    if !body.market_ids.contains(&body.primary_market_id) {
        return Err(ListError::BadRequest(
            "primary_market_id must be in market_ids".into(),
        ));
    }

    let parsed = parse_multibuy(&body.multibuy);
    if !parsed.errors.is_empty() {
        return Err(ListError::BadRequest(format!(
            "{} multibuy line(s) failed to parse",
            parsed.errors.len()
        )));
    }
    if parsed.lines.is_empty() {
        return Err(ListError::BadRequest("multibuy is empty".into()));
    }
    if parsed.lines.len() > MAX_PARSED_LINES {
        return Err(ListError::BadRequest(format!(
            "too many parsed lines ({}); max {}",
            parsed.lines.len(),
            MAX_PARSED_LINES
        )));
    }
    let distinct_names: Vec<String> = parsed.lines.iter().map(|l| l.name.clone()).collect();
    if distinct_names.len() > MAX_DISTINCT_NAMES {
        return Err(ListError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            distinct_names.len(),
            MAX_DISTINCT_NAMES
        )));
    }

    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &distinct_names)
        .await
        .map_err(internal)?;
    if !unresolved.is_empty() {
        return Err(ListError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    // Refresh prices up front so the snapshot insert below sees fresh data.
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
            .ok_or_else(|| internal(anyhow::anyhow!("resolved missing for {}", p.name)))?;
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

    let mut tx = state.pool.begin().await.map_err(internal)?;

    let list_id: Uuid = sqlx::query_scalar(
        "INSERT INTO lists (group_id, created_by_user_id, destination_label, notes, total_estimate_isk) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(group_id)
    .bind(user_id)
    .bind(body.destination_label.as_deref())
    .bind(body.notes.as_deref())
    .bind(total)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;

    for m in &markets {
        sqlx::query(
            "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
        )
        .bind(list_id)
        .bind(m.id)
        .bind(m.id == body.primary_market_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
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
        .map_err(internal)?;
    }

    tx.commit().await.map_err(internal)?;

    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn list_for_group(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
) -> Result<Json<Vec<ListSummary>>, ListError> {
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
        WHERE l.group_id = $1
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(group_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    rows.into_iter()
        .map(ListSummaryRow::into_summary)
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json)
        .map_err(internal)
}

async fn detail(
    State(state): State<AppState>,
    CurrentList { list_id, .. }: CurrentList,
) -> Result<Json<ListDetail>, ListError> {
    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn patch_list(
    State(state): State<AppState>,
    CurrentList { list_id, .. }: CurrentList,
    Json(body): Json<PatchListBody>,
) -> Result<Json<ListDetail>, ListError> {
    if body.destination_label.is_none() && body.notes.is_none() && body.status.is_none() {
        return Err(ListError::BadRequest("nothing to update".into()));
    }

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
    q.build().execute(&state.pool).await.map_err(internal)?;

    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn delete_list(
    State(state): State<AppState>,
    cur: CurrentList,
) -> Result<StatusCode, ListError> {
    if cur.created_by_user_id != cur.user_id && cur.role != domain::GroupRole::Owner {
        return Err(ListError::Forbidden);
    }
    let r = sqlx::query("DELETE FROM lists WHERE id = $1")
        .bind(cur.list_id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(ListError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn replace_markets(
    State(state): State<AppState>,
    CurrentList {
        list_id, group_id, ..
    }: CurrentList,
    Json(body): Json<ReplaceMarketsBody>,
) -> Result<Json<ListDetail>, ListError> {
    validate_market_ids_size(&body.market_ids)?;
    if !body.market_ids.contains(&body.primary_market_id) {
        return Err(ListError::BadRequest(
            "primary_market_id must be in market_ids".into(),
        ));
    }
    let markets = load_markets(&state, &body.market_ids).await?;
    validate_markets_for_group(&state.pool, group_id, &markets).await?;

    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .ok_or(ListError::NotFound)?;

        sqlx::query("DELETE FROM list_markets WHERE list_id = $1")
            .bind(list_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        for m in &markets {
            sqlx::query(
                "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
            )
            .bind(list_id)
            .bind(m.id)
            .bind(m.id == body.primary_market_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn add_items(
    State(state): State<AppState>,
    CurrentList {
        list_id, user_id, ..
    }: CurrentList,
    Json(body): Json<AddItemsBody>,
) -> Result<Json<ListDetail>, ListError> {
    let new_lines: Vec<(String, i64, i32)> = match (body.multibuy, body.type_name, body.qty) {
        (Some(mb), _, _) => {
            validate_multibuy_size(&mb)?;
            let parsed = parse_multibuy(&mb);
            if !parsed.errors.is_empty() {
                return Err(ListError::BadRequest(format!(
                    "{} multibuy line(s) failed to parse",
                    parsed.errors.len()
                )));
            }
            if parsed.lines.is_empty() {
                return Err(ListError::BadRequest("multibuy is empty".into()));
            }
            if parsed.lines.len() > MAX_PARSED_LINES {
                return Err(ListError::BadRequest(format!(
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
                return Err(ListError::BadRequest("qty must be positive".into()));
            }
            vec![(name, qty, 0)]
        }
        _ => {
            return Err(ListError::BadRequest(
                "provide either {multibuy} or {type_name, qty}".into(),
            ))
        }
    };

    let names: Vec<String> = new_lines.iter().map(|(n, _, _)| n.clone()).collect();
    if names.len() > MAX_DISTINCT_NAMES {
        return Err(ListError::BadRequest(format!(
            "too many distinct items ({}); max {}",
            names.len(),
            MAX_DISTINCT_NAMES
        )));
    }
    let (resolved, unresolved) = market::resolve_type_ids(&state.pool, &state.esi, &names)
        .await
        .map_err(internal)?;
    if !unresolved.is_empty() {
        return Err(ListError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .ok_or(ListError::NotFound)?;
        for (name, qty, line_no) in &new_lines {
            let key = market::types::normalize_key(name);
            let r = resolved
                .get(&key)
                .ok_or_else(|| internal(anyhow::anyhow!("resolved missing for {}", name)))?;
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
            .map_err(internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn patch_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
    Json(body): Json<PatchItemBody>,
) -> Result<Json<ListDetail>, ListError> {
    debug_assert_eq!(cur.list_id, list_id);
    if body.qty_requested.is_none() && body.type_name.is_none() {
        return Err(ListError::BadRequest("nothing to update".into()));
    }

    // If retyping, resolve before any tx.
    let resolved_type: Option<ResolvedType> = if let Some(name) = &body.type_name {
        let (resolved, unresolved) =
            market::resolve_type_ids(&state.pool, &state.esi, std::slice::from_ref(name))
                .await
                .map_err(internal)?;
        if !unresolved.is_empty() {
            return Err(ListError::BadRequest(format!("unknown item: {name}")));
        }
        let key = market::types::normalize_key(name);
        Some(
            resolved
                .get(&key)
                .cloned()
                .ok_or_else(|| internal(anyhow::anyhow!("resolved missing for {name}")))?,
        )
    } else {
        None
    };

    if let Some(qty) = body.qty_requested {
        if qty <= 0 {
            return Err(ListError::BadRequest(
                "qty_requested must be positive".into(),
            ));
        }
    }

    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .ok_or(ListError::NotFound)?;
        let _: (Uuid,) =
            sqlx::query_as("SELECT id FROM list_items WHERE id = $1 AND list_id = $2 FOR UPDATE")
                .bind(item_id)
                .bind(list_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?
                .ok_or(ListError::NotFound)?;
        if let Some(qty) = body.qty_requested {
            sqlx::query("UPDATE list_items SET qty_requested = $1 WHERE id = $2 AND list_id = $3")
                .bind(qty)
                .bind(item_id)
                .bind(list_id)
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
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
            .map_err(internal)?;
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id).await?;
    Ok(Json(detail))
}

async fn delete_item(
    State(state): State<AppState>,
    Path((list_id, item_id)): Path<(Uuid, Uuid)>,
    cur: CurrentList,
) -> Result<Json<ListDetail>, ListError> {
    debug_assert_eq!(cur.list_id, list_id);
    let updated_after = {
        let mut tx = state.pool.begin().await.map_err(internal)?;
        let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
            .bind(list_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .ok_or(ListError::NotFound)?;
        let r = sqlx::query("DELETE FROM list_items WHERE id = $1 AND list_id = $2")
            .bind(item_id)
            .bind(list_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        if r.rows_affected() == 0 {
            return Err(ListError::NotFound);
        }
        let updated_after: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
        )
        .bind(list_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        updated_after
    };

    recompute_estimates(&state, list_id, updated_after).await?;
    let detail = load_list_detail(&state, list_id).await?;
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
) -> Result<HashMap<Uuid, HashMap<i64, market::PriceAggregate>>, ListError> {
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
        .map_err(internal)?;
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
async fn accessible_market_ids(
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

fn validate_multibuy_size(mb: &str) -> Result<(), ListError> {
    if mb.len() > MAX_MULTIBUY_BYTES {
        return Err(ListError::BadRequest(format!(
            "multibuy too large ({} bytes); max {}",
            mb.len(),
            MAX_MULTIBUY_BYTES
        )));
    }
    Ok(())
}

fn validate_market_ids_size(ids: &[Uuid]) -> Result<(), ListError> {
    if ids.is_empty() {
        return Err(ListError::BadRequest("market_ids must not be empty".into()));
    }
    if ids.len() > MAX_MARKET_IDS {
        return Err(ListError::BadRequest(format!(
            "too many market_ids ({}); max {}",
            ids.len(),
            MAX_MARKET_IDS
        )));
    }
    let dedup: HashSet<Uuid> = ids.iter().copied().collect();
    if dedup.len() != ids.len() {
        return Err(ListError::BadRequest(
            "market_ids contains duplicates".into(),
        ));
    }
    Ok(())
}

async fn load_markets(state: &AppState, ids: &[Uuid]) -> Result<Vec<Market>, ListError> {
    let rows: Vec<MarketRow> = sqlx::query_as(
        "SELECT id, kind, esi_location_id, region_id, name, short_label, is_hub, is_public \
         FROM markets WHERE id = ANY($1::uuid[])",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    if rows.len() != ids.len() {
        return Err(ListError::BadRequest(
            "one or more market_ids do not exist".into(),
        ));
    }
    rows.into_iter()
        .map(MarketRow::into_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(internal)
}

/// Allow if every market is either an NPC hub or present in
/// `group_tracked_markets` for the given group. Discovered-but-untracked
/// citadels are rejected so a user can't pull a Keepstar into a list for a
/// group that hasn't opted in to it.
async fn validate_markets_for_group(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    markets: &[Market],
) -> Result<(), ListError> {
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
        .map_err(internal)?
        .into_iter()
        .collect()
    };

    for m in markets {
        let label = m.short_label.as_deref().unwrap_or("(unnamed)");
        if !m.is_public {
            return Err(ListError::BadRequest(format!(
                "market {label} is not public"
            )));
        }
        match m.kind {
            MarketKind::NpcHub => {
                if !m.is_hub {
                    return Err(ListError::BadRequest(format!(
                        "market {label} is marked as NPC hub but is_hub=false"
                    )));
                }
            }
            MarketKind::PublicStructure => {
                if !tracked.contains(&m.id) {
                    return Err(ListError::BadRequest(format!(
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
) -> Result<(), ListError> {
    for _ in 0..RECOMPUTE_RETRY_LIMIT {
        let group_id: Uuid = sqlx::query_scalar("SELECT group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?
            .ok_or(ListError::NotFound)?;

        let market_rows: Vec<MarketRow> = sqlx::query_as(
            "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                    m.is_hub, m.is_public \
             FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
             WHERE lm.list_id = $1",
        )
        .bind(list_id)
        .fetch_all(&state.pool)
        .await
        .map_err(internal)?;
        let markets: Vec<Market> = market_rows
            .into_iter()
            .map(MarketRow::into_market)
            .collect::<anyhow::Result<Vec<_>>>()
            .map_err(internal)?;

        let items: Vec<(Uuid, i64)> =
            sqlx::query_as("SELECT id, type_id FROM list_items WHERE list_id = $1")
                .bind(list_id)
                .fetch_all(&state.pool)
                .await
                .map_err(internal)?;

        let type_ids = dedup_type_ids(items.iter().map(|(_, t)| *t));
        let prices_by_market =
            fetch_prices_for_markets(state, group_id, &markets, &type_ids).await?;

        let mut tx = state.pool.begin().await.map_err(internal)?;
        let current_updated: DateTime<Utc> =
            sqlx::query_scalar("SELECT updated_at FROM lists WHERE id = $1 FOR UPDATE")
                .bind(list_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?
                .ok_or(ListError::NotFound)?;

        if current_updated != updated_after {
            updated_after = current_updated;
            tx.rollback().await.map_err(internal)?;
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
            .map_err(internal)?;
            if let Some(u) = est_unit {
                total += u * Decimal::from(qty);
            }
        }

        // Skip the write when the total is unchanged so downstream consumers
        // of `updated_at` don't see no-op churn.
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
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        return Ok(());
    }
    Err(ListError::Conflict)
}

async fn load_list_detail(state: &AppState, list_id: Uuid) -> Result<ListDetail, ListError> {
    let list_row: ListRow = sqlx::query_as(
        "SELECT id, group_id, created_by_user_id, destination_label, notes, status, \
                total_estimate_isk, created_at, updated_at \
         FROM lists WHERE id = $1",
    )
    .bind(list_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or(ListError::NotFound)?;
    let group_id = list_row.group_id;
    let list = list_row.into_list().map_err(internal)?;

    let item_rows: Vec<ListItemRow> = sqlx::query_as(
        "SELECT id, list_id, type_id, type_name, qty_requested, qty_fulfilled, \
                est_unit_price_isk, est_priced_market_id, status, source_line_no \
         FROM list_items WHERE list_id = $1 ORDER BY source_line_no NULLS LAST, created_at",
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    let items = item_rows
        .into_iter()
        .map(ListItemRow::into_item)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(internal)?;

    let market_rows: Vec<MarketWithPrimaryRow> = sqlx::query_as(
        "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                m.is_hub, m.is_public, lm.is_primary \
         FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
         WHERE lm.list_id = $1 ORDER BY lm.is_primary DESC, m.short_label",
    )
    .bind(list_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    let primary_market_id = market_rows
        .iter()
        .find_map(|r| if r.is_primary { Some(r.id) } else { None })
        .ok_or_else(|| internal(anyhow::anyhow!("list has no primary market")))?;
    let markets = market_rows
        .into_iter()
        .map(MarketWithPrimaryRow::into_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(internal)?;

    let market_ids: Vec<Uuid> = markets.iter().map(|m| m.id).collect();
    let accessible: Vec<Uuid> = accessible_market_ids(&state.pool, group_id, &market_ids)
        .await
        .map_err(internal)?
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
    .map_err(internal)?;
    let live_prices: Vec<LiveItemPrice> =
        live_rows.into_iter().map(LivePriceRow::into_live).collect();

    Ok(ListDetail {
        list,
        items,
        markets,
        primary_market_id,
        live_prices,
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
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
            created_at: self.created_at,
            updated_at: self.updated_at,
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

pub enum ListError {
    BadRequest(String),
    NotFound,
    Forbidden,
    Conflict,
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> ListError {
    ListError::Internal(e.into())
}

impl IntoResponse for ListError {
    fn into_response(self) -> Response {
        match self {
            ListError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ListError::NotFound => (StatusCode::NOT_FOUND, "list not found").into_response(),
            ListError::Forbidden => {
                (StatusCode::FORBIDDEN, "you cannot perform this action").into_response()
            }
            ListError::Conflict => (
                StatusCode::CONFLICT,
                "list was concurrently modified; please retry",
            )
                .into_response(),
            ListError::Internal(e) => {
                tracing::error!(error = ?e, "lists handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
