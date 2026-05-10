//! Corp contract poller.
//!
//! Each tick:
//! 1. Pick corps whose `contracts_next_poll_at` is due (cursor-based fan-out).
//! 2. For each corp, walk `corp_ambassadors` round-robin (skip disabled /
//!    recently-403'd) to get an authed client.
//! 3. Hit `/corporations/{cid}/contracts/`. Affiliation guard: if an ambassador
//!    returns a contract whose `issuer_corporation_id` doesn't match the corp,
//!    drop it.
//! 4. Resolve issuer/assignee via `domain::principals::resolve_contract_parties`.
//! 5. Upsert contracts; run matcher for new/changed ones; settle terminal ones.

use std::collections::HashSet;
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

use domain::principals::{resolve_contract_parties, EsiContractParties};
use domain::{Principal, PrincipalIndex, PrincipalKind};

use crate::Ctx;

pub(crate) const CORP_CONTRACTS_SCOPE: &str = "esi-contracts.read_corporation_contracts.v1";
/// Exponential back-off ceiling when all ambassadors fail: 24 hours.
const MAX_BACKOFF_SECS: u64 = 86_400;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping corp_contracts tick"
        );
        return Ok(());
    }

    let tick_secs = ctx.config.worker.tick_secs as f64;
    let interval_secs = ctx.config.esi.poll_intervals_secs.corp_contracts as f64;

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
          AND (contracts_next_poll_at IS NULL OR contracts_next_poll_at <= now())
        ORDER BY contracts_next_poll_at NULLS FIRST
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
        "corp_contracts tick"
    );

    let sem = Arc::new(Semaphore::new(ctx.config.worker.corp_contracts_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<Vec<UpsertedContract>>> = Vec::new();

    for corp in due {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        let interval = interval_secs as u64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            match poll_one_corp(
                &pool,
                &token_store,
                &budget,
                corp.id,
                corp.esi_corporation_id,
                interval,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        corp_id = %corp.id,
                        "corp_contracts poll failed"
                    );
                    Vec::new()
                }
            }
        }));
    }

    let mut all_upserts: Vec<UpsertedContract> = Vec::new();
    for h in tasks {
        if let Ok(v) = h.await {
            all_upserts.extend(v);
        }
    }

    // Sync items for any contracts still missing them (corp ambassador path).
    let synced_ids = super::contracts::sync_pending_items(ctx)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = ?e, "corp_contracts items sync failed");
            Vec::new()
        });

    // Run matcher for newly-upserted contracts plus those whose items just landed.
    let mut candidate_ids: Vec<Uuid> = all_upserts.iter().map(|u| u.contract_id).collect();
    for id in &synced_ids {
        if !candidate_ids.contains(id) {
            candidate_ids.push(*id);
        }
    }
    if !candidate_ids.is_empty() {
        if let Err(e) =
            super::contracts::r#match::run_for_contracts(&ctx.pool, &candidate_ids).await
        {
            tracing::warn!(error = ?e, "corp contract matcher failed");
        }
    }

    for u in all_upserts {
        if !u.status_changed {
            continue;
        }
        if let Err(e) = handle_terminal(ctx, &u).await {
            tracing::warn!(
                error = ?e,
                esi_contract_id = u.esi_contract_id,
                "corp terminal transition failed"
            );
        }
    }

    Ok(())
}

#[derive(Clone)]
struct UpsertedContract {
    contract_id: Uuid,
    esi_contract_id: i64,
    status: String,
    status_changed: bool,
}

async fn poll_one_corp(
    pool: &PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    corp_id: Uuid,
    esi_corp_id: i64,
    interval_secs: u64,
) -> anyhow::Result<Vec<UpsertedContract>> {
    if !budget.has_budget() {
        return Ok(Vec::new());
    }

    // Round-robin ambassadors: pick active ones ordered by last_used_at NULLS FIRST.
    // Skip any that recently errored (> 1 hour backoff).
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
    .bind(CORP_CONTRACTS_SCOPE)
    .fetch_all(pool)
    .await?;

    if ambassadors.is_empty() {
        tracing::warn!(
            corp_id = %corp_id,
            "no usable ambassadors for corp contracts"
        );
        advance_corp_cursor(pool, corp_id, interval_secs).await?;
        return Ok(Vec::new());
    }

    let mut contracts_opt: Option<Vec<EsiContract>> = None;
    let mut used_ambassador: Option<Uuid> = None;

    for amb in &ambassadors {
        let client = match token_store.authed_client_for(amb.character_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    char = %amb.character_id,
                    "failed to build client for corp ambassador"
                );
                continue;
            }
        };

        match client.corp_contracts(esi_corp_id).await {
            Ok(v) => {
                contracts_opt = Some(v);
                used_ambassador = Some(amb.character_id);
                let _ = sqlx::query(
                    "UPDATE corp_ambassadors SET last_used_at = now() \
                     WHERE corp_id = $1 AND character_id = $2",
                )
                .bind(corp_id)
                .bind(amb.character_id)
                .execute(pool)
                .await;
                break;
            }
            Err(EsiError::Api { status: 403, .. }) => {
                tracing::warn!(
                    corp_id = %corp_id,
                    char = %amb.character_id,
                    "ambassador 403 on corp contracts"
                );
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
                advance_corp_cursor(pool, corp_id, interval_secs).await?;
                return Err(e.into());
            }
        }
    }

    let contracts = match contracts_opt {
        Some(v) => v,
        None => {
            // All ambassadors failed.
            let _ = sqlx::query("UPDATE corps SET last_auth_error_at = now() WHERE id = $1")
                .bind(corp_id)
                .execute(pool)
                .await;
            advance_corp_cursor(pool, corp_id, interval_secs.min(MAX_BACKOFF_SECS)).await?;
            return Ok(Vec::new());
        }
    };

    if let Some(amb_id) = used_ambassador {
        let _ = token_store.persist_rotations(amb_id).await;
    }

    // Build principal index from DB.
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
        if c.contract_type != "item_exchange" {
            continue;
        }

        // Affiliation guard: keep contracts where the polled corp is issuer OR assignee
        // (a hauler may issue a contract *to* the corp for corp-funded reimbursements).
        if c.issuer_corporation_id != esi_corp_id && c.assignee_id != Some(esi_corp_id) {
            tracing::warn!(
                esi_contract_id = c.contract_id,
                issuer_corp = c.issuer_corporation_id,
                expected_corp = esi_corp_id,
                "corp affiliation mismatch, dropping"
            );
            continue;
        }

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: c.issuer_id,
                issuer_corporation_id: c.issuer_corporation_id,
                for_corporation: c.for_corporation,
                assignee_id: c.assignee_id,
            },
            &idx,
        );

        // Skip if neither side resolved.
        if parties.issuer_principal_id.is_none() && parties.assignee_principal_id.is_none() {
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
            esi_contract_id: c.contract_id,
            issuer_character_id: c.issuer_id,
            issuer_user_id,
            assignee_character_id: c.assignee_id,
            assignee_user_id,
            issuer_principal_id: parties.issuer_principal_id,
            assignee_principal_id: parties.assignee_principal_id,
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
            source_corp_id: Some(corp_id),
        };

        let outcome = settlement::upsert_contract(&mut tx, &upsert)
            .await
            .with_context(|| format!("corp upsert_contract esi={}", c.contract_id))?;

        let status_changed = outcome.status_changed();
        out.push(UpsertedContract {
            contract_id: outcome.contract_id,
            esi_contract_id: c.contract_id,
            status: outcome.current_status,
            status_changed,
        });
    }
    tx.commit().await?;

    advance_corp_cursor(pool, corp_id, interval_secs).await?;
    Ok(out)
}

async fn advance_corp_cursor(
    pool: &PgPool,
    corp_id: Uuid,
    interval_secs: u64,
) -> anyhow::Result<()> {
    let jitter = jitter_secs(interval_secs);
    let next = (interval_secs as i64) + jitter;
    sqlx::query(
        "UPDATE corps \
         SET contracts_last_polled_at = now(), \
             contracts_next_poll_at = now() + make_interval(secs => $1::double precision) \
         WHERE id = $2",
    )
    .bind(next as f64)
    .bind(corp_id)
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

/// Build a lightweight `PrincipalIndex` for a slice of EVE entity ids.
pub(crate) async fn build_principal_index(
    pool: &PgPool,
    eve_ids: &[i64],
) -> anyhow::Result<PrincipalIndex> {
    let mut idx = PrincipalIndex::default();
    if eve_ids.is_empty() {
        return Ok(idx);
    }

    // Characters → users.
    let char_rows: Vec<(i64, Uuid)> = sqlx::query_as(
        "SELECT character_id, user_id FROM characters WHERE character_id = ANY($1::bigint[])",
    )
    .bind(eve_ids)
    .fetch_all(pool)
    .await?;

    let user_ids: Vec<Uuid> = char_rows.iter().map(|(_, uid)| *uid).collect();
    let user_principals: Vec<(Uuid, Uuid)> = if user_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, user_id FROM principals \
             WHERE kind = 'user' AND user_id = ANY($1::uuid[])",
        )
        .bind(&user_ids)
        .fetch_all(pool)
        .await?
    };

    for (char_id, user_id) in char_rows {
        if let Some((pid, _)) = user_principals.iter().find(|(_, uid)| *uid == user_id) {
            idx.add_user(
                char_id,
                user_id,
                Principal {
                    id: *pid,
                    kind: PrincipalKind::User,
                    user_id: Some(user_id),
                    corp_id: None,
                },
            );
        }
    }

    // Corps.
    let corp_rows: Vec<(i64, Uuid)> = sqlx::query_as(
        "SELECT esi_corporation_id, id FROM corps \
         WHERE esi_corporation_id = ANY($1::bigint[])",
    )
    .bind(eve_ids)
    .fetch_all(pool)
    .await?;

    let corp_ids: Vec<Uuid> = corp_rows.iter().map(|(_, cid)| *cid).collect();
    let corp_principals: Vec<(Uuid, Uuid)> = if corp_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, corp_id FROM principals \
             WHERE kind = 'corp' AND corp_id = ANY($1::uuid[])",
        )
        .bind(&corp_ids)
        .fetch_all(pool)
        .await?
    };

    for (esi_corp_id, corp_id) in corp_rows {
        if let Some((pid, _)) = corp_principals.iter().find(|(_, cid)| *cid == corp_id) {
            idx.add_corp(
                esi_corp_id,
                corp_id,
                Principal {
                    id: *pid,
                    kind: PrincipalKind::Corp,
                    user_id: None,
                    corp_id: Some(corp_id),
                },
            );
        }
    }

    Ok(idx)
}

pub(crate) fn find_user_for_principal(idx: &PrincipalIndex, principal_id: Uuid) -> Option<Uuid> {
    idx.principal_by_user_id
        .values()
        .find(|p| p.id == principal_id)
        .and_then(|p| p.user_id)
}

async fn handle_terminal(ctx: &Ctx, u: &UpsertedContract) -> anyhow::Result<()> {
    let parsed: domain::ContractStatus = match u.status.parse() {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    if parsed.is_terminal_success() {
        let mut tx = ctx.pool.begin().await?;
        let bound: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM reimbursements \
             WHERE contract_id = $1 AND status = 'pending'",
        )
        .bind(u.contract_id)
        .fetch_one(&mut *tx)
        .await?;
        let settled = if bound > 0 {
            settlement::settle_via_contract(&mut tx, u.contract_id)
                .await
                .map_err(|e| anyhow::anyhow!("settle_via_contract: {e}"))?
        } else {
            vec![]
        };
        tx.commit().await?;
        super::webhooks::fire_settlement_webhooks(&ctx.pool, &ctx.webhook_http, settled).await;
    } else if parsed.is_terminal_failure() {
        let mut tx = ctx.pool.begin().await?;
        settlement::unbind_contract(&mut tx, u.contract_id)
            .await
            .map_err(|e| anyhow::anyhow!("unbind_contract: {e}"))?;
        tx.commit().await?;
    }
    Ok(())
}
