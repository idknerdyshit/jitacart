use auth_tokens::TokenEncConfig;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database_url: String,
    /// DSN for the migration-running role (`jitacart_admin`). Opened in a
    /// short-lived pool at startup, used to apply sqlx + tower_sessions
    /// migrations, then closed. Runtime queries go through `database_url`
    /// (`jitacart_app`), which is gated by RLS.
    pub admin_database_url: String,
    pub eve_sso: EveSsoConfig,
    pub esi: EsiConfig,
    /// Legacy single-key shim: base64-encoded 32-byte AES-GCM key. Loaded as
    /// kid `"v1"` and made primary. Use `[token_enc]` instead for rotation
    /// support; if both are present, `[token_enc]` wins.
    #[serde(default)]
    pub token_enc_key: Option<String>,
    /// Multi-key encryption config: a map of kid → base64 key plus a
    /// `primary` kid. Required for online key rotation.
    #[serde(default)]
    pub token_enc: Option<TokenEncConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub limits: AbuseLimits,
    #[serde(default)]
    pub turnstile: TurnstileConfig,
}

/// Per-IP rate-limit knobs. `tower_governor` uses a token-bucket: bucket
/// holds up to `burst` requests, refilled at one per `per_second_period`
/// seconds (so e.g. `per_second_period = 2` means 0.5 req/s sustained).
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Disable the limiter entirely (handy in tests + dev).
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_per_ip_burst")]
    pub per_ip_burst: u32,
    /// Bucket-refill period in seconds: 1 token added every N seconds.
    #[serde(default = "default_per_ip_period_secs")]
    pub per_ip_period_secs: u64,
    /// Stricter bucket for /auth/eve/{login,callback} to deter SSO churn.
    #[serde(default = "default_auth_burst")]
    pub auth_burst: u32,
    #[serde(default = "default_auth_period_secs")]
    pub auth_period_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            per_ip_burst: default_per_ip_burst(),
            per_ip_period_secs: default_per_ip_period_secs(),
            auth_burst: default_auth_burst(),
            auth_period_secs: default_auth_period_secs(),
        }
    }
}

fn default_per_ip_burst() -> u32 {
    30
}
fn default_per_ip_period_secs() -> u64 {
    1
}
fn default_auth_burst() -> u32 {
    5
}
fn default_auth_period_secs() -> u64 {
    10
}

/// Per-tenant size caps. Enforced in handlers; raising them is a config
/// reload, not a migration.
#[derive(Debug, Clone, Deserialize)]
pub struct AbuseLimits {
    #[serde(default = "default_groups_per_user")]
    pub groups_per_user: i64,
    #[serde(default = "default_lists_per_group")]
    pub lists_per_group: i64,
    #[serde(default = "default_items_per_list")]
    pub items_per_list: i64,
    #[serde(default = "default_characters_per_user")]
    pub characters_per_user: i64,
}

impl Default for AbuseLimits {
    fn default() -> Self {
        Self {
            groups_per_user: default_groups_per_user(),
            lists_per_group: default_lists_per_group(),
            items_per_list: default_items_per_list(),
            characters_per_user: default_characters_per_user(),
        }
    }
}

fn default_groups_per_user() -> i64 {
    10
}
fn default_lists_per_group() -> i64 {
    200
}
fn default_items_per_list() -> i64 {
    500
}
fn default_characters_per_user() -> i64 {
    12
}

/// Cloudflare Turnstile (bot-mitigation) config. Gates new-user creation:
/// when `disabled = true` (default in dev) the verification is skipped.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TurnstileConfig {
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub site_key: String,
    #[serde(default)]
    pub secret_key: String,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    /// Set the `Secure` flag on the session cookie. Defaults to true; tests
    /// and local docker setups can override via `JITACART_SERVER__COOKIE_SECURE=false`.
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,
}

fn default_cookie_secure() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct EveSsoConfig {
    pub client_id: String,
    pub client_secret: String,
    pub callback_url: String,
    /// Scopes requested on the *first* login. Upgrades request additional
    /// scopes via `/auth/eve/upgrade`.
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
