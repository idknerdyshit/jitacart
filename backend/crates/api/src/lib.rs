// The Tx extractor and Acquire-based helpers leave explicit `&mut **conn`
// and `&mut *conn` patterns scattered through every handler. Clippy keeps
// asking to drop the leading stars, but the suggested form (`&mut conn`)
// either doesn't compile (TxGuard isn't an Executor) or hides a meaningful
// distinction between `&mut Transaction` (settlement helpers) and
// `&mut PgConnection` (sqlx executors). Suppress the lint crate-wide.
#![allow(clippy::explicit_auto_deref)]

pub mod auth;
pub mod citadels;
pub mod config;
pub mod contracts;
pub mod corps;
pub mod db;
pub mod errors;
pub mod extract;
pub mod fulfillment;
pub mod groups;
pub mod jwt;
pub mod lists;
pub mod markets;
pub mod state;
pub mod turnstile;
pub mod webhooks;

pub use errors::ApiError;
