use std::sync::Arc;

use auth_tokens::{CharacterTokenStore, EsiBudgetGuard, TokenCipher};
use nea_esi::EsiClient;
use sqlx::PgPool;

use crate::{config::Config, jwt::JwksCache};

#[derive(Clone)]
#[allow(dead_code)] // token_store + budget_guard are wired in PR #4/#5.
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub cipher: TokenCipher,
    pub jwks: JwksCache,
    /// Shared web-app client. Used for OAuth (authorize_url, exchange_code).
    /// Per-character ESI calls go through `token_store` instead.
    pub esi: Arc<EsiClient>,
    pub token_store: CharacterTokenStore,
    pub budget_guard: EsiBudgetGuard,
}
