//! Shared startup helpers for api + worker binaries.

use anyhow::Context;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Env var that switches log output to one-JSON-object-per-line for prod.
const LOG_FORMAT_ENV: &str = "JC_LOG_FORMAT";

/// Initialize tracing-subscriber. `JC_LOG_FORMAT=json` for prod (one log
/// object per line, easy to ship to any log collector). Anything else gets
/// the readable fmt layer.
pub fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(env_filter);
    if std::env::var(LOG_FORMAT_ENV).as_deref() == Ok("json") {
        registry.with(fmt::layer().json().flatten_event(true)).init();
    } else {
        registry.with(fmt::layer()).init();
    }
}

/// Load a config from `config.toml` (optional in containerized deploys) +
/// env vars (split on `__`). `context` is attached to the error chain.
pub fn load_config<T: serde::de::DeserializeOwned>(context: &'static str) -> anyhow::Result<T> {
    let mut figment = Figment::new();
    if std::path::Path::new("config.toml").exists() {
        figment = figment.merge(Toml::file("config.toml"));
    }
    figment.merge(Env::raw().split("__")).extract().context(context)
}
