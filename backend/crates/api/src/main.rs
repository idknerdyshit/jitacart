use std::net::SocketAddr;

use anyhow::Context;
use axum::{routing::get, Json, Router};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
    database_url: String,
}

#[derive(Debug, Deserialize)]
struct ServerConfig {
    bind: String,
}

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

    let app = Router::new()
        .route("/healthz", get(healthz))
        .with_state(pool)
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = config.server.bind.parse().context("parsing bind addr")?;
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn healthz(
    axum::extract::State(pool): axum::extract::State<sqlx::PgPool>,
) -> Json<serde_json::Value> {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
        .is_ok();
    Json(serde_json::json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "db": db_ok,
    }))
}
