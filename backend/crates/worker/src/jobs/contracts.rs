//! Contract poller. Each tick:
//!
//! 1. Pick a fan-out slice of characters whose `contracts_next_poll_at` is due.
//! 2. For each, GET `/characters/{cid}/contracts/` (paginated) and upsert any
//!    `item_exchange` contracts that involve at least one user we know about.
//! 3. Items sub-pass: pull `/characters/{cid}/contracts/{c}/items/` for any
//!    contracts whose body has not yet been fetched.
//! 4. For any contract that had a status transition or fresh items, run the
//!    matcher against open reimbursements.
//! 5. For any contract that just transitioned to a terminal status, settle or
//!    unbind via the shared [`settlement`] crate.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;
use nea_esi::{EsiContract, EsiError};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use settlement::ContractUpsert;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::Ctx;

mod r#match;

const CONTRACTS_SCOPE: &str = "esi-contracts.read_character_contracts.v1";
/// Hard ceiling on how many contracts we attempt to fetch items for per tick.
const ITEMS_BATCH: i64 = 50;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping contracts tick"
        );
        return Ok(());
    }

    let tick_secs = ctx.config.worker.tick_secs as f64;
    let interval_secs = ctx.config.esi.poll_intervals_secs.contracts as f64;

    // Fan out staggered: each tick processes a fraction of eligible characters
    // proportional to (tick / interval). Round up so we never starve.
    let eligible_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM characters WHERE $1 = ANY(scopes)")
            .bind(CONTRACTS_SCOPE)
            .fetch_one(&ctx.pool)
            .await?;

    if eligible_count == 0 {
        return Ok(());
    }

    let slice_size = ((eligible_count as f64) * tick_secs / interval_secs).ceil() as i64;
    let slice_size = slice_size.max(1);

    let due: Vec<DueRow> = sqlx::query_as(
        r#"
        SELECT id, character_id
        FROM characters
        WHERE $1 = ANY(scopes)
          AND (contracts_next_poll_at IS NULL OR contracts_next_poll_at <= now())
        ORDER BY contracts_next_poll_at NULLS FIRST
        LIMIT $2
        "#,
    )
    .bind(CONTRACTS_SCOPE)
    .bind(slice_size)
    .fetch_all(&ctx.pool)
    .await?;

    if !due.is_empty() {
        tracing::info!(
            slice = due.len(),
            eligible = eligible_count,
            "contracts tick"
        );
    }

    let mut upserts: Vec<UpsertedContract> = Vec::new();
    let sem = Arc::new(Semaphore::new(ctx.config.worker.contracts_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<Vec<UpsertedContract>>> = Vec::new();

    for row in due {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        let interval_secs_for_jitter = interval_secs as u64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            match poll_one_character(&pool, &token_store, &budget, &row, interval_secs_for_jitter)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = ?e, character_id = row.character_id, "contracts poll failed");
                    Vec::new()
                }
            }
        }));
    }

    for h in tasks {
        if let Ok(v) = h.await {
            upserts.extend(v);
        }
    }

    sync_pending_items(ctx).await?;

    let candidate_ids: Vec<Uuid> = upserts.iter().map(|u| u.contract_id).collect();
    if !candidate_ids.is_empty() {
        if let Err(e) = r#match::run_for_contracts(&ctx.pool, &candidate_ids).await {
            tracing::warn!(error = ?e, "contract matcher failed");
        }
    }

    for u in upserts {
        if !u.status_changed {
            continue;
        }
        if let Err(e) = handle_terminal_transition(&ctx.pool, &u).await {
            tracing::warn!(
                error = ?e,
                esi_contract_id = u.esi_contract_id,
                "terminal transition handler failed"
            );
        }
    }

    Ok(())
}

#[derive(sqlx::FromRow)]
struct DueRow {
    id: Uuid,
    character_id: i64,
}

#[derive(Clone)]
struct UpsertedContract {
    contract_id: Uuid,
    esi_contract_id: i64,
    status: String,
    status_changed: bool,
}

async fn poll_one_character(
    pool: &PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    row: &DueRow,
    interval_secs: u64,
) -> anyhow::Result<Vec<UpsertedContract>> {
    if !budget.has_budget() {
        return Ok(Vec::new());
    }

    let client = token_store
        .authed_client_for(row.id)
        .await
        .with_context(|| format!("authed client for character {}", row.character_id))?;

    let contracts: Vec<EsiContract> = match client.character_contracts(row.character_id).await {
        Ok(v) => v,
        Err(EsiError::Api { status: 403, .. }) => {
            // Scope was revoked in EVE. Drop it from the row and stop polling
            // until the user re-grants.
            sqlx::query(
                "UPDATE characters \
                 SET scopes = array_remove(scopes, $1), \
                     contracts_next_poll_at = NULL \
                 WHERE id = $2",
            )
            .bind(CONTRACTS_SCOPE)
            .bind(row.id)
            .execute(pool)
            .await?;
            return Ok(Vec::new());
        }
        Err(e) => {
            budget.record_non_2xx();
            advance_cursor(pool, row.id, interval_secs).await?;
            return Err(e.into());
        }
    };

    // Filter to item_exchange contracts where at least one party is one of ours.
    // Resolve issuer/assignee EVE ids → user ids via a single batched lookup.
    let all_eve_ids: Vec<i64> = contracts
        .iter()
        .flat_map(|c| std::iter::once(c.issuer_id).chain(c.assignee_id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let user_map: HashMap<i64, Uuid> = if all_eve_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (i64, Uuid)>(
            "SELECT character_id, user_id FROM characters \
             WHERE character_id = ANY($1::bigint[])",
        )
        .bind(&all_eve_ids)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect()
    };

    let mut out: Vec<UpsertedContract> = Vec::new();
    let mut tx = pool.begin().await?;
    for c in &contracts {
        if c.contract_type != "item_exchange" {
            continue;
        }
        let issuer_user_id = user_map.get(&c.issuer_id).copied();
        let assignee_user_id = c.assignee_id.and_then(|a| user_map.get(&a).copied());
        if issuer_user_id.is_none() && assignee_user_id.is_none() {
            // Drop contracts that aren't between two of ours; they can't bind
            // to a JitaCart reimbursement.
            continue;
        }

        let upsert = ContractUpsert {
            esi_contract_id: c.contract_id,
            issuer_character_id: c.issuer_id,
            issuer_user_id,
            assignee_character_id: c.assignee_id,
            assignee_user_id,
            contract_type: c.contract_type.clone(),
            status: c.status.clone(),
            price_isk: Decimal::from_f64(c.price.unwrap_or(0.0)).unwrap_or(Decimal::ZERO),
            reward_isk: Decimal::from_f64(c.reward.unwrap_or(0.0)).unwrap_or(Decimal::ZERO),
            collateral_isk: Decimal::from_f64(c.collateral.unwrap_or(0.0)).unwrap_or(Decimal::ZERO),
            date_issued: c.date_issued,
            date_expired: Some(c.date_expired),
            date_accepted: c.date_accepted,
            date_completed: c.date_completed,
            start_location_id: c.start_location_id,
            end_location_id: c.end_location_id,
            raw_json: serde_json::to_value(c).unwrap_or(Value::Null),
        };

        let outcome = settlement::upsert_contract(&mut tx, &upsert)
            .await
            .with_context(|| format!("upsert_contract esi={}", c.contract_id))?;

        let status_changed = outcome.status_changed();
        out.push(UpsertedContract {
            contract_id: outcome.contract_id,
            esi_contract_id: c.contract_id,
            status: outcome.current_status,
            status_changed,
        });
    }
    tx.commit().await?;

    advance_cursor(pool, row.id, interval_secs).await?;
    if let Err(e) = token_store.persist_rotations(row.id).await {
        tracing::warn!(error = ?e, "persist_rotations failed");
    }
    Ok(out)
}

async fn advance_cursor(
    pool: &PgPool,
    character_id: Uuid,
    interval_secs: u64,
) -> anyhow::Result<()> {
    // ±10% jitter to keep the staggered population from re-clumping.
    let jitter = jitter_secs(interval_secs);
    let next = (interval_secs as i64) + jitter;
    sqlx::query(
        "UPDATE characters \
         SET contracts_last_polled_at = now(), \
             contracts_next_poll_at = now() + make_interval(secs => $1::double precision) \
         WHERE id = $2",
    )
    .bind(next as f64)
    .bind(character_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn jitter_secs(base: u64) -> i64 {
    if base == 0 {
        return 0;
    }
    let span = (base / 10).max(1) as i64;
    let r = rand::random::<u32>() as i64;
    r.rem_euclid(2 * span + 1) - span
}

async fn sync_pending_items(ctx: &Ctx) -> anyhow::Result<()> {
    let needs: Vec<NeedsItemsRow> = sqlx::query_as(
        r#"
        SELECT id, esi_contract_id, issuer_character_id, assignee_character_id
        FROM contracts
        WHERE items_synced_at IS NULL
          AND contract_type = 'item_exchange'
        ORDER BY first_seen_at
        LIMIT $1
        "#,
    )
    .bind(ITEMS_BATCH)
    .fetch_all(&ctx.pool)
    .await?;

    if needs.is_empty() {
        return Ok(());
    }

    let sem = Arc::new(Semaphore::new(ctx.config.worker.contracts_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for row in needs {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = sync_items_for(&pool, &token_store, &budget, &row).await {
                tracing::warn!(error = ?e, esi_contract_id = row.esi_contract_id, "items sync failed");
            }
        }));
    }
    for h in tasks {
        let _ = h.await;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct NeedsItemsRow {
    id: Uuid,
    esi_contract_id: i64,
    issuer_character_id: i64,
    assignee_character_id: Option<i64>,
}

async fn sync_items_for(
    pool: &PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    row: &NeedsItemsRow,
) -> anyhow::Result<()> {
    if !budget.has_budget() {
        return Ok(());
    }
    // Try issuer first (whoever made the contract can read its items), then
    // assignee. Both must be a JitaCart-tracked character with the contracts
    // scope to give us a usable client.
    let candidates: Vec<i64> = std::iter::once(row.issuer_character_id)
        .chain(row.assignee_character_id)
        .collect();
    let pick: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT id, character_id FROM characters \
         WHERE character_id = ANY($1::bigint[]) AND $2 = ANY(scopes) \
         LIMIT 1",
    )
    .bind(&candidates)
    .bind(CONTRACTS_SCOPE)
    .fetch_optional(pool)
    .await?;

    let (char_uuid, eve_char_id) = match pick {
        Some(v) => v,
        None => return Ok(()),
    };

    let client = token_store.authed_client_for(char_uuid).await?;
    let items = match client
        .character_contract_items(eve_char_id, row.esi_contract_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            budget.record_non_2xx();
            return Err(e.into());
        }
    };

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM contract_items WHERE contract_id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;

    if !items.is_empty() {
        let record_ids: Vec<i64> = items.iter().map(|it| it.record_id).collect();
        let type_ids: Vec<i32> = items.iter().map(|it| it.type_id).collect();
        let quantities: Vec<i64> = items.iter().map(|it| it.quantity as i64).collect();
        let is_includeds: Vec<bool> = items.iter().map(|it| it.is_included).collect();
        sqlx::query(
            "INSERT INTO contract_items (contract_id, record_id, type_id, quantity, is_included) \
             SELECT $1, * FROM UNNEST($2::bigint[], $3::int[], $4::bigint[], $5::bool[])",
        )
        .bind(row.id)
        .bind(&record_ids)
        .bind(&type_ids)
        .bind(&quantities)
        .bind(&is_includeds)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("UPDATE contracts SET items_synced_at = now() WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let _ = token_store.persist_rotations(char_uuid).await;
    Ok(())
}

async fn handle_terminal_transition(pool: &PgPool, u: &UpsertedContract) -> anyhow::Result<()> {
    let parsed: domain::ContractStatus = match u.status.parse() {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    if parsed.is_terminal_success() {
        let mut tx = pool.begin().await?;
        // Lock the contract row and confirm there's at least one bound row.
        let bound: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM reimbursements WHERE contract_id = $1 AND status = 'pending'",
        )
        .bind(u.contract_id)
        .fetch_one(&mut *tx)
        .await?;
        if bound > 0 {
            settlement::settle_via_contract(&mut tx, u.contract_id)
                .await
                .map_err(|e| anyhow::anyhow!("settle_via_contract: {e}"))?;
        }
        tx.commit().await?;
    } else if parsed.is_terminal_failure() {
        let mut tx = pool.begin().await?;
        settlement::unbind_contract(&mut tx, u.contract_id)
            .await
            .map_err(|e| anyhow::anyhow!("unbind_contract: {e}"))?;
        tx.commit().await?;
    }
    Ok(())
}

