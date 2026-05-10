//! Background sweeper that rewrites refresh tokens encrypted with a
//! non-primary kid. Drives the long tail of dormant characters onto the
//! current primary key after rotation; active characters get rewritten
//! automatically by `CharacterTokenStore::persist_rotations` whenever a
//! refresh fires.
//!
//! No ESI calls — the budget guard isn't consulted. Bounded batch keeps
//! transactions short.

use auth_tokens::reencrypt_stale;

use super::{JobFuture, JobSlot};
use crate::{Ctx, WorkerConfig};

const BATCH: i64 = 100;
/// Hourly sweep cadence. Active characters get rewritten on refresh; this
/// drains the dormant tail.
const INTERVAL_SECS: u64 = 3600;

pub struct Job;

impl JobSlot for Job {
    fn name(&self) -> &'static str {
        "token_reencrypt"
    }
    fn interval_secs(&self, _cfg: &WorkerConfig) -> u64 {
        INTERVAL_SECS
    }
    fn run<'a>(&'a self, ctx: &'a Ctx) -> JobFuture<'a> {
        Box::pin(run(ctx))
    }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let cipher = ctx.token_store.cipher();
    let outcome = reencrypt_stale(&ctx.pool, cipher, BATCH).await?;
    if outcome.scanned > 0 {
        tracing::info!(
            scanned = outcome.scanned,
            rewritten = outcome.rewritten,
            primary_kid = cipher.primary_kid(),
            "token-kid sweeper rewrote stale rows"
        );
    }
    Ok(())
}
