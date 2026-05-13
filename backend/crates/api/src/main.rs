use std::{net::SocketAddr, sync::Arc};

use anyhow::{anyhow, Context};
use axum::{extract::DefaultBodyLimit, extract::State, routing::get, Json, Router};
use axum_prometheus::PrometheusMetricLayer;
use nea_esi::EsiClient;
use sqlx::postgres::PgPoolOptions;
use time::Duration as TimeDuration;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultMakeSpan, TraceLayer},
};
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;

use auth_tokens::{CharacterTokenStore, EsiBudgetGuard};

use jitacart_api::{
    auth, citadels, config::Config, contracts, corps, db::tx_middleware, fulfillment, groups,
    jwt::JwksCache, lists, markets, state::AppState, webhooks,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::init_tracing();
    let config: Config = bootstrap::load_config("loading config")?;

    // Migrations run as `jitacart_admin` (table owner; CREATE/ALTER allowed).
    // Open a tiny pool, run schema migrations + the tower-sessions store
    // migration, close it, then connect the runtime pool as `jitacart_app`
    // (NOBYPASSRLS).
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&config.admin_database_url)
        .await
        .context("connecting admin pool")?;

    sqlx::migrate!("../../migrations")
        .run(&admin_pool)
        .await
        .context("running migrations")?;

    let session_store_admin = PostgresStore::new(admin_pool.clone());
    session_store_admin
        .migrate()
        .await
        .context("running tower-sessions migrations")?;

    admin_pool.close().await;

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .context("connecting to postgres")?;

    let cipher =
        auth_tokens::build_cipher(config.token_enc.as_ref(), config.token_enc_key.as_deref())
            .context("building token-at-rest cipher")?;

    let http = reqwest::Client::builder()
        .user_agent(&config.esi.common.user_agent)
        .build()
        .context("building reqwest client")?;

    let jwks = JwksCache::new(http, config.eve_sso.common.client_id.clone());

    let esi = EsiClient::with_web_app(
        &config.esi.common.user_agent,
        &config.eve_sso.common.client_id,
        config.eve_sso.common.client_secret.clone(),
    )
    .map_err(|e| anyhow!("EsiClient::with_web_app: {e}"))?
    .with_cache();

    // Sessions at runtime use the app pool. The store's table was created
    // above via the admin pool; tower-sessions only needs SELECT/INSERT
    // /UPDATE/DELETE here, which the public-schema GRANT to jitacart_app
    // covers.
    let session_store = PostgresStore::new(pool.clone());
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(config.server.cookie_secure)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(TimeDuration::days(30)));

    let token_store = CharacterTokenStore::new(
        pool.clone(),
        cipher.clone(),
        config.esi.common.user_agent.clone(),
        config.eve_sso.common.client_id.clone(),
        config.eve_sso.common.client_secret.clone(),
    );

    let budget_guard = EsiBudgetGuard::default();

    let webhook_http = reqwest::Client::builder()
        .user_agent("JitaCart-Webhook/1.0")
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("building webhook reqwest client")?;

    let state = AppState {
        pool,
        config: Arc::new(config),
        cipher: Arc::new(cipher),
        jwks,
        esi: Arc::new(esi),
        token_store,
        budget_guard,
        webhook_http,
    };

    let bind: SocketAddr = state
        .config
        .server
        .bind
        .parse()
        .context("parsing bind addr")?;

    // Two-tier rate limiting: stricter bucket for SSO entry/exit, generous
    // global bucket for everything else. Both keyed by client IP.
    let rl = &state.config.rate_limit;
    let mk_layer = |period_secs, burst, label: &str| {
        GovernorConfigBuilder::default()
            .per_second(period_secs)
            .burst_size(burst)
            .finish()
            .map(GovernorLayer::new)
            .ok_or_else(|| anyhow!("invalid {label} rate-limit config"))
    };

    // /healthz and /readyz live on their own sub-router with no
    // tx_middleware: they only do `SELECT 1` against the app pool and have
    // no use for a tx (and would needlessly open one for every probe).
    let health_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state.clone());

    let auth_routes = Router::new()
        .merge(auth::router())
        .with_state(state.clone());
    let auth_routes = if rl.disabled {
        auth_routes
    } else {
        auth_routes.layer(mk_layer(rl.auth_period_secs, rl.auth_burst, "auth")?)
    };
    // tx_middleware sits *outside* the rate limiter so a rate-limit reject
    // doesn't open a tx, and *inside* SessionManagerLayer (added below at
    // the outer chain) so it can read the session.
    let auth_routes = auth_routes.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        tx_middleware,
    ));

    let other_routes = Router::new()
        .merge(groups::router())
        .merge(markets::router())
        .merge(lists::router())
        .merge(citadels::router())
        .merge(fulfillment::router())
        .merge(contracts::router())
        .merge(corps::router())
        .merge(webhooks::router())
        .with_state(state.clone());
    let other_routes = if rl.disabled {
        other_routes
    } else {
        other_routes.layer(mk_layer(rl.per_ip_period_secs, rl.per_ip_burst, "per-ip")?)
    };
    let other_routes = other_routes.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        tx_middleware,
    ));

    // `prom_handle` is mounted on a *separate* listener so /metrics is never
    // reachable through Caddy.
    let (prom_layer, prom_handle) = PrometheusMetricLayer::pair();

    // Layer order (executes outer-first, propagates inner-first on the way back):
    //   SetRequestId   — generates X-Request-Id if absent
    //   TraceLayer     — opens a span with method, uri, request_id
    //   session        — tower-sessions cookie session
    //   PropagateRequestId — copies the id into the response header
    //   PrometheusMetricLayer — counts / times every matched route
    let app = health_routes
        .merge(auth_routes)
        .merge(other_routes)
        // 256 KiB is generous for our largest expected payload (multibuy
        // pastes); anything larger is almost certainly a misconfigured
        // client or an abuse attempt. Json/extractors inherit this.
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(prom_layer)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(session_layer)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(false)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    // Loopback-only /metrics server. Empty bind skips it entirely
    // (tests). No auth — a loopback bind is unforgeable; a bearer
    // token would be one more secret to rotate.
    spawn_metrics_server(state.config.metrics.bind.clone(), prom_handle).await?;

    tracing::info!("listening on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn spawn_metrics_server(
    bind: String,
    handle: metrics_exporter_prometheus::PrometheusHandle,
) -> anyhow::Result<()> {
    let bind = bind.trim();
    if bind.is_empty() {
        tracing::info!("metrics disabled (metrics.bind empty)");
        return Ok(());
    }
    let addr: SocketAddr = bind.parse().context("parsing metrics.bind")?;
    let app = Router::new().route(
        "/metrics",
        get(move || {
            let h = handle.clone();
            async move { h.render() }
        }),
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding metrics on {addr}"))?;
    tracing::info!(%addr, "metrics listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = ?e, "metrics server stopped");
        }
    });
    Ok(())
}

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();
    Json(serde_json::json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "db": db_ok,
    }))
}

/// Readiness probe. Distinct from `/healthz` (always-200 liveness): this
/// returns **503 Service Unavailable** if the API can't currently serve
/// real traffic — DB unreachable, or pool exhausted such that the next
/// request would block. Point uptime monitors at this, not /healthz.
async fn readyz(
    State(state): State<AppState>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();
    let pool_size = state.pool.size();
    let pool_idle = state.pool.num_idle();
    let pool_ok = pool_idle > 0 || pool_size < state.pool.options().get_max_connections();

    let ready = db_ok && pool_ok;
    let code = if ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(serde_json::json!({
            "ready": ready,
            "db": db_ok,
            "pool_size": pool_size,
            "pool_idle": pool_idle,
            "pool_max": state.pool.options().get_max_connections(),
        })),
    )
}
