//! Shared domain types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
}

/// A linked EVE character. The encrypted token columns are intentionally not
/// modeled here — they live in the `api` crate's persistence layer so the
/// domain stays free of crypto concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Character {
    pub id: Uuid,
    pub user_id: Uuid,
    pub character_id: i64,
    pub character_name: String,
    pub owner_hash: String,
    pub scopes: Vec<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: Option<DateTime<Utc>>,
}
