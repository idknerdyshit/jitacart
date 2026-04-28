//! NPC-hub price refresh tick. Logic preserved from the pre-refactor worker.

use std::collections::HashMap;

use anyhow::anyhow;
use domain::{Market, MarketKind};
use uuid::Uuid;

use crate::Ctx;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping npc_hubs tick"
        );
        return Ok(());
    }

    let ttl = ctx.config.esi.poll_intervals_secs.market_prices as i64;

    let rows: Vec<TickRow> = sqlx::query_as(
        r#"
        SELECT m.id AS market_id, m.kind, m.esi_location_id, m.region_id, m.name,
               m.short_label, m.is_hub, m.is_public,
               li.type_id
        FROM list_items li
        JOIN lists l         ON l.id = li.list_id
        JOIN list_markets lm ON lm.list_id = l.id
        JOIN markets m       ON m.id = lm.market_id
        LEFT JOIN market_prices mp
          ON mp.market_id = m.id AND mp.type_id = li.type_id
        WHERE l.status = 'open'
          AND m.is_public
          AND m.kind = 'npc_hub'
          AND (mp.computed_at IS NULL
               OR mp.computed_at < now() - make_interval(secs => $1::double precision))
        GROUP BY m.id, li.type_id
        "#,
    )
    .bind(ttl as f64)
    .fetch_all(&ctx.pool)
    .await?;

    if rows.is_empty() {
        tracing::debug!("npc_hubs tick: 0 (market, type) pairs to refresh");
        return Ok(());
    }

    let mut by_market: HashMap<Uuid, (Market, Vec<i64>)> = HashMap::new();
    for r in rows {
        let kind = r
            .kind
            .parse::<MarketKind>()
            .map_err(|e| anyhow!("bad market kind: {e}"))?;
        by_market
            .entry(r.market_id)
            .or_insert_with(|| {
                (
                    Market {
                        id: r.market_id,
                        kind,
                        esi_location_id: r.esi_location_id,
                        region_id: r.region_id,
                        name: r.name.clone(),
                        short_label: r.short_label.clone(),
                        is_hub: r.is_hub,
                        is_public: r.is_public,
                    },
                    Vec::new(),
                )
            })
            .1
            .push(r.type_id);
    }

    let total: usize = by_market.values().map(|(_, ids)| ids.len()).sum();
    tracing::info!(markets = by_market.len(), pairs = total, "npc_hubs refresh");

    let pool = &ctx.pool;
    let esi = ctx.esi_anon.as_ref();
    let budget = &ctx.budget;
    let refreshes = by_market.into_iter().map(|(_, (m, type_ids))| async move {
        let label = m.short_label.clone().unwrap_or_else(|| "(unnamed)".into());
        let inner = type_ids.into_iter().map(|type_id| {
            let m = m.clone();
            async move { (type_id, market::refresh_one(pool, esi, &m, type_id).await) }
        });
        let results = futures_util::future::join_all(inner).await;
        for (type_id, res) in results {
            if let Err(e) = res {
                budget.record_non_2xx();
                tracing::warn!(error = ?e, market = %label, type_id, "market refresh failed");
            }
        }
    });
    futures_util::future::join_all(refreshes).await;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct TickRow {
    market_id: Uuid,
    kind: String,
    esi_location_id: i64,
    region_id: Option<i64>,
    name: Option<String>,
    short_label: Option<String>,
    is_hub: bool,
    is_public: bool,
    type_id: i64,
}
