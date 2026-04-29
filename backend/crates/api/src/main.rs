use std::{net::SocketAddr, sync::Arc};

use anyhow::{anyhow, Context};
use axum::{extract::State, routing::get, Json, Router};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use nea_esi::EsiClient;
use secrecy::SecretString;
use sqlx::postgres::PgPoolOptions;
use time::Duration as TimeDuration;
use tower_http::trace::TraceLayer;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use auth_tokens::{CharacterTokenStore, EsiBudgetGuard, TokenCipher};

use jitacart_api::{
    auth, citadels, config::Config, contracts, fulfillment, groups, jwt::JwksCache, lists, markets,
    state::AppState,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer())
        .init();

    let config: Config = Figment::new()
        .merge(Toml::file("config.toml"))
        .merge(Env::raw().split("__"))
        .extract()
        .context("loading config")?;

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&config.database_url)
        .await
        .context("connecting to postgres")?;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    let cipher = TokenCipher::from_b64(&config.token_enc_key)
        .context("building token cipher from TOKEN_ENC_KEY")?;

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

    let state = AppState {
        pool,
        config: Arc::new(config),
        cipher,
        jwks,
        esi: Arc::new(esi),
        token_store,
        budget_guard,
    };

    let bind: SocketAddr = state
        .config
        .server
        .bind
        .parse()
        .context("parsing bind addr")?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .merge(auth::router())
        .merge(groups::router())
        .merge(markets::router())
        .merge(lists::router())
        .merge(citadels::router())
        .merge(fulfillment::router())
        .merge(contracts::router())
        .with_state(state)
        .layer(session_layer)
        .layer(TraceLayer::new_for_http());

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
