//! AES-GCM encryption for refresh tokens at rest, with key rotation via KID.
//!
//! At rest each ciphertext row carries a string `key_id` that names which key
//! was used to encrypt it. The cipher holds a map of `kid -> Aes256Gcm` plus
//! a designated *primary* kid. Encryption always uses the primary; decryption
//! looks up by the row's kid. Unknown kid is a hard error — never silently
//! fall back, since that would mask a misconfiguration during rotation.
//!
//! Rotation procedure (see SECURITY.md):
//!   1. Add a new key alongside the existing one(s), keep primary unchanged.
//!   2. Bounce the api + worker (both must see the new key before it can be
//!      used as primary).
//!   3. Flip primary to the new kid; bounce again.
//!   4. The worker's reencrypt sweeper drains old-kid rows in the background.
//!   5. After the sweep settles, retire the old kid from config.

use std::collections::HashMap;

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng, Payload},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Deserialize;

/// Multi-key encryption config: a map of kid → base64 key plus a `primary`
/// kid. Loaded from the api/worker config files; deserialized via serde.
#[derive(Debug, Deserialize)]
pub struct TokenEncConfig {
    pub primary: String,
    pub keys: HashMap<String, String>,
}

/// Build a [`MultiKeyCipher`] from whichever shape is configured. Errors if
/// neither is present, or the multi-key shape is internally inconsistent.
pub fn build_cipher(
    token_enc: Option<&TokenEncConfig>,
    token_enc_key: Option<&str>,
) -> anyhow::Result<MultiKeyCipher> {
    match (token_enc, token_enc_key) {
        (Some(t), _) => MultiKeyCipher::from_keys(t.keys.clone(), t.primary.clone()),
        (None, Some(k)) => MultiKeyCipher::from_legacy_b64(k),
        (None, None) => Err(anyhow!(
            "neither [token_enc] nor token_enc_key is set; refusing to start without an at-rest encryption key"
        )),
    }
}

/// Kid assigned to legacy single-key configurations (`token_enc_key`).
/// Mirrored as the SQL default for `characters.token_key_id`.
pub const LEGACY_KID: &str = "v1";

#[derive(Clone)]
pub struct MultiKeyCipher {
    keys: HashMap<String, Aes256Gcm>,
    primary: String,
}

impl std::fmt::Debug for MultiKeyCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never expose key material in Debug output. The kid set is fine.
        f.debug_struct("MultiKeyCipher")
            .field("primary", &self.primary)
            .field("kids", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl MultiKeyCipher {
    /// Build from `(kid -> base64-32-bytes)` pairs and a chosen primary kid.
    /// Errors if `primary` is not in the map, or any key is the wrong length.
    pub fn from_keys(keys_b64: HashMap<String, String>, primary: String) -> anyhow::Result<Self> {
        if keys_b64.is_empty() {
            return Err(anyhow!("token_enc.keys must not be empty"));
        }
        if !keys_b64.contains_key(&primary) {
            return Err(anyhow!(
                "token_enc.primary = {primary:?} but no matching key was provided"
            ));
        }
        let mut keys = HashMap::with_capacity(keys_b64.len());
        for (kid, b64) in keys_b64 {
            if kid.is_empty() {
                return Err(anyhow!("token_enc key id must not be empty"));
            }
            let raw = B64
                .decode(b64.trim())
                .with_context(|| format!("token_enc key {kid:?} is not valid base64"))?;
            if raw.len() != 32 {
                return Err(anyhow!(
                    "token_enc key {kid:?} must decode to 32 bytes, got {}",
                    raw.len()
                ));
            }
            let key = Key::<Aes256Gcm>::from_slice(&raw);
            keys.insert(kid, Aes256Gcm::new(key));
        }
        Ok(Self { keys, primary })
    }

    /// Convenience: legacy single-key configuration. The key is loaded under
    /// kid `"v1"` and made primary, matching the schema default. Existing
    /// pre-rotation deployments keep working.
    pub fn from_legacy_b64(key_b64: &str) -> anyhow::Result<Self> {
        let mut keys = HashMap::with_capacity(1);
        keys.insert(LEGACY_KID.to_string(), key_b64.to_string());
        Self::from_keys(keys, LEGACY_KID.to_string())
    }

    pub fn primary_kid(&self) -> &str {
        &self.primary
    }

    pub fn knows(&self, kid: &str) -> bool {
        self.keys.contains_key(kid)
    }

    /// Encrypt with the primary key. Returns `(ciphertext, nonce, kid)`.
    /// `aad` is bound into the AEAD tag — decryption will fail if a row's
    /// ciphertext is moved between characters (or whatever scope `aad` carries).
    pub fn encrypt(
        &self,
        plaintext: &[u8],
        aad: &[u8],
    ) -> anyhow::Result<(Vec<u8>, Vec<u8>, String)> {
        let cipher = self
            .keys
            .get(&self.primary)
            .expect("primary kid validated at construction");
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| anyhow!("AES-GCM encrypt failed: {e}"))?;
        Ok((ct, nonce.to_vec(), self.primary.clone()))
    }

    /// Decrypt with the key named by `kid`. Unknown kid or AAD mismatch is a
    /// hard error.
    pub fn decrypt(
        &self,
        ciphertext: &[u8],
        nonce: &[u8],
        kid: &str,
        aad: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        if nonce.len() != 12 {
            return Err(anyhow!("nonce must be 12 bytes, got {}", nonce.len()));
        }
        let cipher = self.keys.get(kid).ok_or_else(|| {
            anyhow!("token row encrypted with unknown kid {kid:?}; check token_enc config")
        })?;
        let nonce = Nonce::from_slice(nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|e| anyhow!("AES-GCM decrypt failed for kid {kid:?}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    fn fresh_key_b64() -> String {
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        B64.encode(k)
    }

    fn cipher_with(keys: &[(&str, &str)], primary: &str) -> MultiKeyCipher {
        let map = keys
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        MultiKeyCipher::from_keys(map, primary.to_string()).unwrap()
    }

    const AAD_A: &[u8] = b"character-1";
    const AAD_B: &[u8] = b"character-2";

    #[test]
    fn round_trip_single_key() {
        let v1 = fresh_key_b64();
        let c = cipher_with(&[("v1", &v1)], "v1");
        let pt = b"refresh-token-blob";
        let (ct, nonce, kid) = c.encrypt(pt, AAD_A).unwrap();
        assert_eq!(kid, "v1");
        assert_ne!(ct.as_slice(), pt);
        let recovered = c.decrypt(&ct, &nonce, &kid, AAD_A).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn legacy_loader_uses_v1() {
        let c = MultiKeyCipher::from_legacy_b64(&fresh_key_b64()).unwrap();
        assert_eq!(c.primary_kid(), "v1");
        let (ct, nonce, kid) = c.encrypt(b"x", AAD_A).unwrap();
        assert_eq!(kid, "v1");
        assert_eq!(c.decrypt(&ct, &nonce, "v1", AAD_A).unwrap(), b"x");
    }

    #[test]
    fn rotation_decrypt_old_encrypt_new() {
        let v1 = fresh_key_b64();
        let v2 = fresh_key_b64();

        // Encrypt under the old key (primary=v1).
        let old = cipher_with(&[("v1", &v1)], "v1");
        let (ct, nonce, kid) = old.encrypt(b"secret", AAD_A).unwrap();
        assert_eq!(kid, "v1");

        // Now config has both keys, primary=v2. We can still decrypt the
        // legacy ciphertext under v1, and new encrypts go under v2.
        let rotated = cipher_with(&[("v1", &v1), ("v2", &v2)], "v2");
        assert_eq!(rotated.primary_kid(), "v2");
        let recovered = rotated.decrypt(&ct, &nonce, "v1", AAD_A).unwrap();
        assert_eq!(recovered, b"secret");
        let (_, _, new_kid) = rotated.encrypt(b"fresh", AAD_A).unwrap();
        assert_eq!(new_kid, "v2");
    }

    #[test]
    fn unknown_kid_is_error() {
        let v1 = fresh_key_b64();
        let c = cipher_with(&[("v1", &v1)], "v1");
        let (ct, nonce, _) = c.encrypt(b"x", AAD_A).unwrap();
        let err = c
            .decrypt(&ct, &nonce, "v99", AAD_A)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown kid"), "got: {err}");
    }

    #[test]
    fn primary_must_be_in_keys() {
        let v1 = fresh_key_b64();
        let map = std::iter::once(("v1".to_string(), v1)).collect();
        let err = MultiKeyCipher::from_keys(map, "v2".to_string())
            .unwrap_err()
            .to_string();
        assert!(err.contains("primary"));
    }

    #[test]
    fn rejects_short_key() {
        let short = B64.encode([0u8; 16]);
        let map = std::iter::once(("v1".to_string(), short)).collect();
        assert!(MultiKeyCipher::from_keys(map, "v1".to_string()).is_err());
    }

    #[test]
    fn tamper_fails() {
        let v1 = fresh_key_b64();
        let c = cipher_with(&[("v1", &v1)], "v1");
        let (mut ct, nonce, kid) = c.encrypt(b"abc", AAD_A).unwrap();
        ct[0] ^= 1;
        assert!(c.decrypt(&ct, &nonce, &kid, AAD_A).is_err());
    }

    /// Transplant attempt: a ciphertext encrypted bound to AAD_A must not
    /// decrypt against AAD_B. This is the property that prevents a row-level
    /// write from moving a ciphertext to a different character.
    #[test]
    fn aad_mismatch_fails() {
        let v1 = fresh_key_b64();
        let c = cipher_with(&[("v1", &v1)], "v1");
        let (ct, nonce, kid) = c.encrypt(b"refresh", AAD_A).unwrap();
        assert!(c.decrypt(&ct, &nonce, &kid, AAD_B).is_err());
        assert!(c.decrypt(&ct, &nonce, &kid, b"").is_err());
        // Same AAD still works.
        assert_eq!(c.decrypt(&ct, &nonce, &kid, AAD_A).unwrap(), b"refresh");
    }
}
