//! Verify EVE SSO access-token JWTs against EVE's JWKS.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use jsonwebtoken::{
    decode, decode_header,
    jwk::{Jwk, JwkSet},
    DecodingKey, Validation,
};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

const EVE_JWKS_URL: &str = "https://login.eveonline.com/oauth/jwks";
const EVE_ISSUER_HOST: &str = "login.eveonline.com";
const EVE_ISSUER_URL: &str = "https://login.eveonline.com";

#[derive(Debug, Clone, Deserialize)]
pub struct EveClaims {
    /// "CHARACTER:EVE:90000001" — character_id is the suffix.
    pub sub: String,
    pub name: String,
    /// owner_hash. Rotates when a character is transferred.
    pub owner: String,
    /// Either a single space-separated string or an array, depending on token shape.
    #[serde(default)]
    pub scp: ScopesField,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(untagged)]
pub enum ScopesField {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl EveClaims {
    pub fn character_id(&self) -> anyhow::Result<i64> {
        let id = self
            .sub
            .rsplit(':')
            .next()
            .ok_or_else(|| anyhow!("malformed sub claim: {}", self.sub))?;
        id.parse::<i64>()
            .with_context(|| format!("non-numeric character id in sub: {}", self.sub))
    }

    pub fn scopes(&self) -> Vec<String> {
        match &self.scp {
            ScopesField::None => vec![],
            ScopesField::One(s) => s.split(' ').map(str::to_owned).collect(),
            ScopesField::Many(v) => v.clone(),
        }
    }
}

#[derive(Clone)]
pub struct JwksCache {
    http: reqwest::Client,
    keys: Arc<RwLock<Option<JwkSet>>>,
    /// Single-flight: only one fetch in-flight at a time. Concurrent verifies
    /// that hit a kid-miss serialize on this mutex and re-check the cache
    /// after acquiring it.
    fetch_gate: Arc<Mutex<()>>,
    expected_audience: String,
}

impl JwksCache {
    pub fn new(http: reqwest::Client, expected_audience: String) -> Self {
        Self {
            http,
            keys: Arc::new(RwLock::new(None)),
            fetch_gate: Arc::new(Mutex::new(())),
            expected_audience,
        }
    }

    async fn fetch(&self) -> anyhow::Result<JwkSet> {
        self.http
            .get(EVE_JWKS_URL)
            .send()
            .await
            .context("fetching EVE JWKS")?
            .error_for_status()
            .context("EVE JWKS returned non-2xx")?
            .json::<JwkSet>()
            .await
            .context("parsing EVE JWKS")
    }

    async fn key_for(&self, kid: &str) -> anyhow::Result<Jwk> {
        if let Some(set) = self.keys.read().await.as_ref() {
            if let Some(k) = set.find(kid) {
                return Ok(k.clone());
            }
        }

        let _gate = self.fetch_gate.lock().await;
        if let Some(set) = self.keys.read().await.as_ref() {
            if let Some(k) = set.find(kid) {
                return Ok(k.clone());
            }
        }

        let fresh = self.fetch().await?;
        let found = fresh
            .find(kid)
            .cloned()
            .ok_or_else(|| anyhow!("EVE JWKS has no key with kid {kid}"))?;
        *self.keys.write().await = Some(fresh);
        Ok(found)
    }

    pub async fn verify(&self, token: &str) -> anyhow::Result<EveClaims> {
        let header = decode_header(token).context("malformed JWT header")?;
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| anyhow!("JWT header missing kid"))?;

        let jwk = self.key_for(kid).await?;
        let key = DecodingKey::from_jwk(&jwk).context("invalid JWK")?;

        let alg = jwk
            .common
            .key_algorithm
            .ok_or_else(|| anyhow!("JWK missing alg"))?
            .to_string()
            .parse()
            .context("unsupported JWK alg")?;

        let mut validation = Validation::new(alg);
        // EVE has shipped both forms in `iss` historically.
        validation.set_issuer(&[EVE_ISSUER_URL, EVE_ISSUER_HOST]);
        validation.set_audience(&[&self.expected_audience, "EVE Online"]);

        let data =
            decode::<EveClaims>(token, &key, &validation).context("JWT verification failed")?;
        Ok(data.claims)
    }
}
