use std::{net::SocketAddr, sync::Arc};

use anyhow::{anyhow, Context};
use axum::{extract::State, routing::get, Json, Router};
use nea_esi::EsiClient;
use secrecy::SecretString;
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
    auth, citadels, config::Config, contracts, corps, fulfillment, groups, jwt::JwksCache, lists,
    markets, state::AppState, webhooks,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::init_tracing();
    let config: Config = bootstrap::load_config("loading config")?;

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .context("connecting to postgres")?;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    let cipher = auth_tokens::build_cipher(
        config.token_enc.as_ref(),
        config.token_enc_key.as_deref(),
    )
    .context("building token-at-rest cipher")?;

    let http = reqwest::Client::builder()
        .user_agent(&config.esi.user_agent)
        .build()
        .context("building reqwest client")?;

    let jwks = JwksCache::new(http, config.eve_sso.client_id.clone());

    let esi = EsiClient::with_web_app(
        &config.esi.user_agent,
        &config.eve_sso.client_id,
        SecretString::from(config.eve_sso.client_secret.clone()),
    )
    .map_err(|e| anyhow!("EsiClient::with_web_app: {e}"))?
    .with_cache();

    let session_store = PostgresStore::new(pool.clone());
    session_store
        .migrate()
        .await
        .context("running tower-sessions migrations")?;
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(false)
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(TimeDuration::days(30)));

    let token_store = CharacterTokenStore::new(
        pool.clone(),
        cipher.clone(),
        config.esi.user_agent.clone(),
        config.eve_sso.client_id.clone(),
        SecretString::from(config.eve_sso.client_secret.clone()),
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

    let auth_routes = Router::new()
        .merge(auth::router())
        .with_state(state.clone());
    let auth_routes = if rl.disabled {
        auth_routes
    } else {
        auth_routes.layer(mk_layer(rl.auth_period_secs, rl.auth_burst, "auth")?)
    };

    let other_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
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

    // Layer order (executes outer-first, propagates inner-first on the way back):
    //   SetRequestId   — generates X-Request-Id if absent
    //   TraceLayer     — opens a span with method, uri, request_id
    //   session        — tower-sessions cookie session
    //   PropagateRequestId — copies the id into the response header
    let app = auth_routes
        .merge(other_routes)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(session_layer)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(false)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    tracing::info!("listening on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;

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
