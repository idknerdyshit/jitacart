//! Per-(market, type_id) price aggregation, fetched from ESI and cached
//! in `market_prices` with a TTL.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use domain::Market;
use nea_esi::EsiClient;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::Semaphore;

#[derive(Debug, Clone)]
pub struct PriceAggregate {
    pub best_sell: Option<Decimal>,
    pub best_buy: Option<Decimal>,
    pub sell_volume: i64,
    pub buy_volume: i64,
    pub computed_at: DateTime<Utc>,
}

/// Process-wide cap on concurrent ESI fetches issued by this module.
fn esi_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(8))
}

/// Read cached prices, then fan out fresh fetches for any stale or missing
/// rows. Never holds a tx across an ESI call.
pub async fn get_or_refresh_prices(
    pool: &PgPool,
    esi: &EsiClient,
    market: &Market,
    type_ids: &[i64],
    ttl_secs: i64,
) -> anyhow::Result<HashMap<i64, PriceAggregate>> {
    if type_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Read fresh rows up front; we'll only fetch the rest from ESI.
    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::seconds(ttl_secs.max(0));
    let rows: Vec<PriceRow> = sqlx::query_as::<_, PriceRow>(
        "SELECT type_id, best_sell, best_buy, sell_volume, buy_volume, computed_at \
         FROM market_prices \
         WHERE market_id = $1 AND type_id = ANY($2::bigint[]) AND computed_at >= $3",
    )
    .bind(market.id)
    .bind(type_ids)
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    let mut out: HashMap<i64, PriceAggregate> = HashMap::new();
    for r in rows {
        out.insert(
            r.type_id,
            PriceAggregate {
                best_sell: r.best_sell,
                best_buy: r.best_buy,
                sell_volume: r.sell_volume,
                buy_volume: r.buy_volume,
                computed_at: r.computed_at,
            },
        );
    }

    let stale: Vec<i64> = type_ids
        .iter()
        .copied()
        .filter(|id| !out.contains_key(id))
        .collect();
    if stale.is_empty() {
        return Ok(out);
    }

    // Fan out through `refresh_one`, which owns the module-wide semaphore.
    // Use FuturesUnordered so
    // we can borrow the caller's `esi` reference across the futures without
    // cloning into 'static spawned tasks.
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let mut futs = FuturesUnordered::new();
    for type_id in stale {
        let pool = pool.clone();
        let market = market.clone();
        futs.push(async move {
            let res = refresh_one(&pool, esi, &market, type_id).await;
            (type_id, res)
        });
    }

    while let Some((type_id, res)) = futs.next().await {
        match res {
            Ok(agg) => {
                out.insert(type_id, agg);
            }
            Err(e) => {
                tracing::warn!(error = ?e, market = %market.short_label, type_id, "price refresh failed");
            }
        }
    }

    Ok(out)
}

/// Fetch market orders for `(region, type_id)`, filter to the market's
/// `esi_location_id`, compute aggregates, and upsert.
pub async fn refresh_one(
    pool: &PgPool,
    esi: &EsiClient,
    market: &Market,
    type_id: i64,
) -> anyhow::Result<PriceAggregate> {
    let _permit = esi_semaphore()
        .acquire()
        .await
        .map_err(|_| anyhow::anyhow!("ESI semaphore closed"))?;

    let region_id_i32: i32 = market
        .region_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("region_id {} does not fit in i32", market.region_id))?;
    let type_id_i32: i32 = type_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("type_id {} does not fit in i32", type_id))?;

    let orders = esi
        .market_orders(region_id_i32, type_id_i32, None)
        .await
        .map_err(|e| anyhow::anyhow!("ESI market_orders: {e}"))?;

    let mut best_sell_f: Option<f64> = None;
    let mut best_buy_f: Option<f64> = None;
    let mut sell_volume: i64 = 0;
    let mut buy_volume: i64 = 0;

    for o in orders
        .iter()
        .filter(|o| o.location_id == market.esi_location_id)
    {
        if o.is_buy_order {
            buy_volume += o.volume_remain;
            best_buy_f = Some(match best_buy_f {
                Some(c) => c.max(o.price),
                None => o.price,
            });
        } else {
            sell_volume += o.volume_remain;
            best_sell_f = Some(match best_sell_f {
                Some(c) => c.min(o.price),
                None => o.price,
            });
        }
    }

    let best_sell = best_sell_f.and_then(Decimal::from_f64);
    let best_buy = best_buy_f.and_then(Decimal::from_f64);
    let computed_at = Utc::now();

    // `computed_at` is a freshness timestamp, not a changed-at timestamp. Bump
    // it on every successful fetch so unchanged prices do not become
    // permanently stale and refetch on every worker tick.
    sqlx::query(
        r#"
        INSERT INTO market_prices
            (market_id, type_id, best_sell, best_buy, sell_volume, buy_volume, computed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (market_id, type_id) DO UPDATE SET
            best_sell   = EXCLUDED.best_sell,
            best_buy    = EXCLUDED.best_buy,
            sell_volume = EXCLUDED.sell_volume,
            buy_volume  = EXCLUDED.buy_volume,
            computed_at = EXCLUDED.computed_at
        "#,
    )
    .bind(market.id)
    .bind(type_id)
    .bind(best_sell)
    .bind(best_buy)
    .bind(sell_volume)
    .bind(buy_volume)
    .bind(computed_at)
    .execute(pool)
    .await?;

    Ok(PriceAggregate {
        best_sell,
        best_buy,
        sell_volume,
        buy_volume,
        computed_at,
    })
}

#[derive(sqlx::FromRow)]
struct PriceRow {
    type_id: i64,
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: i64,
    buy_volume: i64,
    computed_at: DateTime<Utc>,
}
