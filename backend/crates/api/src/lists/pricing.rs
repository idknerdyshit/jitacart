use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use domain::{Market, MarketKind};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{db::Tx, markets::MarketRow, state::AppState};

pub(super) const RECOMPUTE_RETRY_LIMIT: usize = 3;

pub(super) fn dedup_type_ids(iter: impl IntoIterator<Item = i64>) -> Vec<i64> {
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
pub(super) async fn fetch_prices_for_markets(
    state: &AppState,
    tx: &Tx,
    group_id: Uuid,
    markets: &[Market],
    type_ids: &[i64],
) -> Result<HashMap<Uuid, HashMap<i64, market::PriceAggregate>>, crate::errors::ApiError> {
    let npc_ttl = state.config.esi.poll_intervals_secs.market_prices as i64;
    let citadel_ttl = state
        .config
        .esi
        .poll_intervals_secs
        .citadel_orders
        .saturating_mul(2) as i64;
    let citadel_ids: Vec<Uuid> = markets
        .iter()
        .filter(|m| matches!(m.kind, MarketKind::PublicStructure))
        .map(|m| m.id)
        .collect();
    // accessible_market_ids joins through group_memberships (RLS-enabled), so
    // it MUST run on the request tx where `app.current_user_id` is set.
    let accessible = if citadel_ids.is_empty() {
        HashSet::new()
    } else {
        let mut conn = tx.acquire().await;
        accessible_market_ids(&mut **conn, group_id, &citadel_ids).await?
    };
    let futs = markets.iter().map(|m| {
        let accessible = &accessible;
        async move {
            let map = match m.kind {
                MarketKind::NpcHub => {
                    market::get_or_refresh_prices(&state.pool, &state.esi, m, type_ids, npc_ttl)
                        .await?
                }
                MarketKind::PublicStructure => {
                    read_group_citadel_prices(state, m, type_ids, citadel_ttl, accessible).await?
                }
            };
            Ok::<_, anyhow::Error>((m.id, map))
        }
    });
    let results = futures_util::future::try_join_all(futs).await?;
    Ok(results.into_iter().collect())
}

pub(super) async fn read_group_citadel_prices(
    state: &AppState,
    market: &Market,
    type_ids: &[i64],
    ttl_secs: i64,
    accessible: &HashSet<Uuid>,
) -> anyhow::Result<HashMap<i64, market::PriceAggregate>> {
    if !market.is_public || type_ids.is_empty() || !accessible.contains(&market.id) {
        return Ok(HashMap::new());
    }
    read_cached_prices(&state.pool, market.id, type_ids, ttl_secs).await
}

/// Subset of `market_ids` that the group can read prices for: NPC hubs are
/// always accessible; citadels are accessible iff some group member has an
/// `ok` `character_structure_access` row plus the structure-markets scope.
pub(crate) async fn accessible_market_ids(
    executor: impl sqlx::PgExecutor<'_>,
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
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().collect())
}

pub(super) async fn read_cached_prices(
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

pub(super) fn pick_cheapest(
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
pub(super) async fn recompute_estimates(
    state: &AppState,
    tx: &Tx,
    list_id: Uuid,
    mut updated_after: DateTime<Utc>,
) -> Result<(), crate::errors::ApiError> {
    for _ in 0..RECOMPUTE_RETRY_LIMIT {
        let mut conn = tx.acquire().await;
        let group_id: Uuid = sqlx::query_scalar("SELECT group_id FROM lists WHERE id = $1")
            .bind(list_id)
            .fetch_optional(&mut **conn)
            .await?
            .ok_or_else(crate::errors::ApiError::not_found)?;

        let market_rows: Vec<MarketRow> = sqlx::query_as(
            "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                    m.is_hub, m.is_public \
             FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
             WHERE lm.list_id = $1",
        )
        .bind(list_id)
        .fetch_all(&mut **conn)
        .await?;
        let markets: Vec<domain::Market> = market_rows
            .into_iter()
            .map(MarketRow::into_market)
            .collect();

        let items: Vec<(Uuid, i64)> =
            sqlx::query_as("SELECT id, type_id FROM list_items WHERE list_id = $1")
                .bind(list_id)
                .fetch_all(&mut **conn)
                .await?;

        drop(conn);

        let type_ids = dedup_type_ids(items.iter().map(|(_, t)| *t));
        let prices_by_market =
            fetch_prices_for_markets(state, tx, group_id, &markets, &type_ids).await?;

        let mut conn = tx.acquire().await;
        let current_updated: DateTime<Utc> =
            sqlx::query_scalar("SELECT updated_at FROM lists WHERE id = $1 FOR UPDATE")
                .bind(list_id)
                .fetch_optional(&mut **conn)
                .await?
                .ok_or_else(crate::errors::ApiError::not_found)?;

        if current_updated != updated_after {
            updated_after = current_updated;
            // drop conn and retry (no rollback needed — we're in the request tx)
            drop(conn);
            continue;
        }

        // Build the (id, est_unit, est_market) triples then push them in one
        // round-trip via UNNEST. RETURNING gives us the qty per item so we
        // can fold into the running total without a second SELECT.
        let mut item_ids: Vec<Uuid> = Vec::with_capacity(items.len());
        let mut est_units: Vec<Option<Decimal>> = Vec::with_capacity(items.len());
        let mut est_markets: Vec<Option<Uuid>> = Vec::with_capacity(items.len());
        for (item_id, type_id) in &items {
            let (est_unit, est_market) = pick_cheapest(&markets, &prices_by_market, *type_id);
            item_ids.push(*item_id);
            est_units.push(est_unit);
            est_markets.push(est_market);
        }

        #[derive(sqlx::FromRow)]
        struct UpdRow {
            id: Uuid,
            qty_requested: i64,
        }
        let updated: Vec<UpdRow> = if items.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "UPDATE list_items li \
                 SET est_unit_price_isk = src.est_unit, \
                     est_priced_market_id = src.est_market \
                 FROM UNNEST($1::uuid[], $2::numeric[], $3::uuid[]) \
                     AS src(id, est_unit, est_market) \
                 WHERE li.id = src.id \
                 RETURNING li.id, li.qty_requested",
            )
            .bind(&item_ids)
            .bind(&est_units)
            .bind(&est_markets)
            .fetch_all(&mut **conn)
            .await?
        };

        // RETURNING order isn't guaranteed; index est_unit by item_id so the
        // total is computed deterministically.
        let est_by_id: HashMap<Uuid, Option<Decimal>> = item_ids
            .iter()
            .copied()
            .zip(est_units.iter().copied())
            .collect();
        let mut total: Decimal = Decimal::ZERO;
        for r in &updated {
            if let Some(Some(u)) = est_by_id.get(&r.id) {
                total += *u * Decimal::from(r.qty_requested);
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
        .execute(&mut **conn)
        .await?;
        return Ok(());
    }
    Err(crate::errors::ApiError::Conflict(
        "list was concurrently modified; please retry".into(),
    ))
}
