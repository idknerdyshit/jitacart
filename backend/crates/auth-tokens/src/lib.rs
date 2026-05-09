//! Shared crate for token-at-rest encryption and per-character ESI clients.
//!
//! - [`MultiKeyCipher`] (AES-GCM with a map of keys keyed by id) is moved
//!   here from the api crate so that `worker` and `api` can both
//!   encrypt/decrypt refresh tokens. Each ciphertext row carries the kid it
//!   was encrypted under so we can rotate the primary key online.
//! - [`CharacterTokenStore`] hands out an `EsiClient` configured to act as a
//!   specific character. It caches per-character clients in a `DashMap` to
//!   avoid reconstructing them on every call (each `EsiClient` carries its own
//!   in-memory ETag cache and refresh-mutex; reusing them keeps cache hits
//!   warm and avoids fragmenting the ESI error budget more than necessary).
//! - [`EsiBudgetGuard`] is a process-wide gauge of the recent non-2xx response
//!   count across all per-character clients. Workers consult it before kicking
//!   off a batch and defer if remaining budget is too low.

mod budget;
mod cipher;
mod store;

pub use budget::{budgeted, EsiBudgetGuard};
pub use cipher::{build_cipher, MultiKeyCipher, TokenEncConfig};
pub use store::{reencrypt_stale, CharacterTokenStore, ReencryptOutcome};
