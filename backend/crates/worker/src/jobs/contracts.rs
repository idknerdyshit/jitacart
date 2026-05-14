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

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use nea_esi::{EsiContract, EsiError};
use settlement::ContractUpsert;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::contracts_common::{
    build_principal_index, find_user_for_principal, handle_contract_terminal, UpsertedContract,
};
use super::{isk_or_zero, jitter_secs, JobFuture, JobSlot};

use domain::principals::{resolve_contract_parties, EsiContractParties};

use crate::{Ctx, WorkerConfig};

pub struct Job;

impl JobSlot for Job {
    fn name(&self) -> &'static str {
        "contracts"
    }
    fn interval_secs(&self, cfg: &WorkerConfig) -> u64 {
        cfg.worker.tick_secs
    }
    fn run<'a>(&'a self, ctx: &'a Ctx) -> JobFuture<'a> {
        Box::pin(run(ctx))
    }
}

pub(crate) mod r#match;

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
        // No character scope, but corp ambassadors can still fetch items.
        let synced = sync_pending_items(ctx).await?;
        if !synced.is_empty() {
            if let Err(e) = r#match::run_for_contracts(&ctx.pool, &synced).await {
                tracing::warn!(error = ?e, "contract matcher (items-only) failed");
            }
        }
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

    let synced_ids = sync_pending_items(ctx).await?;

    let candidate_ids: Vec<Uuid> = upserts
        .iter()
        .map(|u| u.contract_id)
        .chain(synced_ids.iter().copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !candidate_ids.is_empty() {
        if let Err(e) = r#match::run_for_contracts(&ctx.pool, &candidate_ids).await {
            tracing::warn!(error = ?e, "contract matcher failed");
        }
    }

    for u in upserts {
        if !u.status_changed {
            continue;
        }
        if let Err(e) = handle_contract_terminal(ctx, &u).await {
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

    // Build a principal index covering all EVE entity ids in this batch.
    // This handles both character→user and corporation→corp-principal lookups,
    // enabling resolution of "hauler issues contract to corp" assignments.
    let all_eve_ids: Vec<i64> = contracts
        .iter()
        .flat_map(|c| {
            std::iter::once(c.issuer_id)
                .chain(c.assignee_id)
                .chain(std::iter::once(c.issuer_corporation_id))
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let idx = build_principal_index(pool, &all_eve_ids).await?;

    let mut out: Vec<UpsertedContract> = Vec::new();
    let mut tx = pool.begin().await?;
    for c in &contracts {
        let Ok(contract_type) = c.contract_type.parse::<domain::ContractType>() else {
            continue;
        };
        if contract_type != domain::ContractType::ItemExchange {
            continue;
        }
        let Ok(status) = c.status.parse::<domain::ContractStatus>() else {
            continue;
        };

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: domain::EsiCharacterId(c.issuer_id),
                issuer_corporation_id: domain::EsiCorporationId(c.issuer_corporation_id),
                for_corporation: c.for_corporation,
                assignee_id: c.assignee_id,
            },
            &idx,
        );

        if parties.issuer_principal_id.is_none() && parties.assignee_principal_id.is_none() {
            // Drop contracts that aren't between two of ours; they can't bind
            // to a JitaCart reimbursement.
            continue;
        }

        // Legacy user-id fields (deprecated; NULL for pure-corp contracts).
        let issuer_user_id = parties
            .issuer_principal_id
            .and_then(|pid| find_user_for_principal(&idx, pid));
        let assignee_user_id = parties
            .assignee_principal_id
            .and_then(|pid| find_user_for_principal(&idx, pid));

        let upsert = ContractUpsert {
            esi_contract_id: domain::EsiContractId(c.contract_id),
            issuer_character_id: domain::EsiCharacterId(c.issuer_id),
            issuer_user_id,
            assignee_character_id: c.assignee_id.map(domain::EsiCharacterId),
            assignee_user_id,
            issuer_principal_id: parties.issuer_principal_id,
            assignee_principal_id: parties.assignee_principal_id,
            contract_type,
            status,
            price_isk: isk_or_zero(c.price),
            reward_isk: isk_or_zero(c.reward),
            collateral_isk: isk_or_zero(c.collateral),
            date_issued: c.date_issued,
            date_expired: Some(c.date_expired),
            date_accepted: c.date_accepted,
            date_completed: c.date_completed,
            start_location_id: c.start_location_id.map(domain::EsiLocationId),
            end_location_id: c.end_location_id.map(domain::EsiLocationId),
            raw_json: serde_json::to_value(c)?,
            source_corp_id: None,
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

/// Sync items for contracts that still have `items_synced_at IS NULL`.
/// Returns the contract IDs that were successfully synced this pass so
/// callers can immediately re-run the matcher on them.
pub(crate) async fn sync_pending_items(ctx: &Ctx) -> anyhow::Result<Vec<Uuid>> {
    let needs: Vec<NeedsItemsRow> = sqlx::query_as(
        r#"
        SELECT id, esi_contract_id, issuer_character_id, assignee_character_id,
               source_corp_id
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
        return Ok(Vec::new());
    }

    let sem = Arc::new(Semaphore::new(ctx.config.worker.contracts_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<Option<Uuid>>> = Vec::new();

    for row in needs {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            let contract_id = row.id;
            match sync_items_for(&pool, &token_store, &budget, &row).await {
                Ok(true) => Some(contract_id),
                Ok(false) => None,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        esi_contract_id = row.esi_contract_id,
                        "items sync failed"
                    );
                    None
                }
            }
        }));
    }

    let mut synced: Vec<Uuid> = Vec::new();
    for h in tasks {
        if let Ok(Some(id)) = h.await {
            synced.push(id);
        }
    }
    Ok(synced)
}

#[derive(sqlx::FromRow)]
struct NeedsItemsRow {
    id: Uuid,
    esi_contract_id: i64,
    issuer_character_id: i64,
    assignee_character_id: Option<i64>,
    source_corp_id: Option<Uuid>,
}

/// Returns `true` when items were fetched and `items_synced_at` was committed,
/// `false` when no usable token was available (will be retried next tick).
async fn sync_items_for(
    pool: &PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    row: &NeedsItemsRow,
) -> anyhow::Result<bool> {
    if !budget.has_budget() {
        return Ok(false);
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

    // token_rotation_char tracks which character we should call persist_rotations for.
    let token_rotation_char: Uuid;
    let items = if let Some((char_uuid, eve_char_id)) = pick {
        token_rotation_char = char_uuid;
        let client = token_store.authed_client_for(char_uuid).await?;
        match client
            .character_contract_items(eve_char_id, row.esi_contract_id)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                budget.record_non_2xx();
                return Err(e.into());
            }
        }
    } else if let Some(corp_uuid) = row.source_corp_id {
        // No tracked character has the character-contracts scope; fall back to
        // the corp endpoint via an active ambassador.
        let amb: Option<(Uuid, i64)> = sqlx::query_as(
            "SELECT ca.character_id, c.esi_corporation_id \
             FROM corp_ambassadors ca \
             JOIN characters ch ON ch.id = ca.character_id \
             JOIN corps c ON c.id = ca.corp_id \
             WHERE ca.corp_id = $1 \
               AND ca.disabled_at IS NULL \
               AND $2 = ANY(ch.scopes) \
               AND (ca.last_auth_error_at IS NULL \
                    OR ca.last_auth_error_at < now() - interval '1 hour') \
             ORDER BY ca.last_used_at NULLS FIRST \
             LIMIT 1",
        )
        .bind(corp_uuid)
        .bind(super::corp_contracts::CORP_CONTRACTS_SCOPE)
        .fetch_optional(pool)
        .await?;

        let (amb_char_uuid, esi_corp_id) = match amb {
            Some(v) => v,
            None => return Ok(false), // no usable ambassador; retried next tick
        };

        token_rotation_char = amb_char_uuid;
        let client = token_store.authed_client_for(amb_char_uuid).await?;
        match client
            .corp_contract_items(esi_corp_id, row.esi_contract_id)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                budget.record_non_2xx();
                return Err(e.into());
            }
        }
    } else {
        return Ok(false); // character-only contract with no usable token; retried next tick
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

    let _ = token_store.persist_rotations(token_rotation_char).await;
    Ok(true)
}
