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
    #[serde(default)]
    pub poll_intervals_secs: PollIntervals,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // contracts/wallet_transactions are read in Phase 5+
pub struct PollIntervals {
    #[serde(default = "default_market_prices_secs")]
    pub market_prices: u64,
    #[serde(default = "default_contracts_secs")]
    pub contracts: u64,
    #[serde(default = "default_wallet_transactions_secs")]
    pub wallet_transactions: u64,
}

impl Default for PollIntervals {
    fn default() -> Self {
        Self {
            market_prices: default_market_prices_secs(),
            contracts: default_contracts_secs(),
            wallet_transactions: default_wallet_transactions_secs(),
        }
    }
}

fn default_market_prices_secs() -> u64 {
    300
}
fn default_contracts_secs() -> u64 {
    300
}
fn default_wallet_transactions_secs() -> u64 {
    3600
}

fn default_login_scopes() -> Vec<String> {
    vec!["publicData".to_string()]
}
