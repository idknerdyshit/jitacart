//! AES-GCM encryption for refresh tokens at rest.
//!
//! The key comes from config (`TOKEN_ENC_KEY`), 32 random bytes base64-encoded.
//! Each encrypt produces a fresh 12-byte nonce; ciphertext + nonce are stored
//! separately so a future key rotation can re-encrypt in place.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

#[derive(Clone)]
pub struct TokenCipher {
    cipher: Aes256Gcm,
}

impl TokenCipher {
    /// Build a cipher from a base64-encoded 32-byte key.
    pub fn from_b64(key_b64: &str) -> anyhow::Result<Self> {
        let raw = B64
            .decode(key_b64.trim())
            .context("TOKEN_ENC_KEY is not valid base64")?;
        if raw.len() != 32 {
            return Err(anyhow!(
                "TOKEN_ENC_KEY must decode to 32 bytes, got {}",
                raw.len()
            ));
        }
        let key = Key::<Aes256Gcm>::from_slice(&raw);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// Encrypt a plaintext, returning `(ciphertext, nonce)`.
    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| anyhow!("AES-GCM encrypt failed: {e}"))?;
        Ok((ct, nonce.to_vec()))
    }

    /// Decrypt with the stored ciphertext + nonce.
    #[allow(dead_code)] // used by the worker crate in later phases
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> anyhow::Result<Vec<u8>> {
        if nonce.len() != 12 {
            return Err(anyhow!("nonce must be 12 bytes, got {}", nonce.len()));
        }
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("AES-GCM decrypt failed: {e}"))
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

    #[test]
    fn round_trip() {
        let cipher = TokenCipher::from_b64(&fresh_key_b64()).unwrap();
        let pt = b"refresh-token-blob";
        let (ct, nonce) = cipher.encrypt(pt).unwrap();
        assert_ne!(ct.as_slice(), pt);
        assert_eq!(nonce.len(), 12);
        let recovered = cipher.decrypt(&ct, &nonce).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn tamper_fails() {
        let cipher = TokenCipher::from_b64(&fresh_key_b64()).unwrap();
        let (mut ct, nonce) = cipher.encrypt(b"abc").unwrap();
        ct[0] ^= 1;
        assert!(cipher.decrypt(&ct, &nonce).is_err());
    }

    #[test]
    fn rejects_short_key() {
        let short = B64.encode([0u8; 16]);
        assert!(TokenCipher::from_b64(&short).is_err());
    }
}
