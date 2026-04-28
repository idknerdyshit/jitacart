use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context};
use domain::{Market, MarketKind};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use nea_esi::EsiClient;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct WorkerConfig {
    database_url: String,
    esi: EsiCfg,
    #[serde(default)]
    worker: WorkerSection,
}

#[derive(Debug, Deserialize)]
struct EsiCfg {
    user_agent: String,
    #[serde(default)]
    poll_intervals_secs: PollIntervals,
}

#[derive(Debug, Deserialize)]
struct PollIntervals {
    #[serde(default = "default_market_prices_secs")]
    market_prices: u64,
}

impl Default for PollIntervals {
    fn default() -> Self {
        Self {
            market_prices: default_market_prices_secs(),
        }
    }
}

fn default_market_prices_secs() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
struct WorkerSection {
    #[serde(default = "default_tick_secs")]
    tick_secs: u64,
}

impl Default for WorkerSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
        }
    }
}

fn default_tick_secs() -> u64 {
    60
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer())
        .init();

    // Resolve config.toml relative to the workspace root, mirroring how `api`
    // works when invoked via `cargo run -p jitacart-api` from the backend dir.
    let config: WorkerConfig = Figment::new()
        .merge(Toml::file("config.toml"))
        .merge(Env::raw().split("__"))
        .extract()
        .context("loading worker config")?;

    tracing::info!(
        tick_secs = config.worker.tick_secs,
        ttl_secs = config.esi.poll_intervals_secs.market_prices,
        "jitacart-worker starting"
    );

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&config.database_url)
        .await
        .context("connecting to postgres")?;

    // UA-only client. The worker only hits public NPC-hub market endpoints.
    let esi = EsiClient::with_user_agent(&config.esi.user_agent)
        .map_err(|e| anyhow!("EsiClient::with_user_agent: {e}"))?
        .with_cache();

    let mut ticker = interval(Duration::from_secs(config.worker.tick_secs));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await; // first tick fires immediately; skip it.
    loop {
        ticker.tick().await;
        if let Err(e) = run_tick(&pool, &esi, &config).await {
            tracing::error!(error = ?e, "worker tick failed");
        }
    }
}

async fn run_tick(pool: &PgPool, esi: &EsiClient, config: &WorkerConfig) -> anyhow::Result<()> {
    let ttl = config.esi.poll_intervals_secs.market_prices as i64;

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
          AND (mp.computed_at IS NULL
               OR mp.computed_at < now() - make_interval(secs => $1::double precision))
        GROUP BY m.id, li.type_id
        "#,
    )
    .bind(ttl as f64)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        tracing::info!("tick: 0 (market, type) pairs to refresh");
        return Ok(());
    }

    // Group by market.
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

    let total_pairs: usize = by_market.values().map(|(_, ids)| ids.len()).sum();
    tracing::info!(
        markets = by_market.len(),
        pairs = total_pairs,
        "tick refresh"
    );

    // Fan out across markets in parallel; the SQL above already filtered to
    // stale rows so we skip the cache-read in `get_or_refresh_prices` and call
    // `refresh_one` directly. The shared ESI semaphore inside `market::prices`
    // bounds concurrency.
    let refreshes = by_market.into_iter().map(|(_, (m, type_ids))| async move {
        let label = m.short_label.clone();
        let inner = type_ids.into_iter().map(|type_id| {
            let m = m.clone();
            async move { (type_id, market::refresh_one(pool, esi, &m, type_id).await) }
        });
        let results = futures_util::future::join_all(inner).await;
        for (type_id, res) in results {
            if let Err(e) = res {
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
    region_id: i64,
    name: String,
    short_label: String,
    is_hub: bool,
    is_public: bool,
    type_id: i64,
}
