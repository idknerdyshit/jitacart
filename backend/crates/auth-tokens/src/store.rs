//! Per-character authed `EsiClient` factory.
//!
//! The store hands out an `EsiClient` configured with credentials and the
//! character's stored refresh token. nea-esi handles refresh-on-near-expiry
//! internally; we listen for rotated tokens and re-encrypt them back to the
//! database. nea-esi does not expose a callback, so we diff
//! `get_tokens().refresh_token` against the value we last persisted and write
//! back when it changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nea_esi::{auth::EsiTokens, EsiClient};
use secrecy::{ExposeSecret, SecretString};
use sqlx::PgPool;
use uuid::Uuid;

use crate::cipher::TokenCipher;

/// Hands out per-character `EsiClient` instances backed by the encrypted
/// refresh tokens persisted in `characters`.
#[derive(Clone)]
pub struct CharacterTokenStore {
    inner: Arc<Inner>,
}

struct Inner {
    pool: PgPool,
    cipher: TokenCipher,
    user_agent: String,
    client_id: String,
    client_secret: SecretString,
    /// One client per character, cached so we keep ETag state warm and avoid
    /// reissuing token-refresh round-trips on every call.
    clients: DashMap<Uuid, CachedClient>,
}

struct CachedClient {
    client: Arc<EsiClient>,
    /// FNV-style hash of the most recently persisted refresh token. Used to
    /// detect rotation without copying the secret out for comparison.
    last_refresh_hash: std::sync::atomic::AtomicU64,
}

impl CharacterTokenStore {
    pub fn new(
        pool: PgPool,
        cipher: TokenCipher,
        user_agent: String,
        client_id: String,
        client_secret: SecretString,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool,
                cipher,
                user_agent,
                client_id,
                client_secret,
                clients: DashMap::new(),
            }),
        }
    }

    /// Build (or reuse) an authed `EsiClient` for `character_id`.
    pub async fn authed_client_for(&self, character_id: Uuid) -> anyhow::Result<Arc<EsiClient>> {
        // Fast path: cached, refresh-token unchanged in DB.
        let row = self.load_tokens(character_id).await?;

        if let Some(entry) = self.inner.clients.get(&character_id) {
            let stored_hash = hash_secret(row.refresh_token.expose_secret());
            let known_hash = entry
                .last_refresh_hash
                .load(std::sync::atomic::Ordering::Relaxed);
            if stored_hash == known_hash {
                return Ok(Arc::clone(&entry.client));
            }
            // DB has a newer token (e.g. another worker process rotated it).
            // Drop the cache entry and fall through to rebuild.
            drop(entry);
            self.inner.clients.remove(&character_id);
        }

        let client = EsiClient::with_web_app(
            &self.inner.user_agent,
            &self.inner.client_id,
            self.inner.client_secret.clone(),
        )
        .map_err(|e| anyhow!("EsiClient::with_web_app: {e}"))?
        .with_cache();

        client
            .set_tokens(EsiTokens {
                access_token: row.access_token.unwrap_or_else(|| SecretString::from("")),
                refresh_token: row.refresh_token.clone(),
                expires_at: row.access_token_expires_at.unwrap_or_else(|| {
                    // Force a refresh on first call by claiming we've already expired.
                    Utc::now() - chrono::Duration::seconds(1)
                }),
            })
            .await;

        let client = Arc::new(client);
        let cached = CachedClient {
            client: Arc::clone(&client),
            last_refresh_hash: std::sync::atomic::AtomicU64::new(hash_secret(
                row.refresh_token.expose_secret(),
            )),
        };
        self.inner.clients.insert(character_id, cached);
        Ok(client)
    }

    /// Pull the latest tokens out of the cached client and, if the refresh
    /// token has rotated, write the new ciphertext back to the database.
    /// Workers should call this after each batch.
    pub async fn persist_rotations(&self, character_id: Uuid) -> anyhow::Result<()> {
        let entry = match self.inner.clients.get(&character_id) {
            Some(e) => e,
            None => return Ok(()),
        };
        let tokens = match entry.client.get_tokens().await {
            Some(t) => t,
            None => return Ok(()),
        };
        let new_hash = hash_secret(tokens.refresh_token.expose_secret());
        let prior = entry
            .last_refresh_hash
            .swap(new_hash, std::sync::atomic::Ordering::Relaxed);
        if prior == new_hash {
            return Ok(());
        }
        let (rt_ct, rt_nonce) = self
            .inner
            .cipher
            .encrypt(tokens.refresh_token.expose_secret().as_bytes())?;
        let (at_ct, at_nonce) = self
            .inner
            .cipher
            .encrypt(tokens.access_token.expose_secret().as_bytes())?;
        sqlx::query(
            "UPDATE characters SET \
                refresh_token_ciphertext = $1, refresh_token_nonce = $2, \
                access_token_ciphertext = $3, access_token_nonce = $4, \
                access_token_expires_at = $5, last_refreshed_at = now() \
             WHERE id = $6",
        )
        .bind(&rt_ct)
        .bind(&rt_nonce)
        .bind(&at_ct)
        .bind(&at_nonce)
        .bind(tokens.expires_at)
        .bind(character_id)
        .execute(&self.inner.pool)
        .await
        .context("persisting rotated token")?;
        Ok(())
    }

    async fn load_tokens(&self, character_id: Uuid) -> anyhow::Result<LoadedTokens> {
        #[derive(sqlx::FromRow)]
        struct Row {
            refresh_token_ciphertext: Vec<u8>,
            refresh_token_nonce: Vec<u8>,
            access_token_ciphertext: Option<Vec<u8>>,
            access_token_nonce: Option<Vec<u8>>,
            access_token_expires_at: Option<DateTime<Utc>>,
        }
        let row: Row = sqlx::query_as(
            "SELECT refresh_token_ciphertext, refresh_token_nonce, \
                    access_token_ciphertext, access_token_nonce, access_token_expires_at \
             FROM characters WHERE id = $1",
        )
        .bind(character_id)
        .fetch_optional(&self.inner.pool)
        .await
        .context("loading character tokens")?
        .ok_or_else(|| anyhow!("character {character_id} not found"))?;

        let refresh_pt = self
            .inner
            .cipher
            .decrypt(&row.refresh_token_ciphertext, &row.refresh_token_nonce)?;
        let refresh_token = SecretString::from(
            String::from_utf8(refresh_pt).context("refresh token plaintext is not UTF-8")?,
        );

        let access_token = match (row.access_token_ciphertext, row.access_token_nonce) {
            (Some(ct), Some(nonce)) => {
                let pt = self.inner.cipher.decrypt(&ct, &nonce)?;
                Some(SecretString::from(
                    String::from_utf8(pt).context("access token plaintext is not UTF-8")?,
                ))
            }
            _ => None,
        };

        Ok(LoadedTokens {
            refresh_token,
            access_token,
            access_token_expires_at: row.access_token_expires_at,
        })
    }
}

struct LoadedTokens {
    refresh_token: SecretString,
    access_token: Option<SecretString>,
    access_token_expires_at: Option<DateTime<Utc>>,
}

fn hash_secret(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
