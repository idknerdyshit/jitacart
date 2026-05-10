//! Corp wallet poller.
//!
//! Each tick:
//! 1. Pick corps whose `wallet_next_poll_at` is due.
//! 2. Ambassador round-robin: GET `/corporations/{cid}/wallets/` for division
//!    balances, then GET `.../wallets/{division}/journal/` per division.
//! 3. Insert journal entries. For entries with `context_id_type='contract_id'`,
//!    queue a verification pass (updates `contracts.wallet_verified_at` and
//!    per-reimbursement `verified_by_wallet`).
//! 4. Page atomicity: a 403 mid-page does not advance the wallet cursor.

use std::sync::Arc;

use anyhow::Context;
use nea_esi::{EsiError, EsiWalletJournalEntry};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::{isk_or_zero, jitter_secs, JobFuture, JobSlot};
use crate::{Ctx, WorkerConfig};

const CORP_WALLET_SCOPE: &str = "esi-wallet.read_corporation_wallets.v1";

pub struct Job;

impl JobSlot for Job {
    fn name(&self) -> &'static str {
        "corp_wallet"
    }
    fn interval_secs(&self, cfg: &WorkerConfig) -> u64 {
        cfg.worker.tick_secs
    }
    fn run<'a>(&'a self, ctx: &'a Ctx) -> JobFuture<'a> {
        Box::pin(run(ctx))
    }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping corp_wallet tick"
        );
        return Ok(());
    }

    let tick_secs = ctx.config.worker.tick_secs as f64;
    let interval_secs = ctx.config.esi.poll_intervals_secs.corp_wallet as f64;

    let eligible_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM corps WHERE disabled_at IS NULL")
            .fetch_one(&ctx.pool)
            .await?;

    if eligible_count == 0 {
        return Ok(());
    }

    let slice_size = ((eligible_count as f64) * tick_secs / interval_secs).ceil() as i64;
    let slice_size = slice_size.max(1);

    #[derive(sqlx::FromRow)]
    struct DueCorp {
        id: Uuid,
        esi_corporation_id: i64,
    }

    let due: Vec<DueCorp> = sqlx::query_as(
        r#"
        SELECT id, esi_corporation_id
        FROM corps
        WHERE disabled_at IS NULL
          AND (wallet_next_poll_at IS NULL OR wallet_next_poll_at <= now())
        ORDER BY wallet_next_poll_at NULLS FIRST
        LIMIT $1
        "#,
    )
    .bind(slice_size)
    .fetch_all(&ctx.pool)
    .await?;

    if due.is_empty() {
        return Ok(());
    }

    tracing::info!(
        slice = due.len(),
        eligible = eligible_count,
        "corp_wallet tick"
    );

    let sem = Arc::new(Semaphore::new(ctx.config.worker.corp_wallet_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<Vec<Uuid>>> = Vec::new();

    for corp in due {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        let interval = interval_secs as u64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            match poll_one_corp_wallet(
                &pool,
                &token_store,
                &budget,
                corp.id,
                corp.esi_corporation_id,
                interval,
            )
            .await
            {
                Ok(contract_ids) => contract_ids,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        corp_id = %corp.id,
                        "corp_wallet poll failed"
                    );
                    Vec::new()
                }
            }
        }));
    }

    let mut all_contract_ids: Vec<Uuid> = Vec::new();
    for h in tasks {
        if let Ok(v) = h.await {
            all_contract_ids.extend(v);
        }
    }

    // Trigger wallet verification for any contracts that appeared in journal entries.
    for contract_id in all_contract_ids {
        match ctx.pool.begin().await {
            Ok(mut tx) => {
                match settlement::verify_contract_against_journal(&mut tx, contract_id).await {
                    Ok(()) => {
                        if let Err(e) = tx.commit().await {
                            tracing::warn!(
                                error = ?e,
                                contract_id = %contract_id,
                                "commit verify_contract_against_journal failed"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            contract_id = %contract_id,
                            "wallet verification failed"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    contract_id = %contract_id,
                    "could not start verify tx"
                );
            }
        }
    }

    Ok(())
}

/// Returns the JitaCart contract UUIDs that appeared in newly-inserted journal
/// entries (for triggering verification).
async fn poll_one_corp_wallet(
    pool: &PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    corp_id: Uuid,
    esi_corp_id: i64,
    interval_secs: u64,
) -> anyhow::Result<Vec<Uuid>> {
    if !budget.has_budget() {
        return Ok(Vec::new());
    }

    // Pick active ambassadors with the wallet scope.
    #[derive(sqlx::FromRow)]
    struct AmbassadorRow {
        character_id: Uuid,
    }

    let ambassadors: Vec<AmbassadorRow> = sqlx::query_as(
        r#"
        SELECT ca.character_id
        FROM corp_ambassadors ca
        JOIN characters ch ON ch.id = ca.character_id
        WHERE ca.corp_id = $1
          AND ca.disabled_at IS NULL
          AND $2 = ANY(ch.scopes)
          AND (ca.last_auth_error_at IS NULL
               OR ca.last_auth_error_at < now() - interval '1 hour')
        ORDER BY ca.last_used_at NULLS FIRST
        LIMIT 5
        "#,
    )
    .bind(corp_id)
    .bind(CORP_WALLET_SCOPE)
    .fetch_all(pool)
    .await?;

    if ambassadors.is_empty() {
        tracing::warn!(corp_id = %corp_id, "no usable ambassadors for corp wallet");
        advance_wallet_cursor(pool, corp_id, interval_secs).await?;
        return Ok(Vec::new());
    }

    // Try each ambassador in turn.
    for amb in &ambassadors {
        let client = match token_store.authed_client_for(amb.character_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    char = %amb.character_id,
                    "failed to get client for wallet ambassador"
                );
                continue;
            }
        };

        // Fetch division balances.
        let wallets = match client.corp_wallet_balances(esi_corp_id).await {
            Ok(v) => v,
            Err(EsiError::Api { status: 403, .. }) => {
                let _ = sqlx::query(
                    "UPDATE corp_ambassadors SET last_auth_error_at = now() \
                     WHERE corp_id = $1 AND character_id = $2",
                )
                .bind(corp_id)
                .bind(amb.character_id)
                .execute(pool)
                .await;
                budget.record_non_2xx();
                continue;
            }
            Err(e) => {
                budget.record_non_2xx();
                return Err(e.into());
            }
        };

        // Upsert division balances.
        for w in &wallets {
            let balance = isk_or_zero(w.balance);
            sqlx::query(
                r#"
                INSERT INTO corp_wallet_divisions (corp_id, division, balance_isk, last_synced_at)
                VALUES ($1, $2, $3, now())
                ON CONFLICT (corp_id, division) DO UPDATE
                    SET balance_isk = EXCLUDED.balance_isk,
                        last_synced_at = now()
                "#,
            )
            .bind(corp_id)
            .bind(w.division as i16)
            .bind(balance)
            .execute(pool)
            .await
            .with_context(|| format!("upsert wallet balance corp={corp_id} div={}", w.division))?;
        }

        // Fetch journal per division.
        let mut contract_esi_ids: Vec<i64> = Vec::new();

        for w in &wallets {
            let journal: Vec<EsiWalletJournalEntry> = match client
                .corp_wallet_journal(esi_corp_id, w.division, None)
                .await
            {
                Ok(v) => v,
                Err(EsiError::Api { status: 403, .. }) => {
                    // 403 mid-page: do NOT advance cursor; mark ambassador.
                    let _ = sqlx::query(
                        "UPDATE corp_ambassadors SET last_auth_error_at = now() \
                         WHERE corp_id = $1 AND character_id = $2",
                    )
                    .bind(corp_id)
                    .bind(amb.character_id)
                    .execute(pool)
                    .await;
                    budget.record_non_2xx();
                    tracing::warn!(
                        corp_id = %corp_id,
                        ambassador = %amb.character_id,
                        division = w.division,
                        "wallet journal 403: ambassador lost wallet scope; \
                         this corp's wallet coverage is paused until another \
                         ambassador can fetch"
                    );
                    return Ok(Vec::new());
                }
                Err(e) => {
                    budget.record_non_2xx();
                    return Err(e.into());
                }
            };

            for entry in &journal {
                let amount = entry.amount.map(isk_or_zero).unwrap_or_default();
                let balance = entry.balance.map(isk_or_zero).unwrap_or_default();
                let raw = serde_json::to_value(entry).unwrap_or(Value::Null);

                // Insert; RETURNING tells us if it was newly inserted (non-conflict).
                let inserted: Option<(Option<i64>,)> = sqlx::query_as(
                    r#"
                    INSERT INTO corp_wallet_journal
                        (corp_id, division, esi_journal_ref_id, date, ref_type,
                         amount, balance, first_party_id, second_party_id,
                         context_id, context_id_type, reason, raw_json)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                    ON CONFLICT (corp_id, division, esi_journal_ref_id) DO NOTHING
                    RETURNING context_id
                    "#,
                )
                .bind(corp_id)
                .bind(w.division as i16)
                .bind(entry.id)
                .bind(entry.date)
                .bind(&entry.ref_type)
                .bind(amount)
                .bind(balance)
                .bind(entry.first_party_id)
                .bind(entry.second_party_id)
                .bind(entry.context_id)
                .bind(entry.context_id_type.as_deref())
                .bind(entry.reason.as_deref())
                .bind(&raw)
                .fetch_optional(pool)
                .await
                .with_context(|| {
                    format!(
                        "insert journal corp={corp_id} div={} ref={}",
                        w.division, entry.id
                    )
                })?;
                let inserted_context_id = inserted.map(|(c,)| c);

                // If a newly-inserted row has context_id_type='contract_id', queue verification.
                if let Some(Some(ctx_id)) = inserted_context_id {
                    if entry.context_id_type.as_deref() == Some("contract_id") {
                        contract_esi_ids.push(ctx_id);
                    }
                }
            }
        }

        // Resolve freshly-inserted ESI contract ids → JitaCart contract UUIDs.
        let mut contract_uuids: Vec<Uuid> = if contract_esi_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_scalar("SELECT id FROM contracts WHERE esi_contract_id = ANY($1::bigint[])")
                .bind(&contract_esi_ids)
                .fetch_all(pool)
                .await
                .with_context(|| format!("resolve contract ids corp={corp_id}"))?
        };

        // Also include any contracts for this corp that have matching journal
        // entries but have never been wallet-verified (covers the race where a
        // journal row arrived before the contract was created and the conflict
        // guard silently dropped it on all subsequent ticks).
        let backfill: Vec<Uuid> = sqlx::query_scalar(
            r#"
            SELECT c.id
            FROM contracts c
            JOIN corp_wallet_journal j
              ON j.context_id = c.esi_contract_id
             AND j.context_id_type = 'contract_id'
             AND j.corp_id = $1
            WHERE c.wallet_verified_at IS NULL
            "#,
        )
        .bind(corp_id)
        .fetch_all(pool)
        .await
        .with_context(|| format!("backfill unverified contracts corp={corp_id}"))?;

        for id in backfill {
            if !contract_uuids.contains(&id) {
                contract_uuids.push(id);
            }
        }

        // Mark ambassador last_used_at.
        sqlx::query(
            "UPDATE corp_ambassadors SET last_used_at = now() \
             WHERE corp_id = $1 AND character_id = $2",
        )
        .bind(corp_id)
        .bind(amb.character_id)
        .execute(pool)
        .await
        .with_context(|| {
            format!(
                "update last_used_at corp={corp_id} char={}",
                amb.character_id
            )
        })?;

        if let Err(e) = token_store.persist_rotations(amb.character_id).await {
            tracing::warn!(error = ?e, char = %amb.character_id, "persist_rotations failed");
        }
        advance_wallet_cursor(pool, corp_id, interval_secs).await?;
        return Ok(contract_uuids);
    }

    // All ambassadors failed.
    let _ = sqlx::query("UPDATE corps SET last_auth_error_at = now() WHERE id = $1")
        .bind(corp_id)
        .execute(pool)
        .await;
    advance_wallet_cursor(pool, corp_id, interval_secs).await?;
    Ok(Vec::new())
}

async fn advance_wallet_cursor(
    pool: &PgPool,
    corp_id: Uuid,
    interval_secs: u64,
) -> anyhow::Result<()> {
    let jitter = jitter_secs(interval_secs);
    let next = (interval_secs as i64) + jitter;
    sqlx::query(
        "UPDATE corps \
         SET wallet_last_polled_at = now(), \
             wallet_next_poll_at = now() + make_interval(secs => $1::double precision) \
         WHERE id = $2",
    )
    .bind(next as f64)
    .bind(corp_id)
    .execute(pool)
    .await?;
    Ok(())
}
