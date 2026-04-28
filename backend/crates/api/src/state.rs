use std::sync::Arc;

use nea_esi::EsiClient;
use sqlx::PgPool;

use crate::{config::Config, crypto::TokenCipher, jwt::JwksCache};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub cipher: TokenCipher,
    pub jwks: JwksCache,
    /// Shared web-app client. Used for OAuth (authorize_url, exchange_code);
    /// per-character ESI calls will build their own client wired to the
    /// character's tokens in later phases.
    pub esi: Arc<EsiClient>,
}
