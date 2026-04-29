//! jitacart-worker: independent per-job tickers selected via `tokio::select!`.
//!
//! Each `jobs::*` task owns its own `tokio::time::interval`, semaphore, and
//! ESI client mode (anonymous for hub-prices/discovery, authed for citadel
//! detail/orders). The shared `EsiBudgetGuard` is consulted before every
//! batch.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context};
use auth_tokens::{CharacterTokenStore, EsiBudgetGuard, TokenCipher};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use nea_esi::EsiClient;
use secrecy::SecretString;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod jobs;

#[derive(Debug, Deserialize)]
pub struct WorkerConfig {
    pub database_url: String,
    pub esi: EsiCfg,
    pub eve_sso: EveSsoCfg,
    /// Base64-encoded 32-byte AES-GCM key. Same key the api crate uses.
    pub token_enc_key: String,
    #[serde(default)]
    pub worker: WorkerSection,
}

#[derive(Debug, Deserialize)]
pub struct EsiCfg {
    pub user_agent: String,
    #[serde(default)]
    pub poll_intervals_secs: PollIntervals,
}

#[derive(Debug, Deserialize)]
pub struct EveSsoCfg {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct PollIntervals {
    #[serde(default = "default_market_prices_secs")]
    pub market_prices: u64,
    #[serde(default = "default_contracts_secs")]
    pub contracts: u64,
    #[serde(default = "default_citadel_discovery_secs")]
    pub citadel_discovery: u64,
    #[serde(default = "default_citadel_details_secs")]
    pub citadel_details: u64,
    #[serde(default = "default_citadel_orders_secs")]
    pub citadel_orders: u64,
    #[serde(default = "default_structure_access_backoff_secs")]
    pub structure_access_backoff: u64,
}

impl Default for PollIntervals {
    fn default() -> Self {
        Self {
            market_prices: default_market_prices_secs(),
            contracts: default_contracts_secs(),
            citadel_discovery: default_citadel_discovery_secs(),
            citadel_details: default_citadel_details_secs(),
            citadel_orders: default_citadel_orders_secs(),
            structure_access_backoff: default_structure_access_backoff_secs(),
        }
    }
}

fn default_market_prices_secs() -> u64 {
    300
}
fn default_contracts_secs() -> u64 {
    300
}
fn default_citadel_discovery_secs() -> u64 {
    3600
}
fn default_citadel_details_secs() -> u64 {
    86400
}
fn default_citadel_orders_secs() -> u64 {
    600
}
fn default_structure_access_backoff_secs() -> u64 {
    86400
}

#[derive(Debug, Deserialize)]
pub struct WorkerSection {
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    #[serde(default = "default_citadel_discovery_missing_threshold")]
    pub citadel_discovery_missing_threshold: i32,
    #[serde(default = "default_citadel_details_concurrency")]
    pub citadel_details_concurrency: usize,
    #[serde(default = "default_citadel_orders_concurrency")]
    pub citadel_orders_concurrency: usize,
    #[serde(default = "default_contracts_concurrency")]
    pub contracts_concurrency: usize,
}

impl Default for WorkerSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            citadel_discovery_missing_threshold: default_citadel_discovery_missing_threshold(),
            citadel_details_concurrency: default_citadel_details_concurrency(),
            citadel_orders_concurrency: default_citadel_orders_concurrency(),
            contracts_concurrency: default_contracts_concurrency(),
        }
    }
}

fn default_tick_secs() -> u64 {
    60
}
fn default_citadel_discovery_missing_threshold() -> i32 {
    3
}
fn default_citadel_details_concurrency() -> usize {
    4
}
fn default_citadel_orders_concurrency() -> usize {
    8
}
fn default_contracts_concurrency() -> usize {
    4
}

pub struct Ctx {
    pub pool: PgPool,
    pub config: Arc<WorkerConfig>,
    /// Anonymous, UA-only client used for hub prices and discovery.
    pub esi_anon: Arc<EsiClient>,
    pub token_store: CharacterTokenStore,
    pub budget: EsiBudgetGuard,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer())
        .init();

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
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .context("connecting to postgres")?;

    let cipher = TokenCipher::from_b64(&config.token_enc_key)
        .context("building token cipher from TOKEN_ENC_KEY")?;

    let token_store = CharacterTokenStore::new(
        pool.clone(),
        cipher,
        config.esi.user_agent.clone(),
        config.eve_sso.client_id.clone(),
        SecretString::from(config.eve_sso.client_secret.clone()),
    );

    let esi_anon = EsiClient::with_user_agent(&config.esi.user_agent)
        .map_err(|e| anyhow!("EsiClient::with_user_agent: {e}"))?
        .with_cache();

    let ctx = Arc::new(Ctx {
        pool,
        config: Arc::new(config),
        esi_anon: Arc::new(esi_anon),
        token_store,
        budget: EsiBudgetGuard::default(),
    });

    let intervals = &ctx.config.esi.poll_intervals_secs;

    let mut hub_prices = mk_interval(ctx.config.worker.tick_secs);
    let mut citadel_discovery = mk_interval(intervals.citadel_discovery);
    let mut citadel_details = mk_interval(intervals.citadel_details.min(300));
    let mut citadel_orders = mk_interval(intervals.citadel_orders);
    // Contracts polls a per-character cursor, so we tick at the worker cadence
    // and let the cursor's `next_poll_at` decide which characters are due.
    let mut contracts = mk_interval(ctx.config.worker.tick_secs);
    let mut budget_reset = mk_interval(60);
    let hub_prices_running = Arc::new(AtomicBool::new(false));
    let citadel_discovery_running = Arc::new(AtomicBool::new(false));
    let citadel_details_running = Arc::new(AtomicBool::new(false));
    let citadel_orders_running = Arc::new(AtomicBool::new(false));
    let contracts_running = Arc::new(AtomicBool::new(false));

    loop {
        tokio::select! {
            _ = hub_prices.tick() => spawn_guarded(&ctx, &hub_prices_running, "npc_hubs", |c| async move {
                jobs::npc_hubs::run(&c).await
            }),
            _ = citadel_discovery.tick() => spawn_guarded(&ctx, &citadel_discovery_running, "citadel_discovery", |c| async move {
                jobs::citadel_discovery::run(&c).await
            }),
            _ = citadel_details.tick() => spawn_guarded(&ctx, &citadel_details_running, "citadel_details", |c| async move {
                jobs::citadel_details::run(&c).await
            }),
            _ = citadel_orders.tick() => spawn_guarded(&ctx, &citadel_orders_running, "citadel_orders", |c| async move {
                jobs::citadel_orders::run(&c).await
            }),
            _ = contracts.tick() => spawn_guarded(&ctx, &contracts_running, "contracts", |c| async move {
                jobs::contracts::run(&c).await
            }),
            _ = budget_reset.tick() => ctx.budget.reset(),
        }
    }
}

fn spawn_guarded<F, Fut>(ctx: &Arc<Ctx>, running: &Arc<AtomicBool>, label: &'static str, job: F)
where
    F: FnOnce(Arc<Ctx>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    if running.swap(true, Ordering::AcqRel) {
        tracing::warn!("{label} tick skipped: previous run still active");
        return;
    }
    let ctx = Arc::clone(ctx);
    let running = Arc::clone(running);
    tokio::spawn(async move {
        let _g = RunningGuard(running);
        if let Err(e) = job(ctx).await {
            tracing::error!(error = ?e, "{label} tick failed");
        }
    });
}

fn mk_interval(secs: u64) -> tokio::time::Interval {
    let mut t = interval(Duration::from_secs(secs.max(1)));
    t.set_missed_tick_behavior(MissedTickBehavior::Delay);
    t
}

struct RunningGuard(Arc<AtomicBool>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
