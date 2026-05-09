//! Background sweeper that rewrites refresh tokens encrypted with a
//! non-primary kid. Drives the long tail of dormant characters onto the
//! current primary key after rotation; active characters get rewritten
//! automatically by `CharacterTokenStore::persist_rotations` whenever a
//! refresh fires.
//!
//! No ESI calls — the budget guard isn't consulted. Bounded batch keeps
//! transactions short.

use auth_tokens::reencrypt_stale;

use crate::Ctx;

const BATCH: i64 = 100;

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
