//! Config types shared by the api and worker binaries.
//!
//! Both binaries declare an `[esi]` and `[eve_sso]` table with the same field
//! names and defaults; that common shape lives here. Binary-specific extras
//! (api's `callback_url`/scopes, each binary's `poll_intervals_secs`) compose
//! with these via `#[serde(flatten)]` in the binary's own struct.

use secrecy::SecretString;
use serde::{Deserialize, Deserializer};

/// Fields common to the api and worker `[esi]` table. Each binary wraps this
/// in its own `EsiConfig` with a binary-specific `poll_intervals_secs`.
#[derive(Debug, Clone, Deserialize)]
pub struct EsiCommonCfg {
    pub user_agent: String,
}

/// Fields common to the api and worker `[eve_sso]` table. The worker uses this
/// directly; the api wraps it with `callback_url`, `login_scopes`,
/// `upgrade_scopes`.
///
/// `client_secret` is a `SecretString` so its `Debug` is redacted and the
/// backing buffer is zeroized on drop — no caller should ever wrap it again.
#[derive(Debug, Clone, Deserialize)]
pub struct EveSsoCommonCfg {
    pub client_id: String,
    #[serde(deserialize_with = "deserialize_secret_string")]
    pub client_secret: SecretString,
}

fn deserialize_secret_string<'de, D>(d: D) -> Result<SecretString, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(d).map(SecretString::from)
}
