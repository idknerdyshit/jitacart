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
    /// Scopes that may be requested by `/auth/eve/upgrade`. Each request is
    /// validated to lie inside `login_scopes ∪ upgrade_scopes`.
    #[serde(default)]
    pub upgrade_scopes: Vec<String>,
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
    #[serde(default = "default_citadel_discovery_secs")]
    pub citadel_discovery: u64,
    #[serde(default = "default_citadel_details_secs")]
    pub citadel_details: u64,
    #[serde(default = "default_citadel_orders_secs")]
    pub citadel_orders: u64,
    #[serde(default = "default_structure_access_backoff_secs")]
    pub structure_access_backoff: u64,
}

impl Default for PollIntervals {
    fn default() -> Self {
        Self {
            market_prices: default_market_prices_secs(),
            contracts: default_contracts_secs(),
            wallet_transactions: default_wallet_transactions_secs(),
            citadel_discovery: default_citadel_discovery_secs(),
            citadel_details: default_citadel_details_secs(),
            citadel_orders: default_citadel_orders_secs(),
            structure_access_backoff: default_structure_access_backoff_secs(),
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

fn default_login_scopes() -> Vec<String> {
    vec!["publicData".to_string()]
}
