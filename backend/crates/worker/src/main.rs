//! jitacart-worker: independent per-job tickers selected via `tokio::select!`.
//!
//! Each `jobs::*` task owns its own `tokio::time::interval`, semaphore, and
//! ESI client mode (anonymous for hub-prices/discovery, authed for citadel
//! detail/orders). The shared `EsiBudgetGuard` is consulted before every
//! batch.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use auth_tokens::{CharacterTokenStore, EsiBudgetGuard, TokenEncConfig};
use nea_esi::EsiClient;
use secrecy::SecretString;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};

use jobs::JobSlot;

mod jobs;

#[derive(Debug, Deserialize)]
pub struct WorkerConfig {
    pub database_url: String,
    pub esi: EsiCfg,
    pub eve_sso: EveSsoCfg,
    /// Legacy single-key shim — same shape as the api crate. Loaded as kid
    /// `"v1"` and made primary if `[token_enc]` is not set.
    #[serde(default)]
    pub token_enc_key: Option<String>,
    /// Multi-key encryption config: a map of kid → base64 key plus a
    /// `primary` kid. Same shape as the api crate's `[token_enc]`.
    #[serde(default)]
    pub token_enc: Option<TokenEncConfig>,
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
    #[serde(default = "default_corp_contracts_secs")]
    pub corp_contracts: u64,
    #[serde(default = "default_corp_wallet_secs")]
    pub corp_wallet: u64,
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
            corp_contracts: default_corp_contracts_secs(),
            corp_wallet: default_corp_wallet_secs(),
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
fn default_corp_contracts_secs() -> u64 {
    300
}
fn default_corp_wallet_secs() -> u64 {
    3600
}
fn default_corp_contracts_concurrency() -> usize {
    2
}
fn default_corp_wallet_concurrency() -> usize {
    2
}

#[derive(Debug, Deserialize)]
pub struct WorkerSection {
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// Bind address for the worker's `/healthz/esi` endpoint. Empty
    /// string disables the server. Default `127.0.0.1:9091` exposes it
    /// only on loopback — point an internal uptime probe at it (or
    /// docker-compose service-to-service curl).
    #[serde(default = "default_healthz_bind")]
    pub healthz_bind: String,
    #[serde(default = "default_citadel_discovery_missing_threshold")]
    pub citadel_discovery_missing_threshold: i32,
    #[serde(default = "default_citadel_details_concurrency")]
    pub citadel_details_concurrency: usize,
    #[serde(default = "default_citadel_orders_concurrency")]
    pub citadel_orders_concurrency: usize,
    #[serde(default = "default_contracts_concurrency")]
    pub contracts_concurrency: usize,
    #[serde(default = "default_corp_contracts_concurrency")]
    pub corp_contracts_concurrency: usize,
    #[serde(default = "default_corp_wallet_concurrency")]
    pub corp_wallet_concurrency: usize,
    #[serde(default = "default_npc_hub_concurrency")]
    pub npc_hub_concurrency: usize,
}

impl Default for WorkerSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            healthz_bind: default_healthz_bind(),
            citadel_discovery_missing_threshold: default_citadel_discovery_missing_threshold(),
            citadel_details_concurrency: default_citadel_details_concurrency(),
            citadel_orders_concurrency: default_citadel_orders_concurrency(),
            contracts_concurrency: default_contracts_concurrency(),
            corp_contracts_concurrency: default_corp_contracts_concurrency(),
            corp_wallet_concurrency: default_corp_wallet_concurrency(),
            npc_hub_concurrency: default_npc_hub_concurrency(),
        }
    }
}

fn default_healthz_bind() -> String {
    "127.0.0.1:9091".to_string()
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
fn default_npc_hub_concurrency() -> usize {
    8
}

pub struct Ctx {
    pub pool: PgPool,
    pub config: Arc<WorkerConfig>,
    /// Anonymous, UA-only client used for hub prices and discovery.
    pub esi_anon: Arc<EsiClient>,
    pub token_store: CharacterTokenStore,
    pub budget: EsiBudgetGuard,
    pub webhook_http: reqwest::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::init_tracing();
    let config: WorkerConfig = bootstrap::load_config("loading worker config")?;

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

    let cipher =
        auth_tokens::build_cipher(config.token_enc.as_ref(), config.token_enc_key.as_deref())
            .context("building token-at-rest cipher")?;

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
        webhook_http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("building webhook http client"),
    });

    spawn_healthz(&ctx).await?;

    // One driver task per slot — each owns its interval and runs the job
    // inline, so a long tick naturally back-pressures only its own slot
    // (MissedTickBehavior::Delay). The "previous tick still running" guard the
    // old `select!` loop needed disappears with this layout.
    for slot in jobs::registry() {
        let ctx_for_slot = Arc::clone(&ctx);
        tokio::spawn(drive_slot(slot, ctx_for_slot));
    }

    // Budget reset isn't a job — it just pokes the rate-limit window.
    let budget_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        let mut t = mk_interval(60);
        loop {
            t.tick().await;
            budget_ctx.budget.reset();
        }
    });

    // Park forever; ctrl-c takes the runtime down.
    std::future::pending::<()>().await;
    Ok(())
}

async fn drive_slot(slot: Box<dyn JobSlot>, ctx: Arc<Ctx>) {
    let secs = slot.interval_secs(&ctx.config);
    let mut t = mk_interval(secs);
    loop {
        t.tick().await;
        if let Err(e) = slot.run(&ctx).await {
            tracing::error!(error = ?e, slot = slot.name(), "tick failed");
        }
    }
}

fn mk_interval(secs: u64) -> tokio::time::Interval {
    let mut t = interval(Duration::from_secs(secs.max(1)));
    t.set_missed_tick_behavior(MissedTickBehavior::Delay);
    t
}

/// Tiny healthz server bound to a (typically loopback) port. Exposes
/// `/healthz/esi` (budget snapshot) and `/healthz` (always-200). Empty
/// `worker.healthz_bind` skips the server, so tests / one-shot tools
/// don't pay the port-binding cost.
async fn spawn_healthz(ctx: &Arc<Ctx>) -> anyhow::Result<()> {
    let bind = ctx.config.worker.healthz_bind.trim();
    if bind.is_empty() {
        tracing::info!("healthz disabled (worker.healthz_bind empty)");
        return Ok(());
    }
    let addr: std::net::SocketAddr = bind.parse().context("parsing worker.healthz_bind")?;
    let budget = ctx.budget.clone();
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route(
            "/healthz/esi",
            axum::routing::get({
                let budget = budget.clone();
                move || {
                    let budget = budget.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "remaining": budget.remaining(),
                            "has_budget": budget.has_budget(),
                        }))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding healthz on {addr}"))?;
    tracing::info!(%addr, "worker healthz listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = ?e, "healthz server stopped");
        }
    });
    Ok(())
}
