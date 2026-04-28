use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database_url: String,
    pub eve_sso: EveSsoConfig,
    pub esi: EsiConfig,
    /// Base64-encoded 32-byte AES-GCM key for refresh-token encryption at rest.
    pub token_enc_key: String,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
}

#[derive(Debug, Deserialize)]
pub struct EveSsoConfig {
    pub client_id: String,
    pub client_secret: String,
    pub callback_url: String,
    /// Scopes requested on the *first* login. Phase 1 only needs `publicData`;
    /// later phases will add per-feature scope upgrades.
    #[serde(default = "default_login_scopes")]
    pub login_scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct EsiConfig {
    pub user_agent: String,
}

fn default_login_scopes() -> Vec<String> {
    vec!["publicData".to_string()]
}
