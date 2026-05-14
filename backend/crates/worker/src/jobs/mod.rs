pub mod citadel_details;
pub mod citadel_discovery;
pub mod citadel_orders;
pub mod contracts;
pub(crate) mod contracts_common;
pub mod corp_contracts;
pub mod corp_wallet;
pub(crate) mod csa;
pub mod npc_hubs;
pub mod pending_webhooks;
pub mod token_reencrypt;

use std::future::Future;
use std::pin::Pin;

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;

use crate::{Ctx, WorkerConfig};

pub type JobFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// A periodic worker slot. Each module that owns a tick implements this on a
/// unit struct, then appends one entry to [`registry`].
pub trait JobSlot: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn interval_secs(&self, cfg: &WorkerConfig) -> u64;
    fn run<'a>(&'a self, ctx: &'a Ctx) -> JobFuture<'a>;
}

pub fn registry() -> Vec<Box<dyn JobSlot>> {
    vec![
        Box::new(npc_hubs::Job),
        Box::new(citadel_discovery::Job),
        Box::new(citadel_details::Job),
        Box::new(citadel_orders::Job),
        Box::new(contracts::Job),
        Box::new(corp_contracts::Job),
        Box::new(corp_wallet::Job),
        Box::new(pending_webhooks::Job),
        Box::new(token_reencrypt::Job),
    ]
}

pub(crate) fn jitter_secs(base: u64) -> i64 {
    if base == 0 {
        return 0;
    }
    let span = (base / 10).max(1) as i64;
    let r = rand::random::<u32>() as i64;
    r.rem_euclid(2 * span + 1) - span
}

/// Convert an ESI ISK amount to `Decimal`.
///
/// ESI ISK values reach us as `f64` — `nea-esi` deserializes every price /
/// reward / collateral / balance / amount field that way, so the JSON→f64
/// rounding has already happened before this is called. `from_f64` cleanly
/// recovers values with up to ~15 significant digits, which covers all
/// typical contracts; precision only degrades above ~10 trillion ISK with
/// sub-ISK fractions (large corp wallet balances). A complete fix needs a
/// Decimal-aware ESI deserializer — see issue tracker.
pub(crate) fn isk_or_zero(v: f64) -> Decimal {
    Decimal::from_f64(v).unwrap_or(Decimal::ZERO)
}
