use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{ListDetail, ListStatus, ListSummary};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::detail::load_list_detail;
use super::pricing::{
    dedup_type_ids, fetch_prices_for_markets, pick_cheapest, recompute_estimates,
};
use crate::{
    db::Tx,
    errors::ApiError,
    extract::{CurrentGroup, CurrentList},
    state::AppState,
    webhooks::{fire_webhook, WebhookEvent},
};

#[derive(Deserialize)]
pub(super) struct PreviewBody {
    multibuy: String,
    market_ids: Vec<Uuid>,
}

#[derive(Serialize)]
pub(super) struct PreviewResponse {
    lines: Vec<PreviewLine>,
    unresolved_names: Vec<String>,
    errors: Vec<domain::multibuy::LineError>,
}

#[derive(Serialize)]
pub(super) struct PreviewLine {
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
pub(super) struct PreviewPrice {
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: i64,
    buy_volume: i64,
    computed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
    destination_label: Option<String>,
    notes: Option<String>,
    market_ids: Vec<Uuid>,
    primary_market_id: Uuid,
    multibuy: String,
}

#[derive(Deserialize)]
pub(super) struct PatchListBody {
    destination_label: Option<Option<String>>,
    notes: Option<Option<String>>,
    status: Option<ListStatus>,
}

#[derive(Deserialize)]
pub(super) struct ReplaceMarketsBody {
    market_ids: Vec<Uuid>,
    primary_market_id: Uuid,
}

#[derive(Deserialize)]
pub(super) struct ListForGroupQuery {
    #[serde(default)]
    include_archived: bool,
}

#[derive(sqlx::FromRow)]
struct ListSummaryRow {
    id: Uuid,
    destination_label: Option<String>,
    status: ListStatus,
    total_estimate_isk: Decimal,
    created_at: DateTime<Utc>,
    item_count: i64,
    primary_short_label: Option<String>,
}

impl ListSummaryRow {
    fn into_summary(self) -> ListSummary {
        ListSummary {
            id: self.id,
            destination_label: self.destination_label,
            status: self.status,
            item_count: self.item_count,
            total_estimate_isk: self.total_estimate_isk,
            primary_market_short_label: self.primary_short_label,
            created_at: self.created_at,
        }
    }
}

pub(super) async fn preview(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
    tx: Tx,
    Json(body): Json<PreviewBody>,
) -> Result<Json<PreviewResponse>, ApiError> {
    use super::{MAX_DISTINCT_NAMES, MAX_PARSED_LINES};
    use domain::multibuy::{parse_multibuy, ParsedLine};
    use domain::ResolvedType;

    super::validate_multibuy_size(&body.multibuy)?;
    super::validate_market_ids_size(&body.market_ids)?;

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

    let markets = {
        let mut conn = tx.acquire().await;
        let m = super::load_markets(&mut **conn, &body.market_ids).await?;
        super::validate_markets_for_group(&mut **conn, group_id, &m).await?;
        m
    };

    // market::resolve_type_ids hits `type_cache` (not RLS-enabled), so a
    // pool connection without app.current_user_id is fine here.
    let (resolved, unresolved) =
        market::resolve_type_ids(&state.pool, &state.esi, &distinct_names).await?;

    let type_ids = dedup_type_ids(resolved.values().map(|r| r.type_id.get() as i64));
    let prices_by_market =
        fetch_prices_for_markets(&state, &tx, group_id, &markets, &type_ids).await?;

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
                        .and_then(|map| map.get(&(r.type_id.get() as i64)));
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
                type_id: resolved_for_line.map(|r| r.type_id.get() as i64),
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

pub(super) async fn create(
    State(state): State<AppState>,
    CurrentGroup {
        user_id,
        group_id,
        role,
    }: CurrentGroup,
    tx: Tx,
    Json(body): Json<CreateBody>,
) -> Result<Json<ListDetail>, ApiError> {
    use super::{MAX_DISTINCT_NAMES, MAX_PARSED_LINES};
    use domain::multibuy::parse_multibuy;

    super::validate_multibuy_size(&body.multibuy)?;
    super::validate_market_ids_size(&body.market_ids)?;
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
    let markets = {
        let mut conn = tx.acquire().await;
        super::require_lists_quota(&mut **conn, &state, group_id).await?;
        let m = super::load_markets(&mut **conn, &body.market_ids).await?;
        super::validate_markets_for_group(&mut **conn, group_id, &m).await?;
        m
    };

    let (resolved, unresolved) =
        market::resolve_type_ids(&state.pool, &state.esi, &distinct_names).await?;
    if !unresolved.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "unknown item(s): {}",
            unresolved.join(", ")
        )));
    }

    let type_ids = dedup_type_ids(resolved.values().map(|r| r.type_id.get() as i64));
    let prices_by_market =
        fetch_prices_for_markets(&state, &tx, group_id, &markets, &type_ids).await?;

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
        let r = resolved.get(&key).ok_or_else(|| {
            ApiError::internal(anyhow::anyhow!("resolved missing for {}", p.name))
        })?;
        let (est_unit, est_market) =
            pick_cheapest(&markets, &prices_by_market, r.type_id.get() as i64);
        if let Some(u) = est_unit {
            total += u * Decimal::from(p.qty);
        }
        items_to_insert.push(ItemRow {
            type_id: r.type_id.get() as i64,
            type_name: r.type_name.clone(),
            qty: p.qty,
            line_no: *p.line_nos.first().unwrap_or(&0) as i32,
            est_unit,
            est_market,
        });
    }

    let mut conn = tx.acquire().await;

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
    .fetch_one(&mut **conn)
    .await?;

    for m in &markets {
        sqlx::query(
            "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
        )
        .bind(list_id)
        .bind(m.id)
        .bind(m.id == body.primary_market_id)
        .execute(&mut **conn)
        .await?;
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
        .execute(&mut **conn)
        .await?;
    }

    let creator_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&mut **conn)
        .await?;
    let event = WebhookEvent::ListCreated {
        list_destination: body.destination_label.unwrap_or_else(|| "(unnamed)".into()),
        item_count: items_to_insert.len() as i64,
        estimate_isk: total.to_string(),
        creator_name,
    };
    fire_webhook(&mut **conn, group_id, &event).await?;
    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn list_for_group(
    State(_state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
    tx: Tx,
    Query(q): Query<ListForGroupQuery>,
) -> Result<Json<Vec<ListSummary>>, ApiError> {
    let mut conn = tx.acquire().await;
    let rows: Vec<ListSummaryRow> = sqlx::query_as(
        r#"
        SELECT l.id,
               l.destination_label,
               l.status,
               l.total_estimate_isk,
               l.created_at,
               COALESCE(agg.item_count, 0) AS item_count,
               (SELECT m.short_label
                  FROM list_markets lm JOIN markets m ON m.id = lm.market_id
                  WHERE lm.list_id = l.id AND lm.is_primary
                  LIMIT 1) AS primary_short_label
        FROM lists l
        LEFT JOIN (
            SELECT list_id, count(*) AS item_count
            FROM list_items
            GROUP BY list_id
        ) agg ON agg.list_id = l.id
        WHERE l.group_id = $1 AND ($2 OR l.status <> 'archived')
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(group_id)
    .bind(q.include_archived)
    .fetch_all(&mut **conn)
    .await?;

    Ok(Json(
        rows.into_iter().map(ListSummaryRow::into_summary).collect(),
    ))
}

pub(super) async fn detail(
    State(state): State<AppState>,
    CurrentList {
        list_id,
        user_id,
        role,
        ..
    }: CurrentList,
    tx: Tx,
) -> Result<Json<ListDetail>, ApiError> {
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn patch_list(
    State(state): State<AppState>,
    cur: CurrentList,
    tx: Tx,
    Json(body): Json<PatchListBody>,
) -> Result<Json<ListDetail>, ApiError> {
    if body.destination_label.is_none() && body.notes.is_none() && body.status.is_none() {
        return Err(ApiError::BadRequest("nothing to update".into()));
    }
    if body.status.is_none() {
        cur.require_mutable()?;
    }
    if body.status.is_some() {
        cur.require_can_manage()?;
    }
    let CurrentList {
        list_id,
        user_id,
        role,
        ..
    } = cur;

    let mut conn = tx.acquire().await;
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
        q.push_bind(status);
    }
    q.push(" WHERE id = ");
    q.push_bind(list_id);
    q.build().execute(&mut **conn).await?;

    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn delete_list(
    State(_state): State<AppState>,
    cur: CurrentList,
    tx: Tx,
) -> Result<StatusCode, ApiError> {
    if cur.created_by_user_id != cur.user_id && cur.role != domain::GroupRole::Owner {
        return Err(ApiError::forbidden());
    }
    let mut conn = tx.acquire().await;
    let r = sqlx::query("DELETE FROM lists WHERE id = $1")
        .bind(cur.list_id)
        .execute(&mut **conn)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn replace_markets(
    State(state): State<AppState>,
    cur: CurrentList,
    tx: Tx,
    Json(body): Json<ReplaceMarketsBody>,
) -> Result<Json<ListDetail>, ApiError> {
    cur.require_mutable()?;
    let CurrentList {
        list_id,
        group_id,
        user_id,
        role,
        ..
    } = cur;
    super::validate_market_ids_size(&body.market_ids)?;
    if !body.market_ids.contains(&body.primary_market_id) {
        return Err(ApiError::BadRequest(
            "primary_market_id must be in market_ids".into(),
        ));
    }
    let mut conn = tx.acquire().await;
    let markets = super::load_markets(&mut **conn, &body.market_ids).await?;
    super::validate_markets_for_group(&mut **conn, group_id, &markets).await?;

    let _: (Uuid,) = sqlx::query_as("SELECT id FROM lists WHERE id = $1 FOR UPDATE")
        .bind(list_id)
        .fetch_optional(&mut **conn)
        .await?
        .ok_or_else(ApiError::not_found)?;

    sqlx::query("DELETE FROM list_markets WHERE list_id = $1")
        .bind(list_id)
        .execute(&mut **conn)
        .await?;
    for m in &markets {
        sqlx::query(
            "INSERT INTO list_markets (list_id, market_id, is_primary) VALUES ($1, $2, $3)",
        )
        .bind(list_id)
        .bind(m.id)
        .bind(m.id == body.primary_market_id)
        .execute(&mut **conn)
        .await?;
    }
    let updated_after: DateTime<Utc> = sqlx::query_scalar(
        "UPDATE lists SET updated_at = now() WHERE id = $1 RETURNING updated_at",
    )
    .bind(list_id)
    .fetch_one(&mut **conn)
    .await?;
    drop(conn);

    recompute_estimates(&state, &tx, list_id, updated_after).await?;
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}
